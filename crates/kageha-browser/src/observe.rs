use crate::cdp::{field, Cdp, Result};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

const MAX_FRAMES: usize = 8;
const MAX_NODES: usize = 80;
pub const MAX_CONTROLS: usize = 18;

#[derive(Clone)]
struct Frame {
    document: String,
    parent: Option<String>,
    url: String,
}

type Frames = BTreeMap<String, Frame>;

struct Attachment {
    target: String,
    session: String,
    frames: Frames,
}

pub async fn capture(
    cdp: &mut Cdp,
    browser: &str,
    target: &str,
    targets: &[Value],
) -> Result<Value> {
    let session = cdp.attach(target).await?;
    let (main, frames, mut truncated) = frame_tree(cdp, &session, MAX_FRAMES).await?;
    let mut attachments = vec![Attachment {
        target: target.into(),
        session,
        frames,
    }];
    let mut routes: BTreeMap<String, String> = attachments[0]
        .frames
        .keys()
        .map(|frame| (frame.clone(), target.into()))
        .collect();

    // Parent renderers do not always list out-of-process children in their frame tree.
    let mut covered = BTreeSet::from([target]);
    while let Some(info) = targets.iter().find(|info| {
        info["type"] == "iframe"
            && info["targetId"]
                .as_str()
                .is_some_and(|id| !covered.contains(id))
            && frame_parent(info).is_some_and(|id| routes.contains_key(id))
    }) {
        let id = field(info, "targetId")?;
        covered.insert(id);
        let remaining =
            MAX_FRAMES.saturating_sub(routes.len()) + usize::from(routes.contains_key(id));
        if remaining == 0 {
            truncated = true;
            continue;
        }
        let session = match cdp.attach(id).await {
            Ok(session) => session,
            Err("cdp_command_failed") => {
                truncated = true;
                continue;
            }
            Err(error) => return Err(error),
        };
        let (root, mut frames, cut) = match frame_tree(cdp, &session, remaining).await {
            Ok(tree) => tree,
            Err("cdp_command_failed") => {
                truncated = true;
                let _ = cdp.detach(&session).await;
                continue;
            }
            Err(error) => return Err(error),
        };
        truncated |= cut;
        if let Some(frame) = frames.get_mut(&root) {
            frame.parent = frame
                .parent
                .take()
                .or_else(|| frame_parent(info).map(str::to_owned));
        }
        routes.extend(frames.keys().map(|frame| (frame.clone(), id.to_owned())));
        attachments.push(Attachment {
            target: id.to_owned(),
            session,
            frames,
        });
    }

    let mut dom = Vec::new();
    let mut ax = Vec::new();
    let child_count = routes.len().saturating_sub(1);
    let main_limit = if child_count == 0 {
        MAX_NODES
    } else {
        MAX_NODES / 2
    };
    let child_limit = (MAX_NODES - main_limit) / child_count.max(1);
    let mut scopes = BTreeMap::new();
    for attachment in &attachments {
        let session = Some(attachment.session.as_str());
        let snapshot = match cdp
            .call(
                session,
                "DOMSnapshot.captureSnapshot",
                json!({"computedStyles": []}),
            )
            .await
        {
            Ok(snapshot) => snapshot,
            Err("cdp_command_failed") if attachment.target != target => {
                truncated = true;
                continue;
            }
            Err(error) => return Err(error),
        };
        let mut local_dom = Vec::new();
        let mut local_ax = Vec::new();
        let mut retained = Vec::new();
        for (frame_id, frame) in &attachment.frames {
            if routes.get(frame_id) != Some(&attachment.target) {
                continue;
            }
            let limit = if *frame_id == main {
                main_limit
            } else {
                child_limit
            };
            let tree = match cdp
                .call(
                    session,
                    "Accessibility.getFullAXTree",
                    json!({"frameId": frame_id}),
                )
                .await
            {
                Ok(tree) => tree,
                Err("cdp_command_failed") if *frame_id != main => {
                    truncated = true;
                    continue;
                }
                Err(error) => return Err(error),
            };
            let nodes = array(&tree["nodes"])?;
            // Chromium may represent an input's value as StaticText descendants,
            // even though the textbox value itself is already omitted. Exclude
            // those descendants before they reach either snapshots or text.
            let private_descendants = input_descendants(nodes);
            let visible = nodes.iter().filter(|node| node["ignored"] != true
                && !node["nodeId"].as_str().is_some_and(|id| private_descendants.contains(id)));
            let is_control = |node: &&Value| control_role(node["role"]["value"].as_str());
            let controls: BTreeSet<_> = visible
                .clone()
                .filter(is_control)
                .filter_map(|node| node["backendDOMNodeId"].as_u64())
                .take(limit)
                .collect();
            read_dom(
                &snapshot,
                frame_id,
                &frame.document,
                limit,
                &controls,
                &mut local_dom,
                &mut truncated,
            )?;
            // Keep actionable controls within the budget even when a modal
            // follows a long page of headings and explanatory text.
            let mut visible = visible
                .clone()
                .filter(is_control)
                .chain(visible.filter(|node| !is_control(node)));
            for node in visible.by_ref().take(limit) {
                let mut states = serde_json::Map::new();
                for name in [
                    "disabled", "readonly", "checked", "selected", "expanded", "required",
                ] {
                    if let Some(value) = node["properties"]
                        .as_array()
                        .and_then(|properties| {
                            properties.iter().find(|property| property["name"] == name)
                        })
                        .map(|property| &property["value"]["value"])
                    {
                        // AX tristates use strings; preserve only the bounded
                        // state vocabulary, never input values or arbitrary text.
                        if value.is_boolean()
                            || matches!(value.as_str(), Some("true" | "false" | "mixed"))
                        {
                            states.insert(name.to_owned(), value.clone());
                        }
                    }
                }
                // Presence metadata supports preserving draft fields without
                // publishing their contents (including secure input values).
                if let Some(present)=input_presence(node){states.insert("value_present".into(),json!(present));}
                local_ax.push(json!({
                    "frame_id": frame_id, "document_id": frame.document,
                    "role": bounded_text(&node["role"]["value"]),
                    "name": bounded_text(&node["name"]["value"]),
                    "identity_digest": accessibility_identity_digest(node),
                    "backend_node_id": node["backendDOMNodeId"].as_u64(),
                    "states": states,
                    "focused": node["properties"].as_array().is_some_and(|properties|
                        properties.iter().any(|property| property["name"] == "focused"
                            && property["value"]["value"] == true)),
                }));
            }
            truncated |= visible.next().is_some();
            retained.push(frame_id);
        }
        let (_, current, _) = match frame_tree(cdp, &attachment.session, MAX_FRAMES).await {
            Ok(tree) => tree,
            Err("cdp_command_failed") if attachment.target != target => {
                truncated = true;
                continue;
            }
            Err(error) => return Err(error),
        };
        for frame_id in retained {
            let frame = &attachment.frames[frame_id];
            if current
                .get(frame_id)
                .is_none_or(|now| now.document != frame.document)
            {
                if *frame_id == main {
                    return Err("document_changed");
                }
                truncated = true;
                continue;
            }
            scopes.insert(frame_id.clone(), json!({
                "frame_id": frame_id, "document_id": frame.document, "target_id": attachment.target,
                "parent_frame_id": frame.parent, "url": frame.url,
            }));
        }
        dom.extend(local_dom);
        ax.extend(local_ax);
    }

    // Drop observations whose parent disappeared during capture. Preserve the main document.
    loop {
        let missing: Vec<_> = scopes
            .iter()
            .filter(|(id, scope)| {
                **id != main
                    && scope["parent_frame_id"]
                        .as_str()
                        .is_none_or(|parent| !scopes.contains_key(parent))
            })
            .map(|(id, _)| id.clone())
            .collect();
        if missing.is_empty() {
            break;
        }
        truncated = true;
        for id in missing {
            scopes.remove(&id);
        }
    }
    let belongs = |item: &Value| {
        item["frame_id"]
            .as_str()
            .is_some_and(|id| scopes.contains_key(id))
    };
    dom.retain(belongs);
    ax.retain(belongs);
    ax.sort_by_key(|node| node["frame_id"] != main);
    let frames: BTreeMap<_, _> = scopes
        .iter()
        .map(|(id, scope)| (id, &scope["document_id"]))
        .collect();
    let (_, current, _) = frame_tree(cdp, &attachments[0].session, 1).await?;
    if current
        .get(&main)
        .zip(scopes.get(&main))
        .is_none_or(|(frame, scope)| scope["document_id"] != frame.document)
    {
        return Err("document_changed");
    }
    let refreshed = cdp.call(None, "Target.getTargets", json!({})).await?;
    for attachment in attachments.iter().rev() {
        match cdp.detach(&attachment.session).await {
            Ok(()) => (),
            Err("cdp_command_failed") if attachment.target != target => (),
            Err(error) => return Err(error),
        }
    }
    let main_document = scopes.get(&main).and_then(|s| s["document_id"].as_str()).unwrap_or("");
    Ok(json!({
        "browser_id": browser, "target_id": target, "frames": frames, "frame_scopes": scopes,
        "main_frame_id": main, "document_id": main_document,
        "url": array(&refreshed["targetInfos"])?.iter().find(|info| info["targetId"] == target)
            .map(|info| info["url"].as_str().unwrap_or("")).ok_or("target_not_owned")?,
        "dom": dom, "ax": ax, "truncated": truncated,
    }))
}

fn input_descendants(nodes: &[Value]) -> BTreeSet<String> {
    let mut parents: BTreeSet<String> = nodes.iter().filter(|node| matches!(node["role"]["value"].as_str(),
        Some("textbox" | "searchbox" | "spinbutton" | "combobox")))
        .filter_map(|node| node["nodeId"].as_str().map(str::to_owned)).collect();
    let mut descendants = BTreeSet::new();
    loop {
        let before = descendants.len();
        for node in nodes {
            let is_parent = node["nodeId"].as_str().is_some_and(|id| parents.contains(id));
            if is_parent {
                for id in node["childIds"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                    descendants.insert(id.to_owned());
                }
            }
            if node["parentId"].as_str().is_some_and(|id| parents.contains(id)) {
                if let Some(id) = node["nodeId"].as_str() { descendants.insert(id.to_owned()); }
            }
        }
        parents.extend(descendants.iter().cloned());
        if descendants.len() == before { break; }
    }
    descendants
}

fn frame_parent(info: &Value) -> Option<&str> {
    info["parentFrameId"]
        .as_str()
        .or_else(|| info["parentId"].as_str())
}

pub fn control_role(role: Option<&str>) -> bool {
    matches!(
        role,
        Some(
            "button"
                | "DisclosureTriangle"
                | "link"
                | "textbox"
                | "searchbox"
                | "combobox"
                | "checkbox"
                | "radio"
                | "listbox"
                | "tab"
                | "menuitem"
                | "menuitemcheckbox"
                | "menuitemradio"
                | "option"
                | "slider"
                | "spinbutton"
                | "treeitem"
                | "switch"
        )
    )
}

pub fn observed_controls(snapshot: &Value) -> impl Iterator<Item = (&Value, &Value)> {
    snapshot["ax"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|ax| {
            if !control_role(ax["role"].as_str()) {
                return None;
            }
            let backend = ax["backend_node_id"].as_u64()?;
            let dom = snapshot["dom"].as_array()?.iter().find(|dom| {
                dom["frame_id"] == ax["frame_id"]
                    && (dom["shadow_root_type"].is_null() || dom["shadow_root_type"] == "open")
                    && dom["backend_node_id"].as_u64() == Some(backend)
            })?;
            Some((ax, dom))
        })
}

fn input_presence(node:&Value)->Option<bool>{
    matches!(node["role"]["value"].as_str(),Some("textbox"|"searchbox"|"combobox"|"spinbutton")).then(||node["value"]["value"].as_str().map(|s|!s.is_empty())).flatten()
}

pub fn compact(snapshot: &Value) -> Value {
    summary(snapshot, MAX_CONTROLS, 3000, 10)
}

fn summary(snapshot: &Value, maximum: usize, budget: usize, text_limit: usize) -> Value {
    // Keep room in the host tool envelope for a navigation or action result.
    let mut remaining = budget;
    let mut fits = |item: &Value| {
        let size = item.to_string().len() + 1;
        if size > remaining {
            return false;
        }
        remaining -= size;
        true
    };
    let controls: Vec<_> = observed_controls(snapshot)
        .enumerate()
        .take(maximum)
        .map(|(index, (ax, dom))| {
            let mut control =
                json!({"ref": format!("e{}", index + 1), "role": ax["role"], "name": ax["name"]});
            if let Some(digest)=ax.get("identity_digest") { control["identity_digest"]=digest.clone(); }
            if let Some(states) = ax["states"].as_object() {
                for (name, value) in states {
                    control[name] = value.clone();
                }
            }
            if control.get("value_present").is_none() {
                if let Some(present) = dom["value_present"].as_bool() {
                    control["value_present"] = json!(present);
                }
            }
            for key in ["tag", "href", "options"] {
                if !dom[key].is_null() { control[key] = dom[key].clone(); }
            }
            if let Some(kind) = dom["input_type"].as_str() {
                control["input_type"] = json!(kind);
            }
            if dom["id"].as_str().is_some_and(|id| !id.is_empty()) {
                control["id"] = dom["id"].clone();
            }
            if dom["input_type"]
                .as_str()
                .is_some_and(|kind| kind.eq_ignore_ascii_case("password"))
            {
                control["secure_input"] = json!(true);
            }
            if ax["frame_id"] != snapshot["main_frame_id"] {
                control["frame_id"] = ax["frame_id"].clone();
            }
            if ax["focused"] == true {
                control["focused"] = json!(true);
            }
            control
        })
        .take_while(&mut fits)
        .collect();
    let text_nodes = snapshot["ax"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|ax| ax["frame_id"] == snapshot["main_frame_id"] && ax["role"] == "StaticText");
    let text: Vec<_> = text_nodes
        .clone()
        .take(text_limit)
        .map(|ax| ax["name"].clone())
        .take_while(&mut fits)
        .collect();
    let truncated = snapshot["truncated"] == true
        || observed_controls(snapshot).nth(controls.len()).is_some()
        || text_nodes.skip(text.len()).next().is_some();
    let frames: Vec<_> = snapshot["frame_scopes"]
        .as_object()
        .into_iter()
        .flat_map(|scopes| scopes.values())
        .filter(|frame| frame["frame_id"] != snapshot["main_frame_id"])
        .map(|frame| json!({"frame_id": frame["frame_id"], "url": frame["url"]}))
        .collect();
    let mut result = json!({"browser_id": snapshot["browser_id"], "target_id": snapshot["target_id"],
           "observation_id": snapshot["observation_id"], "url": snapshot["url"],
           "controls": controls, "text": text, "frames": frames,
           "truncated": truncated});
    if result["controls"]
        .as_array()
        .is_some_and(|controls| controls.iter().any(|c| c["secure_input"] == true))
    {
        result["managed_credentials"] = json!({
            "configured": std::env::var_os("KAGEHA_BROWSER_CREDENTIAL_DIR").is_some(),
            "script_version": 3,
            "create_or_reuse": "browser.credential(account)",
            "fill_password": "browser.fillCredential(uniqueLocator, saved.credential)",
            "scope": "current HTTPS origin and workspace",
            "plaintext_returned": false,
        });
    }
    result
}

pub fn expanded(mut snapshot: Value) -> Value {
    let mut summary = summary(&snapshot, MAX_NODES, 12_000, 40);
    if let Some(controls) = summary["controls"].as_array_mut() {
        for (control, (ax, dom)) in controls.iter_mut().zip(observed_controls(&snapshot)) {
            // Full clients can retain an exact identity while references are
            // renumbered by later observations. Document identity fences reuse.
            control["backend_node_id"] = ax["backend_node_id"].clone();
            control["document_id"] = dom["document_id"].clone();
        }
    }
    snapshot["controls"] = summary["controls"].take();
    snapshot["text"] = summary["text"].take();
    snapshot["truncated"] = summary["truncated"].take();
    if let Some(credentials) = summary.get("managed_credentials") {
        snapshot["managed_credentials"] = credentials.clone();
    }
    snapshot
}

async fn frame_tree(cdp: &mut Cdp, session: &str, limit: usize) -> Result<(String, Frames, bool)> {
    let response = cdp
        .call(Some(session), "Page.getFrameTree", json!({}))
        .await?;
    let root = &response["frameTree"];
    let mut pending = vec![(root, None::<String>)];
    let mut frames = Frames::new();
    let mut truncated = false;
    while let Some((tree, parent)) = pending.pop() {
        let frame = &tree["frame"];
        let (id, document) = match (field(frame, "id"), field(frame, "loaderId")) {
            (Ok(id), Ok(document)) => (id.to_owned(), document.to_owned()),
            _ if parent.is_some() => {
                truncated = true;
                continue;
            }
            _ => return Err("cdp_invalid_reply"),
        };
        frames.insert(
            id.clone(),
            Frame {
                document,
                parent: parent.or_else(|| frame["parentId"].as_str().map(str::to_owned)),
                url: frame["url"].as_str().unwrap_or("").to_owned(),
            },
        );
        if let Some(children) = tree["childFrames"].as_array() {
            pending.extend(children.iter().rev().map(|child| (child, Some(id.clone()))));
        }
        if frames.len() >= limit {
            break;
        }
    }
    Ok((
        field(&root["frame"], "id")?.to_owned(),
        frames,
        truncated || !pending.is_empty(),
    ))
}

fn read_dom(
    snapshot: &Value,
    frame_id: &str,
    document: &str,
    limit: usize,
    controls: &BTreeSet<u64>,
    output: &mut Vec<Value>,
    truncated: &mut bool,
) -> Result<()> {
    let strings = array(&snapshot["strings"])?;
    let string = |index: &Value| {
        index
            .as_u64()
            .and_then(|i| strings.get(i as usize))
            .unwrap_or(&Value::Null)
    };
    let end = output.len() + limit;
    for doc in array(&snapshot["documents"])? {
        let frame = string(&doc["frameId"])
            .as_str()
            .ok_or("cdp_invalid_reply")?;
        if frame != frame_id {
            continue;
        }
        let nodes = &doc["nodes"];
        let mut shadows = BTreeMap::new();
        if let (Some(indices), Some(values)) = (
            nodes["shadowRootType"]["index"].as_array(),
            nodes["shadowRootType"]["value"].as_array(),
        ) {
            for (index, value) in indices.iter().zip(values) {
                if let Some(index) = index.as_u64() {
                    shadows.insert(index as usize, bounded_text(string(value)));
                }
            }
        }
        let mut select_options = BTreeMap::<usize, Vec<String>>::new();
        for (index, node_name) in array(&nodes["nodeName"])?.iter().enumerate() {
            if string(node_name).as_str() != Some("OPTION") { continue; }
            let Some(parent) = nodes["parentIndex"][index].as_u64().map(|n| n as usize) else { continue };
            if string(&nodes["nodeName"][parent]).as_str() != Some("SELECT") ||
                !nodes["backendNodeId"][parent].as_u64().is_some_and(|id| controls.contains(&id)) ||
                nodes["attributes"][parent].as_array().into_iter().flatten().step_by(2).any(|key| string(key) == "multiple") { continue; }
            let attrs = nodes["attributes"][index].as_array().map(Vec::as_slice).unwrap_or_default();
            if attrs.chunks_exact(2).any(|p| string(&p[0]) == "disabled") { continue; }
            let options = select_options.entry(parent).or_default();
            for pair in attrs.chunks_exact(2) {
                if matches!(string(&pair[0]).as_str(), Some("label" | "value")) {
                    if let Some(v) = string(&pair[1]).as_str().filter(|v| !v.is_empty() && v.len() <= 256) {
                        if options.len() < 64 { options.push(v.to_owned()); }
                    }
                }
            }
            if nodes["parentIndex"][index+1] == index && string(&nodes["nodeName"][index+1]) == "#text" {
                if let Some(label) = string(&nodes["nodeValue"][index+1]).as_str().map(str::trim).filter(|v| !v.is_empty() && v.len() <= 256) {
                    if options.len() < 64 && !options.contains(&label.to_owned()) { options.push(label.to_owned()); }
                }
            }
        }
        let elements = array(&nodes["nodeType"])?
            .iter()
            .enumerate()
            .filter_map(|(index, ty)| (ty.as_u64() == Some(1)).then_some(index));
        let is_control = |index: &usize| {
            nodes["backendNodeId"][*index]
                .as_u64()
                .is_some_and(|id| controls.contains(&id))
        };
        for index in elements
            .clone()
            .filter(is_control)
            .chain(elements.filter(|index| !is_control(index)))
        {
            if output.len() == end {
                *truncated = true;
                return Ok(());
            }
            let attribute = |name| {
                nodes["attributes"][index].as_array().and_then(|pairs| {
                    pairs
                        .chunks_exact(2)
                        .find(|pair| string(&pair[0]) == name)
                        .map(|pair| bounded_text(string(&pair[1])))
                })
            };
            let tag = bounded_text(string(&nodes["nodeName"][index]));
            let href = if tag.eq_ignore_ascii_case("a") {
                nodes["attributes"][index].as_array().map(Vec::as_slice).unwrap_or_default()
                    .chunks_exact(2).find(|p| string(&p[0]) == "href")
                    .and_then(|p| url::Url::parse(string(&doc["baseURL"]).as_str()?).ok()?.join(string(&p[1]).as_str()?).ok())
                    .filter(|url| matches!(url.scheme(),"http" | "https") && url.username().is_empty()
                        && url.password().is_none() && url.as_str().len() <= 2048)
                    .map(|url| url.to_string())
            } else { None };
            output.push(json!({
                "frame_id": frame,
                "document_id": document,
                "tag": tag,
                "options": select_options.remove(&index),
                "href": href,
                "id": attribute("id"),
                "input_type": if tag.eq_ignore_ascii_case("input") { attribute("type") } else { None },
                "value_present": dom_input_presence(nodes, index, &tag, strings),
                "shadow_root_type": shadows.get(&index),
                "backend_node_id": nodes["backendNodeId"][index].as_u64(),
            }));
        }
    }
    Ok(())
}

// CDP's sparse inputValue can provide evidence when AX omits a value.
// An absent sparse entry remains unknown, never assumed empty.
// Keep only presence; never retain the value in the published observation.
fn dom_input_presence(nodes: &Value, index: usize, tag: &str, strings: &[Value]) -> Option<bool> {
    if !matches!(tag.to_ascii_lowercase().as_str(), "input" | "textarea") { return None; }
    let values = &nodes["inputValue"];
    let position = values["index"].as_array()?.iter().position(|v| v.as_u64() == Some(index as u64))?;
    let string_index = values["value"].get(position)?.as_u64()? as usize;
    Some(!strings.get(string_index)?.as_str()?.is_empty())
}

fn array(value: &Value) -> Result<&Vec<Value>> {
    value.as_array().ok_or("cdp_invalid_reply")
}

pub fn bounded_text(value: &Value) -> String {
    let text = value.as_str().unwrap_or("");
    let mut end = text.len().min(160);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end]
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

// Display labels are bounded and sanitized; action identity must retain changes
// beyond that display boundary, including whitespace and non-ASCII characters.
pub(crate) fn accessibility_identity_digest(node: &Value) -> String {
    use sha2::{Digest, Sha256};
    let identity = json!([node["role"]["value"], node["name"]["value"]]);
    data_encoding::HEXLOWER.encode(&Sha256::digest(identity.to_string().as_bytes()))
}

pub(crate) fn accessibility_identity_matches(current: &Value, observed: &Value) -> bool {
    if let Some(digest) = observed["identity_digest"].as_str() {
        digest == accessibility_identity_digest(current)
    } else {
        // Older observations retain the original exact, fail-closed behavior.
        current["role"]["value"] == observed["role"]
            && current["name"]["value"] == observed["name"]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_labels_keep_exact_accessibility_identity() {
        let prefix = "Long application question ".repeat(12);
        let node = json!({"role":{"value":"textbox"},"name":{"value":format!("{prefix} α\noriginal")}});
        let observed = json!({"role":bounded_text(&node["role"]["value"]),
            "name":bounded_text(&node["name"]["value"]),
            "identity_digest":accessibility_identity_digest(&node)});
        assert!(accessibility_identity_matches(&node, &observed));
        let mut changed = node.clone();
        changed["name"]["value"] = json!(format!("{prefix} α\nchanged"));
        assert_eq!(bounded_text(&node["name"]["value"]), bounded_text(&changed["name"]["value"]));
        assert!(!accessibility_identity_matches(&changed, &observed));
        changed = node.clone();
        changed["role"]["value"] = json!("combobox");
        assert!(!accessibility_identity_matches(&changed, &observed));
        let legacy = json!({"role":"textbox","name":"Short label"});
        assert!(accessibility_identity_matches(&json!({"role":{"value":"textbox"},"name":{"value":"Short label"}}), &legacy));
        assert!(!accessibility_identity_matches(&node, &legacy));
    }
    fn dense() -> Value {
        let ax: Vec<_> = (1..=28).map(|id| json!({"role":"button","name":format!("Choice {id}"),"backend_node_id":id,"frame_id":"main"})).collect();
        let dom: Vec<_> = (1..=28)
            .map(|id| json!({"backend_node_id":id,"document_id":"doc","frame_id":"main","shadow_root_type":null}))
            .collect();
        json!({"main_frame_id":"main","ax":ax,"dom":dom,"frame_scopes":{},"truncated":false})
    }
    #[test]
    fn expanded_observation_exposes_dense_controls_without_reassigning_refs() {
        let snapshot = dense();
        let short = compact(&snapshot);
        let full = expanded(snapshot);
        assert_eq!(short["controls"].as_array().unwrap().len(), MAX_CONTROLS);
        assert_eq!(full["controls"].as_array().unwrap().len(), 28);
        for (short, full) in short["controls"]
            .as_array()
            .unwrap()
            .iter()
            .zip(full["controls"].as_array().unwrap())
        {
            for (name, value) in short.as_object().unwrap() {
                assert_eq!(full[name], *value);
            }
        }
        assert_eq!(full["controls"][27]["backend_node_id"], 28);
        assert_eq!(full["controls"][27]["document_id"], "doc");
        assert_eq!(full["controls"][27]["ref"], "e28");
        assert_eq!(short["truncated"], true);
        assert_eq!(full["truncated"], false);
    }
    #[test]
    fn open_shadow_controls_are_exposed_but_closed_roots_remain_excluded() {
        let mut snapshot = dense();
        snapshot["dom"][0]["shadow_root_type"] = json!("open");
        snapshot["dom"][1]["shadow_root_type"] = json!("closed");
        let ids: Vec<_> = observed_controls(&snapshot)
            .map(|(_, dom)| dom["backend_node_id"].as_u64().unwrap())
            .collect();
        assert!(ids.contains(&1));
        assert!(!ids.contains(&2));
    }

    #[test]
    fn control_states_explain_readonly_and_checked_without_exposing_input_values() {
        let mut snapshot = dense();
        snapshot["ax"][0]["states"] = json!({"readonly":true,"checked":"mixed"});
        snapshot["ax"][0]["value"] = json!("private input content");
        snapshot["dom"][0]["value_present"] = json!(true);
        let result = compact(&snapshot);
        assert_eq!(result["controls"][0]["readonly"], true);
        assert_eq!(result["controls"][0]["checked"], "mixed");
        assert_eq!(result["controls"][0]["value_present"], true);
        assert!(!result.to_string().contains("private input content"));
    }
}

#[cfg(test)]
mod presence_tests {
 use super::*;
 #[test] fn filled_state_is_boolean_and_never_exposes_content(){
  assert_eq!(input_presence(&json!({"role":{"value":"textbox"},"value":{"value":"private fixture value"}})),Some(true));
  assert_eq!(input_presence(&json!({"role":{"value":"textbox"},"value":{"value":""}})),Some(false));
  assert_eq!(dom_input_presence(&json!({"inputValue":{"index":[2,3],"value":[0,1]}}),2,"INPUT",&[json!(""),json!("secret")]),Some(false));
  assert_eq!(dom_input_presence(&json!({"inputValue":{"index":[2,3],"value":[0,1]}}),3,"INPUT",&[json!(""),json!("secret")]),Some(true));
  assert_eq!(input_presence(&json!({"role":{"value":"button"},"value":{"value":"not a field"}})),None);
 }
}
