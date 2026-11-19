// Feature: autonomous-agent-loop, Property 17: Secrets excluded
//! Property 17, journal part, end to end: the real Run_Journal worker (`ui_learning`,
//! installed with [`install_with`] and a [`SecretValues`] source) gets runtime frames and
//! Host events whose text holds vault values: in commands, tool results (which hold them
//! JSON-escaped), failure messages that a repair turns into a learned fix, call ids and
//! nested event payloads, keys included. Every TEXT and BLOB cell of every table in the
//! learning store's SQLite file is then read raw, and none may hold a vault value, plain or
//! JSON-escaped. The failure index's `example`, which is lowercased, may not hold a value
//! lowercased either.
//!
//! **Validates: Requirements 19.6, 2.4**
use crate::audited_session_tests::Fixture;
use crate::ui_learning::{attach_known_fix, entry, flush, install_with, known_fix, observe, offer, store, FLUSH_WAIT};
use kageha_learning::SecretValues;
use kageha_runtime::execution::ToolCall;
use proptest::prelude::*;
use proptest::sample::select;
use serde_json::{json, Value};

const CASES: u32 = 64;

const HEX: &[u8] = b"0123456789abcdef";
const ALNUM: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
const BASE64: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789+/";
/// Password characters, including the JSON-escaped `"` and `\`. No whitespace and no
/// `[`/`]`, which `[redacted]` is made of.
const SYMBOLS: &[u8] = b"abcdefghijkmnopqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789!@#$%^&*()-_=+{};:'\",.<>/?\\|~";
const UNICODE: &[char] = &['ä', 'ö', 'ü', 'ß', 'é', '€', '🔑', '密', '码', 'a', 'b', 'c', '1', '2', '-'];
/// Words of passphrase-like values; not in [`FILLER`].
const VAULT_WORDS: &[&str] = &["correct", "horse", "battery", "staple", "violet", "marble", "harbor", "lantern"];
const FILLER: &[&str] = &["the", "report", "build", "notes", "check", "output", "run", "file", "login", "failed"];
const WRAPS: &[(&str, &str)] = &[
    ("", ""), (" ", " "), ("(", ")"), ("\"", "\""), ("'", "'"), ("=", ";"), (":", ","), ("/", "/"),
    ("\n", "\n"), ("{\"k\":\"", "\"}"), ("\\", "\\"), ("x", "y"),
];
const SEPARATORS: &[&str] = &["", " ", "\n", ", "];
/// Host events the journal records.
const EVENTS: &[&str] = &["shell_command", "escalation", "recovery", "model_fallback", "outcome_blocked", "completion_check"];
const TOOLS: &[&str] = &["read_file", "web_fetch", "integration_read", "write_file"];

fn chars_from(alphabet: &'static [u8], min: usize, max: usize) -> impl Strategy<Value = String> {
    prop::collection::vec(select(alphabet), min..=max).prop_map(|bytes| String::from_utf8(bytes).unwrap())
}

/// A vault value of 8 to 64 characters. Digit-only values are left out here: the store's
/// own numbers (times, sequence numbers) could hold one by chance.
fn vault_value() -> impl Strategy<Value = String> {
    prop_oneof![
        select(vec![32usize, 40, 64]).prop_flat_map(|len| chars_from(HEX, len, len)),
        chars_from(BASE64, 8, 60),
        chars_from(ALNUM, 8, 64),
        chars_from(SYMBOLS, 8, 64),
        (prop::collection::vec(select(VAULT_WORDS), 2..=4), select(vec!["-", "_", ""]), any::<bool>())
            .prop_map(|(words, separator, capital)| {
                let text = words.join(separator);
                if capital { text.to_uppercase() } else { text }
            }),
        prop::collection::vec(select(UNICODE), 8..=24).prop_map(String::from_iter),
    ]
}

#[derive(Debug, Clone)]
enum Segment {
    Word(&'static str),
    Vault(usize, usize),
}

type Text = Vec<(&'static str, Segment)>;

fn text() -> impl Strategy<Value = Text> {
    let segment = prop_oneof![
        3 => select(FILLER).prop_map(Segment::Word),
        2 => (any::<usize>(), 0..WRAPS.len()).prop_map(|(value, wrap)| Segment::Vault(value, wrap)),
    ];
    prop::collection::vec((select(SEPARATORS), segment), 0..6)
}

fn render(text: &Text, values: &[String]) -> String {
    let mut out = String::new();
    for (separator, segment) in text {
        out.push_str(separator);
        match segment {
            Segment::Word(word) => out.push_str(word),
            Segment::Vault(value, wrap) => {
                let (left, right) = WRAPS[wrap % WRAPS.len()];
                out.push_str(left);
                out.push_str(&values[value % values.len()]);
                out.push_str(right);
            }
        }
    }
    out
}

/// An event payload with vault values in strings, keys and JSON documents held as strings.
#[derive(Debug, Clone)]
enum Body {
    Text(Text),
    Int(i64),
    Encoded(Box<Body>),
    Array(Vec<Body>),
    Object(Vec<(Text, Body)>),
}

fn body() -> impl Strategy<Value = Body> {
    let leaf = prop_oneof![4 => text().prop_map(Body::Text), 1 => any::<i64>().prop_map(Body::Int)];
    leaf.prop_recursive(3, 16, 4, |inner| {
        prop_oneof![
            inner.clone().prop_map(|body| Body::Encoded(Box::new(body))),
            prop::collection::vec(inner.clone(), 0..4).prop_map(Body::Array),
            prop::collection::vec((text(), inner), 0..4).prop_map(Body::Object),
        ]
    })
}

fn build(body: &Body, values: &[String]) -> Value {
    match body {
        Body::Text(text) => Value::String(render(text, values)),
        Body::Int(number) => json!(number),
        Body::Encoded(inner) => Value::String(build(inner, values).to_string()),
        Body::Array(items) => Value::Array(items.iter().map(|item| build(item, values)).collect()),
        Body::Object(entries) => {
            Value::Object(entries.iter().map(|(key, item)| (render(key, values), build(item, values))).collect())
        }
    }
}

/// One thing the Host hands the journal.
#[derive(Debug, Clone)]
enum Step {
    /// A `shell` call and its JSON result.
    Shell { command: Text, exit_code: i64, stdout: Text, stderr: Text },
    /// Another tool's call and its result, an error envelope when `failed`.
    Tool { name: &'static str, path: Text, failed: bool, content: Text },
    /// A script fails with `stderr`, `fix` runs, and the script succeeds: a learned fix.
    Repair { stderr: Text, fix: Text },
    /// A Host event with a payload.
    Event { kind: &'static str, payload: Body },
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        (text(), select(vec![0i64, 1, 2]), text(), text())
            .prop_map(|(command, exit_code, stdout, stderr)| Step::Shell { command, exit_code, stdout, stderr }),
        (select(TOOLS), text(), any::<bool>(), text())
            .prop_map(|(name, path, failed, content)| Step::Tool { name, path, failed, content }),
        (text(), text()).prop_map(|(stderr, fix)| Step::Repair { stderr, fix }),
        (select(EVENTS), body()).prop_map(|(kind, payload)| Step::Event { kind, payload }),
    ]
}

/// One call of `turn` as `webui::execute` and `HostedTools::execute` journal it: the
/// `tools` frame, the Host's known-fix lookup on the result, and the `tool_results` frame.
fn call(fixture: &Fixture, turn: &str, id: &str, name: &str, arguments: Value, content: String, is_error: bool) {
    observe(&fixture.db, turn, &json!({"kind":"tools","calls":[{"id":id,"name":name,"arguments":arguments}]}));
    let tool_call = ToolCall { id: id.into(), name: name.into(), arguments };
    let fix = known_fix(&fixture.db, turn, &tool_call, &content, is_error);
    let content = fix.as_ref().map_or(content.clone(), |fix| attach_known_fix(&content, fix));
    observe(&fixture.db, turn, &json!({"kind":"tool_results","results":[{"tool_call_id":id,"is_error":is_error,"content":content}]}));
}

fn shell_result(exit_code: i64, stdout: &str, stderr: &str) -> String {
    json!({"exit_code":exit_code,"stdout":stdout,"stderr":stderr,"duration_ms":5}).to_string()
}

fn run(fixture: &Fixture, steps: &[Step], values: &[String]) {
    const TURN: &str = "turn-secrets";
    for (index, step) in steps.iter().enumerate() {
        // Some call ids hold a value too.
        let id = |n: usize| if index % 3 == 2 { format!("c{index}-{n}-{}", values[index % values.len()]) } else { format!("c{index}-{n}") };
        match step {
            Step::Shell { command, exit_code, stdout, stderr } => call(fixture, TURN, &id(0), "shell",
                json!({"command":render(command, values)}),
                shell_result(*exit_code, &render(stdout, values), &render(stderr, values)), false),
            Step::Tool { name, path, failed, content } => {
                let content = render(content, values);
                let content = if *failed {
                    format!("Tool failed: {}", json!({"error_code":"not_found","summary":content}))
                } else {
                    content
                };
                call(fixture, TURN, &id(0), name, json!({"path":render(path, values),"url":render(path, values)}), content, *failed);
            }
            Step::Repair { stderr, fix } => {
                // The last line of stderr is the failure message, so the value is in it.
                let stderr = format!("Traceback (most recent call last):\n{}\n", render(stderr, values).replace('\n', " "));
                let script = json!({"command":"python3 extract.py in.pdf"});
                call(fixture, TURN, &id(0), "shell", script.clone(), shell_result(1, "", &stderr), false);
                call(fixture, TURN, &id(1), "shell", json!({"command":format!("uv pip install {}", render(fix, values))}),
                    shell_result(0, "", ""), false);
                call(fixture, TURN, &id(2), "shell", script, shell_result(0, "", ""), false);
            }
            Step::Event { kind, payload } => {
                let event = json!({"kind":kind,"payload":build(payload, values)});
                if let Some(pending) = entry(&fixture.db, "event", &event) {
                    offer(&fixture.db, &fixture.turn, pending);
                }
            }
        }
    }
}

/// `text` as the inside of a JSON string literal.
fn escaped(text: &str) -> String {
    let quoted = serde_json::to_string(text).unwrap();
    quoted[1..quoted.len() - 1].to_owned()
}

/// A vault value and its JSON-escaped forms: plain in a stored JSON string is escaped once,
/// an escaped value in a tool result's JSON text twice.
fn forms(value: &str) -> Vec<String> {
    let mut forms = vec![value.to_owned()];
    for _ in 0..3 {
        let next = escaped(forms.last().unwrap());
        if &next == forms.last().unwrap() {
            break;
        }
        forms.push(next);
    }
    forms
}

/// Every TEXT and BLOB cell of every table in the SQLite file at `path`, as
/// `(table, column, text)`.
fn cells(path: &std::path::Path) -> Vec<(String, String, String)> {
    let connection = rusqlite::Connection::open(path).unwrap();
    let tables: Vec<String> = connection
        .prepare("SELECT name FROM sqlite_master WHERE type='table'").unwrap()
        .query_map([], |row| row.get(0)).unwrap()
        .collect::<Result<_, _>>().unwrap();
    let mut out = Vec::new();
    for table in tables {
        let mut query = connection.prepare(&format!("SELECT * FROM \"{table}\"")).unwrap();
        let names: Vec<String> = query.column_names().into_iter().map(str::to_owned).collect();
        let mut rows = query.query([]).unwrap();
        while let Some(row) = rows.next().unwrap() {
            for (index, name) in names.iter().enumerate() {
                if let rusqlite::types::ValueRef::Text(bytes) | rusqlite::types::ValueRef::Blob(bytes) = row.get_ref(index).unwrap() {
                    out.push((table.clone(), name.clone(), String::from_utf8_lossy(bytes).into_owned()));
                }
            }
        }
    }
    out
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(CASES))]

    // Feature: autonomous-agent-loop, Property 17: Secrets excluded
    /// **Validates: Requirements 19.6, 2.4**
    #[test]
    fn the_learning_store_holds_no_vault_value(
        values in prop::collection::vec(vault_value(), 1..=3),
        steps in prop::collection::vec(step(), 1..=8),
    ) {
        let fixture = Fixture::new();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("learning.sqlite");
        install_with(&fixture.db, &path, Box::new(SecretValues::new(values.clone()))).unwrap();
        run(&fixture, &steps, &values);
        prop_assert!(flush(&fixture.db, FLUSH_WAIT));
        let journaled = store(&fixture.db).unwrap().events("turn-secrets", 0, 1000).unwrap().len()
            + store(&fixture.db).unwrap().events(&crate::ui_task_completion::scope(&fixture.db, &fixture.turn).unwrap(), 0, 1000).unwrap().len();
        prop_assert!(journaled > 0, "nothing was journaled");

        for (table, column, cell) in cells(&path) {
            for value in &values {
                for form in forms(value) {
                    prop_assert!(!cell.contains(&form), "{}.{} holds vault value {:?}: {}", table, column, value, cell);
                }
                if table == "learn_failures" && column == "example" {
                    prop_assert!(!cell.contains(&value.to_lowercase()), "the example holds vault value {:?} lowercased: {}", value, cell);
                }
            }
        }
    }
}
