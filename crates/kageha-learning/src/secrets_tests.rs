use super::*;
use serde_json::json;

/// Test tokens are built at run time so the source holds no token-shaped literal.
fn repeat(unit: &str, times: usize) -> String {
    unit.repeat(times)
}

/// `len` characters stepping through letters and digits, so they do not repeat early.
fn varied(len: usize, step: usize) -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    (0..len).map(|i| ALPHABET[(i * step + 3) % ALPHABET.len()] as char).collect()
}

fn kind(text: &str) -> Option<&'static str> {
    find_pattern(text).map(|finding| finding.kind)
}

struct Unavailable;

impl SecretSource for Unavailable {
    fn values(&self) -> io::Result<Vec<Zeroizing<String>>> {
        Err(io::Error::other("vault locked"))
    }
}

#[test]
fn each_pattern_is_detected_by_name() {
    let jwt = format!("eyJ{}.eyJ{}.{}", repeat("hbGc", 4), repeat("zdWI", 4), repeat("Sf1K", 8));
    let cases = [
        (format!("-----BEGIN RSA PRIVATE KEY-----\n{}\n-----END RSA PRIVATE KEY-----", repeat("MIIE", 4)), "pem_private_key"),
        ("-----BEGIN OPENSSH PRIVATE KEY-----".to_owned(), "pem_private_key"),
        (format!("curl -H 'Authorization: Bearer {}'", repeat("abc1", 6)), "bearer_token"),
        (format!("set the key to sk-{}", repeat("aB3d", 6)), "sk_key"),
        (format!("sk-proj-{}", repeat("Xy9_", 6)), "sk_key"),
        (format!("token ghp_{}", repeat("A1b2", 9)), "github_token"),
        (format!("gho_{}", repeat("Q7r8", 9)), "github_token"),
        (format!("github_pat_{}", repeat("11AB_cd", 4)), "github_token"),
        (format!("aws key AKIA{}", repeat("Z7Q2", 4)), "aws_access_key"),
        (format!("xoxb-{}", repeat("1234-", 4)), "slack_token"),
        (format!("xoxp-{}", repeat("abcd", 4)), "slack_token"),
        (format!("Cookie: session={jwt}"), "jwt"),
        ("login with password=hunter2 then".to_owned(), "password"),
        (r#"{"user": "ana", "password": "correct horse"}"#.to_owned(), "password"),
        ("--db-password = s3cret".to_owned(), "password"),
        ("export OPENAI_API_KEY=abc".to_owned(), "secret_assignment"),
        (r#"GITHUB_TOKEN="value""#.to_owned(), "secret_assignment"),
        ("client_secret=xyz123".to_owned(), "secret_assignment"),
        (format!("the value {} was used", varied(32, 7)), "high_entropy"),
        (format!("{}/{}", varied(24, 11), varied(24, 13)), "high_entropy"),
        (format!("{}_{}", varied(20, 17), varied(20, 19)), "high_entropy"),
    ];
    for (text, expected) in cases {
        assert_eq!(kind(&text), Some(expected), "{text}");
        assert_eq!(contains_secret(&text, &NoSecrets).map(|f| f.kind), Some(expected), "{text}");
    }
}

#[test]
fn digests_ids_and_ordinary_text_are_not_flagged() {
    let sha256 = repeat("0123456789abcdef", 4);
    assert_eq!(sha256.len(), 64);
    let texts = [
        format!("signature {sha256}"),
        format!("sha256:{}", sha256.to_uppercase()),
        // The Host's request ids are 32 hex digits; commit ids are 40.
        format!("learned_from: \"{}\"", &sha256[..32]),
        format!("commit {}", &sha256[..40]),
        "request 123e4567-e89b-12d3-a456-426614174000".to_owned(),
        "When a PDF has no text layer, run OCR with `ocrmypdf` before extraction.".to_owned(),
        "Install it with uv pip install pdfplumber".to_owned(),
        "Write outputs/weekly-report-2024-11-12-final-version-two.md".to_owned(),
        "/Users/someone/Projects/kageha_rust/crates/kageha-learning/src/lib.rs".to_owned(),
        "Use the Bearer authentication header from the connector".to_owned(),
        "pip install sk-learn-extensions-library".to_owned(),
        "pass --password=$PGPASSWORD or password=<your password>".to_owned(),
        r#"{"password": "", "token": null}"#.to_owned(),
        "password=[redacted] API_KEY=[redacted] password=***".to_owned(),
        "the password: field is required".to_owned(),
        "PRIMARY_KEY == 3 and API_KEY=${API_KEY}".to_owned(),
        "a.b.c and eyJshort.x.y".to_owned(),
        "ThisIsAVeryLongCamelCaseIdentifierNameWithoutDigits".to_owned(),
    ];
    for text in texts {
        assert_eq!(kind(&text), None, "{text}");
    }
}

#[test]
fn vault_values_are_found_and_the_finding_never_holds_them() {
    let value = "plain words vault entry";
    let vault = SecretValues::new([value, "short"]);
    let text = format!("Log in with {value} on the portal");
    let finding = contains_secret(&text, &vault).unwrap();
    assert_eq!(finding.kind, VAULT_VALUE);
    for shown in [finding.to_string(), format!("{finding:?}")] {
        assert!(!shown.contains(value), "{shown}");
    }
    // Values under 8 characters are not matched.
    assert_eq!(contains_secret("a short note", &vault), None);
    assert_eq!(scan(&text, &vault.values().unwrap()), Some(finding));
    // A vault that cannot be read drops the text.
    assert_eq!(contains_secret("anything", &Unavailable).unwrap().kind, VAULT_UNAVAILABLE);
    assert_eq!(contains_secret("anything", &NoSecrets), None);
}

#[test]
fn vault_values_are_redacted_longest_first() {
    let values = SecretValues::new(["vault-entry-1", "vault-entry-1-longer", "tiny"]).values().unwrap();
    assert_eq!(redact_values("nothing here tiny", &values), None);
    assert_eq!(
        redact_values("a vault-entry-1-longer and vault-entry-1.", &values).as_deref(),
        Some("a [redacted] and [redacted].")
    );
    let mut body = json!({"excerpt": "echo vault-entry-1", "list": ["ok", "x vault-entry-1"], "vault-entry-1": 3, "n": 4});
    assert_eq!(redact_json(&mut body, &values), 3);
    assert_eq!(body, json!({"excerpt": "echo [redacted]", "list": ["ok", "x [redacted]"], "[redacted]": 3, "n": 4}));
    assert!(!body.to_string().contains("vault-entry-1"));
}

#[test]
fn secret_names_match_the_workspace_rule() {
    for name in ["OPENAI_API_KEY", "github_token", "AWS_SECRET", "KAGEHA_SERVER_TOKEN"] {
        assert!(is_secret_name(name), "{name}");
    }
    for name in ["KEY", "TOKENS", "PATH", "SECRET_NAME"] {
        assert!(!is_secret_name(name), "{name}");
    }
}
