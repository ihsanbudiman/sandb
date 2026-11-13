//! Parser for the mongosh subset that the editor accepts. It never runs JavaScript: arguments are
//! JavaScript object literals as mongosh accepts them (unquoted keys, single quotes, trailing
//! commas, comments, `ObjectId(…)`, `ISODate(…)`, `/regex/`, …), read straight into BSON. Text
//! outside the subset gets an error with its line and column.

use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use mongodb::bson::oid::ObjectId;
use mongodb::bson::spec::BinarySubtype;
use mongodb::bson::{Binary, Bson, DateTime, Decimal128, Document, Regex, Timestamp};

use crate::core::error::AppError;
use crate::core::mongo::options::whole_number;
use crate::core::mongo::command::{Command, Find, Method, Statement};

/// A parse failure. `line` and `column` are 1-based; the column counts characters.
#[derive(Debug, Clone, PartialEq)]
pub struct ParseError {
    pub message: String,
    pub line: u32,
    pub column: u32,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Line {}, column {}: {}",
            self.line, self.column, self.message
        )
    }
}

impl From<ParseError> for AppError {
    fn from(e: ParseError) -> Self {
        AppError::invalid(e.to_string())
    }
}

/// All statements of the editor text, in order. Text with only whitespace and comments gives an
/// empty list.
pub fn parse(text: &str) -> Result<Vec<Statement>, ParseError> {
    let toks = Lexer::new(text).run()?;
    let mut p = Parser {
        src: text,
        toks,
        pos: 0,
    };
    let mut out = Vec::new();
    loop {
        while p.eat_punct(';') {}
        if p.at_end() {
            return Ok(out);
        }
        let start = p.peek().start;
        let command = p.statement()?;
        let end = p.toks[p.pos - 1].end;
        // A statement ends at `;`, at the end of the text or at a line break.
        let next = p.peek();
        if !matches!(next.tok, Tok::Eof | Tok::Punct(';')) && !next.newline_before {
            return Err(p.unexpected(next, "';' or a new line"));
        }
        out.push(Statement {
            text: text[start..end].to_string(),
            command,
        });
    }
}

/// The collection methods the parser accepts, in the order the editor offers them. Completion
/// reads this list, so it cannot offer a method the parser rejects.
pub const COLLECTION_METHODS: &[&str] = &[
    "find",
    "findOne",
    "aggregate",
    "countDocuments",
    "estimatedDocumentCount",
    "distinct",
    "getIndexes",
    "stats",
    "insertOne",
    "insertMany",
    "updateOne",
    "updateMany",
    "replaceOne",
    "deleteOne",
    "deleteMany",
    "findOneAndUpdate",
    "findOneAndReplace",
    "findOneAndDelete",
    "bulkWrite",
    "createIndex",
    "createIndexes",
    "dropIndex",
    "dropIndexes",
    "drop",
    "renameCollection",
];

/// The cursor methods of a `find(...)` chain. The parser also takes `projection` for `project`
/// and `pretty` as a no-op, which are not offered.
pub const CURSOR_METHODS: &[&str] = &[
    "sort",
    "project",
    "collation",
    "limit",
    "skip",
    "hint",
    "maxTimeMS",
    "count",
    "toArray",
];

/// The methods of `db` itself; any other name after `db.` is a collection.
pub const DB_METHODS: &[&str] = &[
    "getCollection",
    "runCommand",
    "adminCommand",
    "createCollection",
    "dropDatabase",
];

/// One value literal and nothing after it, such as the mongosh text of a grid cell.
pub fn parse_value(text: &str) -> Result<Bson, ParseError> {
    let toks = Lexer::new(text).run()?;
    let mut p = Parser {
        src: text,
        toks,
        pos: 0,
    };
    let value = p.value()?;
    if !p.at_end() {
        return Err(p.unexpected(p.peek(), "the end of the value"));
    }
    Ok(value)
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Ident(String),
    Str(String),
    /// The number as written, without a sign.
    Num(String),
    Regex(String, String),
    Punct(char),
    Arrow,
    Eof,
}

#[derive(Debug, Clone)]
struct Token {
    tok: Tok,
    /// Byte range in the source.
    start: usize,
    end: usize,
    /// A line break comes between this token and the one before.
    newline_before: bool,
}

/// Words of JavaScript that never belong in a mongosh query.
const JS_WORDS: &[&str] = &[
    "function",
    "var",
    "let",
    "const",
    "return",
    "if",
    "else",
    "for",
    "while",
    "do",
    "switch",
    "case",
    "break",
    "continue",
    "class",
    "this",
    "typeof",
    "instanceof",
    "delete",
    "throw",
    "try",
    "catch",
    "finally",
    "async",
    "await",
    "yield",
    "import",
    "export",
    "print",
    "printjson",
];

fn position(src: &str, at: usize) -> (u32, u32) {
    let at = at.min(src.len());
    let before = &src[..at];
    let line = before.matches('\n').count() as u32 + 1;
    let line_start = before.rfind('\n').map_or(0, |i| i + 1);
    let column = before[line_start..].chars().count() as u32 + 1;
    (line, column)
}

fn error_at(src: &str, at: usize, message: impl Into<String>) -> ParseError {
    let (line, column) = position(src, at);
    ParseError {
        message: message.into(),
        line,
        column,
    }
}

struct Lexer<'a> {
    src: &'a str,
    b: &'a [u8],
    i: usize,
    out: Vec<Token>,
    newline: bool,
}

impl<'a> Lexer<'a> {
    fn new(src: &'a str) -> Self {
        Lexer {
            src,
            b: src.as_bytes(),
            i: 0,
            out: Vec::new(),
            newline: false,
        }
    }

    fn err(&self, at: usize, message: impl Into<String>) -> ParseError {
        error_at(self.src, at, message)
    }

    fn push(&mut self, tok: Tok, start: usize) {
        self.out.push(Token {
            tok,
            start,
            end: self.i,
            newline_before: self.newline,
        });
        self.newline = false;
    }

    /// A `/` starts a regex only where a value can start; in this grammar that is after one of
    /// these marks (or at the start of the text).
    fn regex_allowed(&self) -> bool {
        match self.out.last() {
            None => true,
            Some(t) => matches!(t.tok, Tok::Punct('(' | ',' | ':' | '[' | '{' | ';')),
        }
    }

    fn run(mut self) -> Result<Vec<Token>, ParseError> {
        while self.i < self.b.len() {
            let c = self.b[self.i];
            let start = self.i;
            match c {
                b'\n' => {
                    self.newline = true;
                    self.i += 1;
                }
                b' ' | b'\t' | b'\r' => self.i += 1,
                b'/' if self.b.get(self.i + 1) == Some(&b'/') => {
                    while self.i < self.b.len() && self.b[self.i] != b'\n' {
                        self.i += 1;
                    }
                }
                b'/' if self.b.get(self.i + 1) == Some(&b'*') => {
                    let Some(e) = self.src[self.i + 2..].find("*/") else {
                        return Err(self.err(start, "Unterminated comment"));
                    };
                    if self.src[self.i..self.i + 2 + e].contains('\n') {
                        self.newline = true;
                    }
                    self.i += e + 4;
                }
                b'/' if self.regex_allowed() => {
                    let tok = self.regex()?;
                    self.push(tok, start);
                }
                b'"' | b'\'' | b'`' => {
                    let s = self.string(c)?;
                    self.push(Tok::Str(s), start);
                }
                b'0'..=b'9' => {
                    self.number();
                    let raw = self.src[start..self.i].to_string();
                    self.push(Tok::Num(raw), start);
                }
                b'.' if self.b.get(self.i + 1).is_some_and(u8::is_ascii_digit) => {
                    self.number();
                    let raw = self.src[start..self.i].to_string();
                    self.push(Tok::Num(raw), start);
                }
                b'=' if self.b.get(self.i + 1) == Some(&b'>') => {
                    self.i += 2;
                    self.push(Tok::Arrow, start);
                }
                c if c == b'_' || c == b'$' || c.is_ascii_alphabetic() || !c.is_ascii() => {
                    // JavaScript names may hold any Unicode letter: `{ café: 1 }` is valid.
                    let word_len = self.src[start..]
                        .char_indices()
                        .find(|&(_, ch)| !(ch == '_' || ch == '$' || ch.is_alphanumeric()))
                        .map_or(self.src.len() - start, |(n, _)| n);
                    if word_len == 0 {
                        let ch = self.src[start..].chars().next().unwrap_or('?');
                        return Err(self.err(start, format!("Unexpected character '{ch}'")));
                    }
                    self.i += word_len;
                    let word = self.src[start..self.i].to_string();
                    self.push(Tok::Ident(word), start);
                }
                c if c.is_ascii_punctuation() => {
                    self.i += 1;
                    self.push(Tok::Punct(c as char), start);
                }
                _ => {
                    let ch = self.src[start..].chars().next().unwrap_or('?');
                    return Err(self.err(start, format!("Unexpected character '{ch}'")));
                }
            }
        }
        let end = self.i;
        self.out.push(Token {
            tok: Tok::Eof,
            start: end,
            end,
            newline_before: true,
        });
        Ok(self.out)
    }

    fn number(&mut self) {
        let b = self.b;
        if b[self.i] == b'0' && matches!(b.get(self.i + 1), Some(b'x' | b'X')) {
            self.i += 2;
            while self.i < b.len() && b[self.i].is_ascii_hexdigit() {
                self.i += 1;
            }
            return;
        }
        while self.i < b.len() && (b[self.i].is_ascii_digit() || b[self.i] == b'.') {
            self.i += 1;
        }
        if matches!(b.get(self.i), Some(b'e' | b'E')) {
            self.i += 1;
            if matches!(b.get(self.i), Some(b'+' | b'-')) {
                self.i += 1;
            }
            while self.i < b.len() && b[self.i].is_ascii_digit() {
                self.i += 1;
            }
        }
    }

    fn string(&mut self, quote: u8) -> Result<String, ParseError> {
        let open = self.i;
        self.i += 1;
        let mut out = String::new();
        loop {
            let Some(ch) = self.src[self.i..].chars().next() else {
                return Err(self.err(open, "Unterminated string"));
            };
            match ch {
                c if c as u32 == u32::from(quote) => {
                    self.i += 1;
                    return Ok(out);
                }
                '\n' if quote != b'`' => return Err(self.err(open, "Unterminated string")),
                '$' if quote == b'`' && self.b.get(self.i + 1) == Some(&b'{') => {
                    return Err(self.err(self.i, "Template placeholders (${…}) are not supported"));
                }
                '\\' => {
                    self.i += 1;
                    self.escape(&mut out)?;
                }
                c => {
                    out.push(c);
                    self.i += c.len_utf8();
                }
            }
        }
    }

    /// One escape after a `\` (already skipped).
    fn escape(&mut self, out: &mut String) -> Result<(), ParseError> {
        let at = self.i - 1;
        let Some(c) = self.src[self.i..].chars().next() else {
            return Err(self.err(at, "Unterminated string"));
        };
        self.i += c.len_utf8();
        match c {
            'n' => out.push('\n'),
            't' => out.push('\t'),
            'r' => out.push('\r'),
            'b' => out.push('\u{8}'),
            'f' => out.push('\u{c}'),
            'v' => out.push('\u{b}'),
            '0' => out.push('\0'),
            // A line continuation adds nothing.
            '\n' => {}
            '\r' => {
                if self.b.get(self.i) == Some(&b'\n') {
                    self.i += 1;
                }
            }
            'x' => {
                let v = self.hex_digits(2, at)?;
                out.push(char::from_u32(v).ok_or_else(|| self.err(at, "Invalid escape"))?);
            }
            'u' => {
                let v = if self.b.get(self.i) == Some(&b'{') {
                    self.i += 1;
                    let close = self.src[self.i..]
                        .find('}')
                        .ok_or_else(|| self.err(at, "Invalid \\u{…} escape"))?;
                    let digits = &self.src[self.i..self.i + close];
                    self.i += close + 1;
                    u32::from_str_radix(digits, 16).map_err(|_| self.err(at, "Invalid escape"))?
                } else {
                    let hi = self.hex_digits(4, at)?;
                    // A UTF-16 surrogate pair is two escapes: `\uD83D\uDE00`.
                    if (0xD800..0xDC00).contains(&hi) && self.src[self.i..].starts_with("\\u") {
                        self.i += 2;
                        let lo = self.hex_digits(4, at)?;
                        if !(0xDC00..0xE000).contains(&lo) {
                            return Err(self.err(at, "Invalid surrogate pair"));
                        }
                        0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                    } else {
                        hi
                    }
                };
                out.push(char::from_u32(v).ok_or_else(|| self.err(at, "Invalid escape"))?);
            }
            other => out.push(other),
        }
        Ok(())
    }

    fn hex_digits(&mut self, n: usize, at: usize) -> Result<u32, ParseError> {
        let digits = self
            .src
            .get(self.i..self.i + n)
            .filter(|d| d.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or_else(|| self.err(at, "Invalid escape"))?;
        self.i += n;
        u32::from_str_radix(digits, 16).map_err(|_| self.err(at, "Invalid escape"))
    }

    /// `/pattern/flags`; the pattern keeps its escapes, as the server reads them.
    fn regex(&mut self) -> Result<Tok, ParseError> {
        let open = self.i;
        self.i += 1;
        let mut class = false;
        loop {
            let Some(ch) = self.src[self.i..].chars().next() else {
                return Err(self.err(open, "Unterminated regular expression"));
            };
            match ch {
                '\n' => return Err(self.err(open, "Unterminated regular expression")),
                '\\' => {
                    self.i += 1;
                    if let Some(next) = self.src[self.i..].chars().next() {
                        self.i += next.len_utf8();
                    }
                }
                '[' => {
                    class = true;
                    self.i += 1;
                }
                ']' => {
                    class = false;
                    self.i += 1;
                }
                '/' if !class => break,
                c => self.i += c.len_utf8(),
            }
        }
        let pattern = self.src[open + 1..self.i].to_string();
        self.i += 1;
        let flags_start = self.i;
        while self.i < self.b.len() && self.b[self.i].is_ascii_alphabetic() {
            self.i += 1;
        }
        let mut flags: Vec<char> = self.src[flags_start..self.i].chars().collect();
        if let Some(bad) = flags.iter().find(|c| !"imsux".contains(**c)) {
            return Err(self.err(
                flags_start,
                format!("Unsupported regular expression flag: {bad}"),
            ));
        }
        flags.sort_unstable();
        flags.dedup();
        Ok(Tok::Regex(pattern, flags.into_iter().collect()))
    }
}

/// A parsed argument and the byte offset where it starts, for error positions.
type Arg = (Bson, usize);

struct Parser<'a> {
    src: &'a str,
    toks: Vec<Token>,
    pos: usize,
}

fn describe(t: &Tok) -> String {
    match t {
        Tok::Ident(s) => format!("'{s}'"),
        Tok::Str(_) => "a string".into(),
        Tok::Num(s) => format!("'{s}'"),
        Tok::Regex(..) => "a regular expression".into(),
        Tok::Punct(c) => format!("'{c}'"),
        Tok::Arrow => "'=>'".into(),
        Tok::Eof => "the end of the text".into(),
    }
}

fn js_error_word(t: &Tok) -> Option<&str> {
    match t {
        Tok::Arrow => Some("=>"),
        Tok::Ident(w) if JS_WORDS.contains(&w.as_str()) => Some(w),
        _ => None,
    }
}

impl Parser<'_> {
    fn peek(&self) -> &Token {
        &self.toks[self.pos]
    }

    fn at_end(&self) -> bool {
        matches!(self.peek().tok, Tok::Eof)
    }

    fn next(&mut self) -> Token {
        let t = self.toks[self.pos].clone();
        if !matches!(t.tok, Tok::Eof) {
            self.pos += 1;
        }
        t
    }

    fn err(&self, at: usize, message: impl Into<String>) -> ParseError {
        error_at(self.src, at, message)
    }

    fn unexpected(&self, t: &Token, expected: &str) -> ParseError {
        if let Some(w) = js_error_word(&t.tok) {
            return self.err(
                t.start,
                format!("JavaScript is not supported: '{w}'. Only mongosh query syntax is allowed"),
            );
        }
        self.err(
            t.start,
            format!("Expected {expected}, found {}", describe(&t.tok)),
        )
    }

    fn at_punct(&self, c: char) -> bool {
        self.peek().tok == Tok::Punct(c)
    }

    fn eat_punct(&mut self, c: char) -> bool {
        if self.at_punct(c) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expect_punct(&mut self, c: char) -> Result<(), ParseError> {
        if self.eat_punct(c) {
            Ok(())
        } else {
            Err(self.unexpected(self.peek(), &format!("'{c}'")))
        }
    }

    /// A property name: JavaScript allows reserved words there (`db.delete`, `{ let: … }`).
    fn expect_ident(&mut self, what: &str) -> Result<(String, usize), ParseError> {
        let t = self.next();
        match t.tok {
            Tok::Ident(s) => Ok((s, t.start)),
            _ => Err(self.unexpected(&t, what)),
        }
    }

    fn statement(&mut self) -> Result<Command, ParseError> {
        let t = self.peek().clone();
        match &t.tok {
            Tok::Ident(w) if w == "show" => {
                self.next();
                let (what, at) = self.expect_ident("dbs or collections")?;
                match what.as_str() {
                    "dbs" | "databases" => Ok(Command::ShowDbs),
                    "collections" | "tables" => Ok(Command::ShowCollections),
                    _ => Err(self.err(
                        at,
                        format!("Unknown show command: {what}. Use show dbs or show collections"),
                    )),
                }
            }
            Tok::Ident(w) if w == "use" => self.use_db(t.end),
            Tok::Ident(w) if w == "db" => self.db_statement(),
            _ => Err(self.unexpected(&t, "a statement that starts with db., show or use")),
        }
    }

    /// `use <name>`: the name is the raw rest of the line, so names with `-` work.
    fn use_db(&mut self, after: usize) -> Result<Command, ParseError> {
        self.next();
        let rest = &self.src[after..];
        let len = rest.find(['\n', ';']).unwrap_or(rest.len());
        let name = rest[..len].trim();
        let name_at = after + (rest.len() - rest.trim_start().len());
        if name.is_empty() {
            return Err(self.err(name_at, "use needs a database name"));
        }
        if name
            .chars()
            .any(|c| c.is_whitespace() || "/\\.\"$*<>:|?'`".contains(c))
        {
            return Err(self.err(name_at, format!("Invalid database name: {name}")));
        }
        let end = after + len;
        while !self.at_end() && self.peek().start < end {
            self.next();
        }
        Ok(Command::Use(name.to_string()))
    }

    fn db_statement(&mut self) -> Result<Command, ParseError> {
        self.next(); // db
        self.expect_punct('.')?;
        let (name, at) = self.expect_ident("a collection or a db method")?;
        if self.at_punct('(') {
            return self.db_method(&name, at);
        }
        // `db.system.profile.find()`: name parts up to the one called as a method.
        let mut collection = name;
        loop {
            self.expect_punct('.')?;
            let (part, part_at) = self.expect_ident("a method name")?;
            if self.at_punct('(') {
                return self.collection_call(collection, part, part_at);
            }
            collection.push('.');
            collection.push_str(&part);
        }
    }

    fn db_method(&mut self, name: &str, at: usize) -> Result<Command, ParseError> {
        let args = self.args()?;
        match name {
            "getCollection" => {
                let [(Bson::String(coll), _)] = args.as_slice() else {
                    return Err(self.err(at, "getCollection needs a collection name"));
                };
                let coll = coll.clone();
                self.expect_punct('.')?;
                let (method, method_at) = self.expect_ident("a method name")?;
                if !self.at_punct('(') {
                    return Err(self.unexpected(self.peek(), "'('"));
                }
                self.collection_call(coll, method, method_at)
            }
            "runCommand" | "adminCommand" => match args.as_slice() {
                [(Bson::Document(d), _)] if !d.is_empty() => Ok(Command::RunCommand {
                    admin: name == "adminCommand",
                    command: d.clone(),
                }),
                // mongosh's string form: db.runCommand("ping") is { ping: 1 }.
                [(Bson::String(s), _)] if !s.is_empty() => {
                    let mut d = Document::new();
                    d.insert(s.clone(), 1);
                    Ok(Command::RunCommand {
                        admin: name == "adminCommand",
                        command: d,
                    })
                }
                _ => Err(self.err(at, format!("{name} needs a command document"))),
            },
            "dropDatabase" => match args.as_slice() {
                [] => Ok(Command::DropDatabase { options: None }),
                [(Bson::Document(o), _)] => Ok(Command::DropDatabase {
                    options: Some(o.clone()),
                }),
                _ => Err(self.err(at, "dropDatabase needs an optional options document")),
            },
            "createCollection" => match args.as_slice() {
                [(Bson::String(n), _)] => Ok(Command::CreateCollection {
                    name: n.clone(),
                    options: None,
                }),
                [(Bson::String(n), _), (Bson::Document(o), _)] => Ok(Command::CreateCollection {
                    name: n.clone(),
                    options: Some(o.clone()),
                }),
                _ => Err(self.err(
                    at,
                    "createCollection needs a name and an optional options document",
                )),
            },
            _ => Err(self.err(at, format!("Unknown db method: {name}"))),
        }
    }

    fn collection_call(
        &mut self,
        collection: String,
        name: String,
        at: usize,
    ) -> Result<Command, ParseError> {
        let args = self.args()?;
        let mut method = self.method(&name, at, args)?;
        while self.at_punct('.') {
            self.next();
            let (chain, chain_at) = self.expect_ident("a cursor method")?;
            let args = self.args_after(&chain, chain_at)?;
            self.chain(&mut method, &chain, chain_at, args)?;
        }
        Ok(Command::Collection { collection, method })
    }

    fn args_after(&mut self, name: &str, at: usize) -> Result<Vec<Arg>, ParseError> {
        if !self.at_punct('(') {
            return Err(self.err(at, format!("Expected '(' after {name}")));
        }
        self.args()
    }

    fn args(&mut self) -> Result<Vec<Arg>, ParseError> {
        self.expect_punct('(')?;
        let mut out = Vec::new();
        loop {
            if self.eat_punct(')') {
                return Ok(out);
            }
            let at = self.peek().start;
            out.push((self.value()?, at));
            if self.eat_punct(',') {
                continue;
            }
            if self.eat_punct(')') {
                return Ok(out);
            }
            return Err(self.unexpected(self.peek(), "',' or ')'"));
        }
    }

    fn method(&self, name: &str, at: usize, args: Vec<Arg>) -> Result<Method, ParseError> {
        let usage = |u: &str| self.err(at, format!("Usage: {name}({u})"));
        let mut a = args.into_iter();
        let mut take = || a.next();
        let m = match name {
            "find" | "findOne" => {
                let filter = self.opt_doc(take(), name)?.unwrap_or_default();
                let projection = self.opt_doc(take(), name)?;
                if take().is_some() {
                    return Err(usage("filter?, projection?"));
                }
                if name == "find" {
                    Method::Find(Find {
                        filter,
                        projection,
                        ..Find::default()
                    })
                } else {
                    Method::FindOne { filter, projection }
                }
            }
            "aggregate" => {
                let Some(p) = take() else {
                    return Err(usage("pipeline, options?"));
                };
                let pipeline = self.docs(p, "aggregate needs an array of stages")?;
                let options = self.opt_doc(take(), name)?;
                if take().is_some() {
                    return Err(usage("pipeline, options?"));
                }
                Method::Aggregate { pipeline, options }
            }
            "countDocuments" => {
                let filter = self.opt_doc(take(), name)?.unwrap_or_default();
                let options = self.opt_doc(take(), name)?;
                if take().is_some() {
                    return Err(usage("filter?, options?"));
                }
                Method::CountDocuments { filter, options }
            }
            "estimatedDocumentCount" => {
                // Options such as maxTimeMS are accepted and not used.
                self.opt_doc(take(), name)?;
                if take().is_some() {
                    return Err(usage("options?"));
                }
                Method::EstimatedDocumentCount
            }
            "distinct" => {
                let Some((Bson::String(field), _)) = take() else {
                    return Err(usage("field, filter?"));
                };
                let filter = self.opt_doc(take(), name)?.unwrap_or_default();
                if take().is_some() {
                    return Err(usage("field, filter?"));
                }
                Method::Distinct { field, filter }
            }
            "getIndexes" | "stats" | "dropIndexes" | "drop" => {
                if take().is_some() {
                    return Err(usage(""));
                }
                match name {
                    "getIndexes" => Method::GetIndexes,
                    "stats" => Method::Stats,
                    "dropIndexes" => Method::DropIndexes,
                    _ => Method::Drop,
                }
            }
            "insertOne" => {
                let (Some(d), None) = (take(), take()) else {
                    return Err(usage("document"));
                };
                Method::InsertOne {
                    document: self.doc(d, "insertOne needs a document")?,
                }
            }
            "insertMany" => {
                let Some(d) = take() else {
                    return Err(usage("documents, options?"));
                };
                let documents = self.docs(d, "insertMany needs an array of documents")?;
                let options = self.opt_doc(take(), name)?;
                if take().is_some() {
                    return Err(usage("documents, options?"));
                }
                Method::InsertMany { documents, options }
            }
            "updateOne" | "updateMany" | "findOneAndUpdate" => {
                let (Some(f), Some(u)) = (take(), take()) else {
                    return Err(usage("filter, update, options?"));
                };
                let filter = self.doc(f, &format!("{name} needs a filter document"))?;
                let update = self.update(u, name)?;
                let options = self.opt_doc(take(), name)?;
                if take().is_some() {
                    return Err(usage("filter, update, options?"));
                }
                match name {
                    "updateOne" => Method::UpdateOne {
                        filter,
                        update,
                        options,
                    },
                    "updateMany" => Method::UpdateMany {
                        filter,
                        update,
                        options,
                    },
                    _ => Method::FindOneAndUpdate {
                        filter,
                        update,
                        options,
                    },
                }
            }
            "replaceOne" | "findOneAndReplace" => {
                let (Some(f), Some(r)) = (take(), take()) else {
                    return Err(usage("filter, replacement, options?"));
                };
                let filter = self.doc(f, &format!("{name} needs a filter document"))?;
                let replacement = self.doc(r, &format!("{name} needs a replacement document"))?;
                let options = self.opt_doc(take(), name)?;
                if take().is_some() {
                    return Err(usage("filter, replacement, options?"));
                }
                if name == "replaceOne" {
                    Method::ReplaceOne {
                        filter,
                        replacement,
                        options,
                    }
                } else {
                    Method::FindOneAndReplace {
                        filter,
                        replacement,
                        options,
                    }
                }
            }
            "deleteOne" | "deleteMany" => {
                let (Some(f), None) = (take(), take()) else {
                    return Err(usage("filter"));
                };
                let filter = self.doc(f, &format!("{name} needs a filter document"))?;
                if name == "deleteOne" {
                    Method::DeleteOne { filter }
                } else {
                    Method::DeleteMany { filter }
                }
            }
            "findOneAndDelete" => {
                let Some(f) = take() else {
                    return Err(usage("filter, options?"));
                };
                let filter = self.doc(f, "findOneAndDelete needs a filter document")?;
                let options = self.opt_doc(take(), name)?;
                if take().is_some() {
                    return Err(usage("filter, options?"));
                }
                Method::FindOneAndDelete { filter, options }
            }
            "bulkWrite" => {
                let Some(o) = take() else {
                    return Err(usage("operations, options?"));
                };
                let operations = self.docs(o, "bulkWrite needs an array of operations")?;
                let options = self.opt_doc(take(), name)?;
                if take().is_some() {
                    return Err(usage("operations, options?"));
                }
                Method::BulkWrite {
                    operations,
                    options,
                }
            }
            "createIndex" => {
                let Some(k) = take() else {
                    return Err(usage("keys, options?"));
                };
                let keys = self.doc(k, "createIndex needs a key document")?;
                let options = self.opt_doc(take(), name)?;
                if take().is_some() {
                    return Err(usage("keys, options?"));
                }
                Method::CreateIndex { keys, options }
            }
            "createIndexes" => {
                let Some(k) = take() else {
                    return Err(usage("keyDocuments, options?"));
                };
                let keys = self.docs(k, "createIndexes needs an array of key documents")?;
                let options = self.opt_doc(take(), name)?;
                if take().is_some() {
                    return Err(usage("keyDocuments, options?"));
                }
                Method::CreateIndexes { keys, options }
            }
            "dropIndex" => match (take(), take()) {
                (Some((index @ (Bson::String(_) | Bson::Document(_)), _)), None) => {
                    Method::DropIndex { index }
                }
                _ => return Err(usage("indexName or keyDocument")),
            },
            "renameCollection" => {
                let Some((Bson::String(to), _)) = take() else {
                    return Err(usage("newName, dropTarget?"));
                };
                let drop_target = match take() {
                    None => false,
                    Some((Bson::Boolean(b), _)) => b,
                    Some((_, at)) => {
                        return Err(
                            self.err(at, "renameCollection: dropTarget must be true or false")
                        );
                    }
                };
                if take().is_some() {
                    return Err(usage("newName, dropTarget?"));
                }
                Method::RenameCollection { to, drop_target }
            }
            _ => return Err(self.err(at, format!("Unknown method: {name}"))),
        };
        Ok(m)
    }

    fn chain(
        &self,
        method: &mut Method,
        name: &str,
        at: usize,
        args: Vec<Arg>,
    ) -> Result<(), ParseError> {
        // Accepted for pasted mongosh code; the result is shown the same way.
        if matches!(name, "toArray" | "pretty")
            && matches!(method, Method::Find(_) | Method::Aggregate { .. })
        {
            if !args.is_empty() {
                return Err(self.err(at, format!("Usage: {name}()")));
            }
            return Ok(());
        }
        let Method::Find(find) = method else {
            return Err(self.err(at, format!(".{name}() only follows find()")));
        };
        if find.count {
            return Err(self.err(at, format!(".{name}() cannot follow count()")));
        }
        let mut a = args.into_iter();
        let (one, extra) = (a.next(), a.next());
        if extra.is_some() || (one.is_none() && name != "count") {
            return Err(self.err(at, format!(".{name}() takes one argument")));
        }
        match name {
            "sort" => find.sort = Some(self.doc(one.unwrap_or_default(), "sort needs a document")?),
            "project" | "projection" => {
                find.projection =
                    Some(self.doc(one.unwrap_or_default(), "project needs a document")?)
            }
            "collation" => {
                find.collation =
                    Some(self.doc(one.unwrap_or_default(), "collation needs a document")?)
            }
            "limit" => find.limit = Some(self.count_arg(one, "limit", at)?),
            "skip" => {
                let n = self.count_arg(one, "skip", at)?;
                find.skip = Some(n as u64);
            }
            "hint" => match one {
                Some((h @ (Bson::String(_) | Bson::Document(_)), _)) => find.hint = Some(h),
                _ => return Err(self.err(at, "hint needs an index name or a key document")),
            },
            "maxTimeMS" => find.max_time_ms = Some(self.count_arg(one, "maxTimeMS", at)? as u64),
            "count" => {
                if one.is_some() {
                    return Err(self.err(at, "Usage: count()"));
                }
                find.count = true;
            }
            _ => return Err(self.err(at, format!("Unknown cursor method: {name}"))),
        }
        Ok(())
    }

    fn doc(&self, (v, at): Arg, message: &str) -> Result<Document, ParseError> {
        match v {
            Bson::Document(d) => Ok(d),
            _ => Err(self.err(at, message)),
        }
    }

    fn opt_doc(&self, a: Option<Arg>, name: &str) -> Result<Option<Document>, ParseError> {
        a.map(|a| self.doc(a, &format!("{name}: expected a document")))
            .transpose()
    }

    fn docs(&self, (v, at): Arg, message: &str) -> Result<Vec<Document>, ParseError> {
        let Bson::Array(items) = v else {
            return Err(self.err(at, message));
        };
        items
            .into_iter()
            .map(|i| match i {
                Bson::Document(d) => Ok(d),
                _ => Err(self.err(at, message)),
            })
            .collect()
    }

    /// An update is an operator document or an array of pipeline stages.
    fn update(&self, (v, at): Arg, name: &str) -> Result<Bson, ParseError> {
        let ok = match &v {
            Bson::Document(_) => true,
            Bson::Array(items) => items.iter().all(|i| matches!(i, Bson::Document(_))),
            _ => false,
        };
        if ok {
            Ok(v)
        } else {
            Err(self.err(
                at,
                format!("{name} needs an update document or a pipeline array"),
            ))
        }
    }

    /// A whole count of a cursor method. A missing argument points at the method name, not at
    /// the start of the text.
    fn count_arg(&self, a: Option<Arg>, name: &str, call: usize) -> Result<i64, ParseError> {
        let Some((v, at)) = a else {
            return Err(self.err(call, format!("{name} needs a number")));
        };
        let Some(n) = whole_number(&v, i64::MIN, i64::MAX) else {
            return Err(self.err(at, format!("{name} needs a whole number")));
        };
        if n < 0 {
            return Err(self.err(at, format!("{name} cannot be negative")));
        }
        Ok(n)
    }

    fn value(&mut self) -> Result<Bson, ParseError> {
        let t = self.next();
        if js_error_word(&t.tok).is_some() {
            return Err(self.unexpected(&t, "a value"));
        }
        match t.tok {
            Tok::Punct('{') => self.object(),
            Tok::Punct('[') => self.array(),
            Tok::Str(s) => Ok(Bson::String(s)),
            Tok::Regex(pattern, options) => Ok(Bson::RegularExpression(Regex { pattern, options })),
            Tok::Num(raw) => self.number(&raw, false, t.start),
            Tok::Punct(sign @ ('-' | '+')) => {
                let n = self.next();
                match n.tok {
                    Tok::Num(raw) => self.number(&raw, sign == '-', t.start),
                    Tok::Ident(w) if w == "Infinity" => Ok(Bson::Double(if sign == '-' {
                        f64::NEG_INFINITY
                    } else {
                        f64::INFINITY
                    })),
                    _ => Err(self.unexpected(&n, "a number")),
                }
            }
            Tok::Ident(w) => match w.as_str() {
                "true" => Ok(Bson::Boolean(true)),
                "false" => Ok(Bson::Boolean(false)),
                "null" | "undefined" => Ok(Bson::Null),
                "Infinity" => Ok(Bson::Double(f64::INFINITY)),
                "NaN" => Ok(Bson::Double(f64::NAN)),
                "new" => {
                    let (name, at) = self.expect_ident("a type such as Date or ObjectId")?;
                    self.helper(&name, at, true)
                }
                name => {
                    if self.at_punct('(') {
                        self.helper(name, t.start, false)
                    } else {
                        Err(self.err(t.start, format!("Unknown name: {name}")))
                    }
                }
            },
            // `() => …`, `(a + b)`: only JavaScript puts a parenthesis where a value starts.
            Tok::Punct('(') => Err(self.err(
                t.start,
                "JavaScript is not supported: functions and expressions. Only mongosh query syntax is allowed",
            )),
            _ => Err(self.unexpected(&t, "a value")),
        }
    }

    fn object(&mut self) -> Result<Bson, ParseError> {
        let mut doc = Document::new();
        loop {
            if self.eat_punct('}') {
                return Ok(Bson::Document(doc));
            }
            let t = self.next();
            let key = match t.tok {
                Tok::Ident(k) | Tok::Str(k) | Tok::Num(k) => k,
                // A computed key, as an export writes `__proto__`: `{["__proto__"]: 1}`. Only a
                // string literal is a key; no JavaScript is evaluated.
                Tok::Punct('[') => {
                    let s = self.next();
                    let Tok::Str(key) = s.tok else {
                        return Err(self.unexpected(&s, "a string key"));
                    };
                    if !self.eat_punct(']') {
                        return Err(self.unexpected(self.peek(), "']'"));
                    }
                    key
                }
                _ => return Err(self.unexpected(&t, "a field name or '}'")),
            };
            if !self.eat_punct(':') {
                return Err(self.unexpected(self.peek(), "':'"));
            }
            let value = self.value()?;
            doc.insert(key, value);
            if self.eat_punct(',') {
                continue;
            }
            if self.eat_punct('}') {
                return Ok(Bson::Document(doc));
            }
            return Err(self.unexpected(self.peek(), "',' or '}'"));
        }
    }

    fn array(&mut self) -> Result<Bson, ParseError> {
        let mut items = Vec::new();
        loop {
            if self.eat_punct(']') {
                return Ok(Bson::Array(items));
            }
            items.push(self.value()?);
            if self.eat_punct(',') {
                continue;
            }
            if self.eat_punct(']') {
                return Ok(Bson::Array(items));
            }
            return Err(self.unexpected(self.peek(), "',' or ']'"));
        }
    }

    /// A JavaScript number as mongosh stores it: a whole number that fits 32 bits is an int32,
    /// any other number is a double.
    fn number(&self, raw: &str, negative: bool, at: usize) -> Result<Bson, ParseError> {
        let bad = || self.err(at, format!("Invalid number: {raw}"));
        let sign = if negative { "-" } else { "" };
        if let Some(hex) = raw.strip_prefix("0x").or_else(|| raw.strip_prefix("0X")) {
            let v = i64::from_str_radix(hex, 16).map_err(|_| bad())?;
            let v = if negative { -v } else { v };
            return Ok(i32::try_from(v).map_or(Bson::Double(v as f64), Bson::Int32));
        }
        if !raw.contains(['.', 'e', 'E'])
            && let Ok(v) = format!("{sign}{raw}").parse::<i64>()
        {
            return Ok(i32::try_from(v).map_or(Bson::Double(v as f64), Bson::Int32));
        }
        match format!("{sign}{raw}").parse::<f64>() {
            Ok(f) if f.is_finite() => Ok(Bson::Double(f)),
            _ => Err(bad()),
        }
    }

    fn helper(&mut self, name: &str, at: usize, with_new: bool) -> Result<Bson, ParseError> {
        if !self.at_punct('(') {
            return Err(self.unexpected(self.peek(), "'('"));
        }
        let args = self.args()?;
        let one_arg = || self.err(at, format!("{name} takes one argument"));
        match name {
            "ObjectId" => match args.as_slice() {
                [] => Ok(Bson::ObjectId(ObjectId::new())),
                [(Bson::String(s), a)] => ObjectId::parse_str(s)
                    .map(Bson::ObjectId)
                    .map_err(|_| self.err(*a, "ObjectId needs 24 hex characters")),
                _ => Err(self.err(at, "ObjectId needs 24 hex characters")),
            },
            "ISODate" | "Date" => {
                // `Date()` without `new` returns a string in JavaScript, not a date.
                if name == "Date" && !with_new {
                    return Err(self.err(at, "Use new Date(…) or ISODate(…)"));
                }
                match args.as_slice() {
                    [] => Ok(Bson::DateTime(DateTime::now())),
                    [(Bson::String(s), a)] => parse_date(s)
                        .map(Bson::DateTime)
                        .ok_or_else(|| self.err(*a, format!("Invalid date: {s}"))),
                    [(Bson::Int32(ms), _)] if name == "Date" => {
                        Ok(Bson::DateTime(DateTime::from_millis(i64::from(*ms))))
                    }
                    [(Bson::Double(ms), _)] if name == "Date" && ms.is_finite() => {
                        Ok(Bson::DateTime(DateTime::from_millis(*ms as i64)))
                    }
                    _ => Err(self.err(at, format!("{name} needs a date string"))),
                }
            }
            "NumberInt" | "Int32" => {
                let [(v, a)] = args.as_slice() else {
                    return Err(one_arg());
                };
                let bad = || self.err(*a, format!("{name} needs a 32-bit integer"));
                match v {
                    Bson::Int32(i) => Ok(Bson::Int32(*i)),
                    Bson::Double(f) if f.fract() == 0.0 && f.abs() <= f64::from(i32::MAX) => {
                        Ok(Bson::Int32(*f as i32))
                    }
                    Bson::String(s) => s.trim().parse().map(Bson::Int32).map_err(|_| bad()),
                    _ => Err(bad()),
                }
            }
            "NumberLong" | "Long" => {
                let [(v, a)] = args.as_slice() else {
                    return Err(one_arg());
                };
                let bad = || self.err(*a, format!("{name} needs an integer"));
                match v {
                    Bson::Int32(i) => Ok(Bson::Int64(i64::from(*i))),
                    // Above 2^53 a double has lost digits: the string form keeps them.
                    Bson::Double(f) if f.fract() == 0.0 && f.abs() <= 9_007_199_254_740_991.0 => {
                        Ok(Bson::Int64(*f as i64))
                    }
                    Bson::String(s) => s.trim().parse().map(Bson::Int64).map_err(|_| bad()),
                    _ => Err(bad()),
                }
            }
            "NumberDecimal" | "Decimal128" => {
                let [(v, a)] = args.as_slice() else {
                    return Err(one_arg());
                };
                let text = match v {
                    Bson::String(s) => s.trim().to_string(),
                    Bson::Int32(i) => i.to_string(),
                    Bson::Double(f) => f.to_string(),
                    _ => return Err(self.err(*a, format!("{name} needs a number or a string"))),
                };
                text.parse::<Decimal128>()
                    .map(Bson::Decimal128)
                    .map_err(|_| self.err(*a, "Invalid decimal"))
            }
            "Double" => {
                let [(v, a)] = args.as_slice() else {
                    return Err(one_arg());
                };
                let bad = || self.err(*a, "Double needs a number");
                match v {
                    Bson::Int32(i) => Ok(Bson::Double(f64::from(*i))),
                    Bson::Int64(i) => Ok(Bson::Double(*i as f64)),
                    Bson::Double(f) => Ok(Bson::Double(*f)),
                    _ => Err(bad()),
                }
            }
            "UUID" => match args.as_slice() {
                [] => Ok(uuid_bson(*uuid::Uuid::new_v4().as_bytes())),
                [(Bson::String(s), a)] => uuid::Uuid::parse_str(s)
                    .map(|u| uuid_bson(*u.as_bytes()))
                    .map_err(|_| self.err(*a, "UUID needs 32 hex characters")),
                _ => Err(self.err(at, "UUID needs 32 hex characters")),
            },
            "BinData" => match args.as_slice() {
                [(sub, sa), (Bson::String(data), da)] => {
                    let subtype = match sub {
                        Bson::Int32(n) if (0..=255).contains(n) => BinarySubtype::from(*n as u8),
                        _ => return Err(self.err(*sa, "BinData needs a subtype from 0 to 255")),
                    };
                    let bytes = B64
                        .decode(data)
                        .map_err(|_| self.err(*da, "BinData needs base64 data"))?;
                    Ok(Bson::Binary(Binary { subtype, bytes }))
                }
                _ => Err(self.err(at, "Usage: BinData(subtype, base64)")),
            },
            "Timestamp" => {
                let part = |v: &Bson, a: usize| match v {
                    Bson::Int32(i) if *i >= 0 => Ok(*i as u32),
                    Bson::Double(f)
                        if f.fract() == 0.0 && *f >= 0.0 && *f <= f64::from(u32::MAX) =>
                    {
                        Ok(*f as u32)
                    }
                    _ => Err(self.err(a, "Timestamp needs two whole numbers")),
                };
                let (time, increment) = match args.as_slice() {
                    [] => (0, 0),
                    [(t, ta), (i, ia)] => (part(t, *ta)?, part(i, *ia)?),
                    [(Bson::Document(d), a)] => match (d.get("t"), d.get("i")) {
                        (Some(t), Some(i)) if d.len() == 2 => (part(t, *a)?, part(i, *a)?),
                        _ => return Err(self.err(*a, "Usage: Timestamp({t: …, i: …})")),
                    },
                    _ => return Err(self.err(at, "Usage: Timestamp(t, i)")),
                };
                Ok(Bson::Timestamp(Timestamp { time, increment }))
            }
            "RegExp" => match args.as_slice() {
                [(Bson::String(pattern), _), (Bson::String(flags), _)] => {
                    Ok(Bson::RegularExpression(Regex {
                        pattern: pattern.clone(),
                        options: flags.clone(),
                    }))
                }
                _ => Err(self.err(at, "Usage: RegExp(pattern, flags)")),
            },
            "Code" => match args.as_slice() {
                [(Bson::String(code), _)] => Ok(Bson::JavaScriptCode(code.clone())),
                _ => Err(self.err(at, "Usage: Code(\"...\")")),
            },
            "MinKey" | "MaxKey" => {
                if !args.is_empty() {
                    return Err(self.err(at, format!("Usage: {name}()")));
                }
                Ok(if name == "MinKey" {
                    Bson::MinKey
                } else {
                    Bson::MaxKey
                })
            }
            _ if JS_WORDS.contains(&name) => Err(self.err(
                at,
                format!(
                    "JavaScript is not supported: '{name}'. Only mongosh query syntax is allowed"
                ),
            )),
            _ => Err(self.err(at, format!("Unknown function: {name}"))),
        }
    }
}

fn uuid_bson(bytes: [u8; 16]) -> Bson {
    Bson::Binary(Binary {
        subtype: BinarySubtype::Uuid,
        bytes: bytes.to_vec(),
    })
}

/// RFC 3339, or a date or date-time without a zone, which counts as UTC (as mongosh does).
fn parse_date(s: &str) -> Option<DateTime> {
    let s = s.trim().replacen(' ', "T", 1);
    if let Ok(d) = DateTime::parse_rfc3339_str(&s) {
        return Some(d);
    }
    let candidates = if s.len() == 10 {
        vec![format!("{s}T00:00:00Z")]
    } else {
        vec![format!("{s}Z"), format!("{s}:00Z")]
    };
    candidates
        .iter()
        .find_map(|c| DateTime::parse_rfc3339_str(c).ok())
}

#[cfg(test)]
mod tests {
    use mongodb::bson::doc;

    use super::*;

    fn one(text: &str) -> Command {
        let mut v = parse(text).unwrap_or_else(|e| panic!("{text}: {e}"));
        assert_eq!(v.len(), 1, "{text}");
        v.remove(0).command
    }

    fn method(text: &str) -> Method {
        match one(text) {
            Command::Collection { method, .. } => method,
            other => panic!("{other:?}"),
        }
    }

    fn err(text: &str) -> ParseError {
        parse(text).expect_err(text)
    }

    /// The value of field `v` in `db.c.insertOne({v: <text>})`.
    fn value(text: &str) -> Bson {
        match method(&format!("db.c.insertOne({{v: {text}}})")) {
            Method::InsertOne { mut document } => document.remove("v").expect("v"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parse_value_reads_one_value_only() {
        assert_eq!(parse_value(" 42 ").unwrap(), Bson::Int32(42));
        assert_eq!(
            parse_value("/a/i").unwrap(),
            Bson::RegularExpression(Regex {
                pattern: "a".into(),
                options: "i".into(),
            })
        );
        let e = parse_value("{a: 1} x").unwrap_err();
        assert_eq!(e.message, "Expected the end of the value, found 'x'");
        assert!(parse_value("1; 2").is_err());
    }

    #[test]
    fn shell_helpers() {
        assert_eq!(one("show dbs"), Command::ShowDbs);
        assert_eq!(one("show databases"), Command::ShowDbs);
        assert_eq!(one("show collections"), Command::ShowCollections);
        assert_eq!(one("show tables;"), Command::ShowCollections);
        assert_eq!(one("use shop-staging"), Command::Use("shop-staging".into()));
        assert_eq!(
            err("show users").message,
            "Unknown show command: users. Use show dbs or show collections"
        );
        assert_eq!(err("use a.b").message, "Invalid database name: a.b");
    }

    #[test]
    fn find_with_chain_keeps_the_last_value() {
        let Method::Find(f) = method(
            "db.users.find({age: {$gt: 30}}, {name: 1})\n  .sort({age: -1}).limit(5).skip(10).limit(7)\n  .hint('age_1').collation({locale: 'en'})",
        ) else {
            panic!()
        };
        assert_eq!(f.filter, doc! {"age": {"$gt": 30}});
        assert_eq!(f.projection, Some(doc! {"name": 1}));
        assert_eq!(f.sort, Some(doc! {"age": -1}));
        assert_eq!(f.limit, Some(7));
        assert_eq!(f.skip, Some(10));
        assert_eq!(f.hint, Some(Bson::String("age_1".into())));
        assert_eq!(f.collation, Some(doc! {"locale": "en"}));
        assert!(!f.count);
        let Method::Find(f) = method("db.users.find().project({a: 1}).count()") else {
            panic!()
        };
        assert!(f.count);
        assert_eq!(f.filter, Document::new());
        assert_eq!(f.projection, Some(doc! {"a": 1}));
        assert_eq!(
            err("db.users.find().count().limit(1)").message,
            ".limit() cannot follow count()"
        );
        assert_eq!(
            err("db.users.aggregate([]).sort({a: 1})").message,
            ".sort() only follows find()"
        );
        assert_eq!(
            err("db.users.find().batchSize(5)").message,
            "Unknown cursor method: batchSize"
        );
        assert_eq!(
            err("db.users.find().limit(-1)").message,
            "limit cannot be negative"
        );
        // Pasted mongosh output helpers are accepted.
        assert!(matches!(
            method("db.users.find().toArray()"),
            Method::Find(_)
        ));
        assert!(matches!(
            method("db.users.aggregate([]).pretty()"),
            Method::Aggregate { .. }
        ));
    }

    #[test]
    fn collection_forms() {
        let Command::Collection { collection, method } =
            one("db.getCollection(\"order items\").findOne({_id: 1})")
        else {
            panic!()
        };
        assert_eq!(collection, "order items");
        assert!(matches!(method, Method::FindOne { .. }));
        let Command::Collection { collection, .. } = one("db.system.profile.find()") else {
            panic!()
        };
        assert_eq!(collection, "system.profile");
        assert_eq!(
            one("db.runCommand({ping: 1})"),
            Command::RunCommand {
                admin: false,
                command: doc! {"ping": 1}
            }
        );
        assert_eq!(
            one("db.adminCommand('buildInfo')"),
            Command::RunCommand {
                admin: true,
                command: doc! {"buildInfo": 1}
            }
        );
        assert_eq!(
            one("db.createCollection('logs', {capped: true, size: 4096})"),
            Command::CreateCollection {
                name: "logs".into(),
                options: Some(doc! {"capped": true, "size": 4096})
            }
        );
        assert_eq!(
            err("db.getSiblingDB('x')").message,
            "Unknown db method: getSiblingDB"
        );
    }

    #[test]
    fn drop_database_parses() {
        assert_eq!(
            one("db.dropDatabase()"),
            Command::DropDatabase { options: None }
        );
        assert_eq!(
            one("db.dropDatabase({ writeConcern: { w: 'majority' } })"),
            Command::DropDatabase {
                options: Some(doc! { "writeConcern": { "w": "majority" } })
            }
        );
        for text in [
            "db.dropDatabase(1)",
            "db.dropDatabase({}, {})",
            "db.dropDatabase('shop')",
        ] {
            assert_eq!(
                err(text).message,
                "dropDatabase needs an optional options document",
                "{text}"
            );
        }
    }

    #[test]
    fn every_method_parses() {
        let cases: &[(&str, &str)] = &[
            (
                "db.c.aggregate([{$match: {a: 1}}, {$count: 'n'}], {allowDiskUse: true})",
                "aggregate",
            ),
            ("db.c.countDocuments({a: 1})", "countDocuments"),
            ("db.c.estimatedDocumentCount()", "estimatedDocumentCount"),
            ("db.c.distinct('city', {active: true})", "distinct"),
            ("db.c.getIndexes()", "getIndexes"),
            ("db.c.stats()", "stats"),
            ("db.c.insertOne({a: 1})", "insertOne"),
            (
                "db.c.insertMany([{a: 1}, {a: 2}], {ordered: false})",
                "insertMany",
            ),
            (
                "db.c.updateOne({a: 1}, {$set: {b: 2}}, {upsert: true})",
                "updateOne",
            ),
            ("db.c.updateMany({}, [{$set: {b: '$a'}}])", "updateMany"),
            ("db.c.replaceOne({a: 1}, {a: 2})", "replaceOne"),
            ("db.c.deleteOne({a: 1})", "deleteOne"),
            ("db.c.deleteMany({})", "deleteMany"),
            (
                "db.c.findOneAndUpdate({a: 1}, {$inc: {n: 1}}, {returnDocument: 'after'})",
                "findOneAndUpdate",
            ),
            (
                "db.c.findOneAndReplace({a: 1}, {a: 3})",
                "findOneAndReplace",
            ),
            ("db.c.findOneAndDelete({a: 1})", "findOneAndDelete"),
            (
                "db.c.bulkWrite([{insertOne: {document: {a: 1}}}])",
                "bulkWrite",
            ),
            ("db.c.createIndex({a: 1}, {unique: true})", "createIndex"),
            ("db.c.createIndexes([{a: 1}, {b: -1}])", "createIndexes"),
            ("db.c.dropIndex('a_1')", "dropIndex"),
            ("db.c.dropIndex({a: 1})", "dropIndex"),
            ("db.c.dropIndexes()", "dropIndexes"),
            ("db.c.drop()", "drop"),
            ("db.c.renameCollection('d', true)", "renameCollection"),
        ];
        for (text, name) in cases {
            assert_eq!(method(text).name(), *name, "{text}");
        }
        assert_eq!(
            method("db.c.renameCollection('d')"),
            Method::RenameCollection {
                to: "d".into(),
                drop_target: false
            }
        );
        assert_eq!(
            method("db.c.updateMany({}, [{$set: {b: '$a'}}])"),
            Method::UpdateMany {
                filter: Document::new(),
                update: Bson::Array(vec![Bson::Document(doc! {"$set": {"b": "$a"}})]),
                options: None,
            }
        );
    }

    #[test]
    fn wrong_arguments_name_the_method() {
        assert_eq!(
            err("db.c.insertOne()").message,
            "Usage: insertOne(document)"
        );
        assert_eq!(
            err("db.c.insertOne([1])").message,
            "insertOne needs a document"
        );
        assert_eq!(
            err("db.c.deleteMany()").message,
            "Usage: deleteMany(filter)"
        );
        assert_eq!(
            err("db.c.updateOne({a: 1}, 5)").message,
            "updateOne needs an update document or a pipeline array"
        );
        assert_eq!(
            err("db.c.aggregate({$match: {}})").message,
            "aggregate needs an array of stages"
        );
        assert_eq!(err("db.c.drop(1)").message, "Usage: drop()");
        assert_eq!(err("db.c.mapReduce()").message, "Unknown method: mapReduce");
    }

    #[test]
    fn numbers_follow_mongosh() {
        assert_eq!(value("42"), Bson::Int32(42));
        assert_eq!(value("-2147483648"), Bson::Int32(i32::MIN));
        assert_eq!(value("2147483648"), Bson::Double(2147483648.0));
        assert_eq!(value("1.5"), Bson::Double(1.5));
        assert_eq!(value("1e3"), Bson::Double(1000.0));
        assert_eq!(value(".5"), Bson::Double(0.5));
        assert_eq!(value("0x1f"), Bson::Int32(31));
        assert_eq!(value("+7"), Bson::Int32(7));
        assert_eq!(value("-Infinity"), Bson::Double(f64::NEG_INFINITY));
        assert!(matches!(value("NaN"), Bson::Double(f) if f.is_nan()));
        assert_eq!(
            value("NumberLong('9007199254740993')"),
            Bson::Int64(9_007_199_254_740_993)
        );
        assert_eq!(value("NumberLong(5)"), Bson::Int64(5));
        assert_eq!(value("NumberInt('7')"), Bson::Int32(7));
        assert_eq!(
            value("NumberDecimal('1.10')"),
            Bson::Decimal128("1.10".parse().unwrap())
        );
        assert_eq!(
            value("Decimal128('0.1')"),
            Bson::Decimal128("0.1".parse().unwrap())
        );
        assert_eq!(
            err("db.c.insertOne({v: NumberDecimal('abc')})").message,
            "Invalid decimal"
        );
        assert_eq!(
            err("db.c.insertOne({v: 1.2.3})").message,
            "Invalid number: 1.2.3"
        );
    }

    #[test]
    fn a_missing_cursor_argument_points_at_the_method() {
        let e = err("db.c.find().limit()");
        assert_eq!((e.line, e.column), (1, 13));
        assert_eq!(e.message, ".limit() takes one argument");
    }

    #[test]
    fn strings_comments_and_trailing_commas() {
        assert_eq!(
            value(r"'a\n\t\'\\b\u0041\x42\u{1F600}'"),
            Bson::String("a\n\t'\\bAB😀".into())
        );
        assert_eq!(value(r#""say \"hi\"""#), Bson::String("say \"hi\"".into()));
        assert_eq!(value("`plain`"), Bson::String("plain".into()));
        assert_eq!(value(r"'\uD83D\uDE00'"), Bson::String("😀".into()));
        let Method::Find(f) = method(
            "db.c.find({ // who\n  name: 'a', /* and */ tags: [1, 2,], 'odd key': {x: 1,}, 3: true, })",
        ) else {
            panic!()
        };
        assert_eq!(
            f.filter,
            doc! {"name": "a", "tags": [1, 2], "odd key": {"x": 1}, "3": true}
        );
        // A computed key with a string literal, as an export of `__proto__` writes it.
        let Method::InsertOne { document } =
            method("db.c.insertOne({[\"__proto__\"]: {[\"a\"]: 1}, ['b']: 2, ok: 3})")
        else {
            panic!()
        };
        assert_eq!(document, doc! {"__proto__": {"a": 1}, "b": 2, "ok": 3});
        for (bad, message) in [
            (
                "db.c.insertOne({[a]: 1})",
                "Expected a string key, found 'a'",
            ),
            (
                "db.c.insertOne({[1]: 1})",
                "Expected a string key, found '1'",
            ),
            ("db.c.insertOne({['a' 1})", "Expected ']', found '1'"),
            ("db.c.insertOne({['a': 1})", "Expected ']', found ':'"),
        ] {
            assert_eq!(err(bad).message, message, "{bad}");
        }
        assert!(
            err("db.c.insertOne({s: `a${b}c`})")
                .message
                .contains("Template placeholders")
        );
        assert_eq!(err("db.c.find({a: 'x})").message, "Unterminated string");
        assert_eq!(err("db.c.find() /* open").message, "Unterminated comment");
    }

    #[test]
    fn literals_and_helpers() {
        // Reserved words are fine as names: `$lookup` takes `let`.
        assert_eq!(
            value("{let: {u: '$a'}, for: 1}"),
            Bson::Document(doc! {"let": {"u": "$a"}, "for": 1})
        );
        let Command::Collection { collection, .. } = one("db.delete.find()") else {
            panic!()
        };
        assert_eq!(collection, "delete");
        assert_eq!(value("true"), Bson::Boolean(true));
        assert_eq!(value("null"), Bson::Null);
        assert_eq!(value("undefined"), Bson::Null);
        assert_eq!(
            value("/^ab;c'[/]x/mi"),
            Bson::RegularExpression(Regex {
                pattern: "^ab;c'[/]x".into(),
                options: "im".into()
            })
        );
        assert_eq!(
            err("db.c.find({a: /x/g})").message,
            "Unsupported regular expression flag: g"
        );
        let oid = ObjectId::parse_str("507f1f77bcf86cd799439011").unwrap();
        assert_eq!(
            value("ObjectId('507f1f77bcf86cd799439011')"),
            Bson::ObjectId(oid)
        );
        assert_eq!(
            value("new ObjectId(\"507f1f77bcf86cd799439011\")"),
            Bson::ObjectId(oid)
        );
        assert!(matches!(value("ObjectId()"), Bson::ObjectId(_)));
        assert_eq!(
            err("db.c.find({_id: ObjectId('xyz')})").message,
            "ObjectId needs 24 hex characters"
        );
        let day = DateTime::parse_rfc3339_str("2024-01-15T00:00:00Z").unwrap();
        assert_eq!(value("ISODate('2024-01-15')"), Bson::DateTime(day));
        assert_eq!(
            value("new Date('2024-01-15T00:00:00Z')"),
            Bson::DateTime(day)
        );
        assert_eq!(value("ISODate('2024-01-15T00:00')"), Bson::DateTime(day));
        assert_eq!(value("ISODate('2024-01-15 00:00:00')"), Bson::DateTime(day));
        assert_eq!(
            value(&format!("new Date({})", 1_705_276_800_000_i64)),
            Bson::DateTime(day)
        );
        assert!(matches!(value("new Date()"), Bson::DateTime(_)));
        assert_eq!(
            err("db.c.find({d: Date()})").message,
            "Use new Date(…) or ISODate(…)"
        );
        assert_eq!(
            err("db.c.find({d: ISODate('soon')})").message,
            "Invalid date: soon"
        );
        let Bson::Binary(u) = value("UUID('123e4567-e89b-12d3-a456-426614174000')") else {
            panic!()
        };
        assert_eq!(
            (u.subtype, u.bytes.len(), u.bytes[0]),
            (BinarySubtype::Uuid, 16, 0x12)
        );
        assert!(matches!(value("UUID()"), Bson::Binary(b) if b.subtype == BinarySubtype::Uuid));
        assert_eq!(
            value("BinData(0, 'SGVsbG8=')"),
            Bson::Binary(Binary {
                subtype: BinarySubtype::Generic,
                bytes: b"Hello".to_vec()
            })
        );
        assert_eq!(
            value("Timestamp(5, 6)"),
            Bson::Timestamp(Timestamp {
                time: 5,
                increment: 6
            })
        );
        assert_eq!(
            value("Timestamp({t: 5, i: 6})"),
            Bson::Timestamp(Timestamp {
                time: 5,
                increment: 6
            })
        );
        assert_eq!(value("MinKey()"), Bson::MinKey);
        assert_eq!(value("new MaxKey()"), Bson::MaxKey);
        assert_eq!(
            err("db.c.find({x: Foo()})").message,
            "Unknown function: Foo"
        );
        assert_eq!(err("db.c.find({x: bar})").message, "Unknown name: bar");
    }

    #[test]
    fn statements_split_on_semicolons_and_lines() {
        let text = "db.a.find();  db.b.find()\n\n// note\nshow dbs\nuse analytics\ndb.c.find({\n  x: 1\n})\n  .limit(2);";
        let v = parse(text).unwrap();
        assert_eq!(v.len(), 5);
        assert_eq!(v[0].text, "db.a.find()");
        assert_eq!(v[1].text, "db.b.find()");
        assert_eq!(v[2].command, Command::ShowDbs);
        assert_eq!(v[3].command, Command::Use("analytics".into()));
        assert_eq!(v[4].text, "db.c.find({\n  x: 1\n})\n  .limit(2)");
        assert!(
            parse("  // only a comment\n/* and */ ;; ")
                .unwrap()
                .is_empty()
        );
        let e = err("db.a.find() db.b.find()");
        assert_eq!((e.line, e.column), (1, 13));
        assert_eq!(e.message, "Expected ';' or a new line, found 'db'");
    }

    /// Completion offers these names, so every one must reach the parser's arguments instead
    /// of its unknown-name refusal.
    #[test]
    fn every_offered_name_is_parsed() {
        let accepted = |text: &str| match parse(text) {
            Ok(_) => true,
            Err(e) => !e.message.starts_with("Unknown "),
        };
        for name in COLLECTION_METHODS {
            let text = format!("db.c.{name}()");
            assert!(accepted(&text), "{text}");
        }
        for name in CURSOR_METHODS {
            let text = format!("db.c.find().{name}()");
            assert!(accepted(&text), "{text}");
        }
        for name in DB_METHODS {
            let text = format!("db.{name}()");
            assert!(accepted(&text), "{text}");
        }
    }

    #[test]
    fn errors_carry_line_and_column() {
        let e = err("db.c.find({\n  a: 1\n  b: 2\n})");
        assert_eq!(
            (e.line, e.column, e.message.as_str()),
            (3, 3, "Expected ',' or '}', found 'b'")
        );
        let e = err("db.c.find({a: })");
        assert_eq!((e.line, e.column), (1, 15));
        assert_eq!(e.message, "Expected a value, found '}'");
        // Columns count characters, not bytes.
        let e = err("db.c.find({é: 'ü', x})");
        assert_eq!((e.line, e.column), (1, 21));
        assert_eq!(
            AppError::from(e).message,
            "Line 1, column 21: Expected ':', found '}'"
        );
    }

    #[test]
    fn javascript_is_refused() {
        for text in [
            "db.users.find().forEach(function(d) { print(d) })",
            "var x = 1",
            "for (let i = 0; i < 3; i++) db.c.insertOne({i})",
            "db.c.find({$where: () => true})",
            "const a = db.c.find()",
        ] {
            let e = err(text);
            assert!(
                e.message.starts_with("JavaScript is not supported"),
                "{text}: {}",
                e.message
            );
        }
        assert_eq!(
            err("x = 1").message,
            "Expected a statement that starts with db., show or use, found 'x'"
        );
        assert_eq!(
            err("db.c.find({a: 1 + 2})").message,
            "Expected ',' or '}', found '+'"
        );
    }
}
