//! Private, narrowly scoped Gmail verification-code retrieval.
//! No message content or code is returned to the model.
use crate::webui::Database;
use serde_json::{json, Value};
use std::{io, sync::atomic::AtomicBool};
use zeroize::{Zeroize, Zeroizing};

struct PrivateValue(Value);
impl Drop for PrivateValue {
    fn drop(&mut self) {
        fn erase(v: &mut Value) {
            match v {
                Value::String(s) => s.zeroize(),
                Value::Array(a) => a.iter_mut().for_each(erase),
                Value::Object(m) => m.values_mut().for_each(erase),
                _ => {}
            }
        }
        erase(&mut self.0);
    }
}

/// Recognize a single code following a verification label, not every number in
/// an email (dates and phone numbers often occur nearby). Reject ambiguous mail.
fn text_codes(text: &str) -> Vec<Zeroizing<String>> {
    let mut codes = Vec::new();
    for line in text.lines() {
        let words: Vec<_> = line.split(|c: char| !c.is_ascii_alphanumeric()).filter(|s| !s.is_empty()).collect();
        for (index, word) in words.iter().enumerate() {
            if !["code", "passcode", "otp", "verification"].iter().any(|label| word.eq_ignore_ascii_case(label)) { continue; }
            let tail = &words[index + 1..];
            // "verification code" is one label; the next iteration handles it.
            if word.eq_ignore_ascii_case("verification") && tail.first().is_some_and(|v| v.eq_ignore_ascii_case("code")) { continue; }
            let Some(pos) = tail.iter().position(|value| !["is", "the", "your", "for", "verification"].iter().any(|skip| value.eq_ignore_ascii_case(skip))) else { continue };
            let plausible = |candidate: &&str| (4..=12).contains(&candidate.len())
                && candidate.bytes().all(|b| b.is_ascii_alphanumeric())
                && (candidate.bytes().any(|b| b.is_ascii_digit()) || candidate.bytes().all(|b| b.is_ascii_uppercase()));
            if plausible(&tail[pos]) {
                codes.push(Zeroizing::new(tail[pos].to_owned()));
                // A second code-like token makes the message ambiguous.
                for candidate in tail.iter().skip(pos + 1).filter(|candidate| plausible(candidate)) {
                    codes.push(Zeroizing::new((*candidate).to_owned()));
                }
            }
        }
    }
    codes
}

fn header<'a>(message: &'a Value, name: &str) -> Option<&'a str> {
    let mut values = message["payload"]["headers"].as_array()?.iter()
        .filter(|h| h["name"].as_str().is_some_and(|n| n.eq_ignore_ascii_case(name)))
        .filter_map(|h| h["value"].as_str());
    let one = values.next()?;
    if values.next().is_some() { return None; }
    Some(one)
}
pub(crate) fn address(value: &str) -> Option<&str> {
    let value = value.trim();
    let addr = match (value.rfind('<'), value.rfind('>')) {
        (Some(a), Some(b)) if a < b && b == value.len() - 1 => &value[a+1..b],
        (None, None) => value,
        _ => return None,
    };
    if addr.is_empty() || addr.len() > 320 || !addr.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'+' | b'@')) || addr.matches('@').count() != 1 { return None; }
    let (local, domain) = addr.split_once('@')?;
    if local.is_empty() || domain.is_empty() || domain.starts_with('.') || domain.ends_with('.') || domain.contains("..") { return None; }
    Some(addr)
}
fn code_from(message: &Value, sender: &str, recipient: &str, host: &str, after: u64, now: u64) -> Option<Zeroizing<String>> {
    let sender = address(sender)?;
    let recipient = address(recipient)?;
    let sender_domain = sender.rsplit_once('@')?.1;
    if host.is_empty() { return None; }
    if !address(header(message, "From")?)?.eq_ignore_ascii_case(sender)
        || !address(header(message, "To")?)?.eq_ignore_ascii_case(recipient) { return None; }
    let auth = header(message, "Authentication-Results")?.to_ascii_lowercase();
    // Require the Gmail-generated authentication result, not a bare From header.
    if !auth.starts_with("mx.google.com;") || !auth.split(';').any(|item| {
        let item = item.trim();
        item.split_whitespace().any(|word| word == "dmarc=pass")
            && item.split_whitespace().any(|word| word == format!("header.from={}", sender_domain.to_ascii_lowercase()))
    }) { return None; }
    let sent = message["internalDate"].as_str()?.parse::<u64>().ok()?;
    if sent < after || sent > now || now.saturating_sub(sent) > 10 * 60 * 1000 { return None; }
    fn collect(part: &Value, codes: &mut Vec<Zeroizing<String>>, depth: usize) -> Option<()> {
        if depth > 8 { return None; }
        if part["mimeType"] == "text/plain" {
            let encoded = part["body"]["data"].as_str()?;
            if encoded.len() > 131072 { return None; }
            let decoded = Zeroizing::new(data_encoding::BASE64URL_NOPAD.decode(encoded.as_bytes())
                .or_else(|_| data_encoding::BASE64URL.decode(encoded.as_bytes())).ok()?);
            let text = std::str::from_utf8(&decoded).ok()?;
            codes.extend(text_codes(text));
        }
        if let Some(parts) = part["parts"].as_array() {
            if parts.len() > 32 { return None; }
            for child in parts { collect(child, codes, depth + 1)?; }
        }
        Some(())
    }
    let mut codes = Vec::new();
    collect(&message["payload"], &mut codes, 0)?;
    if codes.len() != 1 { return None; }
    codes.pop()
}

pub(crate) fn sender_authorized(sender: &str, host: &str, page_text: &[&str]) -> bool {
    let Some(sender) = address(sender) else { return false };
    let Some((_, domain)) = sender.rsplit_once('@') else { return false };
    if domain.eq_ignore_ascii_case(host) { return true; }
    // Only a whole address in visible main-frame page text binds an external
    // sender to this site. An arbitrary substring or another frame is not proof.
    page_text.iter().any(|text| text.split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '+' | '@')))
        .any(|word| word.trim_matches('.').eq_ignore_ascii_case(sender)))
}

pub(crate) fn gmail_code(db: &Database, agent: &str, sender: &str, recipient: &str, origin: &str, after: u64, cancel: &AtomicBool) -> io::Result<Zeroizing<String>> {
    let url = url::Url::parse(origin).map_err(|_| io::Error::other("Invalid verification destination"))?;
    if url.scheme() != "https" || url.origin().ascii_serialization() != origin { return Err(io::Error::other("Verification requires an HTTPS origin")); }
    let host = url.host_str().ok_or_else(|| io::Error::other("Invalid verification destination"))?;
    if address(sender) != Some(sender) || address(recipient) != Some(recipient) {
        return Err(io::Error::other("Invalid verification sender or recipient"));
    }
    let query = format!("from:{sender} to:{recipient} newer_than:1d");
    let search = PrivateValue(crate::native_auth::gmail_read(db, agent, &json!({"operation":"search", "query":query}), cancel).map_err(|(_, e)| io::Error::other(e))?);
    if search.0["status"] != "ok" { return Err(io::Error::other("Gmail access is unavailable for this agent")); }
    let rows = search.0["data"]["messages"].as_array().ok_or_else(|| io::Error::other("No matching verification email"))?;
    if rows.len() > 10 || search.0["data"]["nextPageToken"].is_string() { return Err(io::Error::other("Verification email is ambiguous")); }
    let now = kageha_browser_client::milliseconds();
    if after > now || now.saturating_sub(after) > 10 * 60 * 1000 { return Err(io::Error::other("Verification attempt expired")); }
    let mut codes = Vec::new();
    for row in rows {
        let Some(id) = row["id"].as_str() else { return Err(io::Error::other("Invalid verification message")); };
        let message = PrivateValue(crate::native_auth::gmail_read(db, agent, &json!({"operation":"message", "id":id}), cancel).map_err(|(_, e)| io::Error::other(e))?);
        if message.0["status"] != "ok" { return Err(io::Error::other("Verification message unavailable")); }
        if let Some(code) = code_from(&message.0["data"], sender, recipient, host, after, now) { codes.push(code); }
    }
    if codes.len() != 1 { return Err(io::Error::other("No unique, recent verification message was found")); }
    Ok(codes.pop().unwrap())
}

/// Interpret the explicitly configured connector's normalized response. The connector
/// is trusted to assert authentication, timestamps, recipient and completeness; all
/// claims are still checked against this attempt and never returned to the model.
fn connector_code(response: &Value, sender: &str, recipient: &str, after: u64, now: u64) -> Option<Zeroizing<String>> {
    let domain = address(sender)?.rsplit_once('@')?.1;
    let sender = address(sender)?;
    let recipient = address(recipient)?;
    let content = &response["structuredContent"];
    if content["complete"] != true { return None; }
    let messages = content["messages"].as_array()?;
    if messages.len() > 10 { return None; }
    let mut codes = Vec::new();
    for message in messages {
        let received = message["received_ms"].as_u64()?;
        if received < after || received > now || now.saturating_sub(received) > 600_000 { continue; }
        if !address(message["sender"].as_str()?)?.eq_ignore_ascii_case(sender)
            || !address(message["recipient"].as_str()?)?.eq_ignore_ascii_case(recipient)
            || message["authentication"]["dmarc"] != "pass"
            || !message["authentication"]["aligned_domain"].as_str()?.eq_ignore_ascii_case(domain) { continue; }
        let text = message["text"].as_str()?;
        if text.len() > 131_072 { return None; }
        let found = text_codes(text);
        if found.len() != 1 { return None; }
        codes.extend(found);
    }
    if codes.len() == 1 { codes.pop() } else { None }
}

pub(crate) fn mcp_code(db: &Database, session: &str, turn: &str, config: &Value,
    sender: &str, recipient: &str, after: u64, cancel: &AtomicBool) -> io::Result<Zeroizing<String>> {
    if address(sender) != Some(sender) || address(recipient) != Some(recipient) { return Err(io::Error::other("Invalid verification sender or recipient")); }
    let now = kageha_browser_client::milliseconds();
    if after > now || now - after > 600_000 { return Err(io::Error::other("Verification attempt expired")); }
    let response = PrivateValue(crate::ui_mcp::verification_mail_read(db, session, turn, config,
        json!({"sender":sender,"recipient":recipient,"after_ms":after}), cancel)?);
    connector_code(&response.0, sender, recipient, after, kageha_browser_client::milliseconds())
        .ok_or_else(|| io::Error::other("No unique authenticated verification message was found"))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn message(body: &str) -> Value {
        json!({"internalDate":"1000000", "payload":{"mimeType":"text/plain", "headers":[
            {"name":"From","value":"Accounts <accounts@site.test>"},{"name":"To","value":"user@example.test"},
            {"name":"Authentication-Results","value":"mx.google.com; dmarc=pass header.from=site.test"}],
            "body":{"data":data_encoding::BASE64URL_NOPAD.encode(body.as_bytes())}}})
    }
    #[test]
    fn requires_unique_recent_authenticated_and_scoped_code() {
        let msg = message("Your verification code is 938271");
        assert_eq!(&*code_from(&msg,"accounts@site.test","user@example.test","site.test",999999,1000000).unwrap(),"938271");
        assert!(!sender_authorized("accounts@site.test","evil.test", &[]));
        assert!(code_from(&msg,"accounts@site.test","other@example.test","site.test",999999,1000000).is_none());
        assert!(code_from(&msg,"other@site.test","user@example.test","site.test",999999,1000000).is_none());
        assert!(code_from(&msg,"accounts@site.test","user@example.test","site.test",1000001,1000002).is_none());
        assert!(code_from(&msg,"accounts@site.test","user@example.test","site.test",999999,1600001).is_none());
        assert!(code_from(&message("code 123456 or 234567"),"accounts@site.test","user@example.test","site.test",999999,1000000).is_none());
        let mut forged=msg.clone(); forged["payload"]["headers"][2]["value"]=json!("mx.google.com; dmarc=fail header.from=site.test");
        assert!(code_from(&forged,"accounts@site.test","user@example.test","site.test",999999,1000000).is_none());
        let mut duplicate = msg.clone();
        duplicate["payload"]["headers"].as_array_mut().unwrap().push(json!({"name":"Authentication-Results","value":"mx.google.com; dmarc=pass header.from=site.test"}));
        assert!(code_from(&duplicate,"accounts@site.test","user@example.test","site.test",999999,1000000).is_none());
    }
    #[test]
    fn external_senders_need_exact_visible_site_evidence() {
        assert!(sender_authorized("accounts@site.test", "site.test", &[]));
        assert!(!sender_authorized("@site.test", "site.test", &[]));
        assert!(!sender_authorized("security@vendor.test", "site.test", &["email security@vendor.test.evil.test"]));
        assert!(sender_authorized("security@vendor.test", "site.test", &["We will email you from security@vendor.test."]));
        let mut msg = message("Your code is AB12CD");
        msg["payload"]["headers"][0]["value"] = json!("security@vendor.test");
        msg["payload"]["headers"][2]["value"] = json!("mx.google.com; dmarc=pass header.from=vendor.test");
        assert_eq!(&*code_from(&msg, "security@vendor.test", "user@example.test", "site.test", 999999, 1000000).unwrap(), "AB12CD");
        msg["payload"]["headers"][2]["value"] = json!("mx.google.com; dmarc=pass header.from=site.test");
        assert!(code_from(&msg, "security@vendor.test", "user@example.test", "site.test", 999999, 1000000).is_none());
        assert_eq!(text_codes("Verification: 1AB2CD")[0].as_str(), "1AB2CD");
    }
    #[test]
    fn trusted_connector_requires_complete_authenticated_matching_fresh_mail() {
        let mut response = json!({"structuredContent":{"complete":true,"messages":[{
            "sender":"security@vendor.test","recipient":"user@example.test","received_ms":1000000,
            "authentication":{"dmarc":"pass","aligned_domain":"vendor.test"},"text":"Your code is AB12CD"}]}});
        let read = |v: &Value| connector_code(v, "security@vendor.test", "user@example.test", 999999, 1000000).map(|s| s.to_string());
        assert_eq!(read(&response).as_deref(), Some("AB12CD"));
        response["structuredContent"]["complete"] = json!(false); assert!(read(&response).is_none());
        response["structuredContent"]["complete"] = json!(true);
        response["structuredContent"]["messages"][0]["authentication"]["dmarc"] = json!("fail"); assert!(read(&response).is_none());
        response["structuredContent"]["messages"][0]["authentication"]["dmarc"] = json!("pass");
        response["structuredContent"]["messages"][0]["received_ms"] = json!(999998); assert!(read(&response).is_none());
    }
}
