//! Spill large tool results to the Task_Workspace (Requirement 11.2).
//!
//! A result over [`SPILL_BYTES`] is written in full to `.kageha/tool-output/<call>.txt`, and
//! the model receives a bounded excerpt naming that file. Only the model-facing copy is
//! shortened: completion evidence and tool outcomes are computed from the full result before
//! spilling, and tool cards show the full result. The excerpt of a given result is
//! deterministic and within the runtime's context cap, so the runtime never changes it later.
use kageha_workspace::task::TaskWorkspace;
use serde_json::{json, Value};
use std::{fs, io};

/// Results larger than this are spilled. Equal to the runtime's per-result context cap.
pub(crate) const SPILL_BYTES: usize = kageha_runtime::turn::TOOL_RESULT_BYTES;
/// Workspace-relative directory of spill files.
pub(crate) const SPILL_DIR: &str = ".kageha/tool-output/";
const FAILED: &str = "Tool failed: ";
const HINT: &str = "The full output is saved at spilled_path. Use read_file with offset or shell to inspect the full output. Do not repeat the action.";
/// Envelope fields of a JSON result that stay in the excerpt, so the model keeps error codes,
/// receipt identity, pagination and a shell command's exit status. `exit_code` and `job_id`
/// are what `ui_task_completion` reads to accept a `shell` result as validation, so a card
/// that holds an excerpt is judged like the full result.
const KEPT_FIELDS: [&str; 20] = [
    "reason_code", "outcome_id", "next_tool", "required_arguments", "mismatched_fields", "submission_evidence",
    "error_code",
    "retryable",
    "summary",
    "status",
    "execution_phase",
    "request_id",
    "request_id_kind",
    "input_dispatched",
    "tool_call_id",
    "upload_receipt_ids",
    "source_id",
    "next_offset",
    "exit_code",
    "job_id",
];

/// Largest `known_fix` kept in an excerpt. A fix is at most 3 steps of
/// `kageha_learning::MAX_STEP_BYTES`, so this keeps every fix the Host attaches.
const KNOWN_FIX_BYTES: usize = 4096;

/// The model-facing copy of a tool result: `None` when the result is within the bound,
/// otherwise the excerpt after the full result is written to its spill file.
pub(crate) fn spill(workspace: &TaskWorkspace, call_id: &str, content: &str) -> io::Result<Option<String>> {
    if content.len() <= SPILL_BYTES {
        return Ok(None);
    }
    let path = write(workspace, call_id, content)?;
    Ok(Some(excerpt(content, &path)))
}

/// A file name made of `[A-Za-z0-9_-]` from the call id.
pub(crate) fn file_name(call_id: &str) -> String {
    let name: String = call_id
        .chars()
        .take(128)
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    if name.is_empty() { "call".into() } else { name }
}

/// Writes the full result and returns its workspace-relative path. A file with the same
/// bytes is reused (a replayed call). Some providers repeat call ids across turns, so a
/// different file with the same name gets a numbered suffix instead of being replaced.
fn write(workspace: &TaskWorkspace, call_id: &str, content: &str) -> io::Result<String> {
    let base = file_name(call_id);
    for n in 1..=100u32 {
        let path = if n == 1 { format!("{SPILL_DIR}{base}.txt") } else { format!("{SPILL_DIR}{base}-{n}.txt") };
        let file = workspace.host_path(&path).ok_or_else(|| io::Error::other("invalid spill path"))?;
        match fs::symlink_metadata(&file) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                workspace.write_host_file(&path, content.as_bytes())?;
                return Ok(path);
            }
            Err(error) => return Err(error),
            Ok(meta) if meta.is_file() && meta.len() == content.len() as u64
                && fs::read(&file)? == content.as_bytes() => return Ok(path),
            Ok(_) => continue,
        }
    }
    Err(io::Error::other("Too many spilled outputs share this tool call id"))
}

/// A JSON envelope of at most [`SPILL_BYTES`] with the start and end of the result. A
/// failure keeps its `Tool failed: ` prefix outside the envelope.
pub(crate) fn excerpt(content: &str, spilled_path: &str) -> String {
    let (prefix, body) = content.strip_prefix(FAILED).map_or(("", content), |body| (FAILED, body));
    let mut envelope = json!({"truncated":true,"spilled_path":spilled_path,"original_bytes":content.len(),"hint":HINT});
    if let Ok(Value::Object(mut fields)) = serde_json::from_str::<Value>(body) {
        // Hooks can wrap the result in a string. Preserve deterministic recovery
        // metadata from the inner result as well as from a direct error envelope.
        if let Some(inner) = fields.get("tool_result").and_then(Value::as_str) {
            let inner = inner.strip_prefix(FAILED).unwrap_or(inner);
            if let Ok(Value::Object(inner)) = serde_json::from_str::<Value>(inner) {
                if inner.get("error_code").is_some_and(|v| v == "invalid_outcome_evidence") {
                    for field in KEPT_FIELDS {
                        if let Some(value) = inner.get(field) { fields.insert(field.into(), value.clone()); }
                    }
                    fields.remove("known_fix");
                }
            }
        }
        for field in KEPT_FIELDS {
            if let Some(value) = fields.get(field)
                .filter(|value| serde_json::to_vec(value).is_ok_and(|bytes| bytes.len() < if matches!(field,"required_arguments"|"mismatched_fields"|"submission_evidence") {2048} else {256}))
            {
                envelope[field] = value.clone();
            }
        }
        // A learned fix for a failure (`ui_learning::attach_known_fix`) stays with it.
        if let Some(fix) = fields.get("known_fix")
            .filter(|value| serde_json::to_vec(value).is_ok_and(|bytes| bytes.len() <= KNOWN_FIX_BYTES))
        {
            envelope["known_fix"] = fix.clone();
        }
    }
    // JSON escaping can expand the preview, so shrink it until the envelope fits.
    let mut budget = SPILL_BYTES;
    loop {
        let preview = head_and_tail(body, budget);
        let preview_bytes = serde_json::to_string(&preview).map_or(0, |text| text.len());
        envelope["preview"] = json!(preview);
        let rendered = format!("{prefix}{envelope}");
        if rendered.len() <= SPILL_BYTES || budget == 0 {
            return rendered;
        }
        let room = SPILL_BYTES.saturating_sub(rendered.len() - preview_bytes);
        let scaled = (budget as u128 * room as u128 / preview_bytes.max(1) as u128) as usize;
        budget = scaled.min(budget - 1);
    }
}

/// About `budget` bytes of `text`: its first and last halves on character boundaries.
fn head_and_tail(text: &str, budget: usize) -> String {
    if text.len() <= budget {
        return text.to_owned();
    }
    let mut head = budget / 2;
    while !text.is_char_boundary(head) {
        head -= 1;
    }
    let mut tail = text.len() - (budget - head);
    while !text.is_char_boundary(tail) {
        tail += 1;
    }
    format!("{}\n[... {} bytes omitted; see spilled_path ...]\n{}", &text[..head], tail - head, &text[tail..])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> (tempfile::TempDir, TaskWorkspace) {
        let dir = tempfile::tempdir().unwrap();
        let workspace = TaskWorkspace::open(&dir.path().join("chat")).unwrap();
        (dir, workspace)
    }

    #[test]
    fn structured_evidence_recovery_survives_spilling() {
        let content=json!({"error_code":"invalid_outcome_evidence","reason_code":"missing_proof","outcome_id":"submit","next_tool":"amend_required_outcome","required_arguments":["id","proof","reason"],"mismatched_fields":[],"padding":"x".repeat(SPILL_BYTES*2)}).to_string();
        let result:Value=serde_json::from_str(&excerpt(&content,"saved.txt")).unwrap();
        assert_eq!(result["reason_code"],"missing_proof");
        assert_eq!(result["required_arguments"],json!(["id","proof","reason"]));
        assert!(result.to_string().len()<=SPILL_BYTES);
        let wrapped=json!({"tool_result":format!("Tool failed: {content}"),"hook_context":"x".repeat(SPILL_BYTES*2),"known_fix":{"steps":["stale"]}}).to_string();
        let result:Value=serde_json::from_str(&excerpt(&wrapped,"saved.txt")).unwrap();
        assert_eq!(result["reason_code"],"missing_proof");
        assert_eq!(result["next_tool"],"amend_required_outcome");
        assert!(result.get("known_fix").is_none());
    }
    #[test]
    fn large_results_are_spilled_in_full_with_a_bounded_excerpt() {
        let (_dir, workspace) = workspace();
        let full = format!("{}{}", "line of output\n".repeat(5_000), "🌲 final error\n".repeat(100));
        let excerpt = spill(&workspace, "call_1", &full).unwrap().unwrap();
        assert!(excerpt.len() <= SPILL_BYTES);
        let value: Value = serde_json::from_str(&excerpt).unwrap();
        assert_eq!(value["spilled_path"], ".kageha/tool-output/call_1.txt");
        assert_eq!(value["original_bytes"], full.len());
        assert!(value["hint"].as_str().unwrap().contains("read_file with offset"));
        let preview = value["preview"].as_str().unwrap();
        assert!(preview.starts_with("line of output"));
        assert!(preview.ends_with("🌲 final error\n"));
        let saved = fs::read(workspace.root().join(".kageha/tool-output/call_1.txt")).unwrap();
        assert_eq!(saved, full.as_bytes());
        // Spill files are host state, not workspace changes.
        assert!(workspace.scan_changes().unwrap().is_empty());
        // Deterministic: the same result yields the same excerpt and reuses the file.
        assert_eq!(spill(&workspace, "call_1", &full).unwrap().unwrap(), excerpt);
        // A different result under a repeated call id does not replace the first file.
        let other = "x".repeat(SPILL_BYTES + 1);
        let second: Value = serde_json::from_str(&spill(&workspace, "call_1", &other).unwrap().unwrap()).unwrap();
        assert_eq!(second["spilled_path"], ".kageha/tool-output/call_1-2.txt");
        assert_eq!(fs::read(workspace.root().join(".kageha/tool-output/call_1.txt")).unwrap(), full.as_bytes());
    }

    #[test]
    fn results_within_the_bound_are_unchanged_and_not_spilled() {
        let (_dir, workspace) = workspace();
        let exact = "a".repeat(SPILL_BYTES);
        assert_eq!(spill(&workspace, "small", &exact).unwrap(), None);
        assert!(!workspace.root().join(".kageha/tool-output/small.txt").exists());
    }

    #[test]
    fn json_envelopes_keep_their_identity_fields_and_failures_keep_their_prefix() {
        let (_dir, workspace) = workspace();
        let body = json!({"error_code":"observation_too_large","request_id":"r-1","source_id":"s-1",
            "next_offset":40,"status":"partial","items":"\u{1}".repeat(40_000)}).to_string();
        let full = format!("{FAILED}{body}");
        let excerpt = spill(&workspace, "read-retry:abc/../x", &full).unwrap().unwrap();
        // Escaped control characters expand sixfold; the envelope still fits.
        assert!(excerpt.len() <= SPILL_BYTES);
        let value: Value = serde_json::from_str(excerpt.strip_prefix(FAILED).unwrap()).unwrap();
        for (field, expected) in [("error_code", json!("observation_too_large")), ("request_id", json!("r-1")),
            ("source_id", json!("s-1")), ("next_offset", json!(40)), ("status", json!("partial"))] {
            assert_eq!(value[field], expected, "{field}");
        }
        assert_eq!(value["spilled_path"], ".kageha/tool-output/read-retry_abc____x.txt");
        assert_eq!(fs::read(workspace.root().join(".kageha/tool-output/read-retry_abc____x.txt")).unwrap(), full.as_bytes());
        assert_eq!(file_name(""), "call");
    }
}
