//! Formatting helpers that the codecs need but that have nothing to do with the
//! document model: hex, base64, ISO-8601, and IEEE 754 decimal128.
//!
//! These are hand-rolled rather than pulled from crates.io because each one is
//! small, and decimal128 in particular is on the hot path when a BSON document
//! is rendered.

const DECIMAL128_BIAS: i32 = 6176;
/// Canonical decimal128 significands are below 10^34.
const DECIMAL128_MAX_SIGNIFICAND: u128 = 10_000_000_000_000_000_000_000_000_000_000_000;

const HEX_LOWER: &[u8; 16] = b"0123456789abcdef";
const B64_ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

// ------------------------------------------------------------------ hex

pub fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX_LOWER[(b >> 4) as usize] as char);
        out.push(HEX_LOWER[(b & 0x0f) as usize] as char);
    }
    out
}

pub fn hex_decode(text: &str) -> Option<Vec<u8>> {
    let cleaned: Vec<u8> = text
        .trim()
        .strip_prefix("0x")
        .or_else(|| text.trim().strip_prefix("0X"))
        .unwrap_or(text.trim())
        .bytes()
        .filter(|b| !b.is_ascii_whitespace())
        .collect();
    if !cleaned.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(cleaned.len() / 2);
    for pair in cleaned.chunks(2) {
        let hi = hex_digit(pair[0])?;
        let lo = hex_digit(pair[1])?;
        out.push((hi << 4) | lo);
    }
    Some(out)
}

fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// True when the text is nothing but hex digits (with an optional `0x`).
pub fn looks_like_hex(text: &str) -> bool {
    let t = text.trim();
    let t = t
        .strip_prefix("0x")
        .or_else(|| t.strip_prefix("0X"))
        .unwrap_or(t);
    !t.is_empty()
        && t.len().is_multiple_of(2)
        && t.bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f' | b'A'..=b'F'))
}

// --------------------------------------------------------------- base64

pub fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64_ALPHABET[(n >> 18) as usize & 0x3f] as char);
        out.push(B64_ALPHABET[(n >> 12) as usize & 0x3f] as char);
        if chunk.len() > 1 {
            out.push(B64_ALPHABET[(n >> 6) as usize & 0x3f] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(B64_ALPHABET[n as usize & 0x3f] as char);
        } else {
            out.push('=');
        }
    }
    out
}

pub fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let input: Vec<u8> = text.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    let mut symbols = 0usize;
    for &b in &input {
        if b == b'=' {
            break;
        }
        b64_value(b)?;
        symbols += 1;
    }
    if symbols % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(symbols / 4 * 3 + 3);
    let mut buf = 0u32;
    let mut bits = 0u32;
    for &b in input.iter().take(symbols) {
        buf = (buf << 6) | b64_value(b)? as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Some(out)
}

fn b64_value(b: u8) -> Option<u8> {
    match b {
        b'A'..=b'Z' => Some(b - b'A'),
        b'a'..=b'z' => Some(b - b'a' + 26),
        b'0'..=b'9' => Some(b - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

/// True when the text is plausible base64 (multiples of four valid symbols).
pub fn looks_like_base64(text: &str) -> bool {
    let t: Vec<u8> = text.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    t.len() >= 8
        && t.len().is_multiple_of(4)
        && t.iter().all(|b| b64_value(*b).is_some() || *b == b'=')
}

// -------------------------------------------------------------- iso 8601

/// Milliseconds since the Unix epoch to `YYYY-MM-DDTHH:MM:SS.mmmZ`.
pub fn iso8601(millis: i64) -> String {
    let days = millis.div_euclid(86_400_000);
    let rem = millis.rem_euclid(86_400_000);
    let (h, m, s, ms) = (
        (rem / 3_600_000) as u32,
        (rem / 60_000 % 60) as u32,
        (rem / 1000 % 60) as u32,
        (rem % 1000) as u32,
    );
    let (y, mo, d) = civil_from_days(days);
    let mut out = String::with_capacity(24);
    if y < 0 {
        out.push('-');
    }
    out.push_str(&format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        y.abs(),
        mo,
        d,
        h,
        m,
        s
    ));
    if ms != 0 {
        out.push_str(&format!(".{:03}", ms));
    }
    out.push('Z');
    out
}

/// Accepts `YYYY-MM-DDTHH:MM:SS[.frac][Z|±HH:MM]` and a bare integer of
/// milliseconds, which is what people paste into the value editor.
pub fn parse_iso8601(text: &str) -> Option<i64> {
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    if let Ok(millis) = t.parse::<i64>() {
        return Some(millis);
    }
    let bytes = t.as_bytes();
    if bytes.len() < 19 {
        return None;
    }
    let y: i64 = t.get(0..4)?.parse().ok()?;
    let mo: u32 = t.get(5..7)?.parse().ok()?;
    let d: u32 = t.get(8..10)?.parse().ok()?;
    let h: u32 = t.get(11..13)?.parse().ok()?;
    let mi: u32 = t.get(14..16)?.parse().ok()?;
    let s: u32 = t.get(17..19)?.parse().ok()?;
    if mo == 0 || mo > 12 || d == 0 || d > 31 || h > 23 || mi > 59 || s > 60 {
        return None;
    }
    let mut millis = days_from_civil(y, mo, d) * 86_400_000
        + h as i64 * 3_600_000
        + mi as i64 * 60_000
        + s as i64 * 1000;

    let rest = &t[19..];
    let rest = if let Some(r) = rest.strip_prefix('.') {
        let frac_digits = r.bytes().take_while(|b| b.is_ascii_digit()).count();
        let used = frac_digits.min(3);
        let frac: i64 = format!("{:0<3}", &r[..used]).parse().ok()?;
        millis += frac;
        &r[frac_digits..]
    } else {
        rest
    };
    match rest.as_bytes().first() {
        None | Some(b'Z') | Some(b'z') => Some(millis),
        Some(b'+') | Some(b'-') => {
            let sign = if rest.starts_with('-') { 1 } else { -1 };
            let oh: i64 = rest.get(1..3)?.parse().ok()?;
            let om: i64 = rest.get(4..6)?.parse().ok()?;
            Some(millis + sign * (oh * 3_600_000 + om * 60_000))
        }
        _ => None,
    }
}

/// Howard Hinnant's `civil_from_days`: days since 1970-01-01 to a proleptic
/// Gregorian date.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (y + i64::from(m <= 2), m, d)
}

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = y - i64::from(m <= 2);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let mp = if m > 2 { m - 3 } else { m + 9 } as u64;
    let doy = (153 * mp + 2) / 5 + d as u64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe as i64 - 719_468
}

// ----------------------------------------------------------- decimal128

/// Decode the 128-bit BID representation MongoDB uses into its string form.
///
/// Follows the BSON decimal128 spec: the combination field selects between the
/// two exponent/significand layouts, significands at or above 10^34 are
/// non-canonical and read as zero, and output switches to scientific notation
/// outside the plain-notation window.
pub fn decimal128_to_string(lo: u64, hi: u64) -> String {
    let negative = hi >> 63 == 1;
    let combination = (hi >> 58) & 0x1F;
    if combination == 0x1F {
        return "NaN".to_string();
    }
    if combination == 0x1E {
        return if negative {
            "-Infinity".to_string()
        } else {
            "Infinity".to_string()
        };
    }

    let (biased, significand) = if combination >= 0x18 {
        // bits[126:125] == "11": exponent is bits[124:111] and the significand
        // carries an implicit leading "100".
        let sig = ((4u128) << 111) | (((hi & 0x0000_7FFF_FFFF_FFFF) as u128) << 64) | lo as u128;
        (((hi >> 47) & 0x3FFF) as i32, sig)
    } else {
        let sig = (((hi & 0x0001_FFFF_FFFF_FFFF) as u128) << 64) | lo as u128;
        (((hi >> 49) & 0x3FFF) as i32, sig)
    };
    let significand = if significand >= DECIMAL128_MAX_SIGNIFICAND {
        0
    } else {
        significand
    };
    format_decimal(negative, significand, biased - DECIMAL128_BIAS)
}

fn format_decimal(negative: bool, significand: u128, exponent: i32) -> String {
    let digits = significand.to_string();
    let len = digits.len() as i32;
    let adjusted = exponent + len - 1;

    let mut out = String::with_capacity(digits.len() + 12);
    if negative {
        out.push('-');
    }
    if exponent <= 0 && adjusted >= -6 {
        let point = len + exponent;
        if point > 0 {
            out.push_str(&digits[..point as usize]);
            if (point as usize) < digits.len() {
                out.push('.');
                out.push_str(&digits[point as usize..]);
            }
        } else {
            out.push_str("0.");
            out.extend(std::iter::repeat_n('0', (-point) as usize));
            out.push_str(&digits);
        }
    } else {
        out.push_str(&digits[..1]);
        if len > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        out.push('E');
        if adjusted >= 0 {
            out.push('+');
        }
        out.push_str(&adjusted.to_string());
    }
    out
}

/// Encode a decimal literal into the 128-bit BID representation.
///
/// Returns `None` when the value cannot be represented exactly, so the editor
/// can refuse the input instead of silently rounding.
pub fn decimal128_from_str(text: &str) -> Option<(u64, u64)> {
    let t = text.trim();
    let (negative, rest) = match t.as_bytes().first() {
        Some(b'-') => (true, &t[1..]),
        Some(b'+') => (false, &t[1..]),
        _ => (false, t),
    };
    let sign_bit = (negative as u64) << 63;
    let lower = rest.to_ascii_lowercase();
    if lower == "nan" {
        return Some((0, sign_bit | (0x1F << 58)));
    }
    if lower == "infinity" || lower == "inf" {
        return Some((0, sign_bit | (0x1E << 58)));
    }

    let (mantissa, exp_part) = match rest.split_once(['e', 'E']) {
        Some((m, e)) => (m, e.trim_start_matches('+').parse::<i32>().ok()?),
        None => (rest, 0),
    };
    let (int_part, frac_part) = match mantissa.split_once('.') {
        Some((a, b)) => (a, b),
        None => (mantissa, ""),
    };
    if !int_part
        .bytes()
        .chain(frac_part.bytes())
        .all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let mut digits: String = format!("{}{}", int_part, frac_part);
    let mut exponent = exp_part - frac_part.len() as i32;

    // Leading zeros are never significant. Trailing zeros *are*: decimal128 is
    // a decimal type, so "1.5000" and "1.5" are distinct values and "1000" is
    // distinct from "1E+3". They may only be dropped when the coefficient is
    // wider than the 34 digits the format can store.
    let leading = digits.len() - digits.trim_start_matches('0').len();
    digits.drain(..leading.min(digits.len().saturating_sub(1)));
    while digits.len() > 34 {
        if !digits.ends_with('0') {
            return None;
        }
        digits.pop();
        exponent += 1;
    }
    // Canonical decimal128 exponents run from -6176 to 6111, and the adjusted
    // exponent (the one that shows up in scientific notation) is capped at the
    // same 6111, so a wide significand shrinks the usable exponent range.
    const EXPONENT_MAX: i32 = 6111;
    const EXPONENT_MIN: i32 = -6176;
    if !(EXPONENT_MIN..=EXPONENT_MAX).contains(&exponent) {
        return None;
    }
    if exponent + digits.len() as i32 - 1 > EXPONENT_MAX {
        return None;
    }
    let significand: u128 = digits.parse().ok()?;
    let biased = exponent + DECIMAL128_BIAS;
    let hi =
        sign_bit | ((biased as u64) << 49) | (((significand >> 64) as u64) & 0x0001_FFFF_FFFF_FFFF);
    Some((significand as u64, hi))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips() {
        assert_eq!(hex_encode(&[0x00, 0x0f, 0xa5, 0xff]), "000fa5ff");
        assert_eq!(
            hex_decode("000fa5ff").unwrap(),
            vec![0x00, 0x0f, 0xa5, 0xff]
        );
        assert_eq!(
            hex_decode("0x000fa5ff").unwrap(),
            vec![0x00, 0x0f, 0xa5, 0xff]
        );
        assert_eq!(hex_decode("abc"), None);
        assert_eq!(hex_decode("zz"), None);
    }

    #[test]
    fn base64_matches_known_vectors() {
        for (text, encoded) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64_encode(text.as_bytes()), encoded, "encode {text:?}");
            assert_eq!(
                base64_decode(encoded).unwrap(),
                text.as_bytes(),
                "decode {encoded:?}"
            );
        }
        assert_eq!(base64_decode("!!!!"), None);
    }

    #[test]
    fn base64_handles_binary_payloads() {
        let bytes: Vec<u8> = (0..=255).collect();
        assert_eq!(base64_decode(&base64_encode(&bytes)).unwrap(), bytes);
    }

    #[test]
    fn iso8601_matches_known_instants() {
        assert_eq!(iso8601(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso8601(1_000_000_000_000), "2001-09-09T01:46:40Z");
        assert_eq!(iso8601(1_700_000_000_123), "2023-11-14T22:13:20.123Z");
        assert_eq!(iso8601(-1), "1969-12-31T23:59:59.999Z");
        assert_eq!(iso8601(-86_400_000), "1969-12-31T00:00:00Z");
    }

    #[test]
    fn iso8601_round_trips_through_the_parser() {
        for millis in [
            0i64,
            1,
            -1,
            1_700_000_000_123,
            -86_400_000,
            9_999_999_999_999,
        ] {
            let text = iso8601(millis);
            assert_eq!(parse_iso8601(&text), Some(millis), "{text}");
        }
        assert_eq!(
            parse_iso8601("2023-11-14T22:13:20.123+02:00"),
            Some(1_699_992_800_123)
        );
        assert_eq!(parse_iso8601("1700000000123"), Some(1_700_000_000_123));
        assert_eq!(parse_iso8601("not a date"), None);
    }

    #[test]
    fn decimal128_decodes_known_words() {
        // Words derived from the BID layout: zero and one share the biased
        // exponent 6176 (0x1820) shifted into bits[126:113], Infinity uses
        // combination field 0x1E and NaN uses 0x1F.
        let cases: &[(u64, u64, &str)] = &[
            (0, 0x3040_0000_0000_0000, "0"),
            (1, 0x3040_0000_0000_0000, "1"),
            (1, 0xB040_0000_0000_0000, "-1"),
            (15, 0x303E_0000_0000_0000, "1.5"),
            (0, 0x7800_0000_0000_0000, "Infinity"),
            (0, 0xF800_0000_0000_0000, "-Infinity"),
            (0, 0x7C00_0000_0000_0000, "NaN"),
        ];
        for &(lo, hi, expected) in cases {
            assert_eq!(
                decimal128_to_string(lo, hi),
                expected,
                "lo={lo:#x} hi={hi:#x}"
            );
        }
        // The encoder must agree with the decoder on every one of these.
        for &(lo, hi, expected) in cases {
            let encoded = decimal128_from_str(expected).unwrap_or_else(|| panic!("{expected}"));
            assert_eq!(encoded, (lo, hi), "{expected} encodes to the known words");
        }
    }

    #[test]
    fn decimal128_storage_order_is_low_word_first() {
        // BSON writes the 128-bit value as two little-endian words, low first.
        let (lo, hi) = decimal128_from_str("1.5").unwrap();
        let mut bytes = Vec::with_capacity(16);
        bytes.extend_from_slice(&lo.to_le_bytes());
        bytes.extend_from_slice(&hi.to_le_bytes());
        assert_eq!(hex_encode(&bytes), "0f000000000000000000000000003e30");
    }

    #[test]
    fn decimal128_formats_exponents_per_spec() {
        fn enc(text: &str) -> (u64, u64) {
            decimal128_from_str(text).unwrap_or_else(|| panic!("{text} should encode"))
        }
        let roundtrip = |text: &str, expected: &str| {
            let (lo, hi) = enc(text);
            assert_eq!(decimal128_to_string(lo, hi), expected, "{text}");
        };
        roundtrip("0", "0");
        // decimal128 preserves the sign of zero, unlike a double.
        roundtrip("-0", "-0");
        roundtrip("1.5", "1.5");
        // Trailing zeros in the coefficient are significant.
        roundtrip("1.5000", "1.5000");
        roundtrip("1000", "1000");
        roundtrip("1e3", "1E+3");
        roundtrip("000123", "123");
        roundtrip("1E+30", "1E+30");
        roundtrip("0.000001", "0.000001");
        roundtrip("0.0000001", "1E-7");
        roundtrip("-42.25", "-42.25");
        roundtrip("Infinity", "Infinity");
        roundtrip("-Infinity", "-Infinity");
        roundtrip("NaN", "NaN");
        // Same numeric value, different encodings.
        assert_ne!(enc("1000"), enc("1E+3"));
    }

    #[test]
    fn decimal128_rejects_unrepresentable_input() {
        assert_eq!(decimal128_from_str(""), None);
        assert_eq!(decimal128_from_str("abc"), None);
        assert_eq!(decimal128_from_str("1.2.3"), None);
        // 35 significant digits cannot be stored exactly.
        assert_eq!(
            decimal128_from_str("12345678901234567890123456789012345"),
            None
        );
        // 35 digits that collapse to 34 once trailing zeros are removed can.
        assert!(decimal128_from_str("12345678901234567890123456789012340").is_some());
        assert_eq!(decimal128_from_str("1e9999"), None);
    }

    #[test]
    fn decimal128_largest_canonical_value_round_trips() {
        let text = "9999999999999999999999999999999999";
        let (lo, hi) = decimal128_from_str(text).unwrap();
        assert_eq!(decimal128_to_string(lo, hi), text);
    }
}
