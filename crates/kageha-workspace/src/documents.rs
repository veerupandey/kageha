//! Native document text extraction. Office ZIP entries and output are bounded.
use std::{
    io::{self, Read},
    path::Path,
};
const MAX: usize = 1024 * 1024;
pub fn extract(path: &Path) -> io::Result<String> {
    let control = kageha_contracts::execution::Control {
        cancelled: &std::sync::atomic::AtomicBool::new(false),
        deadline: None,
    };
    let bytes = crate::file_snapshot(path, None, &control)?.bytes;
    extract_bytes(
        &bytes,
        path.extension().and_then(|s| s.to_str()).unwrap_or("txt"),
    )
}
pub fn extract_bytes(bytes: &[u8], suffix: &str) -> io::Result<String> {
    if bytes.len() > crate::DOCUMENT_LIMIT {
        return Err(io::Error::other("Document exceeds 16 MiB"));
    }
    let suffix = suffix.to_ascii_lowercase();
    let result = match suffix.as_str() {
        "pdf" => pdf_extract::extract_text_from_mem(bytes).map_err(|_| {
            io::Error::other("PDF text could not be extracted; an image-only PDF needs OCR")
        })?,
        "docx" | "pptx" | "xlsx" | "xlsm" => {
            let mut zip =
                zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(io::Error::other)?;
            if zip.len() > 10000 {
                return Err(io::Error::other("Too many Office entries"));
            }
            let mut names: Vec<String> = zip
                .file_names()
                .filter(|name| match suffix.as_str() {
                    "docx" => {
                        *name == "word/document.xml"
                            || name.starts_with("word/header")
                            || name.starts_with("word/footer")
                    }
                    "pptx" => name.starts_with("ppt/slides/slide") && name.ends_with(".xml"),
                    _ => {
                        *name == "xl/sharedStrings.xml"
                            || name.starts_with("xl/worksheets/sheet") && name.ends_with(".xml")
                    }
                })
                .map(str::to_owned)
                .collect();
            names.sort_by_key(|name| {
                (
                    name.chars()
                        .filter(char::is_ascii_digit)
                        .collect::<String>()
                        .parse::<u64>()
                        .unwrap_or(0),
                    name.clone(),
                )
            });
            let mut output = String::new();
            let mut total = 0u64;
            let mut shared = Vec::<String>::new();
            for name in names {
                let file = zip.by_name(&name).map_err(io::Error::other)?;
                if file.size() > (16 * 1024 * 1024u64).saturating_sub(total) {
                    return Err(io::Error::other("Office content expands beyond 16 MiB"));
                }
                let mut bytes = Vec::new();
                file.take(16 * 1024 * 1024 + 1 - total)
                    .read_to_end(&mut bytes)?;
                total += bytes.len() as u64;
                if total > 16 * 1024 * 1024 {
                    return Err(io::Error::other("Office content expands beyond 16 MiB"));
                }
                if suffix == "xlsx" || suffix == "xlsm" {
                    if name == "xl/sharedStrings.xml" {
                        shared = excel_strings(&bytes)?;
                        continue;
                    }
                    output += &format!("{}\n{}", name, excel_sheet(&bytes, &shared)?);
                } else {
                    output += &xml_text(&bytes)?;
                }
                output.push('\n');
                if output.len() > MAX {
                    return Err(io::Error::other("Extracted text exceeds 1 MiB"));
                }
            }
            output
        }
        "html" | "htm" => xml_text_inner(bytes, true)?,
        "xml" => xml_text(bytes)?,
        _ => String::from_utf8(bytes.to_vec())
            .map_err(|_| io::Error::other("Unsupported binary document type"))?,
    };
    if result.len() > MAX {
        return Err(io::Error::other("Extracted text exceeds 1 MiB"));
    }
    if result.trim().is_empty() {
        return Err(io::Error::other("Document contains no extractable text"));
    }
    Ok(result)
}
fn xml_text(bytes: &[u8]) -> io::Result<String> {
    xml_text_inner(bytes, false)
}
fn xml_text_inner(bytes: &[u8], html: bool) -> io::Result<String> {
    use quick_xml::{events::Event, Reader};
    if html {
        return html_text(bytes);
    }
    let mut reader = Reader::from_reader(bytes);
    let mut output = String::new();
    let mut hidden = 0;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                if e.local_name().as_ref().eq_ignore_ascii_case(b"script")
                    || e.local_name().as_ref().eq_ignore_ascii_case(b"style")
                {
                    hidden += 1;
                }
            }
            Ok(Event::End(e)) => {
                if e.local_name().as_ref().eq_ignore_ascii_case(b"script")
                    || e.local_name().as_ref().eq_ignore_ascii_case(b"style")
                {
                    hidden = usize::saturating_sub(hidden, 1);
                }
                if matches!(
                    e.local_name().as_ref(),
                    b"p" | b"tr" | b"row" | b"div" | b"br"
                ) {
                    output.push('\n');
                }
            }
            Ok(Event::Text(e)) if hidden == 0 => {
                let decoded = e.decode().map_err(io::Error::other)?;
                output += &quick_xml::escape::unescape(&decoded).map_err(io::Error::other)?;
                output.push(' ');
            }
            Ok(Event::GeneralRef(e)) if hidden == 0 => {
                let reference = e.decode().map_err(io::Error::other)?;
                let escaped = format!("&{reference};");
                output += &quick_xml::escape::unescape(&escaped).map_err(io::Error::other)?;
            }
            Ok(Event::CData(e)) if hidden == 0 => {
                output += &e.decode().map_err(io::Error::other)?;
            }
            Ok(Event::Eof) => break,
            Err(_) => return Err(io::Error::other("Invalid document XML")),
            _ => {}
        }
        if output.len() > MAX {
            return Err(io::Error::other("Extracted text exceeds 1 MiB"));
        }
    }
    Ok(output)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn xml_entities_and_hidden_text() {
        assert_eq!(
            xml_text(b"<root><p>Hello &amp; Rust</p><script>secret</script></root>")
                .unwrap()
                .trim(),
            "Hello  & Rust"
        );
    }
    #[test]
    fn invalid_xml_fails() {
        assert!(xml_text(b"<root><p></bad>").is_err());
    }
}

fn excel_strings(bytes: &[u8]) -> io::Result<Vec<String>> {
    use quick_xml::{events::Event, Reader};
    let mut reader = Reader::from_reader(bytes);
    let mut values = Vec::new();
    let mut inside = false;
    let mut text = String::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) if e.local_name().as_ref() == b"si" => {
                inside = true;
                text.clear();
            }
            Ok(Event::End(e)) if e.local_name().as_ref() == b"si" => {
                inside = false;
                values.push(text.clone());
            }
            Ok(Event::Text(e)) if inside => text += &e.decode().map_err(io::Error::other)?,
            Ok(Event::GeneralRef(e)) if inside => {
                text += &quick_xml::escape::unescape(&format!(
                    "&{};",
                    e.decode().map_err(io::Error::other)?
                ))
                .map_err(io::Error::other)?;
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(io::Error::other(e)),
            _ => {}
        }
    }
    Ok(values)
}
fn excel_sheet(bytes: &[u8], shared: &[String]) -> io::Result<String> {
    use quick_xml::{events::Event, Reader};
    let mut reader = Reader::from_reader(bytes);
    let mut output = String::new();
    let (mut cell, mut value, mut kind) = (String::new(), String::new(), String::new());
    let mut capture = false;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) if e.local_name().as_ref() == b"c" => {
                cell.clear();
                value.clear();
                kind.clear();
                for attr in e.attributes() {
                    let attr = attr.map_err(io::Error::other)?;
                    let decoded = attr.unescape_value().map_err(io::Error::other)?;
                    if attr.key.as_ref() == b"r" {
                        cell = decoded.into_owned();
                    } else if attr.key.as_ref() == b"t" {
                        kind = decoded.into_owned();
                    }
                }
            }
            Ok(Event::Start(e)) if matches!(e.local_name().as_ref(), b"v" | b"t") => capture = true,
            Ok(Event::End(e)) if matches!(e.local_name().as_ref(), b"v" | b"t") => capture = false,
            Ok(Event::Text(e)) if capture => value += &e.decode().map_err(io::Error::other)?,
            Ok(Event::GeneralRef(e)) if capture => {
                value += &quick_xml::escape::unescape(&format!(
                    "&{};",
                    e.decode().map_err(io::Error::other)?
                ))
                .map_err(io::Error::other)?
            }
            Ok(Event::End(e)) if e.local_name().as_ref() == b"c" => {
                let resolved = if kind == "s" {
                    shared
                        .get(value.parse::<usize>().map_err(io::Error::other)?)
                        .ok_or_else(|| io::Error::other("Invalid spreadsheet shared string"))?
                } else {
                    &value
                };
                output += &format!("{cell}: {resolved}\n");
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(io::Error::other(e)),
            _ => {}
        }
        if output.len() > MAX {
            return Err(io::Error::other("Spreadsheet text exceeds 1 MiB"));
        }
    }
    Ok(output)
}
#[cfg(test)]
mod spreadsheet_tests {
    use super::*;
    #[test]
    fn resolves_shared_and_inline_cells() {
        let shared = excel_strings(b"<sst><si><t>Rust &amp; tools</t></si></sst>").unwrap();
        let result=excel_sheet(b"<worksheet><row><c r=\"A1\" t=\"s\"><v>0</v></c><c r=\"B1\"><v>42</v></c></row></worksheet>",&shared).unwrap();
        assert_eq!(result, "A1: Rust & tools\nB1: 42\n");
    }
}

#[cfg(test)]
mod html_tests {
    use super::*;
    #[test]
    fn accepts_html_void_tags_and_named_entities() {
        let text=xml_text_inner(b"<html><body><input name=x><br><p>Browser&nbsp;control</p><SCRIPT>if (a < b && x > y) hidden</SCRIPT>A & B</body></html>",true).unwrap();
        assert!(text.contains("Browser"));
        assert!(text.contains("control"));
        assert!(!text.contains("hidden"));
    }
}

fn html_text(bytes: &[u8]) -> io::Result<String> {
    let source = std::str::from_utf8(bytes).map_err(io::Error::other)?;
    let lower = source.to_ascii_lowercase();
    let mut output = String::new();
    let mut at = 0;
    let append = |out: &mut String, text: &str| {
        let mut rest = text;
        while let Some(amp) = rest.find('&') {
            out.push_str(&rest[..amp]);
            let tail = &rest[amp..];
            if let Some(end) = tail.as_bytes().iter().take(33).position(|b| *b == b';') {
                match quick_xml::escape::unescape(&tail[..=end]) {
                    Ok(value) => {
                        out.push_str(&value);
                        rest = &tail[end + 1..];
                        continue;
                    }
                    Err(_) => {}
                }
            }
            out.push('&');
            rest = &tail[1..];
        }
        out.push_str(rest);
    };
    while at < source.len() {
        let next = source[at..]
            .find('<')
            .map(|i| at + i)
            .unwrap_or(source.len());
        append(&mut output, &source[at..next]);
        if output.len() > MAX {
            return Err(io::Error::other("HTML text exceeds 1 MiB"));
        }
        if next == source.len() {
            break;
        }
        if source[next..].starts_with("<!--") {
            at = source[next + 4..]
                .find("-->")
                .map(|i| next + 4 + i + 3)
                .unwrap_or(source.len());
            continue;
        }
        let mut end = next + 1;
        let mut quote = 0;
        while end < source.len() {
            let b = source.as_bytes()[end];
            if quote != 0 {
                if b == quote {
                    quote = 0;
                }
            } else if b == b'\'' || b == b'"' {
                quote = b;
            } else if b == b'>' {
                break;
            }
            end += 1;
        }
        if end == source.len() {
            append(&mut output, &source[next..]);
            break;
        }
        let raw = lower[next + 1..end].trim();
        let tag = raw
            .trim_start_matches('/')
            .split(|c: char| c.is_ascii_whitespace() || c == '/')
            .next()
            .unwrap_or("");
        if !tag
            .as_bytes()
            .first()
            .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'!')
        {
            output.push('<');
            at = next + 1;
            continue;
        }
        if !raw.starts_with('/') && matches!(tag, "script" | "style") {
            at = lower[end + 1..]
                .find(&format!("</{tag}"))
                .map(|i| end + 1 + i)
                .unwrap_or(source.len());
            continue;
        }
        if matches!(tag, "br" | "p" | "div" | "tr" | "li" | "h1" | "h2" | "h3") {
            output.push('\n');
        } else {
            output.push(' ');
        }
        at = end + 1;
    }
    if output.len() > MAX {
        return Err(io::Error::other("HTML text exceeds 1 MiB"));
    }
    Ok(output)
}
