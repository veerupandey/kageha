//! Failure index (Requirements 9.2, 9.5 and 17.2; design section 15).
//!
//! - [`normalize`] reduces an error message to the text that identifies the failure.
//! - [`FailureSignature::from_failure`] builds the signature
//!   `sha256(tool, error_code, normalize(message), os, arch)` for this host.
//! - [`SqliteLearning::record_repair`] stores the 1–3 calls that repaired a failure as a
//!   fix in `learn_failures`, keyed by the signature and [`fix_hash`], and counts a success.
//! - [`SqliteLearning::record_fix_failure`] counts a failure for every stored fix whose steps
//!   ran before the same failure happened again.
//! - [`LearningPort::known_fix`](crate::LearningPort::known_fix) returns the fix with the
//!   most successes among those whose success ratio `successes / (successes + failures)` is
//!   at least 0.5. It is one query on the `(signature, fix_hash)` primary key.
use crate::{FailureSignature, KnownFix, SqliteLearning};
use rusqlite::{params, TransactionBehavior};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::io;
use std::time::{SystemTime, UNIX_EPOCH};

/// Characters kept from a normalized message.
pub const MAX_MESSAGE_CHARS: usize = 512;
/// Calls between a failure and its repair that are recorded as a fix.
pub const MAX_FIX_STEPS: usize = 3;
/// Largest step of a fix, such as `shell: uv pip install pdfplumber`.
pub const MAX_STEP_BYTES: usize = 512;
/// Largest stored example message.
pub const MAX_EXAMPLE_BYTES: usize = 1024;
/// Fixes of one signature examined when counting a failure.
const MAX_FIXES_PER_SIGNATURE: i64 = 256;

fn sql(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}
fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.to_string())
}
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Normalized output, built with whitespace collapsed and cut at [`MAX_MESSAGE_CHARS`].
struct Out {
    text: String,
    chars: usize,
    space: bool,
}

impl Out {
    fn full(&self) -> bool {
        self.chars >= MAX_MESSAGE_CHARS
    }
    fn push(&mut self, c: char) {
        if self.full() {
            return;
        }
        if c.is_whitespace() {
            self.space = self.chars > 0;
            return;
        }
        if self.space {
            self.space = false;
            // A space is only written when a character follows it within the limit.
            if self.chars + 2 > MAX_MESSAGE_CHARS {
                self.chars = MAX_MESSAGE_CHARS;
                return;
            }
            self.text.push(' ');
            self.chars += 1;
        }
        self.text.push(c);
        self.chars += 1;
    }
    fn push_str(&mut self, text: &str) {
        text.chars().for_each(|c| self.push(c));
    }
    /// The previous output character; a pending space counts as one.
    fn last(&self) -> Option<char> {
        if self.space {
            Some(' ')
        } else {
            self.text.chars().next_back()
        }
    }
}

fn word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}
fn hex_digit(c: char) -> bool {
    c.is_ascii_digit() || ('a'..='f').contains(&c)
}
/// Characters a URL runs over: everything but whitespace, quotes and brackets.
fn url_char(c: char) -> bool {
    !c.is_whitespace() && !matches!(c, '"' | '\'' | '`' | '<' | '>' | '(' | ')' | '[' | ']' | '{' | '}')
}
/// Characters a path runs over: [`url_char`] without separators `,` `;` `:`.
fn path_char(c: char) -> bool {
    url_char(c) && !matches!(c, ',' | ';' | ':')
}
/// Characters after which an absolute path may start.
fn path_boundary(prev: Option<char>) -> bool {
    prev.is_none_or(|c| {
        c.is_whitespace() || matches!(c, '"' | '\'' | '`' | '(' | '[' | '{' | '=' | ',' | ';' | ':')
    })
}

/// `scheme://authority...` at `i`: the end of the URL and its host.
fn url_at(chars: &[char], i: usize) -> Option<(usize, String)> {
    if !chars[i].is_ascii_lowercase() {
        return None;
    }
    let mut j = i + 1;
    while j < chars.len()
        && j - i <= 32
        && (chars[j].is_ascii_lowercase() || chars[j].is_ascii_digit() || matches!(chars[j], '+' | '.' | '-'))
    {
        j += 1;
    }
    if chars.get(j..j + 3) != Some(&[':', '/', '/']) {
        return None;
    }
    let start = j + 3;
    let mut end = start;
    while end < chars.len() && url_char(chars[end]) && !matches!(chars[end], '/' | '?' | '#') {
        end += 1;
    }
    let authority: String = chars[start..end].iter().collect();
    let host = authority.rsplit('@').next().unwrap_or("");
    let host = match host.rsplit_once(':') {
        Some((name, port)) if port.chars().all(|c| c.is_ascii_digit()) => name,
        _ => host,
    };
    while end < chars.len() && url_char(chars[end]) {
        end += 1;
    }
    Some((end, host.to_owned()))
}

/// A UUID (`8-4-4-4-12` hex digits) at `i`: its end.
fn uuid_at(chars: &[char], i: usize) -> Option<usize> {
    let end = i + 36;
    let token = chars.get(i..end)?;
    let shaped = token.iter().enumerate().all(|(n, &c)| {
        if matches!(n, 8 | 13 | 18 | 23) { c == '-' } else { hex_digit(c) }
    });
    (shaped && !chars.get(end).is_some_and(|&c| word(c) || c == '-')).then_some(end)
}

/// A hex token at `i`: `0x` with hex digits, or at least 8 hex characters mixing digits
/// and letters. Pure digit runs are numbers. Returns its end.
fn hex_at(chars: &[char], i: usize) -> Option<usize> {
    let prefixed = chars[i] == '0' && chars.get(i + 1) == Some(&'x');
    let start = if prefixed { i + 2 } else { i };
    let mut end = start;
    while end < chars.len() && hex_digit(chars[end]) {
        end += 1;
    }
    if chars.get(end).is_some_and(|&c| word(c)) {
        return None;
    }
    let token = &chars[start..end];
    let qualifies = if prefixed {
        !token.is_empty()
    } else {
        token.len() >= 8 && token.iter().any(char::is_ascii_digit) && token.iter().any(|c| c.is_ascii_lowercase())
    };
    qualifies.then_some(end)
}

/// An absolute path at `i`: `/x`, `~/x` or `c:\x`. Returns its end.
fn path_at(chars: &[char], i: usize) -> Option<usize> {
    let start = match chars[i] {
        '/' => i + 1,
        '~' if chars.get(i + 1) == Some(&'/') => i + 2,
        c if c.is_ascii_lowercase() && chars.get(i + 1) == Some(&':') && chars.get(i + 2) == Some(&'\\') => i + 3,
        _ => return None,
    };
    let mut end = start;
    while end < chars.len() && path_char(chars[end]) {
        end += 1;
    }
    (end > start).then_some(end)
}

fn scan(chars: &[char], out: &mut Out) {
    let mut i = 0;
    while i < chars.len() && !out.full() {
        let prev = out.last();
        let word_boundary = !prev.is_some_and(word);
        // URLs first, so their paths are dropped with them instead of becoming `<path>`.
        if word_boundary && !matches!(prev, Some('+' | '.' | '-')) {
            if let Some((end, host)) = url_at(chars, i) {
                let host: Vec<char> = host.chars().collect();
                scan(&host, out);
                i = end;
                continue;
            }
        }
        if word_boundary {
            if let Some(end) = uuid_at(chars, i) {
                out.push_str("<id>");
                i = end;
                continue;
            }
        }
        if path_boundary(prev) {
            if let Some(end) = path_at(chars, i) {
                out.push_str("<path>");
                i = end;
                continue;
            }
        }
        if word_boundary && hex_digit(chars[i]) {
            if let Some(end) = hex_at(chars, i) {
                out.push_str("<id>");
                i = end;
                continue;
            }
        }
        if chars[i].is_ascii_digit() {
            let mut end = i;
            loop {
                while end < chars.len() && chars[end].is_ascii_digit() {
                    end += 1;
                }
                if chars.get(end) == Some(&'.') && chars.get(end + 1).is_some_and(char::is_ascii_digit) {
                    end += 1;
                } else {
                    break;
                }
            }
            out.push_str("<n>");
            i = end;
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
}

/// The part of an error message that identifies a failure. The message is lowercased;
/// URLs become their host (before paths are replaced, so a URL's path is dropped with it);
/// UUIDs and hex tokens (`0x...`, or 8+ hex characters mixing digits and letters) become
/// `<id>`; absolute paths (`/...`, `~/...`, `c:\...`) become `<path>`; other digit runs,
/// with decimal parts, become `<n>`; whitespace runs become one space; and the first
/// [`MAX_MESSAGE_CHARS`] characters are kept. The input is scanned once, left to right,
/// so replacements are never rescanned and the result is deterministic.
pub fn normalize(message: &str) -> String {
    let chars: Vec<char> = message.to_lowercase().chars().collect();
    let mut out = Out { text: String::with_capacity(chars.len().min(MAX_MESSAGE_CHARS * 4)), chars: 0, space: false };
    scan(&chars, &mut out);
    out.text
}

impl FailureSignature {
    /// The signature of a failure on this host: `os` and `arch` from
    /// [`std::env::consts`], and `message` already normalized.
    pub fn from_failure(tool: &str, error_code: &str, message: &str) -> Self {
        Self {
            tool: tool.to_owned(),
            error_code: error_code.to_owned(),
            message: normalize(message),
            os: std::env::consts::OS.to_owned(),
            arch: std::env::consts::ARCH.to_owned(),
        }
    }
}

/// Hex sha256 over the length-prefixed steps of a fix.
pub fn fix_hash(steps: &[String]) -> String {
    let mut hash = Sha256::new();
    for step in steps {
        hash.update((step.len() as u64).to_be_bytes());
        hash.update(step.as_bytes());
    }
    hex(&hash.finalize())
}

/// One stored fix of a signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredFix {
    pub fix_hash: String,
    pub steps: Vec<String>,
    pub example: String,
    pub successes: u64,
    pub failures: u64,
    pub last_seen: u64,
}

fn check_steps(steps: &[String]) -> io::Result<()> {
    if steps.is_empty() || steps.len() > MAX_FIX_STEPS {
        return Err(invalid("A fix has 1 to 3 steps"));
    }
    if steps.iter().any(|step| step.trim().is_empty() || step.len() > MAX_STEP_BYTES || step.chars().any(char::is_control)) {
        return Err(invalid("Invalid fix step"));
    }
    Ok(())
}

fn example_text(example: &str) -> String {
    let text: String = example.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let mut cut = text.len().min(MAX_EXAMPLE_BYTES);
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    text[..cut].to_owned()
}

fn parse_steps(text: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(text).unwrap_or_default()
}

impl SqliteLearning {
    /// Records that `steps` (the 1–3 calls between a failure and the success of the same
    /// operation) repaired the failure `signature`: the fix gains a success and its
    /// `last_seen` and `example` are updated.
    pub fn record_repair(&self, signature: &FailureSignature, steps: &[String], example: &str) -> io::Result<()> {
        check_steps(steps)?;
        let encoded = serde_json::to_string(steps)?;
        let db = self.lock()?;
        db.execute(
            "INSERT INTO learn_failures(signature,fix_hash,example,steps,successes,failures,last_seen) VALUES(?,?,?,?,1,0,?)
             ON CONFLICT(signature,fix_hash) DO UPDATE SET successes=successes+1,example=excluded.example,
             last_seen=MAX(last_seen,excluded.last_seen)",
            params![signature.digest(), fix_hash(steps), example_text(example), encoded, now_ms()],
        )
        .map_err(sql)?;
        Ok(())
    }

    /// Records that `signature` failed again after the calls `between` ran since its
    /// previous failure in the same request. Every stored fix of the signature whose steps
    /// appear in `between`, in order and adjacent, gains a failure. Returns how many did.
    pub fn record_fix_failure(&self, signature: &FailureSignature, between: &[String]) -> io::Result<usize> {
        if between.is_empty() {
            return Ok(0);
        }
        let digest = signature.digest();
        let mut db = self.lock()?;
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sql)?;
        let applied: Vec<String> = {
            let mut query = tx
                .prepare("SELECT fix_hash,steps FROM learn_failures WHERE signature=? ORDER BY fix_hash LIMIT ?")
                .map_err(sql)?;
            let rows = query
                .query_map(params![digest, MAX_FIXES_PER_SIGNATURE], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                .map_err(sql)?;
            let mut applied = Vec::new();
            for row in rows {
                let (hash, steps) = row.map_err(sql)?;
                let steps = parse_steps(&steps);
                if !steps.is_empty() && between.windows(steps.len()).any(|window| window == steps.as_slice()) {
                    applied.push(hash);
                }
            }
            applied
        };
        for hash in &applied {
            tx.execute(
                "UPDATE learn_failures SET failures=failures+1 WHERE signature=? AND fix_hash=?",
                params![digest, hash],
            )
            .map_err(sql)?;
        }
        tx.commit().map_err(sql)?;
        Ok(applied.len())
    }

    /// Every stored fix of `signature`, most successes first.
    pub fn fixes(&self, signature: &FailureSignature) -> io::Result<Vec<StoredFix>> {
        let db = self.lock()?;
        let mut query = db
            .prepare(
                "SELECT fix_hash,steps,example,successes,failures,last_seen FROM learn_failures
                 WHERE signature=? ORDER BY successes DESC,last_seen DESC,fix_hash",
            )
            .map_err(sql)?;
        let rows = query
            .query_map([signature.digest()], |r| {
                Ok(StoredFix {
                    fix_hash: r.get(0)?,
                    steps: parse_steps(&r.get::<_, String>(1)?),
                    example: r.get(2)?,
                    successes: r.get::<_, i64>(3)?.max(0) as u64,
                    failures: r.get::<_, i64>(4)?.max(0) as u64,
                    last_seen: r.get::<_, i64>(5)?.max(0) as u64,
                })
            })
            .map_err(sql)?;
        rows.map(|row| row.map_err(sql)).collect()
    }

    /// The fix with the most successes whose success ratio is at least 0.5
    /// (`successes >= failures`, with at least one success).
    pub(crate) fn lookup_known_fix(&self, signature: &FailureSignature) -> io::Result<Option<KnownFix>> {
        let db = self.lock()?;
        let row = rusqlite::OptionalExtension::optional(db.query_row(
            "SELECT steps,successes,last_seen FROM learn_failures
             WHERE signature=? AND successes>0 AND successes>=failures
             ORDER BY successes DESC,last_seen DESC,fix_hash LIMIT 1",
            [signature.digest()],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?)),
        ))
        .map_err(sql)?;
        Ok(row.map(|(steps, successes, last_seen)| KnownFix {
            steps: parse_steps(&steps).into_iter().map(Value::String).collect(),
            successes: successes.max(0) as u64,
            last_seen: last_seen.max(0) as u64,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LearningPort;

    #[test]
    fn normalization_examples() {
        for (message, expected) in [
            ("ModuleNotFoundError: No module named 'pdfplumber'", "modulenotfounderror: no module named 'pdfplumber'"),
            (
                "File \"/Users/ana/work/extract.py\", line 12, in <module>",
                "file \"<path>\", line <n>, in <module>",
            ),
            ("open /tmp/x-1/out.txt: no such file", "open <path>: no such file"),
            ("cat: ~/notes/a.md: No such file", "cat: <path>: no such file"),
            ("Request a1b2c3d4-0000-4000-8000-1234567890ab failed", "request <id> failed"),
            ("commit 3f2a9c1be7d0 not found; ptr 0x7ffee4", "commit <id> not found; ptr <id>"),
            ("GET https://pypi.org/simple/pdfplumber/?x=1 returned 404", "get pypi.org returned <n>"),
            ("fetch http://user:pw@127.0.0.1:8080/a/b failed", "fetch <n> failed"),
            ("python3.11 took 2.5s", "python<n> took <n>s"),
            ("  many \n\t spaces  ", "many spaces"),
            ("C:\\Users\\Ana\\x.txt missing", "<path> missing"),
            ("added faced decade", "added faced decade"),
        ] {
            assert_eq!(normalize(message), expected, "{message}");
        }
        // A space that would be the last kept character is dropped.
        let cut = normalize(&format!("{} b", "a".repeat(MAX_MESSAGE_CHARS - 1)));
        assert_eq!(cut, "a".repeat(MAX_MESSAGE_CHARS - 1));
        let cut = normalize(&"word ".repeat(400));
        assert!(cut.chars().count() == MAX_MESSAGE_CHARS && !cut.ends_with(' '));
        let cut = normalize(&"é".repeat(2000));
        assert_eq!(cut.chars().count(), MAX_MESSAGE_CHARS);
    }

    #[test]
    fn normalization_is_idempotent_and_ignores_paths_numbers_ids_and_url_paths() {
        let a = "Error at /Users/ana/p/a.py line 3: id 5f0c2a9e-1d2b-4c3d-9e8f-0a1b2c3d4e5f, see https://x.org/a/b";
        let b = "Error at /home/bob/q.py line 41: id 00000000-aaaa-bbbb-cccc-dddddddddddd, see https://x.org/zz?q=1";
        assert_eq!(normalize(a), normalize(b));
        assert_eq!(normalize(&normalize(a)), normalize(a));
        assert_ne!(normalize("no module named pdfplumber"), normalize("no module named numpy"));
        let s = |message| FailureSignature::from_failure("shell", "exit_1", message);
        assert_eq!(s(a).digest(), s(b).digest());
        assert_ne!(s(a).digest(), FailureSignature::from_failure("shell", "exit_2", a).digest());
        assert_eq!((s(a).os.as_str(), s(a).arch.as_str()), (std::env::consts::OS, std::env::consts::ARCH));
        // The digest normalizes too, so a raw message gives the same signature.
        let raw = FailureSignature { message: a.into(), ..s(a) };
        assert_eq!(raw.digest(), s(b).digest());
    }

    fn store() -> (tempfile::TempDir, SqliteLearning) {
        let root = tempfile::tempdir().unwrap();
        let store = SqliteLearning::open(&root.path().join("home/learning.sqlite")).unwrap();
        (root, store)
    }
    fn steps(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn repairs_become_known_fixes_ranked_by_successes() {
        let (_root, store) = store();
        let signature = FailureSignature::from_failure("shell", "exit_1", "No module named 'pdfplumber'");
        assert!(store.known_fix(&signature).unwrap().is_none());
        let install = steps(&["shell: uv pip install pdfplumber"]);
        let other = steps(&["shell: pip install pdfplumber", "shell: echo done"]);
        store.record_repair(&signature, &install, "no module named 'pdfplumber'").unwrap();
        let fix = store.known_fix(&signature).unwrap().unwrap();
        assert_eq!(fix.steps, [Value::String("shell: uv pip install pdfplumber".into())]);
        assert_eq!(fix.successes, 1);
        assert!(fix.last_seen > 0);
        store.record_repair(&signature, &other, "x").unwrap();
        store.record_repair(&signature, &other, "x").unwrap();
        let fix = store.known_fix(&signature).unwrap().unwrap();
        assert_eq!((fix.steps.len(), fix.successes), (2, 2));
        let fixes = store.fixes(&signature).unwrap();
        assert_eq!(fixes.len(), 2);
        assert_eq!(fixes[0].fix_hash, fix_hash(&other));
        // Another signature has no fix.
        let numpy = FailureSignature::from_failure("shell", "exit_1", "No module named 'numpy'");
        assert!(store.known_fix(&numpy).unwrap().is_none());
        // Invalid steps are refused.
        assert!(store.record_repair(&signature, &[], "x").is_err());
        assert!(store.record_repair(&signature, &steps(&["a", "b", "c", "d"]), "x").is_err());
        assert!(store.record_repair(&signature, &steps(&["a\nb"]), "x").is_err());
    }

    #[test]
    fn fixes_below_half_success_are_not_suggested() {
        let (_root, store) = store();
        let signature = FailureSignature::from_failure("read_file", "not_found", "File not found");
        let write = steps(&["write_file: outputs/a.md"]);
        let best = steps(&["shell: make data"]);
        store.record_repair(&signature, &best, "x").unwrap();
        store.record_repair(&signature, &best, "x").unwrap();
        store.record_repair(&signature, &write, "x").unwrap();
        // `best` ran, then the failure repeated twice: 2 successes, 2 failures (ratio 0.5).
        let between = steps(&["todo_update: x", "shell: make data"]);
        assert_eq!(store.record_fix_failure(&signature, &between).unwrap(), 1);
        assert_eq!(store.record_fix_failure(&signature, &between).unwrap(), 1);
        assert_eq!(store.known_fix(&signature).unwrap().unwrap().successes, 2);
        // A third failure drops `best` below 0.5; `write` (1 of 1) is suggested instead.
        store.record_fix_failure(&signature, &between).unwrap();
        let fix = store.known_fix(&signature).unwrap().unwrap();
        assert_eq!((fix.steps, fix.successes), (vec![Value::String("write_file: outputs/a.md".into())], 1));
        store.record_fix_failure(&signature, &write).unwrap();
        store.record_fix_failure(&signature, &write).unwrap();
        assert!(store.known_fix(&signature).unwrap().is_none());
        // Steps that did not run, or ran out of order, are not counted.
        assert_eq!(store.record_fix_failure(&signature, &steps(&["shell: other"])).unwrap(), 0);
        assert_eq!(store.record_fix_failure(&signature, &[]).unwrap(), 0);
        let counts: Vec<_> = store.fixes(&signature).unwrap().iter().map(|f| (f.successes, f.failures)).collect();
        assert_eq!(counts, [(2, 3), (1, 2)]);
    }

    #[test]
    fn a_broken_store_returns_lookup_errors() {
        let (root, store) = store();
        rusqlite::Connection::open(root.path().join("home/learning.sqlite"))
            .unwrap()
            .execute_batch("DROP TABLE learn_failures;")
            .unwrap();
        let signature = FailureSignature::from_failure("shell", "exit_1", "boom");
        assert!(store.known_fix(&signature).is_err());
        assert!(store.record_repair(&signature, &steps(&["shell: x"]), "boom").is_err());
    }
}
