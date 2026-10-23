//! Quoting and number formatting the way `Data::Dumper` does it.

/// `'...'` with `\` and `'` escaped.
pub fn single_quote(output: &mut String, value: &str) {
    // grow the buffer to hold the string and some extra characters
    output.reserve(value.len() + 2);
    output.push('\'');
    // copy the runs between special characters in bulk
    let bytes = value.as_bytes();
    let mut start = 0;
    while let Some(n) = memchr::memchr2(b'\'', b'\\', &bytes[start..]) {
        let at = start + n;
        output.push_str(&value[start..at]);
        output.push('\\');
        output.push(bytes[at] as char);
        start = at + 1;
    }
    output.push_str(&value[start..]);
    output.push('\'');
}

/// `"..."` the way `Data::Dumper`'s `Useqq` does: named escapes for the common
/// control characters, octal for the rest, `$` and `@` escaped, and
/// `\x{...}` for anything outside ASCII.
pub fn double_quote(output: &mut String, value: &str) {
    use std::fmt::Write;
    output.reserve(value.len() + 2);
    output.push('"');
    let mut chars = value.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' | '"' | '$' | '@' => {
                output.push('\\');
                output.push(c);
            }
            '\x07' => output.push_str("\\a"),
            '\x08' => output.push_str("\\b"),
            '\t' => output.push_str("\\t"),
            '\n' => output.push_str("\\n"),
            '\x0C' => output.push_str("\\f"),
            '\r' => output.push_str("\\r"),
            '\x1B' => output.push_str("\\e"),
            // octal, padded to three digits if a digit follows
            '\0'..='\x1F' | '\x7F' => {
                let digit_next = chars.peek().is_some_and(char::is_ascii_digit);
                if digit_next {
                    let _ = write!(output, "\\{:03o}", c as u32);
                } else {
                    let _ = write!(output, "\\{:o}", c as u32);
                }
            }
            c if c.is_ascii() => output.push(c),
            c => {
                let _ = write!(output, "\\x{{{:x}}}", c as u32);
            }
        }
    }
    output.push('"');
}

/// What `Data::Dumper` does without `Useqq` for a string with non-ASCII
/// characters: double quotes, `\`, `"`, `$` and `@` escaped, everything above
/// ASCII as `\x{...}`, and control characters left alone. This reads back as
/// the same characters whether or not the Perl source is `use utf8`.
pub fn wide_quote(output: &mut String, value: &str) {
    use std::fmt::Write;
    output.reserve(value.len() + 2);
    output.push('"');
    for c in value.chars() {
        match c {
            '\\' | '"' | '$' | '@' => {
                output.push('\\');
                output.push(c);
            }
            c if c.is_ascii() => output.push(c),
            c => {
                let _ = write!(output, "\\x{{{:x}}}", c as u32);
            }
        }
    }
    output.push('"');
}

/// Is this usable as a hash key without quotes? Same rule as `Data::Dumper`
/// with `Quotekeys = 0`: an identifier, or a canonical integer.
pub fn is_bare_key(value: &str) -> bool {
    let mut chars = value.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {
            value.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        _ => is_safe_int_str(value),
    }
}

/// `0` or `-?[1-9]\d{0,8}`: integers `Data::Dumper` writes without quotes.
pub fn is_safe_int_str(value: &str) -> bool {
    let digits = value.strip_prefix('-').unwrap_or(value);
    if value == "0" {
        return true;
    }
    !digits.is_empty()
        && digits.len() <= 9
        && !digits.starts_with('0')
        && digits.bytes().all(|b| b.is_ascii_digit())
}

/// Integers (as opposed to strings) are written bare when their text,
/// including any sign, is at most 10 characters; longer ones are quoted so a
/// 32-bit Perl reads them as strings instead of overflowing.
pub fn is_short_int(n: i128) -> bool {
    n.to_string().len() <= 10
}

/// Format a float the way Perl stringifies it (`%.15g`), or, when `lossless`,
/// with the shortest digits that round-trip. NaN and infinities are `NaN`,
/// `Inf` and `-Inf`, which Perl reads back as numbers.
pub fn float_text(f: f64, lossless: bool) -> String {
    if f.is_nan() {
        return "NaN".into();
    }
    if f.is_infinite() {
        return if f < 0.0 { "-Inf" } else { "Inf" }.into();
    }
    // 15 significant digits as d.dddddddddddddde±x, or the shortest round-trip
    let sci = if lossless {
        format!("{f:e}")
    } else {
        format!("{f:.14e}")
    };
    let (mantissa, exp) = sci.split_once('e').unwrap();
    let exp: i32 = exp.parse().unwrap();
    let (sign, mantissa) = match mantissa.strip_prefix('-') {
        Some(m) => ("-", m),
        None => ("", mantissa),
    };
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let digits = digits.trim_end_matches('0');
    let digits = if digits.is_empty() { "0" } else { digits };
    if !(-4..15).contains(&exp) {
        let (first, rest) = digits.split_at(1);
        let frac = if rest.is_empty() {
            String::new()
        } else {
            format!(".{rest}")
        };
        let esign = if exp < 0 { '-' } else { '+' };
        format!("{sign}{first}{frac}e{esign}{:02}", exp.abs())
    } else if exp >= 0 {
        let int_len = exp as usize + 1;
        if digits.len() <= int_len {
            format!("{sign}{digits}{}", "0".repeat(int_len - digits.len()))
        } else {
            let (int, frac) = digits.split_at(int_len);
            format!("{sign}{int}.{frac}")
        }
    } else {
        format!("{sign}0.{}{digits}", "0".repeat((-exp - 1) as usize))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_single_quote() {
        let mut output = String::new();
        single_quote(&mut output, "hello");
        assert_eq!(output, "'hello'");

        let mut output = String::new();
        single_quote(&mut output, "hello 'world'");
        assert_eq!(output, "'hello \\'world\\''");

        let mut output = String::new();
        single_quote(&mut output, "a\\b\\");
        assert_eq!(output, "'a\\\\b\\\\'");
    }

    #[test]
    fn test_double_quote() {
        let mut o = String::new();
        double_quote(&mut o, "a\n\t\x01\x7f$@\"\\\u{e9}\u{263a}");
        assert_eq!(o, "\"a\\n\\t\\1\\177\\$\\@\\\"\\\\\\x{e9}\\x{263a}\"");
        let mut o = String::new();
        double_quote(&mut o, "1\x012\0");
        assert_eq!(o, "\"1\\0012\\0\"");
    }

    #[test]
    fn test_wide_quote() {
        let mut o = String::new();
        wide_quote(&mut o, "a\tb'\"$@\\\u{e9}\u{263a}");
        assert_eq!(o, "\"a\tb'\\\"\\$\\@\\\\\\x{e9}\\x{263a}\"");
    }

    #[test]
    fn test_bare_keys() {
        for k in ["a", "_x", "a1", "0", "12", "-5", "123456789"] {
            assert!(is_bare_key(k), "{k}");
        }
        for k in [
            "",
            "007",
            "1x",
            "a::b",
            "b c",
            "-0",
            "1234567890",
            "-",
            "a-b",
        ] {
            assert!(!is_bare_key(k), "{k}");
        }
    }

    #[test]
    fn test_floats_like_perl() {
        let p = |f| float_text(f, false);
        assert_eq!(p(1.5), "1.5");
        assert_eq!(p(3.0), "3");
        assert_eq!(p(0.1 + 0.2), "0.3");
        assert_eq!(p(1e20), "1e+20");
        assert_eq!(p(1e-7), "1e-07");
        assert_eq!(p(-0.5), "-0.5");
        assert_eq!(p(9007199254740992.0), "9.00719925474099e+15");
        assert_eq!(p(123456789012345.0), "123456789012345");
        assert_eq!(p(0.0001), "0.0001");
        assert_eq!(p(f64::NAN), "NaN");
        assert_eq!(p(f64::INFINITY), "Inf");
        assert_eq!(p(f64::NEG_INFINITY), "-Inf");
        // lossless keeps what Perl would drop
        assert_eq!(float_text(0.1 + 0.2, true), "0.30000000000000004");
        assert_eq!(float_text(2.5, true), "2.5");
    }
}
