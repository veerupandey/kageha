//! Images reach a model that can see them (Requirement 20.2, 20.3, 20.6; design section 18).
//!
//! In priority order: an image-capable executor gets image parts; else an image-capable
//! planner takes the model steps whose context has images (`RoleProviders::images_to_planner`);
//! else a configured vision model describes each image once (cached by sha256 and model);
//! else the message says plainly that the image could not be analysed.
use crate::{ui_settings::Provider, ui_store as store, webui::Database};
use kageha_contracts::execution::{Control, ModelPort, ModelRequest, Role};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io;
use std::sync::atomic::AtomicBool;

/// Longest image side sent to a model, in pixels.
pub(crate) const MAX_LONG_SIDE: u32 = 1568;
/// Images at most this size and [`MAX_LONG_SIDE`] are sent unchanged.
const KEEP_BYTES: usize = 1024 * 1024;
/// Extensions `read_file` treats as images.
pub(crate) const IMAGE_EXTENSIONS: [&str; 6] = ["jpg", "jpeg", "png", "gif", "webp", "heic"];
const PROMPT: &str = "Transcribe all visible text in this image verbatim, keeping its reading order and line breaks. Then add a short description of the layout and content. Reply with the text first, then `Description:` and the description.";

/// Who looks at the images of a turn.
pub(crate) enum ImageRoute {
    /// The executor accepts images.
    Executor,
    /// The executor is text-only; the planner accepts images.
    Planner,
    /// No model of the turn accepts images; this configured model describes them.
    Describe(Provider),
    /// No vision-capable model is configured.
    Unavailable,
}
/// How images become message content.
pub(crate) enum Vision<'a> {
    Inline,
    Describe(Describer<'a>),
    Unavailable(String),
}
pub(crate) struct Describer<'a> {
    pub model: &'a (dyn ModelPort + Sync),
    pub name: String,
    pub cancel: &'a AtomicBool,
}
/// The rule order of design section 18 ("Vision refinements").
pub(crate) fn choose<T>(executor: bool, planner: bool, describer: impl FnOnce() -> Option<T>) -> Result<bool, Option<T>> {
    if executor {
        return Ok(false);
    }
    if planner {
        return Ok(true);
    }
    Err(describer())
}
pub(crate) fn route(db: &Database, executor: Option<&Provider>, planner: Option<&Provider>, preferred: Option<&str>) -> ImageRoute {
    let sees = |provider: Option<&Provider>| provider.is_some_and(|p| accepts_images(db, p));
    match choose(sees(executor), sees(planner), || vision_model(db, preferred)) {
        Ok(false) => ImageRoute::Executor,
        Ok(true) => ImageRoute::Planner,
        Err(Some(provider)) => ImageRoute::Describe(provider),
        Err(None) => ImageRoute::Unavailable,
    }
}
/// Whether the settings entry of `provider` has the `vision` flag.
pub(crate) fn accepts_images(db: &Database, provider: &Provider) -> bool {
    let Ok(connection) = db.lock() else { return false };
    let rows = store::list(&connection, "model").unwrap_or_default();
    let id = provider.source.as_ref().map(|source| source.id.as_str());
    rows.iter()
        .find(|row| row["id"].as_str() == id && id.is_some())
        .or_else(|| rows.iter().find(|row| row["model"] == provider.model.as_str()
            && row["protocol"].as_str().unwrap_or("openai_compat") == provider.protocol))
        .is_some_and(kageha_models::catalog::vision)
}
/// The configured vision model: the chat's `vision` preference when it names a
/// vision-capable model, else the first ready vision-capable model.
fn vision_model(db: &Database, preferred: Option<&str>) -> Option<Provider> {
    let rows = crate::ui_settings::models(db).ok()?;
    let usable = |row: &&Value| row["ready"] == true && kageha_models::catalog::vision(row);
    let chosen = preferred
        .filter(|id| !matches!(*id, "" | "auto"))
        .and_then(|id| rows.iter().filter(usable).find(|row| row["id"] == id))
        .or_else(|| rows.iter().find(usable))?;
    crate::ui_settings::configured(db, chosen["id"].as_str()?, "vision").ok()
}
/// The bytes to send for an image: unchanged when small, else scaled to at most
/// [`MAX_LONG_SIDE`] and re-encoded as JPEG.
pub(crate) fn fit(mime: &str, bytes: &[u8]) -> io::Result<(String, Vec<u8>)> {
    let image = image::load_from_memory(bytes)
        .map_err(|error| io::Error::other(format!("the image could not be decoded: {error}")))?;
    let long = image.width().max(image.height());
    if long <= MAX_LONG_SIDE && bytes.len() <= KEEP_BYTES {
        return Ok((mime.into(), bytes.to_vec()));
    }
    let image = if long > MAX_LONG_SIDE {
        image.resize(MAX_LONG_SIDE, MAX_LONG_SIDE, image::imageops::FilterType::Triangle)
    } else {
        image
    };
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85)
        .encode_image(&image.to_rgb8())
        .map_err(io::Error::other)?;
    Ok(("image/jpeg".into(), out))
}
pub(crate) fn image_part(mime: &str, bytes: &[u8]) -> Value {
    json!({"type":"image_url","image_url":{"url":format!("data:{mime};base64,{}",data_encoding::BASE64.encode(bytes))}})
}
/// Whether a message list has image parts.
pub(crate) fn has_images(messages: &[Value]) -> bool {
    messages.iter().any(|message| {
        message["content"].as_array().is_some_and(|parts| {
            parts.iter().any(|part| matches!(part["type"].as_str(), Some("image_url" | "image")))
        })
    })
}
pub(crate) fn unavailable(name: &str, reason: &str) -> String {
    format!("[Image {name}]: image could not be analysed: {reason}. Ask the user what it shows or report a blocker; do not build your own OCR.")
}
/// `[Image <name> — description by <model>]: <text>`, from the cache when this image was
/// described by this model before.
pub(crate) fn describe(db: &Database, describer: &Describer<'_>, name: &str, mime: &str, bytes: &[u8]) -> String {
    describe_for(db, describer, name, mime, bytes, "")
}
/// [`describe`] answering `question` (`view_image`); an empty question asks for the
/// default transcription. Cached by sha256, model and question.
fn describe_for(db: &Database, describer: &Describer<'_>, name: &str, mime: &str, bytes: &[u8], question: &str) -> String {
    match description(db, describer, mime, bytes, question) {
        Ok(text) => format!("[Image {name} — description by {}]: {text}", describer.name),
        Err(error) => unavailable(name, &format!("the vision model {} failed: {error}", describer.name)),
    }
}
fn description(db: &Database, describer: &Describer<'_>, mime: &str, bytes: &[u8], question: &str) -> io::Result<String> {
    let sha = data_encoding::HEXLOWER.encode(&Sha256::digest(bytes));
    let lookup = || -> io::Result<Option<String>> {
        let connection = db.lock().map_err(|_| io::Error::other("Database unavailable"))?;
        connection.execute_batch("CREATE TABLE IF NOT EXISTS ui_image_descriptions(sha256 TEXT NOT NULL,model TEXT NOT NULL,question TEXT NOT NULL DEFAULT '',text TEXT NOT NULL,PRIMARY KEY(sha256,model,question))").map_err(io::Error::other)?;
        use rusqlite::OptionalExtension;
        connection.query_row("SELECT text FROM ui_image_descriptions WHERE sha256=? AND model=? AND question=?", [&sha, &describer.name, question], |row| row.get(0))
            .optional().map_err(io::Error::other)
    };
    if let Some(text) = lookup()? {
        return Ok(text);
    }
    let (mime, fitted) = fit(mime, bytes)?;
    let request = ModelRequest {
        messages: vec![json!({"role":"user","content":[{"type":"text","text":if question.is_empty() { PROMPT } else { question }},image_part(&mime, &fitted)]})],
        tools: vec![],
        role: Role::Executor,
    };
    let reply = describer.model.complete(request, &Control { cancelled: describer.cancel, deadline: None })?;
    let text = match &reply.content {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts.iter().filter_map(|part| part["text"].as_str()).collect::<Vec<_>>().join("\n"),
        _ => String::new(),
    };
    let text = text.trim().to_owned();
    if text.is_empty() {
        return Err(io::Error::other("empty description"));
    }
    db.lock().map_err(|_| io::Error::other("Database unavailable"))?
        .execute("INSERT OR REPLACE INTO ui_image_descriptions(sha256,model,question,text) VALUES (?,?,?,?)", [&sha, &describer.name, question, &text])
        .map_err(io::Error::other)?;
    Ok(text)
}
/// The MIME type of image bytes from their signature.
pub(crate) fn sniff(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF8") {
        Some("image/gif")
    } else if bytes.len() > 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}
/// The `view_image` / `read_file` result for an image (Requirements 20.4, 20.5), never a
/// document-extraction failure. When a model of the turn sees images (`Vision::Inline`) the
/// resized image goes to `attachments`, a user message the runtime appends after the
/// batch's tool results (providers reject images inside tool results), and the result
/// says so. Else a vision model's answer to `question` (default: transcription and a short
/// description), or a plain note.
pub(crate) fn read_image(db: &Database, path: &str, bytes: &[u8], vision: &Vision<'_>, question: Option<&str>, attachments: &std::sync::Mutex<Vec<Value>>) -> String {
    let Some(mime) = sniff(bytes) else {
        return unavailable(path, "the file format is not supported (convert it to PNG or JPEG first)");
    };
    let question = question.map(str::trim).unwrap_or("");
    match vision {
        Vision::Describe(describer) => describe_for(db, describer, path, mime, bytes, question),
        Vision::Unavailable(reason) => unavailable(path, reason),
        Vision::Inline => match fit(mime, bytes) {
            Ok((mime, fitted)) => {
                let mut text = format!("Image {path}, shown by view_image.");
                if !question.is_empty() {
                    text.push_str(&format!(" Question: {question}"));
                }
                let message = json!({"role":"user","content":[{"type":"text","text":text},image_part(&mime, &fitted)]});
                match attachments.lock() {
                    Ok(mut queued) => {
                        queued.push(message);
                        format!("Image {path} attached below.")
                    }
                    Err(_) => unavailable(path, "the image could not be attached"),
                }
            }
            Err(error) => unavailable(path, &error.to_string()),
        },
    }
}
pub(crate) const NO_VISION_MODEL: &str = "no vision-capable model is configured";
/// Runs `f` with the [`Vision`] of `route`, building the describing model if needed.
pub(crate) fn with_vision<R>(route: ImageRoute, cancel: &AtomicBool, f: impl FnOnce(&Vision<'_>) -> io::Result<R>) -> io::Result<R> {
    match route {
        ImageRoute::Executor | ImageRoute::Planner => f(&Vision::Inline),
        ImageRoute::Describe(provider) => {
            let name = provider.model.clone();
            match crate::ui_credentials::model(vec![provider]) {
                Ok(model) => f(&Vision::Describe(Describer { model: &model, name, cancel })),
                Err(error) => f(&Vision::Unavailable(format!("the vision model {name} could not be used: {error}"))),
            }
        }
        ImageRoute::Unavailable => f(&Vision::Unavailable(NO_VISION_MODEL.into())),
    }
}
/// `view_image` and `read_file` of an image in the chat's uploads, the task workspace
/// (including `inputs/`) or the project root, routed by [`route`] (see [`read_image`]).
/// `None` when `path` has no image extension or the file is not found, so `read_file`
/// continues as before.
pub(crate) fn read_file(host: &crate::ui_tools::Tools<'_>, path: &str, question: Option<&str>) -> Option<String> {
    let extension = std::path::Path::new(path).extension()?.to_str()?.to_ascii_lowercase();
    if !IMAGE_EXTENSIONS.contains(&extension.as_str()) {
        return None;
    }
    let relative = path.strip_prefix("workspace/").unwrap_or(path);
    let bytes = crate::ui_uploads::pinned(host.db, host.chat, path).ok()
        .and_then(|(file, _)| std::fs::read(file).ok())
        .or_else(|| {
            let workspace = crate::ui_workspace::open_for_chat(host.db, host.chat).ok()?;
            std::fs::read(workspace.resolve(relative).ok()?).ok()
        })
        .or_else(|| {
            if host.machine.is_some() {
                return None;
            }
            let root = crate::ui_job_worktrees::execution_root(host.db, host.turn, host.workspace).ok()?.canonicalize().ok()?;
            let file = root.join(relative).canonicalize().ok()?;
            file.starts_with(&root).then(|| std::fs::read(file).ok()).flatten()
        })?;
    // An attached image routes the next model step to an image-capable planner
    // (`RoleProviders::images_to_planner`) when the executor is text-only.
    let route = route(host.db, host.providers.first(), host.role_providers.planner.first(), host.media_preferences["vision"].as_str());
    with_vision(route, host.cancel, |vision| Ok(read_image(host.db, path, &bytes, vision, question, &host.attachments))).ok()
}
#[cfg(test)]
#[path = "ui_vision_tests.rs"]
mod tests;
