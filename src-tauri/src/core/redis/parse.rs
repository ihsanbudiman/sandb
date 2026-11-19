//! Editor statements in redis-cli argument syntax. The argument bytes are sent as RESP values,
//! not passed through a shell or reconstructed from the displayed statement.

use crate::core::parse_error::ParseError;

#[derive(Debug, PartialEq, Eq)]
pub struct Statement {
    pub text: String,
    pub args: Vec<Vec<u8>>,
    pub line: u32,
    pub column: u32,
}

pub fn parse(text: &str) -> Result<Vec<Statement>, ParseError> {
    let mut cursor = Cursor {
        text,
        pos: 0,
        line: 1,
        column: 1,
    };
    let mut statements = Vec::new();
    loop {
        while cursor.peek().is_some_and(separator) {
            cursor.bump();
        }
        match cursor.peek() {
            None => return Ok(statements),
            Some('#') => {
                while cursor.peek().is_some_and(|ch| ch != '\n') {
                    cursor.bump();
                }
                continue;
            }
            _ => {}
        }

        let start = cursor.pos;
        let (line, column) = (cursor.line, cursor.column);
        let mut args = Vec::new();
        let end = loop {
            args.push(cursor.arg()?);
            let end = cursor.pos;
            while cursor.peek().is_some_and(horizontal_space) {
                cursor.bump();
            }
            if matches!(cursor.peek(), None | Some('\n')) {
                break end;
            }
        };
        statements.push(Statement {
            text: text[start..end].to_owned(),
            args,
            line,
            column,
        });
    }
}

fn horizontal_space(ch: char) -> bool {
    matches!(ch, ' ' | '\t' | '\r')
}

fn separator(ch: char) -> bool {
    horizontal_space(ch) || ch == '\n'
}

struct Cursor<'a> {
    text: &'a str,
    pos: usize,
    line: u32,
    column: u32,
}

impl Cursor<'_> {
    fn rest(&self) -> &str {
        &self.text[self.pos..]
    }

    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn bump(&mut self) -> Option<char> {
        let ch = self.peek()?;
        self.pos += ch.len_utf8();
        if ch == '\n' {
            self.line += 1;
            self.column = 1;
        } else {
            self.column += 1;
        }
        Some(ch)
    }

    fn arg(&mut self) -> Result<Vec<u8>, ParseError> {
        let Some(quote @ ('"' | '\'')) = self.peek() else {
            let start = self.pos;
            while self.peek().is_some_and(|ch| !separator(ch)) {
                self.bump();
            }
            return Ok(self.text.as_bytes()[start..self.pos].to_vec());
        };
        let (line, column) = (self.line, self.column);
        self.bump();
        let mut arg = Vec::new();
        while let Some(ch) = self.bump() {
            if ch == quote {
                if self.peek().is_some_and(|ch| !separator(ch)) {
                    return Err(ParseError {
                        message: "Closing quote must be followed by a space".into(),
                        line: self.line,
                        column: self.column,
                    });
                }
                return Ok(arg);
            }
            if ch == '\\' && quote == '"' {
                if let Some(escaped) = self.bump() {
                    self.escape(escaped, &mut arg);
                }
            } else if ch == '\\' && self.peek() == Some('\'') {
                self.bump();
                arg.push(b'\'');
            } else {
                arg.extend_from_slice(ch.encode_utf8(&mut [0; 4]).as_bytes());
            }
        }
        Err(ParseError {
            message: "Unclosed quote".into(),
            line,
            column,
        })
    }

    fn escape(&mut self, ch: char, arg: &mut Vec<u8>) {
        let byte = match ch {
            'n' => b'\n',
            'r' => b'\r',
            't' => b'\t',
            'b' => 8,
            'a' => 7,
            'x' => {
                if let [hi, lo, ..] = self.rest().as_bytes()
                    && let (Some(hi), Some(lo)) = (hex(*hi), hex(*lo))
                {
                    self.bump();
                    self.bump();
                    (hi << 4) | lo
                } else {
                    b'x'
                }
            }
            _ => {
                arg.extend_from_slice(ch.encode_utf8(&mut [0; 4]).as_bytes());
                return;
            }
        };
        arg.push(byte);
    }
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::redis::key::quote_arg;

    #[test]
    fn preserves_binary_and_redis_cli_escapes() {
        let text = r#"SET "\xff\x00\xFE" "\"\\\n\r\t\b\a\q\xz\é" '#a\'b\n' foo"bar #literal"#;
        let statements = parse(text).unwrap();
        let [stmt] = statements.as_slice() else {
            panic!("expected one statement");
        };
        assert_eq!(stmt.text, text);
        assert_eq!(
            stmt.args,
            [
                b"SET".as_slice(),
                b"\xff\0\xfe",
                b"\"\\\n\r\t\x08\x07qxz\xc3\xa9",
                b"#a'b\\n",
                b"foo\"bar",
                b"#literal",
            ]
        );
    }

    #[test]
    fn source_spans_comments_and_multiline_quotes() {
        let text =
            "\r\n\t # comment\r\n\t EVAL \"return '\nhello'\" 0 \r\n  GET \"\"\r\n  PING;PONG\n";
        let stmts = parse(text).unwrap();
        assert_eq!(stmts.len(), 3);
        assert_eq!(stmts[0].text, "EVAL \"return '\nhello'\" 0");
        assert_eq!(stmts[0].args[1], b"return '\nhello'");
        assert_eq!((stmts[0].line, stmts[0].column), (3, 3));
        assert_eq!(stmts[1].text, "GET \"\"");
        assert_eq!(stmts[1].args[1], b"");
        assert_eq!((stmts[1].line, stmts[1].column), (5, 3));
        assert_eq!(stmts[2].args[0], b"PING;PONG");
        assert!(parse("\r\n # only a comment").unwrap().is_empty());
    }

    #[test]
    fn reports_quote_errors_in_characters() {
        let unclosed = parse("PING\r\n  GET é 'key\n").unwrap_err();
        assert_eq!(unclosed.to_string(), "Line 2, column 9: Unclosed quote");
        let closing = parse("GET \"é\nx\"tail").unwrap_err();
        assert_eq!(
            closing.to_string(),
            "Line 2, column 3: Closing quote must be followed by a space"
        );
    }

    #[test]
    fn round_trips_quoted_bytes() {
        let all: Vec<u8> = (0..=u8::MAX).collect();
        let mut state = 0x374a_082du32;
        for len in 0..256 {
            let bytes: Vec<u8> = (0..len)
                .map(|_| {
                    state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    (state >> 24) as u8
                })
                .collect();
            assert_eq!(parse(&quote_arg(&bytes)).unwrap()[0].args, [bytes]);
        }
        for bytes in all.chunks(1).chain(std::iter::once(all.as_slice())) {
            assert_eq!(parse(&quote_arg(bytes)).unwrap()[0].args, [bytes]);
        }
    }
}
