//! Input from the interactive preview. Every command belongs to a selected
//! document and viewport, and uses the same control fence as agent input.
use crate::cdp::{Cdp, Result};
use crate::keyboard::Key;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::atomic::{AtomicBool, Ordering};

fn default_dpr() -> f64 {
    1.0
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Size {
    pub width: u64,
    pub height: u64,
    #[serde(default = "default_dpr")]
    pub dpr: f64,
}

pub struct Viewport {
    pub size: Size,
    pub scale: f64,
}

impl Viewport {
    pub async fn read(cdp: &mut Cdp, session: &str, frame: &str) -> Result<Self> {
        let world = cdp
            .call(
                Some(session),
                "Page.createIsolatedWorld",
                json!({
                    "frameId": frame, "worldName": "kageha-panel-geometry",
                }),
            )
            .await?;
        let context = world["executionContextId"]
            .as_u64()
            .ok_or("preview_geometry_unavailable")?;
        // The captured surface includes scrollbars, which cssVisualViewport
        // excludes. Read CSS window dimensions in a world the page cannot patch.
        let result = cdp.call(Some(session), "Runtime.evaluate", json!({
            "contextId": context, "returnByValue": true,
            "expression": "({width: innerWidth, height: innerHeight, scale: visualViewport?.scale ?? 1, dpr: devicePixelRatio ?? 1})",
        })).await?;
        let v = &result["result"]["value"];
        let size = Size {
            width: v["width"].as_u64().ok_or("preview_geometry_unavailable")?,
            height: v["height"].as_u64().ok_or("preview_geometry_unavailable")?,
            dpr: (v["dpr"].as_f64().unwrap_or(1.0) * 10.0).round() / 10.0,
        };
        if !(1..=4096).contains(&size.width) || !(1..=4096).contains(&size.height) {
            return Err("preview_geometry_unavailable");
        }
        Ok(Self {
            size,
            scale: v["scale"].as_f64().ok_or("preview_geometry_unavailable")?,
        })
    }

    pub(crate) fn point(&self, x: f64, y: f64) -> Result<()> {
        if self.scale != 1.0
            || !(0.0..self.size.width as f64).contains(&x)
            || !(0.0..self.size.height as f64).contains(&y)
        {
            return Err("invalid_panel_coordinates");
        }
        Ok(())
    }
}

pub struct Layout {
    pub cdp: Cdp,
    pub session: String,
    pub size: Size,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Button {
    Left,
    Middle,
    Right,
}

impl Button {
    pub(crate) fn name(&self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Middle => "middle",
            Self::Right => "right",
        }
    }
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Input {
    Click {
        x: f64,
        y: f64,
        button: Button,
    },
    Wheel {
        x: f64,
        y: f64,
        delta_x: f64,
        delta_y: f64,
    },
    Move {
        x: f64,
        y: f64,
    },
    Drag {
        x: f64,
        y: f64,
        to_x: f64,
        to_y: f64,
    },
    Type {
        text: String,
    },
    Press {
        key: String,
    },
    Navigate {
        url: String,
    },
    Reload,
    Back,
    Forward,
    Focus,
    TabNew,
    TabClose,
    Layout {
        size: Option<Size>,
    },
}

impl Input {
    pub async fn dispatch(
        self,
        cdp: &mut Cdp,
        session: &str,
        viewport: &Viewport,
        dispatched: &AtomicBool,
        document: Option<&str>,
    ) -> Result<()> {
        let (method, params) = match self {
            Self::Click { x, y, button } => {
                viewport.point(x, y)?;
                let event = |kind| json!({"type": kind, "x": x, "y": y, "button": button.name(), "clickCount": 1});
                dispatched.store(true, Ordering::Relaxed);
                cdp.call(
                    Some(session),
                    "Input.dispatchMouseEvent",
                    event("mousePressed"),
                )
                .await?;
                check_document(cdp, session, document).await?;
                ("Input.dispatchMouseEvent", event("mouseReleased"))
            }
            Self::Wheel {
                x,
                y,
                delta_x,
                delta_y,
            } => {
                viewport.point(x, y)?;
                (
                    "Input.dispatchMouseEvent",
                    json!({"type": "mouseWheel", "x": x, "y": y,
                    "deltaX": delta_x.clamp(-2000.0, 2000.0), "deltaY": delta_y.clamp(-2000.0, 2000.0)}),
                )
            }
            Self::Move { x, y } => {
                viewport.point(x, y)?;
                (
                    "Input.dispatchMouseEvent",
                    json!({"type": "mouseMoved", "x": x, "y": y}),
                )
            }
            Self::Drag { x, y, to_x, to_y } => {
                viewport.point(x, y)?;
                viewport.point(to_x, to_y)?;
                dispatched.store(true, Ordering::Relaxed);
                cdp.call(Some(session), "Input.dispatchMouseEvent", json!({
                    "type": "mousePressed", "x": x, "y": y, "button": "left", "buttons": 1, "clickCount": 1,
                })).await?;
                for step in 1..=8 {
                    check_document(cdp, session, document).await?;
                    let progress = step as f64 / 8.0;
                    cdp.call(
                        Some(session),
                        "Input.dispatchMouseEvent",
                        json!({
                            "type": "mouseMoved", "x": x + (to_x - x) * progress,
                            "y": y + (to_y - y) * progress, "button": "left", "buttons": 1,
                        }),
                    )
                    .await?;
                }
                check_document(cdp, session, document).await?;
                (
                    "Input.dispatchMouseEvent",
                    json!({"type": "mouseReleased", "x": to_x, "y": to_y,
                    "button": "left", "buttons": 0, "clickCount": 1}),
                )
            }
            Self::Type { text } if !text.is_empty() && text.len() <= 4096 => {
                ("Input.insertText", json!({"text": text}))
            }
            Self::Press { key } => {
                let key = Key::parse(&key)?;
                dispatched.store(true, Ordering::Relaxed);
                key.send(cdp, session, true).await?;
                return key.send(cdp, session, false).await;
            }
            Self::Navigate { url } => {
                let trimmed = url.trim();
                let normalized = if !trimmed.is_empty()
                    && !trimmed.contains("://")
                    && !trimmed.starts_with("about:")
                    && !trimmed.starts_with("data:")
                    && !trimmed.starts_with("javascript:")
                {
                    if trimmed.starts_with("localhost")
                        || trimmed.starts_with("127.0.0.1")
                        || trimmed.starts_with("[::1]")
                    {
                        format!("http://{trimmed}")
                    } else {
                        format!("https://{trimmed}")
                    }
                } else {
                    trimmed.to_string()
                };
                cdp.check_url(&normalized)?;
                ("Page.navigate", json!({"url": normalized}))
            }
            Self::Reload => ("Page.reload", json!({})),
            Self::Back | Self::Forward => {
                let history = cdp
                    .call(Some(session), "Page.getNavigationHistory", json!({}))
                    .await?;
                let index = history["currentIndex"]
                    .as_i64()
                    .ok_or("cdp_invalid_reply")?;
                let offset = if matches!(self, Self::Back) { -1 } else { 1 };
                let entry = history["entries"]
                    .get((index + offset) as usize)
                    .and_then(|e| e["id"].as_i64());
                let Some(entry) = entry else {
                    return Ok(());
                };
                ("Page.navigateToHistoryEntry", json!({"entryId": entry}))
            }
            Self::Focus => ("Page.bringToFront", json!({})),
            _ => return Err("invalid_params"),
        };
        dispatched.store(true, Ordering::Relaxed);
        let reply = cdp.call(Some(session), method, params).await?;
        if reply.get("errorText").is_some() {
            return Err("panel_navigation_failed");
        }
        Ok(())
    }
}

async fn check_document(cdp: &mut Cdp, session: &str, document: Option<&str>) -> Result<()> {
    if let Some(document) = document {
        let frame = cdp
            .call(Some(session), "Page.getFrameTree", json!({}))
            .await?;
        if frame["frameTree"]["frame"]["loaderId"] != document {
            return Err("document_changed");
        }
    }
    Ok(())
}
