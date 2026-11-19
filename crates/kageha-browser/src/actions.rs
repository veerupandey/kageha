use crate::action_journal::{Action, BatchStep, Expected, Receipt, ReceiptState};
use crate::application_journal::{ActionBackend, ActionJournal};
use crate::cdp::{field, Cdp, Result};
use crate::keyboard::Key;
use crate::protocol::{now_ms, Request};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::time::Instant;

pub const METHODS: &[&str] = &[
    "close_tab",
    "click_observed",
    "navigate_tab",
    "fill_observed",
    "press_observed",
    "select_observed",
    "check_observed",
    "batch_observed",
    "batch_status",
    "action_receipt",
    "reconcile_click",
    "reconcile_input",
    "reconcile_upload",
    "pending_actions",
    "confirm_submission",
];

pub const DISPATCH_METHODS: &[&str] = &[
    "close_tab",
    "click_observed",
    "navigate_tab",
    "fill_observed",
    "press_observed",
    "select_observed",
    "check_observed",
    "batch_observed",
];

#[derive(Deserialize, Serialize)]
#[serde(untagged)]
pub enum Selection {
    One(String),
    Many(Vec<String>),
}

impl Selection {
    pub(crate) fn valid(&self) -> bool {
        let values = match self {
            Self::One(value) => std::slice::from_ref(value),
            Self::Many(values) => values.as_slice(),
        };
        values.len() <= 64
            && values
                .iter()
                .all(|value| value.len() <= 512 && !value.contains('\0'))
            && values
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                == values.len()
    }
}

#[cfg(test)]
mod selection_tests {
    use super::Selection;
    use serde_json::json;

    #[test]
    fn selection_accepts_existing_strings_and_bounded_unique_lists() {
        for value in [json!("a"), json!(["a", "c"]), json!([])] {
            assert!(serde_json::from_value::<Selection>(value).unwrap().valid());
        }
        for value in [json!(["a", "a"]), json!(["\u{0000}"]), json!(vec!["x"; 65])] {
            assert!(!serde_json::from_value::<Selection>(value).unwrap().valid());
        }
        for value in [json!(null), json!(false), json!(["a", 1])] {
            assert!(serde_json::from_value::<Selection>(value).is_err());
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    pub browser_id: String,
    pub target_id: String,
    pub observation_id: String,
    pub target: Option<String>,
    pub text: Option<String>,
    pub key: Option<String>,
    pub frame_id: Option<String>,
    pub option: Option<Selection>,
    pub checked: Option<bool>,
    pub otp_origin: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Click {
    #[serde(default)]
    pub await_confirmation: bool,
    pub submission_context: Option<Value>,
    pub browser_id: String,
    pub target_id: String,
    pub observation_id: String,
    pub backend_node_id: Option<u64>,
    pub target: Option<String>,
    #[serde(default)]
    pub expected_selector: String,
    #[serde(default)]
    pub expected_text: String,
    pub expected_url: Option<String>,
    pub timeout_ms: Option<u64>,
    pub button: Option<crate::panel::Button>,
    pub click_count: Option<u8>,
    pub gesture: Option<Gesture>,
    pub frame_id: Option<String>,
    pub(crate) dialog: Option<crate::dialog::Dialog>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Gesture {
    viewport: crate::panel::Size,
    input: crate::panel::Input,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Navigate {
    pub browser_id: String,
    pub target_id: String,
    pub observation_id: String,
    url: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reconcile {
    pub browser_id: String,
    pub target_id: String,
    pub observation_id: String,
    pub request_id: String,
    #[serde(default)]
    pub attachment_names: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptQuery {
    pub request_id: String,
}

pub(crate) struct Page {
    pub(crate) session: String,
    tab_session: String,
    pub(crate) context: u64,
    pub(crate) frame: String,
    pub document: String,
    pub(crate) tab_document: String,
    owners: Vec<FrameOwner>,
}

struct FrameOwner {
    session: String,
    frame: String,
    document: String,
    object: String,
}

impl Page {
    pub(crate) async fn inspect(
        &self,
        cdp: &mut Cdp,
        selector: &str,
        text: Option<&str>,
        offset: usize,
        limit: usize,
    ) -> Result<Value> {
        let args = json!({"selector": selector, "text": text, "offset": offset, "limit": limit});
        let expression = format!(
            r#"((query) => {{
            let count = 0, visible = false;
            const items = [];
            const safeSelector = (sel) => {{
                try {{
                    document.querySelectorAll(sel);
                    return sel;
                }} catch (e) {{
                    const normalized = (sel || '').replace(/#([0-9][a-zA-Z0-9_-]*)/g, '[id="$1"]');
                    try {{
                        document.querySelectorAll(normalized);
                        return normalized;
                    }} catch (e2) {{
                        return sel;
                    }}
                }}
            }};
            const selector = safeSelector(query.selector);
            for (const node of document.querySelectorAll(selector)) {{
                const isOption = node instanceof HTMLOptionElement;
                const shown = isOption
                    ? Boolean(node.parentElement && (node.parentElement.checkVisibility({{opacityProperty: true, visibilityProperty: true}}) || node.parentElement.isConnected))
                    : node.checkVisibility({{opacityProperty: true, visibilityProperty: true}});
                const optText = isOption ? (node.text || node.textContent || node.label || '').trim() : '';
                const rawText = shown ? (isOption ? optText : (node.innerText || '').trim()) : '';
                const found = query.text === null ? 0 : rawText.indexOf(query.text);
                if (found < 0) continue;
                const textStart = Math.max(0, found - 100);
                const label = rawText.slice(textStart, textStart + 800);
                count++;
                visible ||= shown;
                if (count > query.offset && items.length < query.limit) {{
                    const bounds = node.getBoundingClientRect();
                    const input = node instanceof HTMLInputElement;
                    const submits = (node instanceof HTMLButtonElement || input)
                        && (node.type === 'submit' || node.type === 'image') && node.form;
                    const formAction = submits ? (node.hasAttribute('formaction') ? node.formAction : node.form.action) : '';
                    const secure = input && node.type === 'password';
                    // One-time codes are frequently ordinary text inputs. Never publish
                    // their contents simply because the field is not type=password.
                    const oneTimeCode = input && (node.hasAttribute('data-kageha-protected-code') || ({verification_field})(node));
                    const reportsValue = (shown || isOption) && !oneTimeCode && ((input && ['text','email','search','tel','url','number'].includes(node.type))
                        || node instanceof HTMLTextAreaElement || node instanceof HTMLSelectElement || isOption);
                    const active = document.activeElement === node;
                    items.push({{
                    node, tag: node.localName, visible: shown, text: label, text_offset: textStart, text_truncated: rawText.length > textStart + 800 || textStart > 0,
                    focused: active,
                    ...(isOption ? {{
                        selected: node.selected,
                        value: (node.value || '').slice(0, 2048)
                    }} : {{}}),
                    ...(node instanceof HTMLSelectElement ? {{
                        selected_value: (node.value || '').slice(0, 2048),
                        selected_text: (node.selectedOptions[0]?.text || node.options[node.selectedIndex]?.text || '').trim().slice(0, 512),
                        selected_label: (node.selectedOptions[0]?.label || node.options[node.selectedIndex]?.label || '').trim().slice(0, 512),
                        options_count: node.options.length
                    }} : {{}}),
                    ...(input && node.type === 'file' ? {{unique_selector:(() => {{
                        // Hidden sibling upload controls can be indistinguishable
                        // by id/name/label. Return a checked CSS path instead of
                        // making the caller guess unsupported locator syntaxes.
                        const parts=[];
                        for(let current=node; current && parts.length<32; current=current.parentElement) {{
                            if(current.id) {{
                                const anchor='#'+CSS.escape(current.id);
                                if(document.querySelectorAll(anchor).length===1) {{parts.unshift(anchor);break;}}
                            }}
                            let index=1;
                            for(let sibling=current.previousElementSibling;sibling;sibling=sibling.previousElementSibling)
                                if(sibling.localName===current.localName) index++;
                            parts.unshift(CSS.escape(current.localName)+':nth-of-type('+index+')');
                        }}
                        const selector=parts.join(' > ');
                        if(selector.length>2048) return null;
                        const matches=document.querySelectorAll(selector);
                        return matches.length===1 && matches[0]===node ? selector : null;
                    }})()}} : {{}}),
                    ...(shown && node instanceof HTMLAnchorElement && /^https?:/.test(node.href) ? {{href:node.href.slice(0,2048)}} : {{}}),
                    ...(shown && /^https?:/.test(formAction) ? {{form_action:formAction.slice(0,2048)}} : {{}}),
                    role: node.getAttribute('role')?.slice(0, 80),
                    label: isOption ? (node.label || node.text || '').trim().slice(0, 512) : node.getAttribute('aria-label')?.slice(0, 180),
                    label_for: node instanceof HTMLLabelElement ? node.htmlFor.slice(0, 180) : null,
                    associated_labels: Array.from(node.labels || []).slice(0, 6)
                        .map(label => (label.innerText || label.textContent || '').trim().slice(0, 512)),
                    labelled_by: (node.getAttribute('aria-labelledby') || '').split(/\s+/).filter(Boolean).slice(0, 8)
                        .map(id => (document.getElementById(id)?.textContent || '').trim().slice(0, 512)),
                    ancestor_context: (() => {{
                        const contexts=[];
                        for(let parent=node.parentElement; parent && contexts.length<4 && !parent.matches('body,html'); parent=parent.parentElement) {{
                            contexts.push({{tag:parent.localName,id:parent.id?.slice(0,180),
                                role:parent.getAttribute('role')?.slice(0,80),
                                text:shown?(parent.innerText || '').trim().slice(0,1200):'',
                                control_count:parent.querySelectorAll('button,input:not([type=hidden]),textarea,select,[role=combobox]').length}});
                        }}
                        return contexts;
                    }})(),
                    id: node.id?.slice(0, 180),
                    name: node.getAttribute('name')?.slice(0, 180),
                    type: input ? node.type : node.getAttribute('type')?.slice(0, 80),
                    ...(input && ['radio','checkbox'].includes(node.type) ? {{
                        checked: node.checked,
                        visible_labels: Array.from(node.labels || [])
                            .filter(label => label.checkVisibility({{opacityProperty:true, visibilityProperty:true}}))
                            .slice(0, 4).map(label => label.innerText.trim().slice(0, 160)),
                    }} : {{}}),
                    aria_checked: node.getAttribute('aria-checked')?.slice(0,80),
                    aria_invalid: node.getAttribute('aria-invalid')?.slice(0,80),
                    required: node.required === true || node.getAttribute('aria-required') === 'true',
                    ...(shown && node.validity ? {{ validity: {{ valid:node.validity.valid,
                        value_missing:node.validity.valueMissing,type_mismatch:node.validity.typeMismatch,
                        pattern_mismatch:node.validity.patternMismatch,custom_error:node.validity.customError }},
                        ...(!secure && !oneTimeCode ? {{validation_message:(node.validationMessage||'').slice(0,512)}} : {{}}) }} : {{}}),
                    ...(secure && shown ? {{ secure_input: true }} : {{}}),
                    ...(oneTimeCode && shown ? {{ one_time_code_input: true, has_value: node.value.length > 0 }} : {{}}),
                    ...(reportsValue ? {{ has_value: node.value.length > 0, value:node.value.slice(0,2048), value_truncated:node.value.length>2048 }} : {{}}),
                    placeholder: node.getAttribute('placeholder')?.slice(0, 180),
                    disabled: node.matches(':disabled, [aria-disabled="true"]'),
                    bounds: {{ x: bounds.x, y: bounds.y, width: bounds.width, height: bounds.height }}
                }}); }}
            }}
            const end = query.offset + items.length;
            return {{count, visible, items, next_offset:end < count ? end : null,
                viewport: {{width: innerWidth, height: innerHeight}}}};
        }})({args})"#,
            verification_field = VERIFICATION_FIELD,
        );
        let reply = cdp
            .call(
                Some(&self.session),
                "Runtime.evaluate",
                json!({
                    "expression": expression, "contextId": self.context, "returnByValue": false,
                }),
            )
            .await?;
        if reply.get("exceptionDetails").is_some() {
            return Err("invalid_locator");
        }
        let object = field(&reply["result"], "objectId")?;
        let metadata = cdp.call(Some(&self.session), "Runtime.callFunctionOn", json!({
            "objectId":object,"returnByValue":true,
            "functionDeclaration":"function() { return {...this, items:this.items.map(({node,...item}) => item)}; }"
        })).await?;
        let mut result = metadata["result"]["value"].clone();
        let mut dom = Vec::new();
        let mut ax = Vec::new();
        for (index, item) in result["items"].as_array_mut().ok_or("cdp_invalid_reply")?.iter_mut().enumerate() {
            if item["visible"] != true { continue; }
            let node = cdp.call(Some(&self.session), "Runtime.callFunctionOn", json!({
                "objectId":object,"functionDeclaration":"function(index) { return this.items[index].node; }",
                "arguments":[{"value":index}]
            })).await?;
            let described = cdp.call(Some(&self.session), "DOM.describeNode", json!({
                "objectId":field(&node["result"], "objectId")?
            })).await?;
            let backend = described["node"]["backendNodeId"].as_u64().ok_or("cdp_invalid_reply")?;
            let tree = cdp.call(Some(&self.session), "Accessibility.getPartialAXTree", json!({
                "backendNodeId":backend,"fetchRelatives":false
            })).await?;
            if let Some(identity) = tree["nodes"].as_array().and_then(|nodes| nodes.iter()
                .find(|node| node["backendDOMNodeId"] == backend && node["ignored"] != true)) {
                let focused = item["focused"] == true
                    || identity["properties"].as_array().is_some_and(|properties| {
                        properties.iter().any(|property| {
                            property["name"] == "focused" && property["value"]["value"] == true
                        })
                    });
                item["ref"] = json!(format!("backend={backend}@{}", self.document));
                item["frame_id"] = json!(self.frame);
                dom.push(json!({"backend_node_id":backend,"document_id":self.document,"frame_id":self.frame,
                    "id":item["id"], "input_type":if item["tag"] == "input" { item["type"].clone() } else { Value::Null }}));
                ax.push(json!({"backend_node_id":backend,"document_id":self.document,"frame_id":self.frame,
                    "role":crate::observe::bounded_text(&identity["role"]["value"]),
                    "name":crate::observe::bounded_text(&identity["name"]["value"]),
                    "identity_digest":crate::observe::accessibility_identity_digest(identity),"states":{},"focused":focused}));
            }
        }
        let _ = cdp.call(Some(&self.session), "Runtime.releaseObject", json!({"objectId":object})).await;
        result["observed_dom"] = json!(dom);
        result["observed_ax"] = json!(ax);
        Ok(result)
    }

    pub(crate) async fn object(&self, cdp: &mut Cdp, backend: u64) -> Result<String> {
        let node = cdp
            .call(
                Some(&self.session),
                "DOM.resolveNode",
                json!({
                    "backendNodeId": backend, "executionContextId": self.context,
                }),
            )
            .await?;
        Ok(field(&node["object"], "objectId")?.to_owned())
    }

    pub(crate) async fn test_object(
        &self,
        cdp: &mut Cdp,
        object: &str,
        function: &str,
        args: Value,
    ) -> Result<bool> {
        let reply = cdp.call(Some(&self.session), "Runtime.callFunctionOn", json!({
            "objectId": object, "functionDeclaration": function, "arguments": args, "returnByValue": true,
        })).await?;
        reply["result"]["value"]
            .as_bool()
            .ok_or("element_state_unavailable")
    }

    pub async fn from_observation(cdp: &mut Cdp, target: &str, snapshot: &Value) -> Result<Self> {
        let session = cdp.attach(target).await?;
        let frame = main_frame(cdp, &session).await?;
        let frame_id = field(&frame, "id")?;
        let document = field(&frame, "loaderId")?;
        if snapshot["frames"][frame_id] != document {
            return Err("document_changed");
        }
        Self::in_frame(cdp, session, &frame).await
    }

    pub(crate) async fn from_scope(
        cdp: &mut Cdp,
        target: &str,
        snapshot: &Value,
        frame_id: Option<&str>,
    ) -> Result<Self> {
        let main = field(snapshot, "main_frame_id")?;
        let Some(frame_id) = frame_id.filter(|frame| *frame != main) else {
            return Self::from_observation(cdp, target, snapshot).await;
        };
        let root = Self::from_observation(cdp, target, snapshot).await?;
        let mut sessions = BTreeMap::from([(target.to_owned(), root.session.clone())]);
        let scopes = &snapshot["frame_scopes"];
        let mut chain = Vec::new();
        let mut id = frame_id;
        while id != main {
            if chain.len() >= 8 {
                return Err("frame_scope_unavailable");
            }
            let scope = &scopes[id];
            chain.push((
                id,
                field(scope, "target_id").map_err(|_| "frame_scope_unavailable")?,
            ));
            id = field(scope, "parent_frame_id").map_err(|_| "frame_scope_unavailable")?;
        }
        let mut parent = root;
        let mut owners = Vec::new();
        let tab_session = parent.session.clone();
        let tab_document = parent.document.clone();
        for (child, renderer) in chain.into_iter().rev() {
            let owner = cdp
                .call(
                    Some(&parent.session),
                    "DOM.getFrameOwner",
                    json!({"frameId": child}),
                )
                .await?;
            let backend = owner["backendNodeId"]
                .as_u64()
                .ok_or("frame_scope_unavailable")?;
            let object = parent.object(cdp, backend).await?;
            if !parent
                .test_object(
                    cdp,
                    &object,
                    "function() { return this.isConnected && this.ownerDocument === document; }",
                    json!([]),
                )
                .await?
            {
                return Err("frame_scope_unavailable");
            }
            owners.push(FrameOwner {
                session: parent.session,
                frame: parent.frame,
                document: parent.document,
                object,
            });
            let session = match sessions.get(renderer) {
                Some(session) => session.clone(),
                None => {
                    let session = cdp.attach(renderer).await?;
                    sessions.insert(renderer.to_owned(), session.clone());
                    session
                }
            };
            let frame = frame_by_id(cdp, &session, child).await?;
            if snapshot["frames"][child] != frame["loaderId"] {
                return Err("document_changed");
            }
            parent = Self::in_frame(cdp, session, &frame).await?;
        }
        parent.tab_session = tab_session;
        parent.tab_document = tab_document;
        parent.owners = owners;
        Ok(parent)
    }

    async fn in_frame(cdp: &mut Cdp, session: String, frame: &Value) -> Result<Self> {
        let frame_id = field(frame, "id")?;
        let document = field(frame, "loaderId")?;
        cdp.check_url(field(frame, "url")?)?;
        let world = cdp
            .call(
                Some(&session),
                "Page.createIsolatedWorld",
                json!({
                    "frameId": frame_id, "worldName": "kageha-fixture-actions"
                }),
            )
            .await?;
        let context = world["executionContextId"]
            .as_u64()
            .ok_or("cdp_invalid_reply")?;
        Ok(Self {
            tab_session: session.clone(),
            tab_document: document.to_owned(),
            session,
            context,
            frame: frame_id.to_owned(),
            document: document.to_owned(),
            owners: Vec::new(),
        })
    }

    pub(crate) async fn check_document(&self, cdp: &mut Cdp) -> Result<()> {
        self.current_frame(cdp).await.map(|_| ())
    }

    pub(crate) async fn current_frame(&self, cdp: &mut Cdp) -> Result<Value> {
        for owner in &self.owners {
            let frame = frame_by_id(cdp, &owner.session, &owner.frame).await?;
            if frame["loaderId"] != owner.document || cdp.check_url(field(&frame, "url")?).is_err()
            {
                return Err("document_changed");
            }
        }
        let frame = if self.owners.is_empty() {
            main_frame(cdp, &self.session).await?
        } else {
            frame_by_id(cdp, &self.session, &self.frame).await?
        };
        if frame["id"] != self.frame
            || frame["loaderId"] != self.document
            || cdp.check_url(field(&frame, "url")?).is_err()
        {
            return Err("document_changed");
        }
        Ok(frame)
    }

    pub async fn expected(&self, cdp: &mut Cdp, expected: &Expected) -> Result<bool> {
        let args = serde_json::to_string(expected).map_err(|_| "invalid_params")?;
        let expression = format!(
            r#"((expected) => {{
            const safeCss = sel => (sel || '').replace(/#([0-9][a-zA-Z0-9_-]*)/g, '[id="$1"]');
            const nodes = document.querySelectorAll(safeCss(expected.selector));
            if (nodes.length !== 1) return false;
            const node = nodes[0];
            return node.checkVisibility({{opacityProperty:true, visibilityProperty:true}})
                && (!expected.text || node.innerText?.trim() === expected.text);
        }})({args})"#
        );
        let result = cdp
            .call(
                Some(&self.session),
                "Runtime.evaluate",
                json!({
                    "expression": expression, "contextId": self.context, "returnByValue": true
                }),
            )
            .await?;
        result["result"]["value"]
            .as_bool()
            .ok_or("invalid_postcondition")
    }

    pub(crate) async fn scroll_to(
        &self,
        cdp: &mut Cdp,
        object: &str,
        allow_label: bool,
    ) -> Result<()> {
        let scrolled_label = allow_label
            && self.click_surface(cdp, object, true, true).await?["result"]["value"] == true;
        if !scrolled_label {
            scroll_object(cdp, &self.session, object).await?;
        }
        for owner in self.owners.iter().rev() {
            scroll_object(cdp, &owner.session, &owner.object).await?;
        }
        Ok(())
    }

    pub(crate) async fn detach(&self, cdp: &mut Cdp) -> Result<()> {
        let mut sessions = std::collections::BTreeSet::from([self.session.as_str()]);
        sessions.extend(self.owners.iter().map(|owner| owner.session.as_str()));
        for session in sessions {
            cdp.detach(session).await?;
        }
        Ok(())
    }

    fn receipt_frame(&self) -> (Option<String>, Option<String>) {
        if self.owners.is_empty() {
            (None, None)
        } else {
            (Some(self.frame.clone()), Some(self.document.clone()))
        }
    }

    async fn click_surface(
        &self,
        cdp: &mut Cdp,
        object: &str,
        allow_label: bool,
        scroll_label: bool,
    ) -> Result<Value> {
        cdp.call(Some(&self.session), "Runtime.callFunctionOn", json!({
            "objectId": object,
            "arguments": [{"value": allow_label}, {"value": scroll_label}],
            "returnByValue": true,
            "functionDeclaration": r#"function(allowLabel, scrollLabel) {
                for (let node = this; node; node = node.parentElement || node.getRootNode().host) {
                    if (node.matches(':disabled,[aria-disabled=true],[inert]')) return null;
                }
                // Script-backed anchors (for example calendar navigation) can
                // expose a link in the observation without having an href.
                if (!this.matches('button,a,input:not([type=hidden]),textarea,select,summary,[role=button],[role=radio],[role=checkbox],[role=combobox],[role=textbox],[role=searchbox],[role=link],[role=tab],[role=menuitem],[role=menuitemcheckbox],[role=menuitemradio],[role=option],[role=listbox],[role=slider],[role=spinbutton],[role=treeitem],[role=switch]')
                    || !this.isConnected || this.ownerDocument !== document
                    || this.closest(':disabled,[aria-disabled=true],[inert]')) return null;
                const visible = node => node.checkVisibility({opacityProperty:true,visibilityProperty:true})
                    && node.getClientRects().length > 0;
                let surface = this;
                if (!visible(this) || getComputedStyle(this).pointerEvents === 'none') {
                    if (!allowLabel || !this.matches('input[type=radio],input[type=checkbox]')) return null;
                    const labels = Array.from(this.labels).filter(label =>
                        label.control === this && visible(label)
                        && !label.closest('[aria-disabled=true],[inert]'));
                    if (labels.length !== 1) return null;
                    surface = labels[0];
                }
                if (scrollLabel) {
                    if (surface === this) return false;
                    surface.scrollIntoView({block:'center', inline:'nearest', behavior:'instant'});
                    return true;
                }
                const rect = surface.getBoundingClientRect();
                const left = Math.max(0, rect.left), right = Math.min(innerWidth, rect.right);
                const top = Math.max(0, rect.top), bottom = Math.min(innerHeight, rect.bottom);
                if (right <= left || bottom <= top) return null;
                // A center-only test rejects partially covered controls.
                // Find a point through read-only hit tests before dispatching
                // the single click; never probe by clicking an obstruction.
                const candidates = [[.5,.5],[.25,.5],[.75,.5],[.5,.25],[.5,.75],
                    [.1,.1],[.9,.1],[.1,.9],[.9,.9]];
                for (const [u,v] of candidates) {
                    const x = left + (right-left)*u, y = top + (bottom-top)*v;
                    let hit = document.elementFromPoint(x, y);
                    // A document hit is the host for an open shadow control.
                    while (hit?.shadowRoot) {
                        const inner = hit.shadowRoot.elementFromPoint(x, y);
                        if (!inner || inner === hit) break;
                        hit = inner;
                    }
                    if (!hit || !surface.contains(hit)) continue;
                    // A nested link/control can intercept label activation.
                    if (surface !== this && hit !== this && hit.closest(
                        'a[href],button,input,select,textarea,summary,[role=button],[role=link],[contenteditable]'
                    )) continue;
                    return {x,y};
                }
                return null;
            }"#
        })).await
    }

    pub(crate) async fn point(
        &self,
        cdp: &mut Cdp,
        object: &str,
        allow_label: bool,
    ) -> Result<(f64, f64)> {
        let result = self.click_surface(cdp, object, allow_label, false).await?;
        let mut point = coordinates(&result)?;
        for owner in self.owners.iter().rev() {
            let mapped = cdp.call(Some(&owner.session), "Runtime.callFunctionOn", json!({
                "objectId": owner.object, "arguments": [{"value": point.0}, {"value": point.1}],
                "returnByValue": true, "functionDeclaration": r#"function(x,y) {
                    if (!this.isConnected || !this.matches('iframe,frame')) return null;
                    for (let el = this; el; el = el.parentElement) {
                        const style = getComputedStyle(el);
                        if (style.transform !== 'none' || style.rotate !== 'none' ||
                            style.scale !== 'none' || style.perspective !== 'none' ||
                            Number(style.zoom) !== 1) return null;
                    }
                    const box = this.getBoundingClientRect();
                    const style = getComputedStyle(this);
                    x += box.left + this.clientLeft + parseFloat(style.paddingLeft);
                    y += box.top + this.clientTop + parseFloat(style.paddingTop);
                    return document.elementFromPoint(x,y) === this ? {x,y} : null;
                }"#,
            })).await?;
            point = coordinates(&mapped)?;
        }
        Ok(point)
    }

    pub(crate) async fn check_label(
        &self,
        cdp: &mut Cdp,
        node_id: u64,
        observed: &Value,
    ) -> Result<()> {
        let tree = cdp
            .call(
                Some(&self.session),
                "Accessibility.getPartialAXTree",
                json!({
                    "backendNodeId": node_id, "fetchRelatives": false,
                }),
            )
            .await?;
        let current = &tree["nodes"][0];
        if current["backendDOMNodeId"].as_u64() != Some(node_id)
            || current["ignored"] != false
            || !crate::observe::accessibility_identity_matches(current, observed)
        {
            return Err("element_identity_changed");
        }
        Ok(())
    }
}

async fn main_frame(cdp: &mut Cdp, session: &str) -> Result<Value> {
    let mut result = cdp
        .call(Some(session), "Page.getFrameTree", json!({}))
        .await?;
    Ok(result["frameTree"]["frame"].take())
}

async fn frame_by_id(cdp: &mut Cdp, session: &str, frame_id: &str) -> Result<Value> {
    let result = cdp
        .call(Some(session), "Page.getFrameTree", json!({}))
        .await?;
    let mut pending = vec![&result["frameTree"]];
    while let Some(node) = pending.pop() {
        if node["frame"]["id"] == frame_id {
            return Ok(node["frame"].clone());
        }
        if let Some(children) = node["childFrames"].as_array() {
            pending.extend(children);
        }
    }
    Err("document_changed")
}

async fn scroll_object(cdp: &mut Cdp, session: &str, object: &str) -> Result<()> {
    cdp.call(
        Some(session),
        "DOM.scrollIntoViewIfNeeded",
        json!({"objectId": object}),
    )
    .await
    .map(|_| ())
    .map_err(|error| {
        if error == "cdp_command_failed" {
            "element_not_actionable"
        } else {
            error
        }
    })?;
    // A control can be inside the viewport while covered by a sticky footer.
    // Center through ordinary scrolling, then keep the existing hit tests.
    cdp.call(Some(session), "Runtime.callFunctionOn", json!({
        "objectId":object,"returnByValue":true,
        "functionDeclaration":"function() { this.scrollIntoView({block:'center',inline:'nearest',behavior:'instant'}); return this.isConnected; }"
    })).await.map(|_| ())
}

fn coordinates(result: &Value) -> Result<(f64, f64)> {
    let point = &result["result"]["value"];
    point["x"]
        .as_f64()
        .zip(point["y"].as_f64())
        .ok_or("element_not_actionable")
}

pub(crate) fn frame_url(frame: &Value) -> Result<String> {
    Ok(format!(
        "{}{}",
        field(frame, "url")?,
        frame["urlFragment"].as_str().unwrap_or("")
    ))
}

pub(crate) fn valid_script_locator(target: &str) -> bool {
    !target.trim().is_empty()
        && target.len() <= 512
        // Each observation can renumber element references. Scripted operations
        // that take a fresh observation need a locator with stable meaning.
        && !target.strip_prefix('e').is_some_and(|n| {
            !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit())
        })
}

pub(crate) async fn resolve_target(
    cdp: &mut Cdp,
    page: &Page,
    snapshot: &Value,
    target: &str,
) -> Result<u64> {
    if target.is_empty() || target.len() > 512 {
        return Err("invalid_locator");
    }
    // A host can pin a scripted action to the exact approved DOM identity,
    // without reusing an e-number after the script's fresh observation.
    if let Some(identity) = target.strip_prefix("backend=") {
        let (backend, document) = identity.split_once('@').ok_or("invalid_locator")?;
        if document != page.document {
            return Err("document_changed");
        }
        let backend = backend.parse::<u64>().map_err(|_| "invalid_locator")?;
        observed_identity(snapshot, &page.frame, backend)?;
        return Ok(backend);
    }
    if let Some(index) = target
        .strip_prefix('e')
        .and_then(|value| value.parse::<usize>().ok())
    {
        return index
            .checked_sub(1)
            .and_then(|index| crate::observe::observed_controls(snapshot).nth(index))
            .filter(|(ax, _)| ax["frame_id"] == page.frame)
            .and_then(|(ax, _)| ax["backend_node_id"].as_u64())
            .ok_or("locator_not_found");
    }
    if let Some(role) = target.strip_prefix("role=") {
        let (role, name) = role.split_once(':').ok_or("invalid_locator")?;
        if !crate::observe::control_role(Some(role)) || name.is_empty() {
            return Err("invalid_locator");
        }
        let tree = cdp
            .call(
                Some(&page.session),
                "Accessibility.getFullAXTree",
                json!({"frameId": page.frame}),
            )
            .await?;
        let mut matches = tree["nodes"]
            .as_array()
            .ok_or("cdp_invalid_reply")?
            .iter()
            .filter(|node| {
                node["ignored"] != true
                    && node["role"]["value"] == role
                    && node["name"]["value"] == name
            });
        let node = matches.next().ok_or("locator_not_found")?;
        if matches.next().is_some() {
            return Err("locator_ambiguous");
        }
        return node["backendDOMNodeId"].as_u64().ok_or("locator_not_found");
    }
    let (kind, value) = target
        .split_once('=')
        .filter(|(kind, _)| matches!(*kind, "text" | "label" | "placeholder" | "testid"))
        .unwrap_or(("css", target));
    if value.trim().is_empty() {
        return Err("invalid_locator");
    }
    let locator = json!({"kind": kind, "value": value});
    let expression = format!(
        r#"((locator) => {{
        const {{kind, value}} = locator;
        const normalize = text => (text || '').trim().replace(/\s+/g, ' ');
        const read = node => normalize(node.innerText);
        let matches;
        if (kind === 'css') {{
            const safeCss = val => {{
                if (/^#[0-9][A-Za-z0-9_-]*$/.test(val)) return '#' + CSS.escape(val.slice(1));
                return (val || '').replace(/#([0-9][a-zA-Z0-9_-]*)/g, '[id="$1"]');
            }};
            matches = document.querySelectorAll(safeCss(value));
        }}
        else if (kind === 'text') {{
            matches = Array.from(document.querySelectorAll('body *')).filter(node =>
                node.checkVisibility() && read(node) === normalize(value)
                && !Array.from(node.children).some(child => read(child) === normalize(value)));
        }} else if (kind === 'label') {{
            matches = Array.from(document.querySelectorAll('input, textarea, select, button, [aria-label]')).filter(node =>
                normalize(node.getAttribute('aria-label')) === normalize(value)
                || Array.from(node.labels || []).some(label => read(label) === normalize(value)));
        }} else {{
            const attr = kind === 'placeholder' ? 'placeholder' : 'data-testid';
            matches = Array.from(document.querySelectorAll(`[${{attr}}]`)).filter(node => node.getAttribute(attr) === value);
        }}
        return matches.length === 1 ? matches[0] : matches.length;
    }})({locator})"#
    );
    let result = cdp
        .call(
            Some(&page.session),
            "Runtime.evaluate",
            json!({"expression": expression, "contextId": page.context}),
        )
        .await?;
    if result.get("exceptionDetails").is_some() {
        return Err("invalid_locator");
    }
    let Some(object) = result["result"]["objectId"].as_str() else {
        return Err(if result["result"]["value"] == 0 {
            "locator_not_found"
        } else {
            "locator_ambiguous"
        });
    };
    let dom = cdp
        .call(
            Some(&page.session),
            "DOM.describeNode",
            json!({"objectId": object}),
        )
        .await?;
    dom["node"]["backendNodeId"]
        .as_u64()
        .ok_or("cdp_invalid_reply")
}

fn observed_element(snapshot: &Value, frame: &str, backend: u64) -> Result<()> {
    if snapshot["dom"].as_array().is_some_and(|nodes| {
        nodes.iter().any(|node| {
            node["backend_node_id"].as_u64() == Some(backend)
                && node["frame_id"] == frame
                && (node["shadow_root_type"].is_null() || node["shadow_root_type"] == "open")
        })
    }) {
        Ok(())
    } else {
        Err("element_not_observed_in_main_frame")
    }
}

pub(crate) fn observed_identity<'a>(
    snapshot: &'a Value,
    frame: &str,
    backend: u64,
) -> Result<&'a Value> {
    observed_element(snapshot, frame, backend)?;
    snapshot["ax"]
        .as_array()
        .and_then(|nodes| {
            nodes.iter().find(|node| {
                node["backend_node_id"].as_u64() == Some(backend) && node["frame_id"] == frame
            })
        })
        .ok_or("element_accessibility_identity_missing")
}

// The site must identify the observed field as a verification input; never infer
// it from a model-provided selector alone. The marker is used only by redaction,
// never as evidence for authorizing an input.
const VERIFICATION_FIELD: &str = r#"function(node) {
    if (!(node instanceof HTMLInputElement)) return false;
    if ((node.autocomplete || '').toLowerCase().split(/\s+/).includes('one-time-code')) return true;
    const labels = Array.from(node.labels || []).map(label => label.textContent || '');
    const references = (node.getAttribute('aria-labelledby') || '').split(/\s+/).filter(Boolean)
        .map(id => node.ownerDocument.getElementById(id)?.textContent || '');
    const visibleLabels = [node.placeholder, node.getAttribute('aria-label'), ...labels, ...references].filter(Boolean);
    const desc = [node.id, node.name, ...visibleLabels]
        .filter(Boolean).join(' ').toLowerCase().replace(/[_\-]+/g, ' ');
    if (/\b(?:otp|passcode|one\s*time(?:\s*(?:password|passcode|code))?|(?:verification|verify|security|authentication|auth|login|sign\s*in|2fa|two\s*factor)\s*(?:code|pin)|(?:code|pin)\s*(?:verification|security|authentication))\b/.test(desc)) return true;
    // Generic labels like "Code" need nearby, visible verification context.
    const names = visibleLabels.length ? visibleLabels : [node.id, node.name];
    if (!names.some(name => /^(?:(?:your|enter|input)\s+)?(?:code|pin)[:\s]*$/i.test((name || '').trim()))) return false;
    const container = node.closest('fieldset,form,[role=group]') || node.parentElement;
    if (!container || !container.checkVisibility({opacityProperty:true,visibilityProperty:true})) return false;
    const context = (container.innerText || '').slice(0, 4000).toLowerCase();
    return /\b(?:verification|authenticate|authentication|security code|one[ -]?time|two[ -]?factor|2fa|(?:code|pin) (?:we (?:sent|emailed)|sent|emailed|from (?:your )?email))\b/.test(context);
}"#;

// A split-code group is accepted only when the selected target is its first
// input and one nearby, visible container has exactly the expected number of
// one-character editable inputs and explicit verification context. No value is
// retained in this plan; only references to the observed DOM nodes are held.
fn verification_group() -> String {
    format!(r#"function(node, length) {{
        if (!(node instanceof HTMLInputElement) || node.maxLength !== 1) return null;
        for (let parent = node.parentElement, depth = 0; parent && depth < 4 && !parent.matches('body,html'); parent = parent.parentElement, depth++) {{
            if (!parent.checkVisibility({{opacityProperty:true,visibilityProperty:true}})) continue;
            const fields = Array.from(parent.querySelectorAll('input,textarea,select')).filter(field =>
                field.checkVisibility({{opacityProperty:true,visibilityProperty:true}}) && !field.closest(':disabled,[aria-disabled=true],[inert]'));
            if (fields.length !== length || fields[0] !== node || fields.some(field =>
                !(field instanceof HTMLInputElement) || !['text','tel','password'].includes(field.type)
                || field.maxLength !== 1 || field.readOnly || field.ownerDocument !== document)) continue;
            const context = [parent.getAttribute('aria-label'), parent.getAttribute('id'),
                (parent.innerText || '').slice(0, 4000)].filter(Boolean).join(' ').toLowerCase().replace(/[_\-]+/g, ' ');
            if (!({VERIFICATION_FIELD})(node) && !/\b(?:otp|passcode|verification|authenticate|authentication|security code|one\s*time|two\s*factor|2fa|(?:code|pin) (?:we (?:sent|emailed)|sent|emailed|from (?:your )?email))\b/.test(context)) continue;
            return fields;
        }}
        return null;
    }}"#)
}

const EDITABLE: &str = r#"function(multiline, length) {
    return this.isConnected && this.ownerDocument === document && !this.readOnly
        && !this.closest(':disabled,[aria-disabled=true],[inert]')
        && this.checkVisibility({opacityProperty:true,visibilityProperty:true})
        && (this.maxLength < 0 || length <= this.maxLength)
        && (this instanceof HTMLTextAreaElement || (!multiline && this instanceof HTMLInputElement
            && ['text','search','email','tel','url','number'].includes(this.type)));
}"#;
const PASSWORD_INPUT: &str = r#"function() {
    return this.isConnected && this instanceof HTMLInputElement && this.type === 'password';
}"#;
const ENABLED_PATH: &str = r#"function() {
    for (let node = this; node; node = node.parentElement || node.getRootNode().host) {
        if (node.matches(':disabled,[aria-disabled=true],[inert]')) return false;
    }
    return this.isConnected;
}"#;
const FOCUSED: &str = r#"function() {
    return this.isConnected && this.ownerDocument === document && this.getRootNode().activeElement === this
        && !this.closest(':disabled,[aria-disabled=true],[inert]');
}"#;
const NON_SELECTABLE_INPUT:&str = "function() { return this instanceof HTMLInputElement && ['email','number'].includes(this.type); }";
const FOCUS_EMAIL:&str = "function() { this.focus(); return this.isConnected && this.getRootNode().activeElement === this; }";
const SELECT_VALUE: &str = r#"function() {
    this.focus();
    if (!this.isConnected || this.getRootNode().activeElement !== this || this.readOnly
        || this.closest(':disabled,[aria-disabled=true],[inert]')) return false;
    this.select();
    return this.getRootNode().activeElement === this && this.selectionStart === 0
        && this.selectionEnd === this.value.length;
}"#;
// A focus-trapping modal must be resolved by an explicit user-facing action,
// never bypassed by writing to a field underneath it. This diagnostic contains
// no field value or secret.
const FOCUS_BLOCKED_BY_MODAL: &str = r#"function() {
    const active = document.activeElement;
    return Array.from(document.querySelectorAll('[role="dialog"][aria-modal="true"], dialog:modal')).some(dialog =>
        dialog.isConnected && dialog.checkVisibility({opacityProperty:true,visibilityProperty:true})
        && !dialog.contains(this) && active && dialog.contains(active)
        && active.checkVisibility({opacityProperty:true,visibilityProperty:true}));
}"#;
// Unlike Input.insertText, this writes only to the previously observed node.
// Validate target, origin, focus, and selection in the same isolated-world task
// before touching the password. The input event is synthetic (isTrusted=false);
// a site's acceptance must be checked separately from DOM value readback.
const SET_CREDENTIAL_VALUE: &str = r#"function(value, origin) {
    if (!(this instanceof HTMLInputElement) || this.type !== 'password'
        || !this.isConnected || this.ownerDocument !== document || document.location.origin !== origin
        || this.readOnly || !this.checkVisibility({opacityProperty:true,visibilityProperty:true})
        || (this.maxLength >= 0 && value.length > this.maxLength)
        || this.getRootNode().activeElement !== this
        || this.selectionStart !== 0 || this.selectionEnd !== this.value.length) return false;
    for (let node = this; node; node = node.parentElement || node.getRootNode().host) {
        if (node.matches(':disabled,[aria-disabled=true],[inert]')) return false;
    }
    const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')?.set;
    if (!setter) return false;
    setter.call(this, value);
    // Never put a password in InputEvent.data. DOM access to the intended field
    // remains available to the site, as with any password entry.
    this.dispatchEvent(new InputEvent('input', {bubbles:true,composed:true,inputType:'insertText',data:null}));
    return true;
}"#;
const MATCHES_VALUE: &str = r#"function(expected) {
    return this.isConnected && this.ownerDocument === document && this.value === expected;
}"#;
const VALUE_SELECTED: &str = r#"function() {
    return this.isConnected && this.getRootNode().activeElement === this && !this.readOnly
        && !this.closest(':disabled,[aria-disabled=true],[inert]')
        && this.selectionStart === 0 && this.selectionEnd === this.value.length;
}"#;

// Use the same enabled option for admission, dispatch, postcondition and receipt evidence.
macro_rules! selection_query {
    ($body:literal) => {
        concat!(r#"function(value) {
            const values = Array.isArray(value) ? value : [value];
            if (!(this instanceof HTMLSelectElement) || !this.isConnected ||
                (!this.multiple && values.length !== 1) || this.closest(':disabled,[aria-disabled=true],[inert]')) return false;
            const options = Array.from(this.options).filter(opt => !opt.closest(':disabled,[aria-disabled=true],[inert]'));
            const norm = text => String(text).trim().toLowerCase();
            const identity = [this.autocomplete, this.id, this.name, this.getAttribute('aria-label'),
                ...Array.from(this.labels || [], label => label.textContent),
                ...(this.getAttribute('aria-labelledby') || '').split(/\s+/).map(id => document.getElementById(id)?.textContent)].join(' ');
            const country = /country/i.test(identity);
            const resolve = target => {
                const key = norm(target);
                let found = options.filter(opt => opt.value === target);
                if (!found.length) found = options.filter(opt => [opt.value,opt.label,opt.text].some(s => norm(s) === key));
                if (!found.length && country) {
                    const names = new Intl.DisplayNames(['en'], {type:'region',fallback:'none'});
                    found = options.filter(opt => /^[a-z]{2}$/i.test(opt.value) &&
                        norm(names.of(opt.value.toUpperCase()) || opt.value) === key);
                }
                return found.length === 1 ? found[0] : null;
            };
            const matched = values.map(resolve);
            if (matched.some(opt => !opt) || new Set(matched).size !== matched.length) return false;
            const alreadySelected = this.selectedOptions.length === matched.length && matched.every(opt => opt.selected);
        "#, $body, "}")
    };
}
const SELECTABLE: &str = selection_query!("return this.checkVisibility({opacityProperty:true,visibilityProperty:true});");
const CHOOSE_OPTION: &str = selection_query!(r#"
    if (alreadySelected) return true;
    if (this.multiple) {
        for (const option of this.options) option.selected = matched.includes(option);
    } else {
        this.selectedIndex = matched[0].index;
    }
    this.dispatchEvent(new Event('input', {bubbles:true, composed:true}));
    if (!this.isConnected) return false;
    this.dispatchEvent(new Event('change', {bubbles:true}));
    return true;
"#);
const MATCHES_SELECTION: &str = selection_query!("return alreadySelected;");
const RESOLVE_OPTION_VALUES: &str = selection_query!("return matched.map(opt => opt.value).sort();");
const CHECKABLE: &str = r#"function(checked) {
    return this instanceof HTMLInputElement
        && (this.type === 'checkbox' || (this.type === 'radio' && checked === true)) && this.isConnected
        && !this.indeterminate && !this.closest(':disabled,[aria-disabled=true],[inert]')
        && (this.checkVisibility({opacityProperty:true,visibilityProperty:true})
            || Array.from(this.labels || []).some(label => label.control === this
                && label.checkVisibility({opacityProperty:true,visibilityProperty:true})));
}"#;
const MATCHES_CHECKED: &str = r#"function(checked) {
    return this.isConnected && ['checkbox','radio'].includes(this.type) && !this.indeterminate && this.checked === checked;
}"#;

enum InputKind {
    Fill {
        target: String,
        text: zeroize::Zeroizing<String>,
        credential_origin: Option<String>,
        otp_origin: Option<String>,
    },
    Select {
        target: String,
        option: Selection,
    },
    Check {
        target: String,
        checked: bool,
    },
    Press(Key),
}

impl InputKind {
    fn expected_value(&self) -> Option<Value> {
        match self {
            Self::Fill { text, .. } => Some(json!(text.as_str())),
            Self::Check { checked, .. } => Some(json!(checked)),
            Self::Select { option, .. } => {
                let mut values = match option {
                    Selection::One(s) => vec![s.clone()],
                    Selection::Many(v) => v.clone(),
                };
                values.sort();
                Some(json!(values))
            }
            Self::Press(_) => None,
        }
    }
    fn parse(method: &str, input: Input) -> Result<(Self, String, String, Option<String>)> {
        if let Some(origin) = &input.otp_origin {
            if method != "fill_observed" || crate::credentials::origin(origin)? != *origin
                || input.text.as_ref().is_none_or(|text| !(4..=12).contains(&text.len()) || !text.bytes().all(|b| b.is_ascii_alphanumeric()))
            {
                return Err("verification_input_invalid");
            }
        }
        let kind = match (
            method,
            input.target,
            input.text,
            input.key,
            input.option,
            input.checked,
        ) {
            ("fill_observed", Some(target), Some(text), None, None, None)
                if text.len() <= 16_384 && !text.contains('\0') =>
            {
                Self::Fill {
                    target,
                    text: zeroize::Zeroizing::new(text.replace("\r\n", "\n").replace('\r', "\n")),
                    credential_origin: None,
                    otp_origin: input.otp_origin,
                }
            }
            ("select_observed", Some(target), None, None, Some(option), None) if option.valid() => {
                Self::Select { target, option }
            }
            ("check_observed", Some(target), None, None, None, Some(checked)) => {
                Self::Check { target, checked }
            }
            ("press_observed", None, None, Some(key), None, None) if key.len() <= 64 => {
                Self::Press(Key::parse(&key)?)
            }
            _ => return Err("invalid_params"),
        };
        Ok((kind, input.browser_id, input.target_id, input.frame_id))
    }

    async fn resolve(&self, cdp: &mut Cdp, page: &Page, snapshot: &Value) -> Result<u64> {
        if let Self::Fill { target, .. }
        | Self::Select { target, .. }
        | Self::Check { target, .. } = self
        {
            return resolve_target(cdp, page, snapshot, target).await;
        }
        let mut focused = snapshot["ax"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|node| node["frame_id"] == page.frame && node["focused"] == true)
            .filter_map(|node| node["backend_node_id"].as_u64())
            .filter(|backend| observed_element(snapshot, &page.frame, *backend).is_ok());
        let backend = focused.next().ok_or("focused_element_not_observed")?;
        if focused.next().is_some() {
            return Err("focused_element_not_observed");
        }
        Ok(backend)
    }

    async fn check(&self, cdp: &mut Cdp, page: &Page, object: &str) -> Result<()> {
        if matches!(self,Self::Fill{otp_origin:None,credential_origin:None,..}) {
            if page.test_object(cdp,object,"function(){return !!this.closest(':disabled,[aria-disabled=true],[inert]');}",json!([])).await? {return Err("input_disabled");}
            if page.test_object(cdp,object,"function(){return !!this.readOnly;}",json!([])).await? {return Err("input_readonly");}
            if page.test_object(cdp,object,"function(){return this instanceof HTMLInputElement && !['text','email','search','tel','url','password','number'].includes(this.type);}",json!([])).await? {return Err("unsupported_input_type");}
        }
        if !page
            .test_object(cdp, object, ENABLED_PATH, json!([]))
            .await?
        {
            return Err("element_not_actionable");
        }
        let group = verification_group();
        let verification_check = format!(r#"function(origin, length, numeric) {{
            return ((({VERIFICATION_FIELD})(this) && (this.maxLength < 0 || length <= this.maxLength))
                || ({group})(this, length) !== null)
                && ['text','tel','number','password'].includes(this.type)
                && (this.type !== 'number' || numeric)
                && this.isConnected && this.ownerDocument === document
                && document.location.origin === origin && !this.readOnly
                && !this.closest(':disabled,[aria-disabled=true],[inert]')
                && this.checkVisibility({{opacityProperty:true,visibilityProperty:true}});
        }}"#);
        let (check, args, error) = match self {
            Self::Fill { text, otp_origin: Some(origin), .. } => (
                verification_check.as_str(),
                json!([{"value":origin},{"value":text.encode_utf16().count()},{"value":text.bytes().all(|b| b.is_ascii_digit())}]),
                "verification_destination_invalid",
            ),
            Self::Fill {
                text,
                credential_origin: Some(origin),
                ..
            } => (
                r#"function(origin, length) {
                    return this instanceof HTMLInputElement && this.type === 'password'
                        && this.isConnected && this.ownerDocument === document
                        && document.location.origin === origin && !this.readOnly
                        && !this.closest(':disabled,[aria-disabled=true],[inert]')
                        && this.checkVisibility({opacityProperty:true,visibilityProperty:true})
                        && (this.maxLength < 0 || length <= this.maxLength);
                }"#,
                json!([{"value":origin},{"value":text.encode_utf16().count()}]),
                "credential_destination_invalid",
            ),
            Self::Fill { text, .. } => (
                EDITABLE,
                json!([{"value": text.contains('\n')}, {"value": text.encode_utf16().count()}]),
                "element_not_editable",
            ),
            Self::Select { option, .. } => (
                SELECTABLE,
                json!([{"value": option}]),
                "select_option_unavailable",
            ),
            Self::Check { checked, .. } => (
                CHECKABLE,
                json!([{"value": checked}]),
                "checkbox_unavailable",
            ),
            Self::Press(_) => (FOCUSED, json!([]), "focused_element_changed"),
        };
        if !page.test_object(cdp, object, check, args).await? {
            if matches!(
                self,
                Self::Fill {
                    credential_origin: None,
                    ..
                }
            ) && page
                .test_object(cdp, object, PASSWORD_INPUT, json!([]))
                .await?
            {
                return Err("secure_input_required");
            }
            if let Self::Fill { text, .. } = self {
                if text.contains('\n')
                    && page
                        .test_object(
                            cdp,
                            object,
                            "function() { return this instanceof HTMLInputElement; }",
                            json!([]),
                        )
                        .await?
                {
                    return Err("single_line_input_rejects_newlines");
                }
            }
            return Err(error);
        }
        if let Self::Fill {text,otp_origin:None,credential_origin:None,..}=self {
            if !cfg!(feature="numeric-inputs") && page.test_object(cdp,object,"function(){return this.type==='number';}",json!([])).await? {return Err("unsupported_input_type");}
            let numeric_check=format!("function(value) {{ if(this.type!=='number')return true; const canonical=({});return canonical(value)!==null; }}",crate::numeric::CANONICAL_JS);
            if !page.test_object(cdp,object,&numeric_check,json!([{"value":text.as_str()}])).await? {return Err("numeric_input_invalid");}
        }
        Ok(())
    }

    async fn focus_error(cdp: &mut Cdp, page: &Page, object: &str) -> &'static str {
        if page.test_object(cdp, object, FOCUS_BLOCKED_BY_MODAL, json!([])).await.unwrap_or(false) {
            "focus_blocked_by_modal"
        } else {
            "focused_element_changed"
        }
    }

    async fn prepare(&self, cdp: &mut Cdp, page: &Page, object: &str) -> Result<()> {
        if let Self::Check { checked, .. } = self {
            if !page
                .test_object(cdp, object, MATCHES_CHECKED, json!([{ "value": checked }]))
                .await?
            {
                page.scroll_to(cdp, object, true).await?;
                page.point(cdp, object, true).await?;
            }
        }
        if let Self::Fill { otp_origin: Some(_), text, .. } = self {
            // Keep inspection redacted if labels change. For a split group retain
            // node identities (never the code) for one journaled action.
            let group = verification_group();
            let prepare = format!(r#"function(length) {{
                const fields = ({group})(this, length);
                if (!fields) {{ this.setAttribute('data-kageha-protected-code',''); return false; }}
                this[Symbol.for('kageha.protected-code-group')] = Object.freeze(fields.slice());
                fields.forEach(field => field.setAttribute('data-kageha-protected-code',''));
                fields[0].focus(); fields[0].select();
                return fields[0].getRootNode().activeElement === fields[0] && fields[0].selectionStart === 0 && fields[0].selectionEnd === fields[0].value.length;
            }}"#);
            if page.test_object(cdp, object, &prepare, json!([{"value":text.len()}])).await? {
                page.check_document(cdp).await?;
                self.check(cdp, page, object).await?;
                return Ok(());
            }
            // A malformed split group is rejected by check before preparation.
        }
        if let Self::Fill { .. } = self {
            if page.test_object(cdp,object,NON_SELECTABLE_INPUT,json!([])).await? {
                if !page.test_object(cdp,object,FOCUS_EMAIL,json!([])).await? {return Err(Self::focus_error(cdp, page, object).await);}
                page.check_document(cdp).await?;self.check(cdp,page,object).await?;return Ok(());
            }
            if !page
                .test_object(cdp, object, SELECT_VALUE, json!([]))
                .await?
            {
                return Err(Self::focus_error(cdp, page, object).await);
            }
            page.check_document(cdp).await?;
            self.check(cdp, page, object).await?;
            if !page
                .test_object(cdp, object, VALUE_SELECTED, json!([]))
                .await?
            {
                return Err(Self::focus_error(cdp, page, object).await);
            }
        }
        Ok(())
    }

    async fn send(
        &self,
        cdp: &mut Cdp,
        page: &Page,
        object: &str,
        sent: &AtomicBool,
    ) -> Result<()> {
        match self {
            Self::Fill { text, otp_origin, credential_origin, .. } => {
                if let Some(origin) = credential_origin {
                    // Conservative journal semantics: failure after dispatch is
                    // uncertain, even when the target rejected the update.
                    sent.store(true, Ordering::Relaxed);
                    if !page.test_object(cdp, object, SET_CREDENTIAL_VALUE, json!([
                        {"value": text.as_str()}, {"value": origin}
                    ])).await? {
                        return Err("credential_target_changed");
                    }
                    return Ok(());
                }
                if otp_origin.is_some() {
                    let group = verification_group();
                    let is_group = "function() { return Array.isArray(this[Symbol.for('kageha.protected-code-group')]); }";
                    if page.test_object(cdp, object, is_group, json!([])).await? {
                        let select = format!(r#"function(index, length) {{
                            const saved = this[Symbol.for('kageha.protected-code-group')];
                            const current = ({group})(this, length);
                            if (!saved || !current || saved.length !== current.length || saved.some((field,i) => field !== current[i])) return false;
                            const field = saved[index];
                            if (!field || !field.isConnected || field.ownerDocument !== document) return false;
                            field.focus(); field.select();
                            return field.getRootNode().activeElement === field && field.selectionStart === 0 && field.selectionEnd === field.value.length;
                        }}"#);
                        for (index, ch) in text.chars().enumerate() {
                            page.check_document(cdp).await?;
                            self.check(cdp, page, object).await?;
                            if !page.test_object(cdp, object, &select, json!([{"value":index},{"value":text.len()}])).await? {
                                return Err("verification_group_changed");
                            }
                            page.check_document(cdp).await?;
                            sent.store(true, Ordering::Relaxed);
                            cdp.call(Some(&page.tab_session), "Input.insertText", json!({"text":ch.to_string()})).await?;
                        }
                        return Ok(());
                    }
                }
                if page.test_object(cdp,object,NON_SELECTABLE_INPUT,json!([])).await? {
                    if !page.test_object(cdp,object,FOCUSED,json!([])).await? {return Err("focused_element_changed");}
                    // Email and number inputs have no selectionStart/selectionEnd API. Use
                    // Chromium's trusted editor selection command after the
                    // durable dispatch marker, then verify the actual value.
                    sent.store(true, Ordering::Relaxed);
                    cdp.call(Some(&page.tab_session),"Input.dispatchKeyEvent",json!({"type":"rawKeyDown","key":"a","code":"KeyA","windowsVirtualKeyCode":65,"modifiers":if cfg!(target_os="macos"){4}else{2},"commands":["selectAll"]})).await?;
                    cdp.call(Some(&page.tab_session),"Input.dispatchKeyEvent",json!({"type":"keyUp","key":"a","code":"KeyA","windowsVirtualKeyCode":65,"modifiers":if cfg!(target_os="macos"){4}else{2}})).await?;
                    page.check_document(cdp).await?;
                    if !page.test_object(cdp,object,FOCUSED,json!([])).await? {return Err("focused_element_changed");}
                }
                if text.is_empty() {
                    if page.test_object(cdp,object,NON_SELECTABLE_INPUT,json!([])).await? || !page
                        .test_object(cdp, object, MATCHES_VALUE, json!([{"value": ""}]))
                        .await?
                    {
                        let key = Key::parse("Backspace")?;
                        sent.store(true, Ordering::Relaxed);
                        key.send(cdp, &page.tab_session, true).await?;
                        page.check_document(cdp).await?;
                        key.send(cdp, &page.tab_session, false).await?;
                    }
                } else {
                    sent.store(true, Ordering::Relaxed);
                    cdp.call(
                        Some(&page.tab_session),
                        "Input.insertText",
                        json!({"text": text}),
                    )
                    .await?;
                }
            }
            Self::Select { option, .. } => {
                sent.store(true, Ordering::Relaxed);
                if !page
                    .test_object(cdp, object, CHOOSE_OPTION, json!([{"value": option}]))
                    .await?
                {
                    return Err("select_option_unavailable");
                }
            }
            Self::Check { checked, .. } => {
                if page
                    .test_object(cdp, object, MATCHES_CHECKED, json!([{"value": checked}]))
                    .await?
                {
                    return Ok(());
                }
                // The native input may be visually hidden by a styled label.
                // Use the same hit-tested associated surface as a normal click;
                // never mutate checked state or force input through an overlay.
                page.scroll_to(cdp, object, true).await?;
                let (x, y) = page.point(cdp, object, true).await?;
                page.check_document(cdp).await?;
                self.check(cdp, page, object).await?;
                sent.store(true, Ordering::Relaxed);
                for event in ["mousePressed", "mouseReleased"] {
                    cdp.call(
                        Some(&page.tab_session),
                        "Input.dispatchMouseEvent",
                        json!({
                            "type":event,"x":x,"y":y,"button":"left","clickCount":1
                        }),
                    )
                    .await?;
                }
            }

            Self::Press(key) => {
                sent.store(true, Ordering::Relaxed);
                key.send(cdp, &page.tab_session, true).await?;
                page.check_document(cdp).await?;
                key.send(cdp, &page.tab_session, false).await?;
            }
        }
        Ok(())
    }
}

pub async fn input(
    cdp: &mut Cdp,
    root: &Path,
    backend: ActionBackend,
    request: &Request,
    input: Input,
    snapshot: &Value,
    dispatched: &AtomicBool,
) -> Result<Value> {
    recover_pending(
        cdp,
        root,
        backend,
        request,
        &input.browser_id,
        &input.target_id,
        snapshot,
    )
    .await?;
    let (kind, browser_id, target_id, frame_id) = InputKind::parse(&request.method, input)?;
    let mut journal = ActionJournal::open(root, backend, request, true).await?;
    input_step(
        cdp,
        &mut journal,
        request,
        kind,
        browser_id,
        target_id,
        frame_id,
        snapshot,
        dispatched,
        None,
    )
    .await
}

/// Only the vault can supply this value. It never enters request params, receipts,
/// scripts, model results, or snapshots. Input uses the usual authority and journal.
pub(crate) async fn credential_input(
    cdp: &mut Cdp,
    journal: &mut ActionJournal<'_>,
    request: &Request,
    browser_id: String,
    target_id: String,
    target: String,
    origin: String,
    secret: zeroize::Zeroizing<String>,
    snapshot: &Value,
    dispatched: &AtomicBool,
    step: BatchStep,
) -> Result<Value> {
    input_step(
        cdp,
        journal,
        request,
        InputKind::Fill {
            target,
            text: secret,
            credential_origin: Some(origin),
            otp_origin: None,
        },
        browser_id,
        target_id,
        None,
        snapshot,
        dispatched,
        Some(step),
    )
    .await
}

pub(crate) async fn batch_input(
    cdp: &mut Cdp,
    journal: &mut ActionJournal<'_>,
    request: &Request,
    input: Input,
    snapshot: &Value,
    dispatched: &AtomicBool,
    step: BatchStep,
) -> Result<Value> {
    let (kind, browser_id, target_id, frame_id) = InputKind::parse(&request.method, input)?;
    input_step(
        cdp,
        journal,
        request,
        kind,
        browser_id,
        target_id,
        frame_id,
        snapshot,
        dispatched,
        Some(step),
    )
    .await
}

async fn input_step(
    cdp: &mut Cdp,
    journal: &mut ActionJournal<'_>,
    request: &Request,
    kind: InputKind,
    browser_id: String,
    target_id: String,
    frame_id: Option<String>,
    snapshot: &Value,
    dispatched: &AtomicBool,
    batch: Option<BatchStep>,
) -> Result<Value> {
    journal.authorize(request).await?;
    let page = Page::from_scope(cdp, &target_id, snapshot, frame_id.as_deref()).await?;
    let (frame_id, frame_document_id) = page.receipt_frame();
    let backend = kind.resolve(cdp, &page, snapshot).await?;
    let observed = observed_identity(snapshot, &page.frame, backend)?;
    page.check_label(cdp, backend, observed).await?;
    let object = page.object(cdp, backend).await?;
    kind.check(cdp, &page, &object).await?;
    // A digest of a short code can be brute-forced even with a random salt.
    // Do not persist any value-derived evidence for protected OTP input.
    let resolved_expected = match &kind {
        InputKind::Select { option, .. } => {
            let res = cdp.call(Some(&page.session), "Runtime.callFunctionOn", json!({
                "objectId": object,
                "functionDeclaration": RESOLVE_OPTION_VALUES,
                "arguments": [{"value": option}],
                "returnByValue": true,
            })).await;
            if let Ok(val) = res {
                val.get("result").and_then(|r| r.get("value")).filter(|v| v.is_array()).cloned()
            } else {
                None
            }
        }
        _ => None,
    };
    let input_evidence = (!matches!(&kind, InputKind::Fill { otp_origin: Some(_), .. }))
        .then(|| resolved_expected.or_else(|| kind.expected_value())).flatten().map(|mut value| {
        let evidence =
            crate::input_evidence::Evidence::new(backend, observed, &snapshot["url"], &value,
                if snapshot["dom"].as_array().is_some_and(|nodes|nodes.iter().any(|node|node["backend_node_id"]==backend && node["frame_id"]==page.frame && node["input_type"]=="number")) {crate::input_evidence::Comparison::NumericDecimalV1}else{crate::input_evidence::Comparison::Text});
        if let Value::String(v) = &mut value {
            use zeroize::Zeroize;
            v.zeroize();
        }
        evidence
    });
    if request.deadline_ms.saturating_sub(now_ms()) < 1000 {
        return Err("input_deadline_budget_exhausted");
    }
    let receipt = Receipt {
        request_id: request.request_id.clone(),
        workspace_id: request.workspace_id.clone(),
        client_id: request.client_id.clone(),
        service_epoch: request.service_epoch.clone().ok_or("stale_service_epoch")?,
        browser_id,
        target_id,
        document_id: page.tab_document.clone(),
        frame_id,
        frame_document_id,
        result_document_id: None,
        action: match kind {
            InputKind::Fill { .. } => Action::Fill,
            InputKind::Press(_) => Action::Press,
            InputKind::Select { .. } => Action::Select,
            InputKind::Check { .. } => Action::Check,
        },
        expected: None,
        input_evidence,
        upload_evidence: None,
        batch,
        plan: None,
        submission_context: None, confirmation: None, resolution: None, confirmation_source: None,
        state: ReceiptState::Dispatching,
    };
    let scope = (&receipt.workspace_id, &receipt.request_id);
    journal.check_new(&receipt).await?;
    // A new fill must deliver input events even when the DOM already has the
    // same value: reactive application state can differ from the DOM property.
    // Lost-acknowledgment recovery remains a separate read-only operation.
    kind.prepare(cdp, &page, &object).await?;
    let preflight = async {
        page.check_document(cdp).await?;
        if kind.resolve(cdp, &page, snapshot).await? != backend {
            return Err("locator_changed");
        }
        page.check_label(cdp, backend, observed).await?;
        journal.authorize(request).await?;
        kind.check(cdp, &page, &object).await?;
        if request.deadline_ms <= now_ms() {
            return Err("deadline_exceeded");
        }
        Ok(())
    }
    .await;
    if let Err(error) = preflight {
        return Err(error);
    }
    if request.deadline_ms.saturating_sub(now_ms()) < 750 {
        return Err("input_deadline_budget_exhausted");
    }
    journal.begin(receipt.clone()).await?;
    dispatched.store(true, Ordering::Relaxed);
    let sent = AtomicBool::new(false);
    {
        if let Err(error) = kind.send(cdp, &page, &object, &sent).await {
            if !sent.load(Ordering::Relaxed) {
                journal
                    .finish(scope.0, scope.1, ReceiptState::NotDispatched)
                    .await?;
                dispatched.store(false, Ordering::Relaxed);
            }
            return Err(error);
        }
    }
    journal
        .finish(scope.0, scope.1, ReceiptState::Returned)
        .await?;
    let verification = match &kind {
        InputKind::Fill { text, otp_origin, .. } => {
            page.check_document(cdp).await?;
            let check = if otp_origin.is_some() {
                let group = verification_group();
                format!(r#"function(expected) {{
                    const saved = this[Symbol.for('kageha.protected-code-group')];
                    if (!saved) return this.isConnected && this.ownerDocument === document && this.value === expected;
                    const current = ({group})(this, expected.length);
                    return !!current && saved.length === current.length && saved.every((field,i) => field === current[i] && field.value === expected[i]);
                }}"#)
            } else { format!("function(expected) {{ if(!this.isConnected||this.ownerDocument!==document)return false; if(this.type!=='number')return this.value===expected;const canonical=({}); const actual=canonical(this.value);return actual!==null&&actual===canonical(expected); }}",crate::numeric::CANONICAL_JS) };
            if !page.test_object(cdp, &object, &check, json!([{"value": text}])).await? {
                return Err("fill_value_unverified");
            }
            "value_readback"
        }
        InputKind::Select { option, .. } => {
            page.check_document(cdp).await?;
            if !page
                .test_object(cdp, &object, MATCHES_SELECTION, json!([{"value": option}]))
                .await?
            {
                return Err("selected_values_unverified");
            }
            match option {
                Selection::One(_) => "value_readback",
                Selection::Many(_) => "selected_values_readback",
            }
        }
        InputKind::Check { checked, .. } => {
            page.check_document(cdp).await?;
            if !page
                .test_object(cdp, &object, MATCHES_CHECKED, json!([{"value": checked}]))
                .await?
            {
                return Err("checked_state_unverified");
            }
            "checked_state"
        }
        InputKind::Press(_) => "key_delivery",
    };
    let field_validation=if matches!(kind,InputKind::Fill{otp_origin:None,credential_origin:None,..}) {
        let read=cdp.call(Some(&page.session),"Runtime.callFunctionOn",json!({"objectId":object,"functionDeclaration":"function(){if(!this.isConnected||this.ownerDocument!==document)throw Error('field changed');if(this.type!=='number')return null;const v=this.validity;return {input_type:'number',valid:v.valid,bad_input:v.badInput,range_underflow:v.rangeUnderflow,range_overflow:v.rangeOverflow,step_mismatch:v.stepMismatch,value_missing:v.valueMissing,min:this.min,max:this.max,step:this.step};}","returnByValue":true})).await?;
        page.check_document(cdp).await?;
        if read.get("exceptionDetails").is_some() {return Err("input_readback_unavailable");}
        read["result"]["value"].clone()
    }else{Value::Null};
    let finished = journal
        .finish(scope.0, scope.1, ReceiptState::Verified)
        .await?;
    page.detach(cdp).await?;
    Ok(
        json!({"receipt": finished, "verification": verification,"input_sent":sent.load(Ordering::Relaxed),"field_validation":field_validation}),
    )
}

pub async fn navigate(
    cdp: &mut Cdp,
    root: &Path,
    backend: ActionBackend,
    request: &Request,
    navigation: Navigate,
    snapshot: &Value,
    dispatched: &AtomicBool,
) -> Result<Value> {
    recover_pending(
        cdp,
        root,
        backend,
        request,
        &navigation.browser_id,
        &navigation.target_id,
        snapshot,
    )
    .await?;
    cdp.check_url(&navigation.url)?;
    let journal = ActionJournal::open(root, backend, request, true).await?;
    journal.authorize(request).await?;
    journal
        .check_navigation(request, &navigation.browser_id, &navigation.target_id)
        .await?;
    let page = Page::from_observation(cdp, &navigation.target_id, snapshot).await?;
    page.check_document(cdp).await?;
    journal.authorize(request).await?;
    if request.deadline_ms <= now_ms() {
        return Err("deadline_exceeded");
    }
    dispatched.store(true, Ordering::Relaxed);
    let result = cdp
        .call(
            Some(&page.session),
            "Page.navigate",
            json!({"url": navigation.url, "frameId": page.frame}),
        )
        .await?;
    if result["frameId"] != page.frame
        || result.get("errorText").is_some()
        || result["isDownload"] == true
    {
        return Err("fixture_navigation_failed");
    }
    loop {
        let frame = main_frame(cdp, &page.session).await?;
        if frame["id"] != page.frame || frame.get("unreachableUrl").is_some() {
            return Err("fixture_navigation_failed");
        }
        let url = frame_url(&frame)?;
        let committed = if let Some(loader) = result["loaderId"].as_str() {
            frame["loaderId"] == loader
        } else {
            frame["loaderId"] == page.document && url == navigation.url
        };
        if committed {
            cdp.check_url(&url)?;
            cdp.detach(&page.session).await?;
            return Ok(
                json!({"browser_id": navigation.browser_id, "target_id": navigation.target_id,
                             "url": url, "document_id": frame["loaderId"]}),
            );
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

pub async fn click(
    cdp: &mut Cdp,
    root: &Path,
    backend: ActionBackend,
    request: &Request,
    mut click: Click,
    snapshot: &Value,
    dispatched: &AtomicBool,
    after_dispatch: impl FnOnce(),
) -> Result<Value> {
    recover_pending(
        cdp,
        root,
        backend,
        request,
        &click.browser_id,
        &click.target_id,
        snapshot,
    )
    .await?;
    let mut journal = ActionJournal::open(root, backend, request, true).await?;
    if let Some(gesture) = click.gesture.take() {
        use crate::panel::Input;
        if click.await_confirmation || click.target.is_some()
            || click.backend_node_id.is_some()
            || click.frame_id.is_some()
            || !click.expected_selector.is_empty()
            || !click.expected_text.is_empty()
            || click.expected_url.is_some()
            || click.dialog.is_some()
            || click.button.is_some()
            || click.click_count.is_some()
            || !matches!(
                &gesture.input,
                Input::Click { .. } | Input::Wheel { .. } | Input::Move { .. } | Input::Drag { .. }
            )
        {
            return Err("invalid_click_gesture");
        }
        if snapshot["viewport_changed"] == true {
            return Err("observation_stale");
        }
        let page = Page::from_observation(cdp, &click.target_id, snapshot).await?;
        let viewport = crate::panel::Viewport::read(cdp, &page.session, &page.frame).await?;
        if viewport.size != gesture.viewport {
            return Err("browser_binding_changed");
        }
        let receipt = Receipt {
            request_id: request.request_id.clone(),
            workspace_id: request.workspace_id.clone(),
            client_id: request.client_id.clone(),
            service_epoch: request.service_epoch.clone().ok_or("stale_service_epoch")?,
            browser_id: click.browser_id,
            target_id: click.target_id,
            document_id: page.document.clone(),
            frame_id: None,
            frame_document_id: None,
            result_document_id: None,
            action: Action::Click,
            expected: None,
            input_evidence: None,
            upload_evidence: None,
            batch: None,
            plan: None,
            submission_context: None, confirmation: None, resolution: None, confirmation_source: None,
            state: ReceiptState::Dispatching,
        };
        journal.check_new(&receipt).await?;
        journal.authorize(request).await?;
        page.check_document(cdp).await?;
        journal.begin(receipt.clone()).await?;
        let result = gesture
            .input
            .dispatch(
                cdp,
                &page.session,
                &viewport,
                dispatched,
                Some(&page.document),
            )
            .await;
        if let Err(error) = result {
            if !dispatched.load(Ordering::Relaxed) {
                journal
                    .finish(
                        &receipt.workspace_id,
                        &receipt.request_id,
                        ReceiptState::NotDispatched,
                    )
                    .await?;
            }
            return Err(error);
        }
        after_dispatch();
        return finish_click(cdp, &mut journal, &page, &receipt, "").await;
    }
    click_step(
        cdp,
        &mut journal,
        request,
        click,
        snapshot,
        dispatched,
        None,
        after_dispatch,
    )
    .await
}

pub(crate) async fn click_step(
    cdp: &mut Cdp,
    journal: &mut ActionJournal<'_>,
    request: &Request,
    click: Click,
    snapshot: &Value,
    dispatched: &AtomicBool,
    batch: Option<BatchStep>,
    after_dispatch: impl FnOnce(),
) -> Result<Value> {
    let expected = Expected {
        selector: click.expected_selector,
        text: click.expected_text,
        url: click.expected_url,
    };
    if (expected.selector.trim().is_empty() && !expected.text.is_empty())
        || expected.selector.len() > 512
        || expected.text.len() > 512
    {
        return Err("invalid_postcondition");
    }
    let verify_outcome = !expected.selector.trim().is_empty() || expected.url.is_some();
    if click.await_confirmation && (verify_outcome || click.submission_context.is_none()) {
        return Err("invalid_postcondition");
    }
    let button = click
        .button
        .as_ref()
        .map_or("left", crate::panel::Button::name);
    let click_count = click.click_count.unwrap_or(1);
    if !matches!(click_count, 1 | 2)
        || (click_count == 2 && button != "left")
        || (click.dialog.is_some() && (click_count != 1 || button != "left"))
    {
        return Err("invalid_click_gesture");
    }
    if let Some(url) = &expected.url {
        if url == "about:blank"
            || click
                .frame_id
                .as_deref()
                .is_some_and(|id| snapshot["main_frame_id"] != id)
        {
            return Err("invalid_postcondition");
        }
        cdp.check_url(url)?;
    }
    if click.dialog.as_ref().is_some_and(|dialog| {
        !dialog.valid()
            || click
                .frame_id
                .as_deref()
                .is_some_and(|id| snapshot["main_frame_id"] != id)
    }) {
        return Err("invalid_browser_dialog");
    }
    let max_timeout = crate::protocol::limit("deadline_ms") as u64;
    if click
        .timeout_ms
        .is_some_and(|ms| ms == 0 || ms > max_timeout)
    {
        return Err("invalid_click_timeout");
    }
    let remaining = request.deadline_ms.saturating_sub(now_ms() + 50);
    let deadline = Instant::now()
        + Duration::from_millis(remaining.min(click.timeout_ms.unwrap_or(max_timeout)));
    journal.authorize(request).await?;
    let page = Page::from_scope(cdp, &click.target_id, snapshot, click.frame_id.as_deref()).await?;
    let (frame_id, frame_document_id) = page.receipt_frame();
    let backend_node_id = match (click.target.as_deref(), click.backend_node_id) {
        (Some(target), None) => resolve_target(cdp, &page, snapshot, target).await?,
        (None, Some(backend)) => backend,
        _ => return Err("invalid_locator"),
    };
    let observed = observed_identity(snapshot, &page.frame, backend_node_id)?;
    if let Some(context)=&click.submission_context {
        if context["document_id"]!=page.tab_document || context["url"]!=snapshot["url"] {return Err("submission_identity_changed");}
    }
    let receipt = Receipt {
        request_id: request.request_id.clone(),
        workspace_id: request.workspace_id.clone(),
        client_id: request.client_id.clone(),
        service_epoch: request.service_epoch.clone().ok_or("stale_service_epoch")?,
        browser_id: click.browser_id,
        target_id: click.target_id,
        document_id: page.tab_document.clone(),
        frame_id,
        frame_document_id,
        result_document_id: None,
        action: Action::Click,
        expected: verify_outcome.then(|| expected.clone()),
        input_evidence: None,
        upload_evidence: None,
        batch,
        plan: None,
        submission_context: click.submission_context.map(serde_json::from_value).transpose().map_err(|_|"submission_identity_changed")?, confirmation: None, resolution: None, confirmation_source: None,
        state: ReceiptState::Dispatching,
    };
    journal.check_new(&receipt).await?;
    if let Some(dialog) = &click.dialog {
        if frame_url(&page.current_frame(cdp).await?)? != dialog.url {
            return Err("browser_dialog_url_mismatch");
        }
        journal.authorize_dialog(dialog).await?;
    }
    page.check_label(cdp, backend_node_id, observed).await?;
    if expected.url.is_some() && !expected.selector.trim().is_empty() {
        // Validate selector syntax before input, even when it will be used on a new document.
        page.expected(cdp, &expected).await?;
    }
    if verify_outcome && click_already_satisfied(cdp, &page, &expected).await? {
        return Err("postcondition_already_satisfied");
    }
    let object = page.object(cdp, backend_node_id).await?;
    let allow_label = button == "left" && click_count == 1 && click.dialog.is_none();
    page.scroll_to(cdp, &object, allow_label).await?;
    let point = page.point(cdp, &object, allow_label).await?;
    tokio::time::sleep(Duration::from_millis(32)).await;
    let (x, y) = page.point(cdp, &object, allow_label).await?;
    if point != (x, y) {
        return Err("element_moved");
    }
    if Instant::now() >= deadline {
        return Err("deadline_exceeded");
    }
    journal.begin(receipt.clone()).await?;
    // A crash or cancellation from this point preserves the durable hold.
    dispatched.store(true, Ordering::Relaxed);
    let final_check = async {
        page.check_document(cdp).await?;
        if let Some(target) = click.target.as_deref() {
            if resolve_target(cdp, &page, snapshot, target).await? != backend_node_id {
                return Err("locator_changed");
            }
        }
        page.check_label(cdp, backend_node_id, observed).await?;
        if verify_outcome && click_already_satisfied(cdp, &page, &expected).await? {
            return Err("postcondition_already_satisfied");
        }
        if page.point(cdp, &object, allow_label).await? != (x, y) {
            return Err("element_moved");
        }
        journal.authorize(request).await?;
        if Instant::now() >= deadline {
            return Err("deadline_exceeded");
        }
        if let Some(dialog) = &click.dialog {
            if frame_url(&page.current_frame(cdp).await?)? != dialog.url {
                return Err("browser_dialog_url_mismatch");
            }
            cdp.call(Some(&page.tab_session), "Page.enable", json!({}))
                .await?;
            cdp.watch_dialogs();
        }
        Ok(())
    }
    .await;
    if let Err(error) = final_check {
        journal
            .finish(
                &receipt.workspace_id,
                &receipt.request_id,
                ReceiptState::NotDispatched,
            )
            .await?;
        dispatched.store(false, Ordering::Relaxed);
        return Err(error);
    }
    let mut dialog_handled = false;
    let input = async {
        for (count, event) in (1..=click_count)
            .flat_map(|count| ["mousePressed", "mouseReleased"].map(move |event| (count, event)))
        {
            if event == "mouseReleased" || count > 1 {
                page.check_document(cdp).await?;
            }
            if count > 1 && event == "mousePressed" {
                journal.authorize(request).await?;
                if page.point(cdp, &object, allow_label).await? != (x, y) {
                    return Err("element_moved");
                }
            }
            let params =
                json!({"type": event, "x": x, "y": y, "button": button, "clickCount": count});
            if let Some(dialog) = &click.dialog {
                dialog
                    .input(
                        cdp,
                        &page.tab_session,
                        &page.frame,
                        deadline,
                        params,
                        &mut dialog_handled,
                        event == "mouseReleased",
                    )
                    .await?;
            } else {
                cdp.call(Some(&page.tab_session), "Input.dispatchMouseEvent", params)
                    .await?;
            }
        }
        Ok::<_, &'static str>(())
    }
    .await;
    cdp.stop_watching_dialogs();
    input?;
    // Hold the whole workspace if a crash interrupts the pointer sequence.
    // Once release is acknowledged, the unresolved outcome belongs to this tab.
    journal
        .finish(
            &receipt.workspace_id,
            &receipt.request_id,
            ReceiptState::Returned,
        )
        .await?;
    after_dispatch();
    if click.await_confirmation {
        // Keep Returned durable: an acknowledged click is not an accepted
        // application. Further input remains fenced until read-only confirmation.
        let mut pending = receipt.clone();
        pending.state = ReceiptState::Returned;
        page.detach(cdp).await?;
        return Ok(json!({"receipt":pending,
            "input_sent":true,"verification":"awaiting_site_confirmation",
            "next_step":"Read the site, then reconcile this exact request in confirmation mode. Never repeat submit."}));
    }
    if !verify_outcome {
        // The browser acknowledged input delivery. An application outcome is
        // only verified when the caller supplied a condition we could observe.
        return finish_click(cdp, journal, &page, &receipt, "").await;
    }
    let document = tokio::time::timeout_at(deadline, async {
        loop {
            if Instant::now() >= deadline {
                return Err("postcondition_unobserved");
            }
            match click_outcome(cdp, &page, &expected).await {
                Ok(Some(document)) => return Ok(document),
                Ok(None) => (),
                // The destination can replace its execution context or navigate while loading.
                Err("cdp_command_failed") => (),
                Err("document_changed") => (),
                Err(error) => return Err(error),
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .map_err(|_| "postcondition_unobserved")??;
    finish_click(cdp, journal, &page, &receipt, &document).await
}

async fn click_already_satisfied(cdp: &mut Cdp, page: &Page, expected: &Expected) -> Result<bool> {
    match &expected.url {
        Some(url) => Ok(frame_url(&page.current_frame(cdp).await?)? == *url),
        None => page.expected(cdp, expected).await,
    }
}

async fn click_outcome(cdp: &mut Cdp, page: &Page, expected: &Expected) -> Result<Option<String>> {
    if let Some(url) = &expected.url {
        let frame = match main_frame(cdp, &page.session).await {
            Ok(frame) => frame,
            Err(_) => return Ok(None),
        };
        if frame.get("unreachableUrl").is_some() || frame_url(&frame).map(|u| u != *url).unwrap_or(true) {
            return Ok(None);
        }
        let document = match field(&frame, "loaderId") {
            Ok(doc) => doc.to_owned(),
            Err(_) => return Ok(None),
        };
        if document.is_empty() {
            return Ok(None);
        }
        if !expected.selector.trim().is_empty() {
            let destination = match Page::in_frame(cdp, page.session.clone(), &frame).await {
                Ok(p) => p,
                Err(_) => return Ok(None),
            };
            let matched = destination.expected(cdp, expected).await.unwrap_or(false);
            let latest = match main_frame(cdp, &page.session).await {
                Ok(frame) => frame,
                Err(_) => return Ok(None),
            };
            if !matched
                || latest["loaderId"] != document
                || latest.get("unreachableUrl").is_some()
                || frame_url(&latest).map(|u| u != *url).unwrap_or(true)
            {
                return Ok(None);
            }
        }
        return Ok(Some(document));
    }

    if page.check_document(cdp).await.is_ok() {
        if let Ok(matched) = page.expected(cdp, expected).await {
            if matched && page.check_document(cdp).await.is_ok() {
                return Ok(Some(page.document.clone()));
            }
        }
    }

    if page.owners.is_empty() {
        if let Ok(frame) = main_frame(cdp, &page.session).await {
            if frame.get("unreachableUrl").is_none() {
                if let Ok(document) = field(&frame, "loaderId") {
                    if !document.is_empty() && document != page.document {
                        if !expected.selector.trim().is_empty() {
                            if let Ok(destination) = Page::in_frame(cdp, page.session.clone(), &frame).await {
                                if destination.expected(cdp, expected).await.unwrap_or(false) {
                                    if let Ok(latest) = main_frame(cdp, &page.session).await {
                                        if latest["loaderId"] == document && latest.get("unreachableUrl").is_none() {
                                            return Ok(Some(document.to_owned()));
                                        }
                                    }
                                }
                            }
                        } else {
                            return Ok(Some(document.to_owned()));
                        }
                    }
                }
            }
        }
    }

    Ok(None)
}

async fn finish_click(
    cdp: &mut Cdp,
    journal: &mut ActionJournal<'_>,
    page: &Page,
    receipt: &Receipt,
    document: &str,
) -> Result<Value> {
    page.detach(cdp).await?;
    let result_document_id = if !document.is_empty() && document != receipt.document_id {
        Some(document)
    } else {
        receipt
            .expected
            .as_ref()
            .and_then(|expected| expected.url.as_ref())
            .map(|_| document)
    };
    let result = journal
        .finish_with_document(
            &receipt.workspace_id,
            &receipt.request_id,
            ReceiptState::Verified,
            result_document_id,
        )
        .await?;
    Ok(json!({"receipt": result}))
}

pub(crate) async fn recover_pending(
    cdp: &mut Cdp,
    root: &Path,
    backend: ActionBackend,
    request: &Request,
    browser: &str,
    target: &str,
    snapshot: &Value,
) -> Result<()> {
    if !matches!(backend, ActionBackend::Standalone | ActionBackend::Fixture) {
        return Ok(());
    }
    let journal = ActionJournal::open(root, backend, request, false).await?;
    journal.authorize(request).await?;
    let pending = journal.pending(request, browser, target)?;
    drop(journal);
    for receipt in pending
        .into_iter()
        .filter(|r| r.client_id == request.client_id)
        .take(8)
    {
        let input = receipt.input_evidence.is_some()
            && matches!(
                receipt.action,
                Action::Fill | Action::Select | Action::Check
            );
        let click = receipt.action == Action::Click && receipt.expected.is_some();
        if !input && !click {
            continue;
        }
        if request.deadline_ms.saturating_sub(now_ms()) < 1000 {
            return Err("input_deadline_budget_exhausted");
        }
        let mut repair = request.clone();
        repair.method = if input {
            "reconcile_input"
        } else {
            "reconcile_click"
        }
        .into();
        let p = Reconcile {
            browser_id: browser.into(),
            target_id: target.into(),
            observation_id: snapshot["observation_id"].as_str().unwrap_or("").into(),
            request_id: receipt.request_id,
            attachment_names: None,
        };
        let result = reconcile(cdp, root, backend, &repair, p, snapshot).await;
        if result
            .as_ref()
            .is_ok_and(|r| r["postcondition_matches"] == false)
        {
            // The earlier operation remains unverified, but its current field
            // state is known. Require a new observation and corrective action;
            // never execute the caller's next click on an assumed filled form.
            return Err("input_postcondition_changed");
        }
    }
    Ok(())
}

pub async fn reconcile(
    cdp: &mut Cdp,
    root: &Path,
    backend: ActionBackend,
    request: &Request,
    params: Reconcile,
    snapshot: &Value,
) -> Result<Value> {
    let workspace = &request.workspace_id;
    // Reconciliation writes receipt state, so local SQLite needs a writer lock.
    // Application mode retains its existing metadata-only authorization path.
    let mut journal = ActionJournal::open(root, backend, request, matches!(backend,ActionBackend::Standalone|ActionBackend::Fixture)).await?;
    let receipt = journal.receipt(workspace, &params.request_id).await?;
    if request.method == "reconcile_upload" {
        return crate::upload::reconcile(cdp, &mut journal, request, &params, snapshot, &receipt)
            .await;
    }
    if request.method == "reconcile_input" {
        if !matches!(
            receipt.action,
            Action::Fill | Action::Select | Action::Check
        ) || receipt.client_id != request.client_id
            || receipt.target_id != params.target_id
            || (receipt.browser_id != params.browser_id
                && receipt.service_epoch == request.service_epoch.as_deref().unwrap_or(""))
        {
            return Err("receipt_target_changed");
        }
        if !matches!(
            receipt.state,
            ReceiptState::Dispatching | ReceiptState::Returned
        ) {
            return Err("action_receipt_not_pending");
        }
        let evidence = receipt
            .input_evidence
            .as_ref()
            .ok_or("input_evidence_missing")?;
        let page = Page::from_scope(
            cdp,
            &params.target_id,
            snapshot,
            receipt.frame_id.as_deref(),
        )
        .await?;
        if page.tab_document != receipt.document_id
            || receipt
                .frame_document_id
                .as_ref()
                .is_some_and(|doc| *doc != page.document)
        {
            return Err("receipt_document_changed");
        }
        let matched = evidence
            .matches(cdp, &page, snapshot, receipt.action)
            .await?;
        page.check_document(cdp).await?;
        let result = journal
            .finish(
                workspace,
                &params.request_id,
                if matched {
                    ReceiptState::Verified
                } else {
                    ReceiptState::Reconciled
                },
            )
            .await?;
        page.detach(cdp).await?;
        return Ok(
            json!({"receipt":result,"postcondition_matches":matched,"input_sent":false,
            "verification":if matched {"value_readback"} else {"current_value_differs"}}),
        );
    }
    let expected = receipt
        .expected
        .as_ref()
        .filter(|_| receipt.action == Action::Click)
        .ok_or("action_receipt_not_reconcilable")?;
    if !matches!(
        receipt.state,
        ReceiptState::Dispatching | ReceiptState::Returned
    ) {
        return Err("action_receipt_not_pending");
    }
    if receipt.browser_id != params.browser_id || receipt.target_id != params.target_id {
        return Err("receipt_target_changed");
    }
    let page = Page::from_scope(
        cdp,
        &params.target_id,
        snapshot,
        receipt.frame_id.as_deref(),
    )
    .await?;
    if expected.url.is_none()
        && receipt.frame_id.is_some()
        && receipt
            .frame_document_id
            .as_ref()
            .is_some_and(|document| *document != page.document)
    {
        return Err("receipt_document_changed");
    }
    let document = click_outcome(cdp, &page, expected)
        .await?
        .ok_or("postcondition_unobserved")?;
    finish_click(cdp, &mut journal, &page, &receipt, &document).await
}
