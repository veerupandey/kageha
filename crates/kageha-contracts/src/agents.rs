//! Stable specialist identity rules, including retained Python-era IDs.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 80
        && id.as_bytes()[0].is_ascii_alphanumeric()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}
#[cfg(test)]
mod tests {
    #[test]
    fn retained_ids_are_not_truncated_or_treated_as_paths() {
        for id in ["main".to_owned(), "research.ai".into(), "a".repeat(80)] {
            assert!(super::valid_id(&id));
        }
        for id in [
            "".to_owned(),
            "..".into(),
            "a/b".into(),
            "a\nb".into(),
            "a".repeat(81),
        ] {
            assert!(!super::valid_id(&id));
        }
    }
}
