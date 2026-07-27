//! Secret scan of learned text (design section 15 "Safety"; Requirements 15.6 and 19.6).
//!
//! Lessons and skill files are scanned before they are stored, and a match drops the
//! lesson or skill ([`contains_secret`], [`scan`]). Run_Journal entries are redacted
//! instead: [`redact_values`] and [`redact_json`] replace credential vault values with
//! `[redacted]` (the Host scrubs secret patterns from journal text itself).
//!
//! A scan reports a [`Finding`], which names the kind of match and never holds the
//! matched text, so it can be logged and stored as a drop reason.
//!
//! **Vault values** come from a [`SecretSource`] (the Host's credential vault). Only
//! values of at least [`MIN_VAULT_VALUE_CHARS`] characters are matched, as substrings,
//! both as they are and JSON-escaped up to [`ESCAPE_DEPTH`] levels (a value with a `"` or
//! `\` appears escaped in a tool result's JSON text and in `SKILL.md`'s quoted
//! description). Vault values are never exempt, whatever their shape: a value that looks
//! like a hex digest is still matched. [`redact_json`] also redacts a number whose digits
//! hold a value. A source that cannot be read makes [`contains_secret`] report
//! [`VAULT_UNAVAILABLE`], so the text is dropped rather than stored unchecked.
//!
//! **Patterns** ([`find_pattern`]), in this order:
//! - `pem_private_key`: a `-----BEGIN ... PRIVATE KEY-----` line;
//! - `bearer_token`: `Bearer <token>` where the token has at least 16 characters and
//!   holds a digit or both letter cases;
//! - `sk_key`: `sk-` and at least 20 base64url characters including a digit;
//! - `github_token`: `ghp_` or `gho_` and at least 20 letters or digits, or
//!   `github_pat_` and at least 20 letters, digits or `_`;
//! - `aws_access_key`: `AKIA` and 16 upper-case letters or digits;
//! - `slack_token`: `xoxb-`, `xoxp-`, `xoxa-` or `xoxs-` and at least 10 characters;
//! - `jwt`: three `.`-separated base64url segments, the first starting with `eyJ`
//!   (`{"` encoded), the first two of at least 10 characters;
//! - `password`: `password=<value>`, or `"password": <value>` with the name quoted;
//! - `secret_assignment`: `NAME=<value>` where `NAME` matches [`is_secret_name`];
//! - `high_entropy`: a run of at least 32 characters of letters, digits and `+/=_-`
//!   that mixes letters and digits (see [`high_entropy`] for the exact rule).
//!
//! Prefix patterns need a word boundary before the prefix (`AKIA` does not, as in the
//! design's pattern). For `password` and `secret_assignment` the value must be
//! non-empty and not a reference or placeholder: values starting with `$`, `<`, `[` or
//! `{{` (`$TOKEN`, `<your key>`, `[redacted]`, `{{ secrets.X }}`), `null` and `***`
//! masks are not secrets.
//!
//! Hex strings of digest length (32, 40, 64 or 128 hex digits) and UUIDs are not
//! flagged as `high_entropy`: they are sha256 digests used as signatures and hashes,
//! commit ids, and the Host's own request ids (truncated sha256), which every learned
//! skill's `SKILL.md` names in `metadata.learned_from`.
use serde_json::Value;
use std::{cmp::Reverse, fmt, io};
use zeroize::Zeroizing;

/// Shortest vault value that is matched; shorter values would match ordinary words.
pub const MIN_VAULT_VALUE_CHARS: usize = 8;
/// What a vault value in a journal entry is replaced with.
pub const REDACTED: &str = "[redacted]";
/// The [`Finding::kind`] of a vault value.
pub const VAULT_VALUE: &str = "vault_value";
/// The [`Finding::kind`] reported when the [`SecretSource`] could not be read.
pub const VAULT_UNAVAILABLE: &str = "vault_unavailable";
/// Levels of JSON escaping at which a vault value is still matched: a value in a JSON
/// document held as text (a tool result), that document inside a JSON string, and one
/// more.
pub const ESCAPE_DEPTH: usize = 3;

/// The values held in the credential vault. Implementations return them fresh or from
/// memory only; [`Zeroizing`] clears each value when it is dropped.
pub trait SecretSource {
    fn values(&self) -> io::Result<Vec<Zeroizing<String>>>;
}

/// A source with no values: only the patterns are checked.
pub struct NoSecrets;

impl SecretSource for NoSecrets {
    fn values(&self) -> io::Result<Vec<Zeroizing<String>>> {
        Ok(Vec::new())
    }
}

/// A fixed set of values, for example a snapshot taken once for several scans.
pub struct SecretValues(pub Vec<Zeroizing<String>>);

impl SecretValues {
    pub fn new<I: IntoIterator<Item = S>, S: Into<String>>(values: I) -> Self {
        Self(values.into_iter().map(|value| Zeroizing::new(value.into())).collect())
    }
}

impl SecretSource for SecretValues {
    fn values(&self) -> io::Result<Vec<Zeroizing<String>>> {
        Ok(self.0.clone())
    }
}

/// A secret found in text: the pattern name, [`VAULT_VALUE`] or [`VAULT_UNAVAILABLE`].
/// It never holds the matched text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Finding {
    pub kind: &'static str,
}

impl fmt::Display for Finding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.kind)
    }
}

/// The first secret in `text`: a secret pattern or a value from `vault`. A vault that
/// cannot be read is reported as [`VAULT_UNAVAILABLE`], so callers drop the text.
pub fn contains_secret(text: &str, vault: &dyn SecretSource) -> Option<Finding> {
    if let Some(finding) = find_pattern(text) {
        return Some(finding);
    }
    match vault.values() {
        Ok(values) => find_vault_value(text, &values),
        Err(_) => Some(Finding {
            kind: VAULT_UNAVAILABLE,
        }),
    }
}

/// The first of `files` (path and bytes, such as a skill package's files) whose text holds
/// a secret ([`contains_secret`]), with what was found. Bytes that are not UTF-8 are read
/// lossily, so text around them is still scanned.
pub fn find_in_files<'a>(
    files: impl IntoIterator<Item = (&'a String, &'a Vec<u8>)>,
    vault: &dyn SecretSource,
) -> Option<(&'a str, Finding)> {
    files.into_iter().find_map(|(path, bytes)| {
        contains_secret(&String::from_utf8_lossy(bytes), vault).map(|finding| (path.as_str(), finding))
    })
}

/// [`contains_secret`] with vault values already read.
pub fn scan(text: &str, vault_values: &[Zeroizing<String>]) -> Option<Finding> {
    find_pattern(text).or_else(|| find_vault_value(text, vault_values))
}

fn usable(value: &str) -> bool {
    value.chars().count() >= MIN_VAULT_VALUE_CHARS
}

/// A character that a JSON string literal escapes.
fn json_escaped_char(c: char) -> bool {
    matches!(c, '"' | '\\') || (c as u32) < 0x20
}

/// `text` as the inside of a JSON string literal, escaped as `serde_json` escapes it. The
/// buffer is sized for the worst case, so it is never reallocated and the only copy of
/// the value is zeroized.
fn json_escape(text: &str) -> Zeroizing<String> {
    let mut out = Zeroizing::new(String::with_capacity(text.len() * 6));
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                const HEX: &[u8; 16] = b"0123456789abcdef";
                out.push_str("\\u00");
                out.push(HEX[(c as usize) >> 4] as char);
                out.push(HEX[(c as usize) & 0xf] as char);
            }
            c => out.push(c),
        }
    }
    out
}

/// One string a vault value may appear as.
enum Needle<'a> {
    Plain(&'a str),
    Escaped(Zeroizing<String>),
}

impl Needle<'_> {
    fn as_str(&self) -> &str {
        match self {
            Self::Plain(text) => text,
            Self::Escaped(text) => text.as_str(),
        }
    }
}

/// The forms of the usable `vault_values` that are matched: each value, and, when it holds
/// a `"`, `\` or control character, its JSON-escaped forms up to [`ESCAPE_DEPTH`] levels
/// (the value in a JSON document held as text, such as a tool result or a quoted YAML
/// scalar, and that document inside another JSON string).
fn needles(vault_values: &[Zeroizing<String>]) -> Vec<Needle<'_>> {
    let mut needles = Vec::new();
    for value in vault_values.iter().filter(|value| usable(value)) {
        needles.push(Needle::Plain(value.as_str()));
        let mut form: Option<Zeroizing<String>> = None;
        for _ in 0..ESCAPE_DEPTH {
            let current = form.as_ref().map_or(value.as_str(), |form| form.as_str());
            if !current.chars().any(json_escaped_char) {
                break;
            }
            let next = json_escape(current);
            needles.push(Needle::Escaped(next.clone()));
            form = Some(next);
        }
    }
    needles
}

/// [`VAULT_VALUE`] when `text` holds one of `vault_values` (those of at least
/// [`MIN_VAULT_VALUE_CHARS`] characters), plain or JSON-escaped.
pub fn find_vault_value(text: &str, vault_values: &[Zeroizing<String>]) -> Option<Finding> {
    needles(vault_values)
        .iter()
        .any(|needle| text.contains(needle.as_str()))
        .then_some(Finding { kind: VAULT_VALUE })
}

/// `text` with every vault value, plain or JSON-escaped, replaced by [`REDACTED`],
/// longest first, or `None` when it holds none.
pub fn redact_values(text: &str, vault_values: &[Zeroizing<String>]) -> Option<String> {
    redact_needles(text, &needles(vault_values))
}

fn redact_needles(text: &str, needles: &[Needle<'_>]) -> Option<String> {
    let mut found: Vec<&str> = needles
        .iter()
        .map(Needle::as_str)
        .filter(|needle| text.contains(needle))
        .collect();
    if found.is_empty() {
        return None;
    }
    found.sort_by_key(|needle| Reverse(needle.len()));
    let mut out = text.to_owned();
    for needle in found {
        out = out.replace(needle, REDACTED);
    }
    Some(out)
}

/// Redacts vault values, plain or JSON-escaped, in every string and object key of
/// `value`; a number whose digits hold a value becomes the string [`REDACTED`]. Returns
/// how many strings, keys and numbers changed.
pub fn redact_json(value: &mut Value, vault_values: &[Zeroizing<String>]) -> usize {
    let needles = needles(vault_values);
    if needles.is_empty() {
        return 0;
    }
    redact_json_needles(value, &needles)
}

fn redact_json_needles(value: &mut Value, needles: &[Needle<'_>]) -> usize {
    match value {
        Value::String(text) => match redact_needles(text, needles) {
            Some(redacted) => {
                *text = redacted;
                1
            }
            None => 0,
        },
        Value::Number(number) => {
            let digits = number.to_string();
            if needles.iter().any(|needle| digits.contains(needle.as_str())) {
                *value = Value::String(REDACTED.into());
                1
            } else {
                0
            }
        }
        Value::Array(items) => items.iter_mut().map(|item| redact_json_needles(item, needles)).sum(),
        Value::Object(object) => {
            let mut changed = 0;
            let entries = std::mem::take(object);
            for (key, mut item) in entries {
                changed += redact_json_needles(&mut item, needles);
                let key = match redact_needles(&key, needles) {
                    Some(redacted) => {
                        changed += 1;
                        redacted
                    }
                    None => key,
                };
                object.insert(key, item);
            }
            changed
        }
        _ => 0,
    }
}

/// True for names matching `*_TOKEN`, `*_KEY`, `*_SECRET` or `KAGEHA_SERVER_TOKEN`,
/// ignoring case. A copy of `kageha_workspace::shell::is_secret_name`, so this crate does
/// not depend on the workspace crate.
pub fn is_secret_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    upper == "KAGEHA_SERVER_TOKEN"
        || upper.ends_with("_TOKEN")
        || upper.ends_with("_KEY")
        || upper.ends_with("_SECRET")
}

/// The first secret pattern `text` matches (see the module documentation).
pub fn find_pattern(text: &str) -> Option<Finding> {
    const CHECKS: [(&str, fn(&str) -> bool); 10] = [
        ("pem_private_key", pem_private_key),
        ("bearer_token", bearer_token),
        ("sk_key", sk_key),
        ("github_token", github_token),
        ("aws_access_key", aws_access_key),
        ("slack_token", slack_token),
        ("jwt", jwt),
        ("password", password),
        ("secret_assignment", secret_assignment),
        ("high_entropy", high_entropy),
    ];
    CHECKS
        .iter()
        .find(|(_, check)| check(text))
        .map(|(kind, _)| Finding { kind: *kind })
}

fn base64url(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')
}

fn alphanumeric(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
}

/// No letter or digit right before `at`.
fn boundary_before(text: &str, at: usize) -> bool {
    at == 0 || !text.as_bytes()[at - 1].is_ascii_alphanumeric()
}

/// The length of the run of `class` bytes at the start of `text`.
fn run(text: &str, class: fn(u8) -> bool) -> usize {
    text.bytes().take_while(|byte| class(*byte)).count()
}

/// `prefix` at a word boundary, followed by at least `min` bytes of `class`, including a
/// digit when `digit` is set.
fn prefixed(text: &str, prefix: &str, min: usize, class: fn(u8) -> bool, digit: bool) -> bool {
    text.match_indices(prefix).any(|(at, _)| {
        let rest = &text[at + prefix.len()..];
        let len = run(rest, class);
        boundary_before(text, at)
            && len >= min
            && (!digit || rest.as_bytes()[..len].iter().any(u8::is_ascii_digit))
    })
}

fn pem_private_key(text: &str) -> bool {
    text.match_indices("-----BEGIN ").any(|(at, begin)| {
        let line = text[at + begin.len()..].split('\n').next().unwrap_or("");
        line.find("PRIVATE KEY-----").is_some_and(|end| end <= 64)
    })
}

fn bearer_token(text: &str) -> bool {
    let token_char = |byte: u8| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'+' | b'/' | b'=');
    let lower = text.to_ascii_lowercase();
    lower.match_indices("bearer").any(|(at, word)| {
        let rest = &text[at + word.len()..];
        let value = rest.trim_start_matches([' ', '\t']);
        if !boundary_before(text, at) || value.len() == rest.len() {
            return false;
        }
        let token = &value.as_bytes()[..run(value, token_char)];
        let has = |test: fn(&u8) -> bool| token.iter().any(test);
        token.len() >= 16 && (has(u8::is_ascii_digit) || (has(u8::is_ascii_lowercase) && has(u8::is_ascii_uppercase)))
    })
}

fn sk_key(text: &str) -> bool {
    prefixed(text, "sk-", 20, base64url, true)
}

fn github_token(text: &str) -> bool {
    prefixed(text, "ghp_", 20, alphanumeric, false)
        || prefixed(text, "gho_", 20, alphanumeric, false)
        || prefixed(text, "github_pat_", 20, |byte| byte.is_ascii_alphanumeric() || byte == b'_', false)
}

fn aws_access_key(text: &str) -> bool {
    text.match_indices("AKIA").any(|(at, prefix)| {
        run(&text[at + prefix.len()..], |byte| byte.is_ascii_uppercase() || byte.is_ascii_digit()) >= 16
    })
}

fn slack_token(text: &str) -> bool {
    ["xoxb-", "xoxp-", "xoxa-", "xoxs-"]
        .iter()
        .any(|prefix| prefixed(text, prefix, 10, |byte| byte.is_ascii_alphanumeric() || byte == b'-', false))
}

fn jwt(text: &str) -> bool {
    text.match_indices("eyJ").any(|(at, _)| {
        if !boundary_before(text, at) {
            return false;
        }
        let mut rest = &text[at..];
        let mut lengths = [0usize; 3];
        for (index, length) in lengths.iter_mut().enumerate() {
            *length = run(rest, base64url);
            rest = &rest[*length..];
            if index < 2 {
                match rest.strip_prefix('.') {
                    Some(next) => rest = next,
                    None => return false,
                }
            }
        }
        lengths[0] >= 10 && lengths[1] >= 10 && lengths[2] >= 1
    })
}

/// Whether the text after `=` or `:` starts a value that may be a secret: not empty, and
/// not a reference, placeholder, mask, `null` or redacted value.
fn real_value(rest: &str) -> bool {
    let rest = rest.trim_start_matches([' ', '\t']);
    let (quote, rest) = match rest.chars().next() {
        Some(quote @ ('"' | '\'')) => (Some(quote), &rest[1..]),
        _ => (None, rest),
    };
    let end = rest
        .find(|c: char| match quote {
            Some(quote) => c == quote || c == '\n',
            None => c.is_whitespace() || matches!(c, ',' | ';' | '&' | '}' | ')' | ']' | '"' | '\''),
        })
        .unwrap_or(rest.len());
    let value = &rest[..end];
    !(value.is_empty()
        || value.starts_with(['$', '<', '['])
        || value.starts_with("{{")
        || value.chars().all(|c| c == '*')
        || value.eq_ignore_ascii_case("null"))
}

fn password(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.match_indices("password").any(|(at, name)| {
        let rest = &text[at + name.len()..];
        let (quoted, rest) = match rest.as_bytes().first() {
            Some(b'"' | b'\'') => (true, &rest[1..]),
            _ => (false, rest),
        };
        let rest = rest.trim_start_matches([' ', '\t']);
        match rest.as_bytes().first() {
            Some(b'=') => !rest[1..].starts_with('=') && real_value(&rest[1..]),
            Some(b':') if quoted => real_value(&rest[1..]),
            _ => false,
        }
    })
}

fn secret_assignment(text: &str) -> bool {
    text.match_indices('=').any(|(at, _)| {
        let before = &text[..at];
        let start = before
            .char_indices()
            .rev()
            .find(|(_, c)| !(c.is_ascii_alphanumeric() || *c == '_'))
            .map_or(0, |(index, c)| index + c.len_utf8());
        let name = &before[start..];
        let value = &text[at + 1..];
        !name.is_empty() && is_secret_name(name) && !value.starts_with('=') && real_value(value)
    })
}

/// A hex digest or id: 32, 40, 64 or 128 hex digits, or a UUID.
fn hex_id(word: &str) -> bool {
    let hex = |text: &str| text.bytes().all(|byte| byte.is_ascii_hexdigit());
    let uuid = word.len() == 36
        && word.split('-').map(str::len).eq([8, 4, 4, 4, 12])
        && hex(&word.replace('-', ""));
    uuid || (matches!(word.len(), 32 | 40 | 64 | 128) && hex(word))
}

/// Shannon entropy of `word` in bits per character.
fn entropy(word: &str) -> f64 {
    let mut counts = [0u32; 256];
    for byte in word.bytes() {
        counts[byte as usize] += 1;
    }
    let len = word.len() as f64;
    counts
        .iter()
        .filter(|count| **count > 0)
        .map(|count| {
            let p = f64::from(*count) / len;
            -p * p.log2()
        })
        .sum()
}

/// A long high-entropy string: a run of at least 32 letters, digits and `+/=_-`
/// (outer `-`, `_` and `/` trimmed) with an entropy of at least 3 bits per character
/// that holds a digit and a letter. A run with `-` or `_` must also hold both letter
/// cases (so `weekly-report-2024-11-12-final` slugs pass), and one with `/` both
/// cases and at least 40 characters (so paths pass). Hex digests and ids ([`hex_id`])
/// are not flagged.
fn high_entropy(text: &str) -> bool {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '_' | '-')))
        .map(|word| word.trim_matches(['-', '_', '/']))
        .any(|word| {
            if word.len() < 32 || hex_id(word) {
                return false;
            }
            let has = |test: fn(&u8) -> bool| word.as_bytes().iter().any(test);
            let (lower, upper, digit) = (has(u8::is_ascii_lowercase), has(u8::is_ascii_uppercase), has(u8::is_ascii_digit));
            let mixed = if word.contains('/') {
                word.len() >= 40 && lower && upper && digit
            } else if word.contains(['-', '_']) {
                lower && upper && digit
            } else {
                digit && (lower || upper)
            };
            mixed && entropy(word) >= 3.0
        })
}

#[cfg(test)]
#[path = "secrets_tests.rs"]
mod tests;
