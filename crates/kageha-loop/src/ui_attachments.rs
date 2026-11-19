//! Pinned chat images become provider-native multimodal content or descriptions, never local paths.
use crate::{ui_uploads, ui_vision::{self, Vision}, webui::Database};
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use std::io;
pub(crate) fn decorate(
    db: &Database,
    chat: &str,
    request: &str,
    messages: &mut [Value],
    vision: &Vision<'_>,
) -> io::Result<()> {
    let paths: Option<String> = db
        .lock()
        .map_err(|_| io::Error::other("Database unavailable"))?
        .query_row(
            "SELECT paths FROM ui_turn_uploads WHERE chat_id=? AND request_id=?",
            params![chat, request],
            |r| r.get(0),
        )
        .optional()
        .map_err(io::Error::other)?;
    let paths: Vec<String> = serde_json::from_str(&paths.unwrap_or_else(|| "[]".into()))?;
    let mut images = Vec::new();
    for path in paths {
        let (file, metadata) = ui_uploads::pinned(db, chat, &path)?;
        let mime = metadata["content_type"].as_str().unwrap_or("");
        if !matches!(
            mime,
            "image/png" | "image/jpeg" | "image/gif" | "image/webp"
        ) {
            continue;
        }
        images.push((path, mime.to_owned(), std::fs::read(file)?));
    }
    add_images(db, images, messages, vision)
}
/// Adds `(name, mime, bytes)` images to the last user message as `vision` says.
pub(crate) fn add_images(
    db: &Database,
    images: Vec<(String, String, Vec<u8>)>,
    messages: &mut [Value],
    vision: &Vision<'_>,
) -> io::Result<()> {
    let mut parts = Vec::new();
    let mut notes = Vec::new();
    // Images past the per-message limit stay files in `inputs/`; the message says so
    // instead of failing the turn (task 31.1: two 2.9 MB photos failed turn 151).
    let mut not_inlined = Vec::new();
    let mut total = 0;
    for (path, mime, bytes) in images {
        if ui_vision::sniff(&bytes) != Some(mime.as_str()) {
            return Err(io::Error::other(
                "Image attachment does not match its file type",
            ));
        }
        match vision {
            Vision::Inline => {
                // Scaled to at most 1568 px and re-encoded, so the limit is rarely reached.
                let (mime, bytes) = ui_vision::fit(&mime, &bytes).unwrap_or((mime, bytes));
                if total + bytes.len() > MAX_INLINE_IMAGE_BYTES {
                    not_inlined.push(path);
                    continue;
                }
                total += bytes.len();
                parts.push(ui_vision::image_part(&mime, &bytes));
            }
            Vision::Describe(describer) => notes.push(ui_vision::describe(db, describer, &path, &mime, &bytes)),
            Vision::Unavailable(reason) => notes.push(ui_vision::unavailable(&path, reason)),
        }
    }
    if parts.is_empty() && notes.is_empty() && not_inlined.is_empty() {
        return Ok(());
    }
    let user = messages
        .iter_mut()
        .rev()
        .find(|m| m["role"] == "user")
        .ok_or_else(|| io::Error::other("Image attachment needs a user message"))?;
    let mut text = user["content"].as_str().unwrap_or("").to_owned();
    for note in notes {
        text.push_str("\n\n");
        text.push_str(&note);
    }
    if !not_inlined.is_empty() {
        let list = not_inlined.iter().map(|path| format!("`{path}`")).collect::<Vec<_>>().join(", ");
        text.push_str(&format!(
            "\n\n[Host note: the images {list} are not shown inline because the images of one message are limited to {} MiB. They are attached as files; open them with a tool to look at them.]",
            MAX_INLINE_IMAGE_BYTES / (1024 * 1024)
        ));
    }
    if parts.is_empty() {
        user["content"] = json!(text);
    } else {
        let mut blocks = vec![json!({"type":"text","text":text})];
        blocks.extend(parts);
        user["content"] = json!(blocks);
    }
    Ok(())
}
/// Largest total size of the images inlined into one user message.
pub(crate) const MAX_INLINE_IMAGE_BYTES: usize = 4 * 1024 * 1024;
