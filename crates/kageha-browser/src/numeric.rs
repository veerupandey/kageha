//! Exact decimal comparison for number controls; never round or clamp requested values.
pub fn canonical(text: &str) -> Option<String> {
    if text.is_empty() {
        return Some(String::new());
    }
    if text.len() > 4096 || !text.is_ascii() || !text.parse::<f64>().ok()?.is_finite() {
        return None;
    }
    let (negative, s) = text.strip_prefix('-').map_or((false, text), |s| (true, s));
    let parts: Vec<_> = s.split(['e', 'E']).collect();
    if parts.len() > 2 {
        return None;
    }
    let exponent = if parts.len() == 2 {
        let p = parts[1];
        if p.is_empty() {
            return None;
        }
        let digits = p.strip_prefix(['+', '-']).unwrap_or(p);
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        p.parse::<i32>().ok()?
    } else {
        0
    };
    if exponent.unsigned_abs() > 1_000_000 {
        return None;
    }
    let fraction: Vec<_> = parts[0].split('.').collect();
    if fraction.len() > 2 {
        return None;
    }
    if fraction[0].is_empty() && fraction.len() == 1 {
        return None;
    }
    if !fraction[0].bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let decimals = if fraction.len() == 2 {
        if fraction[1].is_empty() || !fraction[1].bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        fraction[1]
    } else {
        ""
    };
    let combined = format!("{}{}", fraction[0], decimals);
    let digits = combined.trim_start_matches('0');
    if digits.is_empty() {
        return Some("0e0".into());
    }
    let trimmed = digits.trim_end_matches('0');
    let exponent = exponent
        .checked_sub(decimals.len() as i32)?
        .checked_add((digits.len() - trimmed.len()) as i32)?;
    Some(format!(
        "{}{trimmed}e{exponent}",
        if negative { "-" } else { "" }
    ))
}
pub const CANONICAL_JS: &str = r#"function numericCanonical(value) {
    if(value==='')return '';
    if(typeof value!=='string'||value.length>4096||!Number.isFinite(Number(value)))return null;
    const m=/^(-?)(?:(\d+)(?:\.(\d+))?|\.(\d+))(?:[eE]([+-]?\d+))?$/.exec(value);
    if(!m)return null;
    let exponent=Number(m[5]||0);if(!Number.isSafeInteger(exponent)||Math.abs(exponent)>1000000)return null;
    const fraction=m[3]||m[4]||'';
    let digits=((m[2]||'')+fraction).replace(/^0+/,'');if(!digits)return '0e0';
    const trim=digits.replace(/0+$/,'');exponent=exponent-fraction.length+digits.length-trim.length;
    return m[1]+trim+'e'+exponent;
}"#;
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_decimal_comparison_preserves_precision() {
        for (a, b) in [
            ("3.00", "3"),
            (".5", "5e-1"),
            ("-0", "0"),
            ("30e-1", "3"),
            ("-123.4500", "-12345e-2"),
        ] {
            assert_eq!(canonical(a), canonical(b));
        }
        assert_ne!(canonical("9007199254740993"), canonical("9007199254740992"));
        for text in [
            "NaN", "Infinity", "1e309", "+3", "3.", " 3", "3\n", "3,00", "1e", "--2",
        ] {
            assert!(canonical(text).is_none(), "{text}");
        }
        assert_eq!(canonical(""), Some(String::new()));
    }
}
