//! Native media adapters and immutable voice references behind the existing WebUI API.
use crate::{
    native_io, server, ui_settings,
    ui_store::{self as store, db_error, ApiResult},
    ui_uploads,
    webui::Database,
};
use rusqlite::params;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    io::{self, Read, Write},
    sync::atomic::AtomicBool,
    time::Duration,
};
use tiny_http::{Header, Method, Request, Response};
const KIND: &str = "media-provider";
pub(crate) fn initialize(db: &rusqlite::Connection) -> rusqlite::Result<()> {
    db.execute_batch("CREATE TABLE IF NOT EXISTS ui_turn_artifacts(chat_id TEXT NOT NULL,turn_id TEXT NOT NULL,path TEXT NOT NULL,PRIMARY KEY(chat_id,turn_id,path));")?;
    if let Ok(raw) = std::env::var("KAGEHA_MEDIA_CONFIG") {
        let rows: Vec<Value> = serde_json::from_str(&raw)
            .map_err(|_| rusqlite::Error::ToSqlConversionFailure(Box::new(io::Error::other("Invalid media provider configuration"))))?;
        for mut row in rows {
            let id = row["id"].as_str().unwrap_or("").to_owned();
            if !server::identifier(&id) {
                continue;
            }
            if let Some(key)=row["api_key"].as_str(){seed_key(db,&id,key)?;}
            row.as_object_mut().unwrap().remove("api_key");
            if store::get(db, KIND, &id)?.is_none() {
                store::put(db, KIND, &id, row)?;
            }
        }
    }
    Ok(())
}
// Reuse the saved OpenAI API credential only at its official API origin.
fn openai_image(db: &rusqlite::Connection) -> ApiResult<Option<Value>> {
    let key = ui_settings::model_key(db, &json!({"provider":"openai","api_key_env":"OPENAI_API_KEY"}))?;
    Ok((!key.is_empty()).then(|| json!({
        "id":"openai-images","protocol":"openai_compat",
        "base_url":"https://api.openai.com/v1","api_key":key,
        "models":{"image":"gpt-image-2.5-flare"}
    })))
}
pub(crate) fn catalog(db: &Database) -> ApiResult<Value> {
    let connection = db.lock().map_err(db_error)?;
    let rows = store::list(&connection, KIND).map_err(db_error)?;
    let mut result = json!({"image":[],"vision":[],"tts":[],"stt":[]});
    for row in rows {
        let credential=format!("media:{}",row["id"].as_str().unwrap_or(""));let ready=row["enabled"]!=false && (row["protocol"]!="gemini" || crate::ui_credentials::managed(&connection,&credential)?) && crate::ui_credentials::usable_or_anonymous(&connection,&credential);
        for kind in ["image", "tts", "stt"] {
            if let Some(model) = row["models"][kind].as_str().filter(|s| !s.is_empty()) {
                result[kind].as_array_mut().unwrap().push(json!({"id":row["id"],"protocol":row["protocol"],"ready":ready,"voice_cloning":row["protocol"]=="audio8_http","models":[model],"default_model":model}));
            }
        }
    }
    if result["image"].as_array().is_some_and(Vec::is_empty)
        && openai_image(&connection).is_ok_and(|cfg| cfg.is_some())
    {
        result["image"].as_array_mut().unwrap().push(json!({"id":"openai-images","protocol":"openai_compat","ready":true,"models":["gpt-image-2.5-flare"],"default_model":"gpt-image-2.5-flare"}));
    }
    Ok(json!({"ok":true,"providers":result}))
}
fn config(db: &Database, id: &str, kind: &str) -> ApiResult<Value> {
    let connection = db.lock().map_err(db_error)?;
    let rows = store::list(&connection, KIND).map_err(db_error)?;
    let eligible: Vec<_> = rows.iter().filter(|row| {
        row["enabled"] != false && row["models"][kind].as_str().is_some_and(|s| !s.is_empty())
    }).collect();
    let row = if matches!(id, "" | "auto") {
        eligible.first().copied()
    } else {
        eligible.iter().copied().find(|row| row["id"] == id).or_else(|| {
            let alias = id.to_ascii_lowercase().replace([' ', '-', '_'], "");
            eligible.iter().copied().find(|row| kind == "image" && row["protocol"] == "gemini"
                && (row["models"]["image"] == id || matches!(alias.as_str(), "gemini" | "nanobanana" | "gemininanobanana")))
        })
    };
    let Some(mut row) = row.cloned() else {
        if kind == "image" && matches!(id, "" | "auto" | "openai-images") {
            if let Some(cfg) = openai_image(&connection)? {
                return Ok(cfg);
            }
        }
        if !eligible.is_empty() {
            let ids = eligible.iter().filter_map(|v| v["id"].as_str()).collect::<Vec<_>>().join(", ");
            return Err((409, format!("Unknown {kind} provider. Configured IDs: {ids}")));
        }
        return Err((409, format!("Configure a native {kind} provider first")));
    };
    let key = ui_settings::secret(
        &connection,
        &format!("media:{}", row["id"].as_str().unwrap_or("")),
    )?;
    if key.is_empty()&&crate::ui_credentials::managed(&connection,&format!("media:{}",row["id"].as_str().unwrap_or("")))?{return Err((403,"Media credential is revoked or unavailable".into()));}
    row["api_key"] = json!(key);
    Ok(row)
}
fn headers(cfg: &Value) -> Vec<(String, String)> {
    let mut result = vec![];
    if let Some(key) = cfg["api_key"].as_str().filter(|s| !s.is_empty()) {
        result.push(("Authorization".into(), format!("Bearer {key}")));
    }
    result
}
fn endpoint(cfg: &Value, path: &str) -> ApiResult<String> {
    let base = store::text(cfg, "base_url", 2048)?;
    native_io::valid_url(base).map_err(|_| (400, "Invalid media endpoint".into()))?;
    Ok(format!("{}{path}", base.trim_end_matches('/')))
}
fn wav(bytes: &[u8]) -> ApiResult<f64> {
    if bytes.len() < 44 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err((400, "Use a valid 16-bit PCM WAV recording".into()));
    }
    let declared = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize + 8;
    if declared != bytes.len() {
        return Err((400, "Reference WAV is truncated".into()));
    }
    let mut at = 12;
    let mut format = None;
    let mut size = None;
    while at + 8 <= bytes.len() {
        let len = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().unwrap()) as usize;
        let end = at
            .checked_add(8)
            .and_then(|v| v.checked_add(len))
            .filter(|v| *v <= bytes.len())
            .ok_or((400, "Invalid WAV chunk".into()))?;
        let chunk = &bytes[at + 8..end];
        if &bytes[at..at + 4] == b"fmt " && len >= 16 {
            format = Some((
                u16::from_le_bytes(chunk[..2].try_into().unwrap()),
                u16::from_le_bytes(chunk[2..4].try_into().unwrap()),
                u32::from_le_bytes(chunk[4..8].try_into().unwrap()),
                u16::from_le_bytes(chunk[14..16].try_into().unwrap()),
            ));
        }
        if &bytes[at..at + 4] == b"data" {
            size = Some(len);
        }
        at = end + (len % 2);
    }
    let (f, ch, rate, bits) = format.ok_or((400, "WAV format missing".into()))?;
    if f != 1 || !matches!(ch, 1 | 2) || !(8000..=192000).contains(&rate) || bits != 16 {
        return Err((400, "Use mono/stereo, 8–192 kHz, 16-bit PCM WAV".into()));
    }
    let size = size.ok_or((400, "WAV data missing".into()))?;
    if size % (ch as usize * 2) != 0 {
        return Err((400, "WAV data misaligned".into()));
    }
    let duration = size as f64 / (ch as f64 * rate as f64 * 2.0);
    if !(1.0..=30.0).contains(&duration) {
        return Err((400, "Reference recording must be 1–30 seconds".into()));
    }
    Ok(duration)
}
fn reference(db: &Database, id: &str) -> ApiResult<Value> {
    let connection = db.lock().map_err(db_error)?;
    let row = store::get(&connection, "voice", id)
        .map_err(db_error)?
        .ok_or((404, "Voice reference not found".into()))?;
    let encoded = ui_settings::secret(&connection, &format!("voice:{id}"))?;
    let data = data_encoding::BASE64
        .decode(encoded.as_bytes())
        .map_err(|_| (500, "Voice reference invalid".into()))?;
    if format!("{:x}", Sha256::digest(data)) != row["sha256"].as_str().unwrap_or("") {
        return Err((409, "Voice recording changed".into()));
    }
    Ok(json!({"audio_base64":encoded,"text":row["transcript"]}))
}
pub(crate) fn speech(
    db: &Database,
    provider: &str,
    text: &str,
    voice: &str,
    cancel: &AtomicBool,
) -> ApiResult<Vec<u8>> {
    if text.is_empty() || text.len() > 16000 {
        return Err((400, "Speech text must contain 1–16000 bytes".into()));
    }
    let cfg = config(db, provider, "tts")?;
    let cloned = cfg["protocol"] == "audio8_http";
    let mut body = json!({"model":cfg["models"]["tts"],"input":text,"voice":if voice.is_empty(){"alloy"}else{voice},"response_format":"wav"});
    if cloned {
        body = json!({"model":cfg["models"]["tts"],"text":text});
        if !voice.is_empty() {
            body["voice_profile"] = reference(db, voice)?;
        }
    } else if !voice.is_empty()
        && store::get(&*db.lock().map_err(db_error)?, "voice", voice)
            .map_err(db_error)?
            .is_some()
    {
        return Err((
            400,
            "This provider does not support reference voice cloning".into(),
        ));
    }
    let mut h = headers(&cfg);
    h.push(("Content-Type".into(), "application/json".into()));
    let out = native_io::http(
        &endpoint(&cfg, if cloned { "/tts" } else { "/audio/speech" })?,
        "POST",
        &h,
        serde_json::to_vec(&body).map_err(db_error)?,
        cancel,
        Duration::from_secs(120),
        16 * 1024 * 1024,
    )
    .map_err(|_| (502, "Speech request failed or was cancelled".into()))?;
    if !(200..300).contains(&out.status) {
        return Err((502, format!("Speech provider returned HTTP {}", out.status)));
    }
    if out.bytes.len() < 12 || &out.bytes[..4] != b"RIFF" || &out.bytes[8..12] != b"WAVE" {
        return Err((502, "Speech provider did not return WAV audio".into()));
    }
    Ok(out.bytes)
}
pub(crate) fn transcribe(
    db: &Database,
    provider: &str,
    bytes: &[u8],
    filename: &str,
    cancel: &AtomicBool,
) -> ApiResult<String> {
    if bytes.len() > 16 * 1024 * 1024 {
        return Err((400, "Audio exceeds 16 MiB".into()));
    }
    let cfg = config(db, provider, "stt")?;
    let boundary = format!("kageha{}", crate::browser_client::unique_id());
    let name = filename
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        .take(80)
        .collect::<String>();
    let mut body=format!("--{boundary}\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\n{}\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\nContent-Type: application/octet-stream\r\n\r\n",cfg["models"]["stt"].as_str().unwrap_or("")).into_bytes();
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    let mut h = headers(&cfg);
    h.push((
        "Content-Type".into(),
        format!("multipart/form-data; boundary={boundary}"),
    ));
    let response = native_io::http(
        &endpoint(&cfg, "/audio/transcriptions")?,
        "POST",
        &h,
        body,
        cancel,
        Duration::from_secs(120),
        1024 * 1024,
    )
    .map_err(|_| (502, "Transcription request failed".into()))?;
    if !(200..300).contains(&response.status) {
        return Err((
            502,
            format!("Transcription provider returned HTTP {}", response.status),
        ));
    }
    let value: Value = serde_json::from_slice(&response.bytes).map_err(db_error)?;
    Ok(store::text(&value, "text", 262144)?.into())
}
pub(crate) fn artifact(
    db: &Database,
    chat: &str,
    turn: &str,
    bytes: &[u8],
    filename: &str,
) -> io::Result<Value> {
    use std::os::unix::fs::OpenOptionsExt;
    let root = ui_uploads::root(db, chat)?;
    let folder = root.join("artifacts");
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&folder)?;
    }
    crate::browser_client::private(&folder)?;
    let hash = format!("{:x}", Sha256::digest(bytes));
    let path = format!(
        "artifacts/{}-{filename}",
        crate::browser_client::unique_id()
    );
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(root.join(&path))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    {
        let mut connection = db
            .lock()
            .map_err(|_| io::Error::other("Database unavailable"))?;
        let tx = connection.transaction().map_err(io::Error::other)?;
        tx.execute(
            "INSERT INTO ui_uploads VALUES (?,?,?,?,?,?)",
            params![
                chat,
                path,
                filename,
                ui_uploads::mime(filename),
                hash,
                bytes.len()
            ],
        )
        .map_err(io::Error::other)?;
        tx.execute(
            "INSERT INTO ui_turn_artifacts VALUES (?,?,?)",
            params![chat, turn, path],
        )
        .map_err(io::Error::other)?;
        tx.commit().map_err(io::Error::other)?;
    }
    Ok(
        json!({"path":path,"filename":filename,"sha256":hash,"content_type":ui_uploads::mime(filename),"bytes":bytes.len()}),
    )
}
pub(crate) fn image(
    db: &Database,
    provider: &str,
    prompt: &str,
    cancel: &AtomicBool,
) -> ApiResult<(Vec<u8>, &'static str)> {
    if prompt.trim().is_empty() || prompt.len() > 16000 {
        return Err((400, "A bounded image prompt is required".into()));
    }
    let cfg = config(db, provider, "image")?;
    let model = store::text(&cfg["models"], "image", 160)?;
    let gemini = cfg["protocol"] == "gemini";
    let (url, request, mut request_headers) = if gemini {
        validate_gemini(&cfg)?;
        let key = store::text(&cfg, "api_key", 8192)
            .map_err(|_| (403, "Gemini's image credential is unavailable".into()))?;
        (
            format!("{GEMINI_API}/models/{model}:generateContent"),
            json!({"contents":[{"parts":[{"text":prompt}]}],"generationConfig":{"responseModalities":["IMAGE"]}}),
            vec![("x-goog-api-key".into(), key.to_owned())],
        )
    } else {
        let mut body = json!({"model":model,"prompt":prompt,"n":1});
        if !model.starts_with("gpt-image-") {
            body["response_format"] = json!("b64_json");
        }
        (endpoint(&cfg, "/images/generations")?, body, headers(&cfg))
    };
    request_headers.push(("Content-Type".into(), "application/json".into()));
    let response = native_io::http(
        &url,
        "POST",
        &request_headers,
        serde_json::to_vec(&request).map_err(|_| (500, "Could not encode image request".into()))?,
        cancel,
        Duration::from_secs(120),
        24 * 1024 * 1024,
    )
    .map_err(|_| (502, "Image provider connection failed or timed out".into()))?;
    if !(200..300).contains(&response.status) {
        if gemini && response.status == 429 {
            return Err((429, gemini_quota_error(&response.bytes).into()));
        }
        return Err((502, format!("Image provider returned HTTP {}", response.status)));
    }
    let response: Value = serde_json::from_slice(&response.bytes)
        .map_err(|_| (502, "Image provider returned invalid JSON".into()))?;
    image_bytes(&response, gemini)
}

const GEMINI_API: &str = "https://generativelanguage.googleapis.com/v1beta";

fn gemini_quota_error(body: &[u8]) -> &'static str {
    let error: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    let zero = error["error"]["message"].as_str().unwrap_or("").lines()
        .any(|line| line.contains("limit: 0,") || line.trim_end().ends_with("limit: 0"));
    if zero {
        "Gemini is connected, but this Google API project has zero quota for the image model. Enable image quota in Google AI Studio or connect a project with image quota. Retrying with this project will not produce an image."
    } else {
        "Gemini is connected, but Google reports an image quota or rate limit for this project."
    }
}

fn validate_gemini(cfg: &Value) -> ApiResult<()> {
    let model = store::text(&cfg["models"], "image", 160)?;
    if cfg["base_url"].as_str().unwrap_or("").trim_end_matches('/') != GEMINI_API
        || !model.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'-' | b'_'))
        || ["stt", "tts"].iter().any(|kind| cfg["models"][kind].is_string())
    {
        return Err((400, "Use Google's Gemini API endpoint and an image model ID".into()));
    }
    Ok(())
}

fn image_bytes(response: &Value, gemini: bool) -> ApiResult<(Vec<u8>, &'static str)> {
    let encoded = if gemini {
        response["candidates"][0]["content"]["parts"].as_array().into_iter().flatten()
            .filter(|part| part["thought"] != true)
            .find_map(|part| part["inlineData"]["data"].as_str())
            .ok_or((502, "Gemini did not return a finished image".into()))?
    } else {
        response["data"][0]["b64_json"].as_str()
            .ok_or((502, "Image provider must return inline b64_json".into()))?
    };
    let bytes = data_encoding::BASE64.decode(encoded.as_bytes())
        .map_err(|_| (502, "Image bytes invalid".into()))?;
    if bytes.len() > 16 * 1024 * 1024 {
        return Err((502, "Image exceeds the 16 MiB limit".into()));
    }
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Ok((bytes, "image.png"))
    } else if gemini && bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Ok((bytes, "image.jpg"))
    } else {
        Err((502, "Image provider returned an unsupported image format".into()))
    }
}
pub(crate) fn handles(path: &str) -> bool {
    path == "/api/media-providers"
        || path == "/api/voice-profiles"
        || path.starts_with("/api/voice-profiles/")
        || path == "/api/voice-preview"
        || path.starts_with("/api/sessions/") && (path.ends_with("/tts") || path.ends_with("/stt"))
}
pub(crate) fn handle(mut request: Request, db: &Database) -> io::Result<()> {
    let path = request.url().split('?').next().unwrap_or("").to_owned();
    let method = request.method().clone();
    let parts: Vec<_> = path.split('/').collect();
    let chat = if parts.len() == 5 && parts[2] == "sessions" {
        parts[3]
    } else {
        ""
    };
    if !chat.is_empty() && ui_uploads::root(db, chat).is_err() {
        server::response(request, 404, json!({"error":"Chat not found"}));
        return Ok(());
    }
    if path.ends_with("/stt") && method == Method::Post {
        let result = ui_uploads::upload(&mut request, db, chat)
            .map_err(|e| (400, e.to_string()))
            .and_then(|upload| {
                let (file, _) = ui_uploads::pinned(db, chat, upload["path"].as_str().unwrap_or(""))
                    .map_err(db_error)?;
                let bytes = std::fs::read(file).map_err(db_error)?;
                let prefs = store::get(
                    &*db.lock().map_err(db_error)?,
                    "capability-preferences",
                    chat,
                )
                .map_err(db_error)?
                .unwrap_or(json!({}));
                let text = transcribe(
                    db,
                    prefs["stt"].as_str().unwrap_or("auto"),
                    &bytes,
                    upload["filename"].as_str().unwrap_or("voice.webm"),
                    &AtomicBool::new(false),
                )?;
                Ok(json!({"session_id":chat,"text":text}))
            });
        match result {
            Ok(v) => server::response(request, 200, v),
            Err((c, e)) => server::response(request, c, json!({"error":e})),
        }
        return Ok(());
    }
    let body = if matches!(method, Method::Post | Method::Put) {
        let mut bytes = Vec::new();
        request
            .as_reader()
            .take(17 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > 17 * 1024 * 1024 {
            Err((400, "Media request exceeds limit".into()))
        } else {
            serde_json::from_slice(&bytes).map_err(|_| (400, "Invalid JSON".into()))
        }
    } else {
        Ok(json!({}))
    };
    if path == "/api/voice-preview" || path.ends_with("/tts") {
        let audio = body.and_then(|p| {
            let prefs = if chat.is_empty() {
                json!({})
            } else {
                store::get(
                    &*db.lock().map_err(db_error)?,
                    "capability-preferences",
                    chat,
                )
                .map_err(db_error)?
                .unwrap_or(json!({}))
            };
            speech(
                db,
                p["provider"]
                    .as_str()
                    .or_else(|| prefs["tts"].as_str())
                    .unwrap_or("auto"),
                p["text"]
                    .as_str()
                    .unwrap_or("Hello, I am Kageha. Your local voice assistant is ready."),
                p["voice"]
                    .as_str()
                    .or_else(|| prefs["tts_voice"].as_str())
                    .unwrap_or(""),
                &AtomicBool::new(false),
            )
        });
        match audio {
            Ok(bytes) => {
                let _ = request.respond(
                    Response::from_data(bytes)
                        .with_header(Header::from_bytes("Content-Type", "audio/wav").unwrap())
                        .with_header(Header::from_bytes("Cache-Control", "no-store").unwrap()),
                );
            }
            Err((c, e)) => server::response(request, c, json!({"error":e})),
        }
        return Ok(());
    }
    let result=body.and_then(|payload|{if path=="/api/media-providers"{if method==Method::Post{let id=store::text(&payload,"id",64)?;if !server::identifier(id)||!matches!(payload["protocol"].as_str(),Some("openai_compat"|"audio8_http"|"gemini")){return Err((400,"Choose a native media protocol and valid provider ID".into()));}native_io::valid_url(store::text(&payload,"base_url",2048)?).map_err(|_|(400,"Invalid media URL".into()))?;if payload["protocol"]=="gemini"{validate_gemini(&payload)?;}let mut public=payload.clone();let key=public.as_object_mut().unwrap().remove("api_key").and_then(|v|v.as_str().map(str::to_owned));let connection=db.lock().map_err(db_error)?;if let Some(key)=key{ui_settings::save_secret(&connection,&format!("media:{id}"),&key)?;}store::put(&connection,KIND,id,public).map_err(db_error)?;}return catalog(db);}
 if path=="/api/voice-profiles"{if method==Method::Get{return Ok(json!({"profiles":store::list(&*db.lock().map_err(db_error)?,"voice").map_err(db_error)?}));}if method==Method::Post{let name=store::text(&payload,"name",100)?;let transcript=store::text(&payload,"transcript",2000)?;let encoded=store::text(&payload,"audio_base64",16*1024*1024+4)?;let bytes=data_encoding::BASE64.decode(encoded.as_bytes()).map_err(|_|(400,"Reference recording is not valid base64".into()))?;if bytes.len()>12*1024*1024{return Err((400,"Reference WAV exceeds 12 MiB".into()));}let duration=wav(&bytes)?;let id=crate::browser_client::unique_id();let profile=json!({"id":id,"name":name,"transcript":transcript,"duration_seconds":duration,"sha256":format!("{:x}",Sha256::digest(bytes))});let mut connection=db.lock().map_err(db_error)?;let tx=connection.transaction().map_err(db_error)?;ui_settings::save_secret(&tx,&format!("voice:{id}"),encoded)?;store::put(&tx,"voice",&id,profile.clone()).map_err(db_error)?;tx.commit().map_err(db_error)?;return Ok(json!({"ok":true,"profile":profile}));}}
 if let Some(id)=path.strip_prefix("/api/voice-profiles/").filter(|_|method==Method::Delete){let mut c=db.lock().map_err(db_error)?;let tx=c.transaction().map_err(db_error)?;tx.execute("DELETE FROM ui_documents WHERE kind='voice' AND id=?",[id]).map_err(db_error)?;crate::ui_credentials::delete(&tx,&format!("voice:{id}"))?;tx.commit().map_err(db_error)?;return Ok(json!({"ok":true}));}Err((405,"Unsupported media operation".into()))});
    match result {
        Ok(v) => server::response(request, 200, v),
        Err((c, e)) => server::response(request, c, json!({"error":e})),
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gemini_returns_the_finished_image_with_its_real_extension() {
        let response = json!({"candidates":[{"content":{"parts":[
            {"thought":true,"inlineData":{"data":"aWdub3Jl"}},
            {"text":"done"},
            {"inlineData":{"mimeType":"image/jpeg","data":"/9j/2Q=="}}
        ]}}]});
        let (bytes, name) = image_bytes(&response, true).unwrap();
        assert_eq!(bytes, [0xff, 0xd8, 0xff, 0xd9]);
        assert_eq!(name, "image.jpg");
        assert!(image_bytes(&json!({"candidates":[]}), true).is_err());
        assert!(validate_gemini(&json!({"base_url":"https://example.com","models":{"image":"gemini"}})).is_err());
    }

    #[test]
    fn zero_image_quota_is_distinct_from_temporary_rate_limiting() {
        let zero = br#"{"error":{"message":"Quota exceeded for a model, limit: 0, model: gemini"}}"#;
        assert!(gemini_quota_error(zero).contains("zero quota"));
        assert!(!gemini_quota_error(b"Too many requests").contains("zero quota"));
    }

    #[test]
    fn invalid_voice_references_are_rejected() {
        assert!(wav(b"not a wave").is_err());
        let mut bytes=b"RIFF\x24\0\0\0WAVEfmt \x10\0\0\0\x01\0\x01\0\x40\x1f\0\0\x80\x3e\0\0\x02\0\x10\0data\0\0\0\0".to_vec();
        assert!(wav(&bytes).is_err());
        bytes.extend_from_slice(&[0; 4]);
        assert!(wav(&bytes).is_err());
    }
}

pub(crate) fn turn_artifacts(
    db: &rusqlite::Connection,
    chat: &str,
    turn: &str,
) -> rusqlite::Result<Value> {
    let mut query = db.prepare(
        "SELECT path FROM ui_turn_artifacts WHERE chat_id=? AND turn_id=? ORDER BY path",
    )?;
    let rows = query
        .query_map(params![chat, turn], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(json!(rows))
}

fn seed_key(db:&rusqlite::Connection,id:&str,key:&str)->rusqlite::Result<()>{let name=format!("media:{id}");if !crate::ui_credentials::managed(db,&name).map_err(|(_,e)|rusqlite::Error::InvalidParameterName(e))?{ui_settings::save_secret(db,&name,key).map_err(|(_,e)|rusqlite::Error::InvalidParameterName(e))?;}Ok(())}
#[cfg(test)]mod credential_tests {use super::*;#[test]fn startup_and_dispatch_cannot_resurrect_revoked_media_keys(){let db=rusqlite::Connection::open_in_memory().unwrap();store::initialize(&db).unwrap();crate::ui_credentials::initialize(&db).unwrap();seed_key(&db,"fixture","old-environment-key").unwrap();crate::ui_credentials::delete(&db,"media:fixture").unwrap();seed_key(&db,"fixture","old-environment-key").unwrap();assert!(ui_settings::secret(&db,"media:fixture").unwrap().is_empty());store::put(&db,KIND,"fixture",json!({"id":"fixture","enabled":true,"models":{"tts":"fixture"},"base_url":"https://example.test"})).unwrap();let db=crate::agent_host::DatabaseState::open(db).unwrap();assert_eq!(config(&db,"fixture","tts").unwrap_err().0,403);}}
