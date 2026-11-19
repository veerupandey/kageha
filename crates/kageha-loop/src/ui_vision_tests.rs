use super::*;
use crate::ui_attachments::{add_images, MAX_INLINE_IMAGE_BYTES};
use kageha_contracts::execution::AssistantMessage;
use std::sync::Mutex;

/// A vision model that records each request and answers with fixed text.
struct Seeing {
    requests: Mutex<Vec<ModelRequest>>,
}
impl ModelPort for Seeing {
    fn accounts_invocations(&self) -> bool {
        true
    }
    fn complete(&self, request: ModelRequest, _: &Control<'_>) -> io::Result<AssistantMessage> {
        self.requests.lock().unwrap().push(request);
        Ok(AssistantMessage { content: json!("Software Engineer, Acme, Berlin\nDescription: a job list"), tool_calls: vec![], continuation: None })
    }
}
fn db() -> Database {
    crate::agent_host::DatabaseState::open(rusqlite::Connection::open_in_memory().unwrap()).unwrap()
}
fn encode(image: image::DynamicImage, format: image::ImageFormat) -> Vec<u8> {
    let mut out = std::io::Cursor::new(Vec::new());
    image.write_to(&mut out, format).unwrap();
    out.into_inner()
}
/// Incompressible pixels, so the PNG is about 3 bytes per pixel.
fn noisy_png(width: u32, height: u32) -> Vec<u8> {
    let mut seed = 0x2545_f491_u32;
    let image = image::RgbImage::from_fn(width, height, |_, _| {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        image::Rgb([seed as u8, (seed >> 8) as u8, (seed >> 16) as u8])
    });
    encode(image::DynamicImage::ImageRgb8(image), image::ImageFormat::Png)
}
fn small_jpeg() -> Vec<u8> {
    encode(image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(64, 48, image::Rgb([200, 30, 30]))), image::ImageFormat::Jpeg)
}
fn user() -> Vec<Value> {
    vec![json!({"role":"user","content":"what is this?"})]
}

#[test]
fn rule_order_executor_then_planner_then_vision_model_then_none() {
    assert_eq!(choose(true, true, || Some("v")), Ok(false));
    assert_eq!(choose(false, true, || Some("v")), Ok(true));
    assert_eq!(choose(false, false, || Some("v")), Err(Some("v")));
    assert_eq!(choose::<&str>(false, false, || None), Err(None));
}

#[test]
fn an_image_capable_executor_gets_a_resized_photo_under_the_limit() {
    let photo = noisy_png(2000, 1000);
    assert!(photo.len() > 5_800_000, "fixture is {} bytes", photo.len());
    let mut messages = user();
    add_images(&db(), vec![("inputs/photo.png".into(), "image/png".into(), photo)], &mut messages, &Vision::Inline).unwrap();
    let parts = messages[0]["content"].as_array().unwrap();
    assert_eq!(parts[0]["text"], "what is this?");
    let url = parts[1]["image_url"]["url"].as_str().unwrap();
    let encoded = url.strip_prefix("data:image/jpeg;base64,").unwrap();
    let bytes = data_encoding::BASE64.decode(encoded.as_bytes()).unwrap();
    assert!(bytes.len() < MAX_INLINE_IMAGE_BYTES);
    let decoded = image::load_from_memory(&bytes).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (MAX_LONG_SIDE, MAX_LONG_SIDE / 2));
    assert!(has_images(&messages));
}

#[test]
fn a_text_only_turn_gets_a_cached_description_from_the_vision_model() {
    let db = db();
    let model = Seeing { requests: Mutex::new(Vec::new()) };
    let cancel = AtomicBool::new(false);
    let vision = Vision::Describe(Describer { model: &model, name: "cf-glm-5.3".into(), cancel: &cancel });
    for _ in 0..2 {
        let mut messages = user();
        add_images(&db, vec![("inputs/jobs.jpg".into(), "image/jpeg".into(), small_jpeg())], &mut messages, &vision).unwrap();
        let text = messages[0]["content"].as_str().unwrap();
        assert!(text.contains("[Image inputs/jobs.jpg — description by cf-glm-5.3]: Software Engineer, Acme, Berlin"), "{text}");
        assert!(!has_images(&messages));
    }
    let requests = model.requests.lock().unwrap();
    assert_eq!(requests.len(), 1, "the second look is a cache hit");
    assert!(has_images(&requests[0].messages));
    assert!(requests[0].messages[0]["content"][0]["text"].as_str().unwrap().contains("verbatim"));
}

#[test]
fn without_a_vision_model_the_message_says_so_plainly() {
    let mut messages = user();
    add_images(&db(), vec![("inputs/jobs.jpg".into(), "image/jpeg".into(), small_jpeg())], &mut messages,
        &Vision::Unavailable(NO_VISION_MODEL.into())).unwrap();
    let text = messages[0]["content"].as_str().unwrap();
    assert!(text.contains("image could not be analysed: no vision-capable model is configured"), "{text}");
    assert!(text.contains("ask the user") || text.contains("Ask the user"));
}

#[test]
fn read_file_on_a_jpeg_returns_a_description_not_an_extraction_error() {
    let db = db();
    let model = Seeing { requests: Mutex::new(Vec::new()) };
    let cancel = AtomicBool::new(false);
    let vision = Vision::Describe(Describer { model: &model, name: "vision-fixture".into(), cancel: &cancel });
    let queued = Mutex::new(Vec::new());
    let result = read_image(&db, "inputs/jobs.jpg", &small_jpeg(), &vision, None, &queued);
    assert!(result.starts_with("[Image inputs/jobs.jpg — description by vision-fixture]: Software Engineer"), "{result}");
    assert!(!result.contains("extraction"));
    let heic = read_image(&db, "inputs/photo.heic", b"\0\0\0\x18ftypheic", &vision, None, &queued);
    assert!(queued.lock().unwrap().is_empty());
    assert!(heic.contains("image could not be analysed"), "{heic}");
}

#[test]
fn view_image_attaches_the_image_after_the_result_when_the_model_sees_images() {
    let queued = Mutex::new(Vec::new());
    let result = read_image(&db(), "inputs/photo.png", &noisy_png(2000, 1000), &Vision::Inline, Some("what is the title?"), &queued);
    assert_eq!(result, "Image inputs/photo.png attached below.");
    let messages = queued.into_inner().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["role"], "user");
    assert!(messages[0]["content"][0]["text"].as_str().unwrap().contains("what is the title?"));
    let url = messages[0]["content"][1]["image_url"]["url"].as_str().unwrap();
    let bytes = data_encoding::BASE64.decode(url.strip_prefix("data:image/jpeg;base64,").unwrap().as_bytes()).unwrap();
    let decoded = image::load_from_memory(&bytes).unwrap();
    assert_eq!(decoded.width().max(decoded.height()), MAX_LONG_SIDE, "resized by fit()");
    assert!(has_images(&messages));
}

#[test]
fn view_image_on_a_text_only_turn_answers_the_question_and_caches_per_question() {
    let db = db();
    let model = Seeing { requests: Mutex::new(Vec::new()) };
    let cancel = AtomicBool::new(false);
    let vision = Vision::Describe(Describer { model: &model, name: "vision-fixture".into(), cancel: &cancel });
    let queued = Mutex::new(Vec::new());
    for question in [Some("Which company?"), Some("Which company?"), None] {
        let result = read_image(&db, "inputs/jobs.jpg", &small_jpeg(), &vision, question, &queued);
        assert!(result.starts_with("[Image inputs/jobs.jpg — description by vision-fixture]: Software Engineer"), "{result}");
    }
    assert!(queued.lock().unwrap().is_empty());
    let requests = model.requests.lock().unwrap();
    assert_eq!(requests.len(), 2, "the repeated question is a cache hit; another question is not");
    assert_eq!(requests[0].messages[0]["content"][0]["text"], "Which company?");
    assert!(requests[1].messages[0]["content"][0]["text"].as_str().unwrap().contains("verbatim"));
}

#[test]
fn view_image_without_a_vision_model_returns_the_note() {
    let queued = Mutex::new(Vec::new());
    let result = read_image(&db(), "inputs/jobs.jpg", &small_jpeg(), &Vision::Unavailable(NO_VISION_MODEL.into()), Some("Which company?"), &queued);
    assert!(result.contains("image could not be analysed: no vision-capable model is configured"), "{result}");
    assert!(queued.lock().unwrap().is_empty());
}
