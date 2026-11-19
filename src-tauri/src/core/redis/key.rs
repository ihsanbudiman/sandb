//! Canonical Redis key names for the catalog: one redis-cli argument, including for binary keys.

use std::borrow::Cow;

use crate::core::error::AppError;

fn bare(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'_' | b':'
                | b'.'
                | b'-'
                | b'+'
                | b'*'
                | b'/'
                | b'@'
                | b'{'
                | b'}'
                | b'['
                | b']'
                | b'$'
                | b'!'
                | b'?'
                | b'='
                | b'<'
                | b'>'
                | b'%'
                | b'^'
                | b'&'
                | b'|'
                | b'~'
                | b','
        )
}

pub(super) fn bare_arg(bytes: &[u8]) -> Option<&str> {
    if bytes.is_empty() || !bytes.iter().copied().all(bare) {
        None
    } else {
        Some(std::str::from_utf8(bytes).expect("bare bytes are ASCII"))
    }
}

pub fn quote_arg(bytes: &[u8]) -> String {
    if let Some(text) = bare_arg(bytes) {
        return text.to_owned();
    }
    let mut token = String::with_capacity(bytes.len() + 2);
    token.push('"');
    push_quoted_body(&mut token, bytes);
    token.push('"');
    token
}

pub(super) fn push_quoted_body(token: &mut String, bytes: &[u8]) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for &byte in bytes {
        match byte {
            b'\\' => token.push_str(r"\\"),
            b'"' => token.push_str(r#"\""#),
            b'\n' => token.push_str(r"\n"),
            b'\r' => token.push_str(r"\r"),
            b'\t' => token.push_str(r"\t"),
            0x20..=0x7e => token.push(char::from(byte)),
            _ => {
                token.push_str(r"\x");
                token.push(char::from(HEX[usize::from(byte >> 4)]));
                token.push(char::from(HEX[usize::from(byte & 0xf)]));
            }
        }
    }
}

/// Decode the single key token sent back by a catalog action, not a Redis glob or command line.
pub(super) fn parse_arg(token: &str) -> Result<Cow<'_, [u8]>, AppError> {
    let invalid = || AppError::invalid("Expected a Redis key name in redis-cli syntax");
    let bytes = token.as_bytes();
    if !bytes.is_empty() && bytes.iter().copied().all(bare) {
        return Ok(Cow::Borrowed(bytes));
    }
    let Some(body) = token.strip_prefix('"').and_then(|s| s.strip_suffix('"')) else {
        return Err(invalid());
    };
    let body = body.as_bytes();
    let Some(start) = body.iter().position(|&b| !literal(b)) else {
        return Ok(Cow::Borrowed(body));
    };
    let mut decoded = Vec::with_capacity(body.len());
    decoded.extend_from_slice(&body[..start]);
    let mut rest = body[start..].iter().copied();
    while let Some(byte) = rest.next() {
        decoded.push(match byte {
            b'\\' => match rest.next() {
                Some(b'\\') => b'\\',
                Some(b'"') => b'"',
                Some(b'n') => b'\n',
                Some(b'r') => b'\r',
                Some(b't') => b'\t',
                Some(b'x') => {
                    let high = rest.next().and_then(hex_digit).ok_or_else(invalid)?;
                    let low = rest.next().and_then(hex_digit).ok_or_else(invalid)?;
                    high << 4 | low
                }
                _ => return Err(invalid()),
            },
            byte if literal(byte) => byte,
            _ => return Err(invalid()),
        });
    }
    Ok(Cow::Owned(decoded))
}

fn literal(byte: u8) -> bool {
    matches!(byte, 0x20..=0x7e) && !matches!(byte, b'"' | b'\\')
}

fn hex_digit(byte: u8) -> Option<u8> {
    char::from(byte).to_digit(16).map(|digit| digit as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_tokens_round_trip_all_bytes() {
        let all = (0..=u8::MAX).collect::<Vec<_>>();
        for bytes in all.iter().map(std::slice::from_ref).chain([all.as_slice()]) {
            assert_eq!(parse_arg(&quote_arg(bytes)).unwrap().as_ref(), bytes);
        }
        for (bytes, token) in [
            (&b""[..], r#""""#),
            (b"bin:1", "bin:1"),
            (b"#comment", "\"#comment\""),
            (b"key with spaces", r#""key with spaces""#),
            (b"a\r\n\tb", r#""a\r\n\tb""#),
            (b"\xff\0", r#""\xff\x00""#),
            (br"\xff\x00", r#""\\xff\\x00""#),
            (br#""bin:1""#, r#""\"bin:1\"""#),
            ("café".as_bytes(), r#""caf\xc3\xa9""#),
        ] {
            assert_eq!(quote_arg(bytes), token);
            assert_eq!(parse_arg(token).unwrap().as_ref(), bytes);
        }
    }

    #[test]
    fn rejects_malformed_key_tokens() {
        for token in [
            "",
            "\"",
            "a b",
            "#comment",
            "\"a\" b",
            r#""\x0""#,
            r#""\x0g""#,
            r#""\""#,
        ] {
            assert!(parse_arg(token).is_err(), "accepted {token:?}");
        }
    }
}
