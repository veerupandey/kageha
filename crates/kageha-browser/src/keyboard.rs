use crate::cdp::{Cdp, Result};
use serde_json::{json, Value};

pub struct Key {
    key: String,
    code: String,
    virtual_key: u8,
    modifiers: u8,
    text: Option<String>,
}

impl Key {
    pub fn parse(value: &str) -> Result<Self> {
        // A literal '+' is itself a key, including in chords such as Control++.
        let (name, prefix) = if let Some(prefix) = value.strip_suffix('+') {
            if prefix.is_empty() {
                ("+", "")
            } else {
                let prefix = prefix
                    .strip_suffix('+')
                    .filter(|prefix| !prefix.is_empty())
                    .ok_or("invalid_key")?;
                ("+", prefix)
            }
        } else if let Some((prefix, name)) = value.rsplit_once('+') {
            if prefix.is_empty() {
                return Err("invalid_key");
            }
            (name, prefix)
        } else {
            (value, "")
        };
        let mut modifiers = 0;
        for modifier in (!prefix.is_empty())
            .then_some(prefix)
            .into_iter()
            .flat_map(|prefix| prefix.split('+'))
        {
            let flag = match modifier {
                "Alt" => 1,
                "Control" => 2,
                "Meta" => 4,
                "Shift" => 8,
                _ => return Err("invalid_key"),
            };
            if modifiers & flag != 0 {
                return Err("invalid_key");
            }
            modifiers |= flag;
        }
        let (key, code, virtual_key, text) = match name {
            "Backspace" => named(name, 8, None),
            "Tab" => named(name, 9, None),
            "Enter" => named(name, 13, Some("\r")),
            "Escape" => named(name, 27, None),
            "Space" => (" ".into(), name.into(), 32, Some(" ".into())),
            "PageUp" => named(name, 33, None),
            "PageDown" => named(name, 34, None),
            "End" => named(name, 35, None),
            "Home" => named(name, 36, None),
            "ArrowLeft" => named(name, 37, None),
            "ArrowUp" => named(name, 38, None),
            "ArrowRight" => named(name, 39, None),
            "ArrowDown" => named(name, 40, None),
            "Delete" => named(name, 46, None),
            _ if name.len() == 1 && name.as_bytes()[0].is_ascii_alphanumeric() => {
                let byte = name.as_bytes()[0];
                let upper = byte.to_ascii_uppercase();
                let code = if byte.is_ascii_digit() {
                    "Digit"
                } else {
                    "Key"
                };
                let key = if modifiers & 8 != 0 && byte.is_ascii_digit() {
                    b")!@#$%^&*("[(byte - b'0') as usize]
                } else if modifiers & 8 != 0 {
                    upper
                } else {
                    byte
                };
                let key = char::from(key).to_string();
                (
                    key.clone(),
                    format!("{code}{}", char::from(upper)),
                    upper,
                    Some(key),
                )
            }
            _ if name.len() == 1 && name.as_bytes()[0].is_ascii_punctuation() => {
                let byte = name.as_bytes()[0];
                let mut found = None;
                for (plain, shifted, code, virtual_key) in [
                    (b'`', b'~', "Backquote", 192),
                    (b'-', b'_', "Minus", 189),
                    (b'=', b'+', "Equal", 187),
                    (b'[', b'{', "BracketLeft", 219),
                    (b']', b'}', "BracketRight", 221),
                    (b'\\', b'|', "Backslash", 220),
                    (b';', b':', "Semicolon", 186),
                    (b'\'', b'"', "Quote", 222),
                    (b',', b'<', "Comma", 188),
                    (b'.', b'>', "Period", 190),
                    (b'/', b'?', "Slash", 191),
                ] {
                    if byte == plain || byte == shifted {
                        if byte == shifted {
                            modifiers |= 8;
                        }
                        let key = if modifiers & 8 != 0 { shifted } else { plain };
                        let text = char::from(key).to_string();
                        found = Some((text.clone(), code.into(), virtual_key, Some(text)));
                        break;
                    }
                }
                // Shifted number-row symbols, including !, @ and #.
                if found.is_none() {
                    if let Some(index) = b")!@#$%^&*(".iter().position(|key| *key == byte) {
                        modifiers |= 8;
                        let digit = b'0' + index as u8;
                        let text = char::from(byte).to_string();
                        found = Some((
                            text.clone(),
                            format!("Digit{}", char::from(digit)),
                            digit,
                            Some(text),
                        ));
                    }
                }
                found.ok_or("invalid_key")?
            }
            _ => return Err("invalid_key"),
        };
        let text = if modifiers & 7 == 0 { text } else { None };
        Ok(Self {
            key,
            code,
            virtual_key,
            modifiers,
            text,
        })
    }

    pub async fn send(&self, cdp: &mut Cdp, session: &str, down: bool) -> Result<()> {
        let mut event = json!({
            "type": if down { if self.text.is_some() { "keyDown" } else { "rawKeyDown" } } else { "keyUp" },
            "key": self.key, "code": self.code, "windowsVirtualKeyCode": self.virtual_key,
            "modifiers": self.modifiers,
        });
        if down {
            if let Some(text) = &self.text {
                event["text"] = Value::String(text.clone());
            }
        }
        cdp.call(Some(session), "Input.dispatchKeyEvent", event)
            .await?;
        Ok(())
    }
}

fn named(name: &str, code: u8, text: Option<&str>) -> (String, String, u8, Option<String>) {
    (name.into(), name.into(), code, text.map(str::to_owned))
}

#[cfg(test)]
mod tests {
    use super::Key;

    #[test]
    fn printable_keys_preserve_physical_codes_shift_and_shortcuts() {
        for (input, key, code, modifiers, text) in [
            (".", ".", "Period", 0, Some(".")),
            ("Shift+1", "!", "Digit1", 8, Some("!")),
            ("!", "!", "Digit1", 8, Some("!")),
            ("+", "+", "Equal", 8, Some("+")),
            ("Shift+=", "+", "Equal", 8, Some("+")),
            ("Control++", "+", "Equal", 10, None),
            ("Control+A", "A", "KeyA", 2, None),
        ] {
            let parsed = Key::parse(input).unwrap();
            assert_eq!(
                (
                    parsed.key.as_str(),
                    parsed.code.as_str(),
                    parsed.modifiers,
                    parsed.text.as_deref()
                ),
                (key, code, modifiers, text),
                "{input}"
            );
        }
        for input in [
            "",
            "++",
            "Control+",
            "Control+++",
            "+A",
            "Shift++A",
            "Control+Control+A",
            "Unknown+A",
        ] {
            assert!(Key::parse(input).is_err(), "{input}");
        }
    }
}
