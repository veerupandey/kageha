// Feature: autonomous-agent-loop, Property 17: Secrets excluded
//! Property 17, learning part: for any credential vault contents, no stored journal entry,
//! lesson or skill file holds a vault value or a pattern-matched secret.
//!
//! **Validates: Requirements 19.6, 2.4**
//!
//! - Journal: [`redact_json`] and [`redact_values`], which the Host's journal worker
//!   applies to every entry (`kageha-loop` checks the worker end to end), must leave no
//!   vault value in any string, key or number, also when the value sits in a JSON document
//!   held as text (JSON-escaped once or more).
//! - Lessons: [`SqliteLearning::store_lessons`] must drop every draft holding a vault value
//!   or a secret pattern; the rows are read back from the database file.
//! - Skills: a package built with [`build_skill`] from a draft holding a vault value or a
//!   secret pattern must be rejected by [`find_in_files`], the scan the skill promoter runs
//!   before it tests and stores a package; a package the scan accepts holds no secret.
//!
//! Vault values are never exempt from the scan: a value shaped like a hex digest, which
//! the `high_entropy` pattern exempts, is still redacted or dropped.
//!
//! Secret-shaped strings are built at run time, so this file holds no token literal.
use kageha_learning::secrets::{find_in_files, find_pattern, redact_json, redact_values, scan, SecretValues};
use kageha_learning::skills::{build_skill, parse_skill_draft, script_invocation, skill_name};
use kageha_learning::{
    JournalEntry, JournalKind, LearningPort, LessonDraft, OutcomeStatus, RequestSummary, SqliteLearning,
};
use proptest::prelude::*;
use proptest::sample::select;
use serde_json::{json, Value};
use zeroize::Zeroizing;

const CASES: u32 = 1000;

const HEX: &[u8] = b"0123456789abcdef";
const DIGITS: &[u8] = b"0123456789";
const ALNUM: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
const UPPER_DIGITS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
const BASE64: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789+/";
const BASE64URL: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_";
/// Password characters, including the JSON-escaped `"` and `\`. No whitespace (lesson and
/// skill text is whitespace-collapsed) and no `[`/`]`, which `[redacted]` is made of.
const SYMBOLS: &[u8] = b"abcdefghijkmnopqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789!@#$%^&*()-_=+{};:'\",.<>/?\\|~";
const UNICODE: &[char] = &['ä', 'ö', 'ü', 'ß', 'é', '€', '🔑', '密', '码', 'a', 'b', 'c', '1', '2', '-'];
/// Words of passphrase-like values. Not in [`FILLER`], so clean text never holds one.
const VAULT_WORDS: &[&str] = &[
    "correct", "horse", "battery", "staple", "violet", "marble", "harbor", "lantern", "quartz", "meadow",
];
/// Words of ordinary text around the values.
const FILLER: &[&str] = &[
    "the", "report", "build", "table", "weekly", "notes", "check", "output", "when", "run", "script", "file",
    "then", "retry", "open", "save",
];
/// Text right before and after an embedded vault value. `x`/`y` glue it to letters.
const WRAPS: &[(&str, &str)] = &[
    ("", ""),
    (" ", " "),
    ("(", ")"),
    ("\"", "\""),
    ("'", "'"),
    ("=", ";"),
    (": ", ","),
    ("<", ">"),
    ("/", "/"),
    ("\n", "\n"),
    ("\t", "\t"),
    ("{\"k\":\"", "\"}"),
    ("\\", "\\"),
    ("x", "y"),
    ("--", "--"),
    ("[", "]"),
];
/// Separators between text segments; `""` joins them with nothing in between.
const SEPARATORS: &[&str] = &["", " ", "\n", ", "];

fn chars_from(alphabet: &'static [u8], min: usize, max: usize) -> impl Strategy<Value = String> {
    prop::collection::vec(select(alphabet), min..=max).prop_map(|bytes| String::from_utf8(bytes).unwrap())
}

/// A credential vault value of 8 to 64 characters.
fn vault_value() -> impl Strategy<Value = String> {
    prop_oneof![
        // Hex digests and ids, which the `high_entropy` pattern exempts.
        select(vec![32usize, 40, 64]).prop_flat_map(|len| chars_from(HEX, len, len)),
        (chars_from(BASE64, 8, 60), 0usize..3).prop_map(|(text, pad)| format!("{text}{}", "=".repeat(pad))),
        chars_from(ALNUM, 8, 64),
        chars_from(SYMBOLS, 8, 64),
        chars_from(DIGITS, 8, 19),
        (prop::collection::vec(select(VAULT_WORDS), 2..=5), select(vec![" ", "-", "_", ".", ""]), any::<bool>())
            .prop_map(|(words, separator, capital)| {
                let text = words.join(separator);
                if capital { text.to_uppercase() } else { text }
            }),
        prop::collection::vec(select(UNICODE), 8..=24).prop_map(String::from_iter),
    ]
}

fn vault() -> impl Strategy<Value = Vec<String>> {
    prop::collection::vec(vault_value(), 1..=4)
}

/// A secret that one of the scan's patterns matches.
fn pattern_secret() -> impl Strategy<Value = String> {
    prop_oneof![
        chars_from(ALNUM, 36, 36).prop_map(|text| format!("ghp_{text}")),
        chars_from(BASE64URL, 20, 40).prop_map(|text| format!("sk-{text}7")),
        chars_from(UPPER_DIGITS, 16, 16).prop_map(|text| format!("AKIA{text}")),
        (chars_from(BASE64URL, 10, 30), chars_from(BASE64URL, 10, 30), chars_from(BASE64URL, 8, 40))
            .prop_map(|(header, payload, signature)| format!("eyJ{header}.eyJ{payload}.{signature}")),
        chars_from(BASE64, 40, 64).prop_map(|body| {
            format!("-----BEGIN RSA PRIVATE KEY-----\n{body}\n-----END RSA PRIVATE KEY-----")
        }),
    ]
}

/// A piece of text: a filler word, or a vault value (by index into the vault) with a wrap.
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
    prop::collection::vec((select(SEPARATORS), segment), 0..8)
}

fn wrapped(value: &str, wrap: usize) -> String {
    let (left, right) = WRAPS[wrap % WRAPS.len()];
    format!("{left}{value}{right}")
}

fn render(text: &Text, values: &[String]) -> String {
    let mut out = String::new();
    for (separator, segment) in text {
        out.push_str(separator);
        match segment {
            Segment::Word(word) => out.push_str(word),
            Segment::Vault(value, wrap) => out.push_str(&wrapped(&values[value % values.len()], *wrap)),
        }
    }
    out
}

/// A journal entry body, with vault values in strings, keys, numbers and JSON documents
/// held as strings, nested in objects and arrays.
#[derive(Debug, Clone)]
enum Body {
    Text(Text),
    /// A digit-only vault value as a JSON number, else this number.
    Number(usize),
    Int(i64),
    Bool(bool),
    Null,
    /// The inner body serialized into a string, as tool results hold JSON.
    Encoded(Box<Body>),
    Array(Vec<Body>),
    Object(Vec<(Text, Body)>),
}

fn body() -> impl Strategy<Value = Body> {
    let leaf = prop_oneof![
        4 => text().prop_map(Body::Text),
        1 => any::<usize>().prop_map(Body::Number),
        1 => any::<i64>().prop_map(Body::Int),
        1 => any::<bool>().prop_map(Body::Bool),
        1 => Just(Body::Null),
    ];
    // Depth 3 nests at most 3 encoded documents.
    leaf.prop_recursive(3, 32, 4, |inner| {
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
        Body::Number(index) => {
            let value = &values[index % values.len()];
            value.parse::<u64>().map_or_else(|_| json!(*index as u64), |number| json!(number))
        }
        Body::Int(number) => json!(number),
        Body::Bool(flag) => json!(flag),
        Body::Null => Value::Null,
        Body::Encoded(inner) => Value::String(build(inner, values).to_string()),
        Body::Array(items) => Value::Array(items.iter().map(|item| build(item, values)).collect()),
        Body::Object(entries) => Value::Object(
            entries.iter().map(|(key, item)| (render(key, values), build(item, values))).collect(),
        ),
    }
}

/// `text` as the inside of a JSON string literal.
fn escaped(text: &str) -> String {
    let quoted = serde_json::to_string(text).unwrap();
    quoted[1..quoted.len() - 1].to_owned()
}

/// A vault value and its JSON-escaped forms, up to 3 levels.
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

/// The first vault value whose plain or JSON-escaped form `haystack` holds.
fn leaked<'a>(haystack: &str, values: &'a [String]) -> Option<&'a str> {
    values
        .iter()
        .find(|value| forms(value).iter().any(|form| haystack.contains(form.as_str())))
        .map(String::as_str)
}

/// Every string and object key of `value`, and the text of every number.
fn strings(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(text) => out.push(text.clone()),
        Value::Number(number) => out.push(number.to_string()),
        Value::Array(items) => items.iter().for_each(|item| strings(item, out)),
        Value::Object(object) => object.iter().for_each(|(key, item)| {
            out.push(key.clone());
            strings(item, out);
        }),
        _ => {}
    }
}

fn zeroizing(values: &[String]) -> Vec<Zeroizing<String>> {
    values.iter().map(|value| Zeroizing::new(value.clone())).collect()
}

/// Plain text that may hold vault values: a sentence, a JSON document (a tool result), or
/// that document inside another JSON string.
fn plain(text: &Text, values: &[String], shape: u8) -> String {
    let sentence = render(text, values);
    let document = json!({"exit_code": 1, "stdout": sentence, "stderr": sentence}).to_string();
    match shape % 3 {
        0 => sentence,
        1 => document,
        _ => Value::String(document).to_string(),
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(CASES))]

    // Feature: autonomous-agent-loop, Property 17: Secrets excluded
    /// **Validates: Requirements 19.6, 2.4**
    #[test]
    fn redacted_journal_entries_hold_no_vault_value(values in vault(), template in body(), sentence in text(), shape in any::<u8>()) {
        let vault = zeroizing(&values);
        let mut entry = build(&template, &values);
        redact_json(&mut entry, &vault);
        let mut texts = Vec::new();
        strings(&entry, &mut texts);
        for text in &texts {
            prop_assert!(leaked(text, &values).is_none(), "a vault value is left in {:?} of {}", text, entry);
        }
        let stored = serde_json::to_string(&entry).unwrap();
        prop_assert!(leaked(&stored, &values).is_none(), "a vault value is left in {}", stored);

        let text = plain(&sentence, &values, shape);
        let held = leaked(&text, &values).is_some();
        let redacted = redact_values(&text, &vault);
        prop_assert_eq!(redacted.is_some(), held, "redact_values reports a change iff the text holds a value: {:?}", text);
        let redacted = redacted.unwrap_or(text);
        prop_assert!(leaked(&redacted, &values).is_none(), "a vault value is left in {:?}", redacted);
    }
}

/// Where a lesson draft holds a secret, if anywhere.
#[derive(Debug, Clone)]
enum Secret {
    None,
    /// A hex commit id that is not a vault value: exempt from the scan, so the lesson stays.
    HexId(String),
    Vault(usize, usize),
    Pattern(String),
}

fn secret() -> impl Strategy<Value = Secret> {
    prop_oneof![
        1 => Just(Secret::None),
        1 => chars_from(HEX, 40, 40).prop_map(Secret::HexId),
        3 => (any::<usize>(), 0..WRAPS.len()).prop_map(|(value, wrap)| Secret::Vault(value, wrap)),
        2 => pattern_secret().prop_map(Secret::Pattern),
    ]
}

impl Secret {
    fn render(&self, values: &[String]) -> Option<String> {
        match self {
            Self::None => None,
            Self::HexId(id) => Some(format!(" {id} ")),
            Self::Vault(value, wrap) => Some(wrapped(&values[value % values.len()], *wrap)),
            Self::Pattern(secret) => Some(format!(" {secret} ")),
        }
    }
    fn is_secret(&self) -> bool {
        matches!(self, Self::Vault(..) | Self::Pattern(_))
    }
}

/// Filler words with `secret` inserted after word `at`, joined by spaces.
fn sentence(words: &[&str], secret: Option<&str>, at: usize) -> String {
    let mut parts: Vec<String> = words.iter().map(|word| (*word).to_owned()).collect();
    if let Some(secret) = secret {
        parts.insert(at % (parts.len() + 1), secret.to_owned());
    }
    parts.join(" ")
}

#[derive(Debug, Clone)]
struct DraftTemplate {
    trigger: Vec<&'static str>,
    action: Vec<&'static str>,
    secret: Secret,
    in_action: bool,
    at: usize,
}

fn draft() -> impl Strategy<Value = DraftTemplate> {
    (
        prop::collection::vec(select(FILLER), 1..=6),
        prop::collection::vec(select(FILLER), 1..=6),
        secret(),
        any::<bool>(),
        any::<usize>(),
    )
        .prop_map(|(trigger, action, secret, in_action, at)| DraftTemplate { trigger, action, secret, in_action, at })
}

impl DraftTemplate {
    fn build(&self, values: &[String]) -> LessonDraft {
        let secret = self.secret.render(values);
        let (trigger, action) = if self.in_action { (None, secret.as_deref()) } else { (secret.as_deref(), None) };
        LessonDraft {
            task_type: "notes".into(),
            trigger: sentence(&self.trigger, trigger, self.at),
            action: sentence(&self.action, action, self.at),
            evidence: vec![1],
        }
    }
}

/// Every TEXT and BLOB cell of `table` in the store at `path`, with its column name.
fn cells(path: &std::path::Path, table: &str) -> Vec<(String, String)> {
    let connection = rusqlite::Connection::open(path).unwrap();
    let mut query = connection.prepare(&format!("SELECT * FROM \"{table}\"")).unwrap();
    let names: Vec<String> = query.column_names().into_iter().map(str::to_owned).collect();
    let mut rows = query.query([]).unwrap();
    let mut out = Vec::new();
    while let Some(row) = rows.next().unwrap() {
        for (index, name) in names.iter().enumerate() {
            if let rusqlite::types::ValueRef::Text(bytes) | rusqlite::types::ValueRef::Blob(bytes) =
                row.get_ref(index).unwrap()
            {
                out.push((name.clone(), String::from_utf8_lossy(bytes).into_owned()));
            }
        }
    }
    out
}

fn reflected_store(directory: &tempfile::TempDir) -> (SqliteLearning, kageha_learning::ReflectionClaim) {
    let store = SqliteLearning::open(&directory.path().join("learning.sqlite")).unwrap();
    store
        .journal("request-1", JournalEntry { kind: JournalKind::Tool, body: json!({"tool": "read_file"}), at: 1 })
        .unwrap();
    let summary = RequestSummary {
        agent: "main".into(),
        chat_id: "chat-1".into(),
        task_type: Some("notes".into()),
        status: "verified".into(),
        model_calls: 1,
        cost_usd: 0.0,
        cached_tokens: 0,
        outcomes: vec![OutcomeStatus { id: "notes".into(), status: "verified".into() }],
        started: 1,
        ended: 2,
    };
    store.close_request("request-1", summary).unwrap();
    let claim = store.claim_reflection().unwrap().unwrap();
    (store, claim)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(CASES))]

    // Feature: autonomous-agent-loop, Property 17: Secrets excluded
    /// **Validates: Requirements 19.6, 2.4**
    #[test]
    fn stored_lessons_hold_no_secret(values in vault(), templates in prop::collection::vec(draft(), 1..=5)) {
        let directory = tempfile::tempdir().unwrap();
        let (store, claim) = reflected_store(&directory);
        let drafts: Vec<LessonDraft> = templates.iter().map(|template| template.build(&values)).collect();
        let report = store.store_lessons(&claim, &drafts, None, &SecretValues::new(values.clone())).unwrap();

        // Exactly the drafts holding a secret are dropped, and the reason names only the kind.
        for (index, template) in templates.iter().enumerate() {
            let dropped = report.dropped.iter().find(|dropped| dropped.index == index);
            if template.secret.is_secret() {
                let reason = &dropped.unwrap_or_else(|| panic!("draft {index} with a secret was kept: {:?}", drafts[index])).reason;
                prop_assert!(reason.starts_with("secret:"), "{}", reason);
                prop_assert!(leaked(reason, &values).is_none());
            } else {
                prop_assert!(dropped.is_none(), "clean draft {} dropped: {:?}", index, dropped);
            }
        }
        prop_assert_eq!(report.stored.len(), templates.iter().filter(|template| !template.secret.is_secret()).count());

        let patterns: Vec<&String> = templates.iter()
            .filter_map(|template| match &template.secret { Secret::Pattern(secret) => Some(secret), _ => None })
            .collect();
        let vault = zeroizing(&values);
        for lesson in store.lessons("main").unwrap() {
            for text in [&lesson.trigger, &lesson.action] {
                prop_assert!(scan(text, &vault).is_none(), "a stored lesson holds a secret: {:?}", text);
            }
        }
        for (column, cell) in cells(&directory.path().join("learning.sqlite"), "learn_lessons") {
            prop_assert!(leaked(&cell, &values).is_none(), "learn_lessons.{} holds a vault value: {:?}", column, cell);
            for secret in &patterns {
                prop_assert!(!cell.contains(secret.as_str()), "learn_lessons.{} holds a pattern secret", column);
            }
        }
    }
}

/// The skill draft field a secret is put in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    Name,
    Description,
    Instructions,
    Script,
    TestSh,
    /// The script as it ran, used when the draft keeps it (`script: null`).
    Original,
}

const FIELDS: &[Field] = &[Field::Name, Field::Description, Field::Instructions, Field::Script, Field::TestSh, Field::Original];

proptest! {
    #![proptest_config(ProptestConfig::with_cases(CASES))]

    // Feature: autonomous-agent-loop, Property 17: Secrets excluded
    /// **Validates: Requirements 19.6, 2.4**
    #[test]
    fn skill_packages_holding_a_secret_are_rejected(
        values in vault(),
        secret in secret(),
        field in select(FIELDS),
        words in prop::collection::vec(select(FILLER), 1..=6),
        at in any::<usize>(),
    ) {
        let inserted = secret.render(&values);
        let put = |wanted: Field, base: &str| match &inserted {
            Some(text) if field == wanted => format!("{base} {} {text}", sentence(&words, None, at)),
            _ => base.to_owned(),
        };
        let script = format!("{}\n", put(Field::Script, "print('report')"));
        let reply = json!({
            "name": put(Field::Name, "make report"),
            "description": put(Field::Description, "Build the weekly report"),
            "instructions": put(Field::Instructions, "Use this to build the weekly report.\n\nRun `python3 scripts/make_report.py`."),
            "script": if field == Field::Original { Value::Null } else { json!(script) },
            "test_sh": format!("set -e\npython3 scripts/make_report.py\n{}\n", put(Field::TestSh, "test -s outputs/report.md")),
        });
        let original = format!("{}\n", put(Field::Original, "print('report')"));
        let draft = parse_skill_draft(&reply).unwrap();
        let run = script_invocation("python3 tools/make_report.py").unwrap();
        let name = skill_name(&draft.name, &run);
        let built = build_skill(&name, &draft, original.as_bytes(), &run, "request-1");
        let vault = SecretValues::new(values.clone());
        let built = match built {
            Ok(built) => built,
            Err(error) => {
                // Nothing is stored; a clean draft must build.
                prop_assert!(secret.is_secret(), "a clean skill did not build: {}", error);
                return Ok(());
            }
        };
        let rejected = find_in_files(&built.package.files, &vault);
        if !secret.is_secret() {
            prop_assert!(rejected.is_none(), "a clean skill was rejected: {:?}", rejected);
        } else if field != Field::Name {
            prop_assert!(rejected.is_some(), "a skill with a secret in its {:?} was accepted: {:?}", field, built.package.files);
        }
        if rejected.is_none() {
            // An accepted package holds no secret in any file or parsed field.
            let skill = &built.package.skill;
            let mut texts: Vec<String> = built.package.files.values().map(|bytes| String::from_utf8_lossy(bytes).into_owned()).collect();
            texts.extend([skill.name.clone(), skill.description.clone(), skill.body.clone(), skill.metadata.to_string()]);
            for text in &texts {
                prop_assert!(leaked(text, &values).is_none(), "an accepted skill holds a vault value: {:?}", text);
                prop_assert!(find_pattern(text).is_none(), "an accepted skill matches a secret pattern: {:?}", text);
                if let Secret::Pattern(secret) = &secret {
                    prop_assert!(!text.contains(secret.as_str()));
                }
            }
        }
    }
}

/// A vault value shaped like a hex digest is exempt from the `high_entropy` pattern, but
/// never from the vault scan.
#[test]
fn a_hex_shaped_vault_value_is_redacted_and_dropped() {
    let value: String = (0..32).map(|i| HEX[(i * 7 + 3) % 16] as char).collect();
    assert!(find_pattern(&format!("commit {value}")).is_none(), "hex ids are exempt from the patterns");
    let vault = vec![Zeroizing::new(value.clone())];
    let mut entry = json!({"stdout": format!("id={value}"), value.clone(): [json!({"x": value})]});
    assert_eq!(redact_json(&mut entry, &vault), 3);
    assert!(!entry.to_string().contains(&value), "{entry}");

    let directory = tempfile::tempdir().unwrap();
    let (store, claim) = reflected_store(&directory);
    let draft = LessonDraft {
        task_type: "notes".into(),
        trigger: "when the report build fails".into(),
        action: format!("sign in with {value}"),
        evidence: vec![1],
    };
    let report = store.store_lessons(&claim, &[draft], None, &SecretValues::new([value.clone()])).unwrap();
    assert_eq!(report.dropped.iter().map(|d| d.reason.as_str()).collect::<Vec<_>>(), ["secret:vault_value"]);
    assert!(store.lessons("main").unwrap().is_empty());

    let reply = json!({"name": "make report", "description": "Build the weekly report",
        "instructions": format!("Run with `--key {value}`."), "script": null, "test_sh": "set -e\n"});
    let draft = parse_skill_draft(&reply).unwrap();
    let run = script_invocation("python3 tools/make_report.py").unwrap();
    let built = build_skill("make-report", &draft, b"print('report')\n", &run, "request-1").unwrap();
    let (path, finding) = find_in_files(&built.package.files, &SecretValues::new([value])).unwrap();
    assert_eq!((path, finding.kind), ("SKILL.md", "vault_value"));
}

/// A value with a quote is held JSON-escaped by a tool result and by `SKILL.md`'s quoted
/// description; both forms are found.
#[test]
fn json_escaped_vault_values_are_found() {
    let value = "pa\"ss\\word-2024";
    let vault = vec![Zeroizing::new(value.to_owned())];
    let result = json!({"exit_code": 0, "stdout": format!("login {value} ok")}).to_string();
    assert!(!result.contains(value));
    let redacted = redact_values(&result, &vault).expect("the escaped form is redacted");
    assert!(!redacted.contains(&escaped(value)), "{redacted}");
    let mut entry = json!({"excerpt": result});
    assert_eq!(redact_json(&mut entry, &vault), 1);
    assert!(leaked(&entry.to_string(), &[value.to_owned()]).is_none(), "{entry}");

    let reply = json!({"name": "make report", "description": format!("Build with {value}"),
        "instructions": "Run it.", "script": null, "test_sh": "set -e\n"});
    let draft = parse_skill_draft(&reply).unwrap();
    let run = script_invocation("python3 tools/make_report.py").unwrap();
    let built = build_skill("make-report", &draft, b"print('report')\n", &run, "request-1").unwrap();
    assert_eq!(built.package.skill.description, format!("Build with {value}"));
    let (path, _) = find_in_files(&built.package.files, &SecretValues::new([value])).unwrap();
    assert_eq!(path, "SKILL.md");
}

/// A digit-only vault value held as a JSON number is redacted.
#[test]
fn a_numeric_vault_value_is_redacted_from_numbers() {
    let vault = vec![Zeroizing::new("4111111111111111".to_owned())];
    let mut entry = json!({"card": 4111111111111111u64, "exit_code": 0});
    assert_eq!(redact_json(&mut entry, &vault), 1);
    assert_eq!(entry, json!({"card": "[redacted]", "exit_code": 0}));
}
