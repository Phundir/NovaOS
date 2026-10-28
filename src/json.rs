//! JSON (RFC 8259): a reader and a writer, bounded, with no float.
//!
//! ## Why it exists
//!
//! Phase 15 reads its registry as JSON (`propositions/mises-a-jour.md`:
//! one manifest per application, published on a static web host), and
//! the JavaScript engine of phase 16 needs `JSON.parse` and
//! `JSON.stringify`. Written once, here, for both.
//!
//! ## Three bounds, because the input comes from the network
//!
//! - **Depth**: nesting stops at `MAX_DEPTH` levels, and before each level
//!   the parser asks `scheduler::stack_headroom` -- task stacks have no
//!   guard page, and an overflow is not reported, it corrupts.
//! - **Size**: a document above `MAX_INPUT` is refused before reading.
//! - **Progress**: every loop consumes at least one byte or stops
//!   (DECISIONS, "Un analyseur qui n'avance pas").
//!
//! ## Numbers keep their text
//!
//! The kernel is soft-float and holds no `f64` (CLAUDE.md). A number is
//! validated against the grammar of RFC 8259 and kept as written;
//! `as_i64` / `as_u64` give the integer when it is one and fits. The
//! JavaScript engine converts the text with its own exact routine
//! (`web::js::num`) -- a second, approximate conversion here would only
//! be a second place to be wrong.
//!
//! ## Duplicate keys are refused
//!
//! RFC 8259 leaves their meaning open ("names SHOULD be unique"), and
//! implementations disagree: some keep the first, some the last. For a
//! signed manifest that is an attack -- the signer reads one value, the
//! installer another. An object with a repeated name is an error.

use alloc::string::String;
use alloc::vec::Vec;

/// Deepest nesting accepted (arrays and objects together).
pub const MAX_DEPTH: usize = 64;

/// Largest document accepted, in bytes.
pub const MAX_INPUT: usize = 16 << 20;

/// Stack the parser keeps free below it before descending a level.
const STACK_RESERVE: usize = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Null,
    Bool(bool),
    Number(Number),
    String(String),
    Array(Vec<Value>),
    Object(Object),
}

/// A number as written in the document, checked against the grammar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Number(String);

/// An object: its members in document order, names unique.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Object {
    members: Vec<(String, Value)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    TooLarge,
    TooDeep,
    UnexpectedEnd,
    UnexpectedByte(u8),
    BadNumber,
    BadEscape,
    BadUnicode,
    ControlInString,
    DuplicateKey,
    TrailingData,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Error {
    pub kind: ErrorKind,
    /// Byte offset where the problem was seen.
    pub offset: usize,
}

impl Error {
    pub fn message(&self) -> String {
        let what = match self.kind {
            ErrorKind::TooLarge => String::from("document trop grand"),
            ErrorKind::TooDeep => String::from("imbrication trop profonde"),
            ErrorKind::UnexpectedEnd => String::from("fin inattendue"),
            ErrorKind::UnexpectedByte(b) if (0x20..0x7f).contains(&b) => {
                alloc::format!("caractere inattendu '{}'", b as char)
            }
            ErrorKind::UnexpectedByte(b) => alloc::format!("octet inattendu 0x{:02x}", b),
            ErrorKind::BadNumber => String::from("nombre mal forme"),
            ErrorKind::BadEscape => String::from("echappement invalide"),
            ErrorKind::BadUnicode => String::from("unicode invalide"),
            ErrorKind::ControlInString => String::from("caractere de controle dans une chaine"),
            ErrorKind::DuplicateKey => String::from("cle en double"),
            ErrorKind::TrailingData => String::from("donnees apres la valeur"),
        };
        alloc::format!("JSON : {} (octet {})", what, self.offset)
    }
}

// ============================================================
//  Values
// ============================================================

impl Number {
    pub fn from_i64(value: i64) -> Number {
        Number(alloc::format!("{}", value))
    }

    pub fn from_u64(value: u64) -> Number {
        Number(alloc::format!("{}", value))
    }

    /// A number given as text, if it follows the grammar of RFC 8259.
    pub fn from_text(text: &str) -> Option<Number> {
        let bytes = text.as_bytes();
        (scan_number(bytes, 0) == Some(bytes.len())).then(|| Number(String::from(text)))
    }

    /// The text, as written in the document.
    pub fn as_text(&self) -> &str {
        &self.0
    }

    /// The value, when it is an integer that fits (no fraction, no
    /// exponent).
    pub fn as_i64(&self) -> Option<i64> {
        let text = self.0.as_str();
        if text.contains(['.', 'e', 'E']) {
            return None;
        }
        text.parse::<i64>().ok()
    }

    pub fn as_u64(&self) -> Option<u64> {
        let text = self.0.as_str();
        if text.contains(['.', 'e', 'E', '-']) {
            return None;
        }
        text.parse::<u64>().ok()
    }
}

impl Object {
    pub fn new() -> Object {
        Object { members: Vec::new() }
    }

    pub fn get(&self, name: &str) -> Option<&Value> {
        self.members.iter().find(|(key, _)| key == name).map(|(_, value)| value)
    }

    /// Sets a member, replacing one of the same name.
    pub fn insert(&mut self, name: &str, value: Value) {
        if let Some(slot) = self.members.iter_mut().find(|(key, _)| key == name) {
            slot.1 = value;
        } else {
            self.members.push((String::from(name), value));
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &Value)> {
        self.members.iter().map(|(key, value)| (key.as_str(), value))
    }

    pub fn len(&self) -> usize {
        self.members.len()
    }

    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }
}

impl Value {
    pub fn get(&self, name: &str) -> Option<&Value> {
        match self {
            Value::Object(object) => object.get(name),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(text) => Some(text),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(value) => Some(*value),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Number(number) => number.as_i64(),
            _ => None,
        }
    }

    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Value::Number(number) => number.as_u64(),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(items) => Some(items),
            _ => None,
        }
    }

    pub fn as_object(&self) -> Option<&Object> {
        match self {
            Value::Object(object) => Some(object),
            _ => None,
        }
    }

    /// A string member of an object, the most common lookup of a manifest.
    pub fn str_of(&self, name: &str) -> Option<&str> {
        self.get(name).and_then(Value::as_str)
    }
}

// ============================================================
//  Reading
// ============================================================

/// Parses a complete document: one value, surrounded by white space only.
pub fn parse(text: &[u8]) -> Result<Value, Error> {
    if text.len() > MAX_INPUT {
        return Err(Error { kind: ErrorKind::TooLarge, offset: 0 });
    }
    let mut parser = Parser { input: text, pos: 0 };
    // A byte-order mark is not JSON (RFC 8259, 8.1), but the RFC lets a
    // parser ignore one; files saved by some editors carry it.
    if text.starts_with(&[0xEF, 0xBB, 0xBF]) {
        parser.pos = 3;
    }
    parser.skip_space();
    let value = parser.value(0)?;
    parser.skip_space();
    if parser.pos != text.len() {
        return Err(parser.error(ErrorKind::TrailingData));
    }
    Ok(value)
}

pub fn parse_str(text: &str) -> Result<Value, Error> {
    parse(text.as_bytes())
}

struct Parser<'a> {
    input: &'a [u8],
    pos: usize,
}

impl Parser<'_> {
    fn error(&self, kind: ErrorKind) -> Error {
        Error { kind, offset: self.pos }
    }

    fn peek(&self) -> Option<u8> {
        self.input.get(self.pos).copied()
    }

    fn skip_space(&mut self) {
        while let Some(b' ' | b'\t' | b'\n' | b'\r') = self.peek() {
            self.pos += 1;
        }
    }

    fn expect(&mut self, byte: u8) -> Result<(), Error> {
        match self.peek() {
            Some(b) if b == byte => {
                self.pos += 1;
                Ok(())
            }
            Some(b) => Err(self.error(ErrorKind::UnexpectedByte(b))),
            None => Err(self.error(ErrorKind::UnexpectedEnd)),
        }
    }

    fn literal(&mut self, word: &[u8], value: Value) -> Result<Value, Error> {
        if self.input[self.pos..].starts_with(word) {
            self.pos += word.len();
            Ok(value)
        } else if self.input.len() - self.pos < word.len() && word.starts_with(&self.input[self.pos..]) {
            Err(Error { kind: ErrorKind::UnexpectedEnd, offset: self.input.len() })
        } else {
            Err(self.error(ErrorKind::UnexpectedByte(self.input[self.pos])))
        }
    }

    fn value(&mut self, depth: usize) -> Result<Value, Error> {
        match self.peek() {
            None => Err(self.error(ErrorKind::UnexpectedEnd)),
            Some(b'{') => self.object(depth + 1),
            Some(b'[') => self.array(depth + 1),
            Some(b'"') => self.string().map(Value::String),
            Some(b't') => self.literal(b"true", Value::Bool(true)),
            Some(b'f') => self.literal(b"false", Value::Bool(false)),
            Some(b'n') => self.literal(b"null", Value::Null),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(b) => Err(self.error(ErrorKind::UnexpectedByte(b))),
        }
    }

    fn descend(&self, depth: usize) -> Result<(), Error> {
        if depth > MAX_DEPTH || crate::scheduler::stack_headroom() < STACK_RESERVE {
            return Err(self.error(ErrorKind::TooDeep));
        }
        Ok(())
    }

    fn array(&mut self, depth: usize) -> Result<Value, Error> {
        self.descend(depth)?;
        self.pos += 1; // '['
        let mut items = Vec::new();
        self.skip_space();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(Value::Array(items));
        }
        loop {
            self.skip_space();
            items.push(self.value(depth)?);
            self.skip_space();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    return Ok(Value::Array(items));
                }
                Some(b) => return Err(self.error(ErrorKind::UnexpectedByte(b))),
                None => return Err(self.error(ErrorKind::UnexpectedEnd)),
            }
        }
    }

    fn object(&mut self, depth: usize) -> Result<Value, Error> {
        self.descend(depth)?;
        self.pos += 1; // '{'
        let mut object = Object::new();
        self.skip_space();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(Value::Object(object));
        }
        loop {
            self.skip_space();
            let at = self.pos;
            if self.peek() != Some(b'"') {
                return Err(match self.peek() {
                    Some(b) => self.error(ErrorKind::UnexpectedByte(b)),
                    None => self.error(ErrorKind::UnexpectedEnd),
                });
            }
            let name = self.string()?;
            if object.get(&name).is_some() {
                return Err(Error { kind: ErrorKind::DuplicateKey, offset: at });
            }
            self.skip_space();
            self.expect(b':')?;
            self.skip_space();
            let value = self.value(depth)?;
            object.members.push((name, value));
            self.skip_space();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(Value::Object(object));
                }
                Some(b) => return Err(self.error(ErrorKind::UnexpectedByte(b))),
                None => return Err(self.error(ErrorKind::UnexpectedEnd)),
            }
        }
    }

    fn number(&mut self) -> Result<Value, Error> {
        let start = self.pos;
        let end = scan_number(self.input, start).ok_or(self.error(ErrorKind::BadNumber))?;
        self.pos = end;
        // The scanned range is ASCII by construction.
        let text = core::str::from_utf8(&self.input[start..end]).unwrap_or("0");
        Ok(Value::Number(Number(String::from(text))))
    }

    fn hex4(&mut self) -> Result<u32, Error> {
        if self.pos + 4 > self.input.len() {
            return Err(Error { kind: ErrorKind::UnexpectedEnd, offset: self.input.len() });
        }
        let mut value = 0u32;
        for _ in 0..4 {
            let digit = (self.input[self.pos] as char)
                .to_digit(16)
                .ok_or(self.error(ErrorKind::BadEscape))?;
            value = value * 16 + digit;
            self.pos += 1;
        }
        Ok(value)
    }

    fn string(&mut self) -> Result<String, Error> {
        self.pos += 1; // opening quote
        let mut out: Vec<u8> = Vec::new();
        loop {
            let Some(byte) = self.peek() else {
                return Err(self.error(ErrorKind::UnexpectedEnd));
            };
            match byte {
                b'"' => {
                    self.pos += 1;
                    break;
                }
                b'\\' => {
                    self.pos += 1;
                    let Some(escape) = self.peek() else {
                        return Err(self.error(ErrorKind::UnexpectedEnd));
                    };
                    self.pos += 1;
                    let simple = match escape {
                        b'"' => Some(b'"'),
                        b'\\' => Some(b'\\'),
                        b'/' => Some(b'/'),
                        b'b' => Some(0x08),
                        b'f' => Some(0x0c),
                        b'n' => Some(b'\n'),
                        b'r' => Some(b'\r'),
                        b't' => Some(b'\t'),
                        b'u' => None,
                        _ => {
                            self.pos -= 1;
                            return Err(self.error(ErrorKind::BadEscape));
                        }
                    };
                    if let Some(b) = simple {
                        out.push(b);
                        continue;
                    }
                    let first = self.hex4()?;
                    let code = if (0xD800..0xDC00).contains(&first) {
                        // A high surrogate must be followed by a low one.
                        if !self.input[self.pos..].starts_with(b"\\u") {
                            return Err(self.error(ErrorKind::BadUnicode));
                        }
                        self.pos += 2;
                        let second = self.hex4()?;
                        if !(0xDC00..0xE000).contains(&second) {
                            return Err(self.error(ErrorKind::BadUnicode));
                        }
                        0x10000 + ((first - 0xD800) << 10) + (second - 0xDC00)
                    } else if (0xDC00..0xE000).contains(&first) {
                        return Err(self.error(ErrorKind::BadUnicode));
                    } else {
                        first
                    };
                    let c = char::from_u32(code).ok_or(self.error(ErrorKind::BadUnicode))?;
                    let mut buffer = [0u8; 4];
                    out.extend_from_slice(c.encode_utf8(&mut buffer).as_bytes());
                }
                0x00..=0x1f => return Err(self.error(ErrorKind::ControlInString)),
                _ => {
                    out.push(byte);
                    self.pos += 1;
                }
            }
        }
        String::from_utf8(out).map_err(|_| self.error(ErrorKind::BadUnicode))
    }
}

/// End of the number starting at `start`, or `None` if the grammar is
/// not followed: `-? (0 | [1-9][0-9]*) (. [0-9]+)? ([eE] [+-]? [0-9]+)?`.
fn scan_number(input: &[u8], start: usize) -> Option<usize> {
    let mut pos = start;
    let digit = |pos: usize| input.get(pos).is_some_and(|b| b.is_ascii_digit());
    if input.get(pos) == Some(&b'-') {
        pos += 1;
    }
    match input.get(pos) {
        Some(b'0') => pos += 1,
        Some(b'1'..=b'9') => {
            while digit(pos) {
                pos += 1;
            }
        }
        _ => return None,
    }
    if input.get(pos) == Some(&b'.') {
        pos += 1;
        if !digit(pos) {
            return None;
        }
        while digit(pos) {
            pos += 1;
        }
    }
    if let Some(b'e' | b'E') = input.get(pos) {
        pos += 1;
        if let Some(b'+' | b'-') = input.get(pos) {
            pos += 1;
        }
        if !digit(pos) {
            return None;
        }
        while digit(pos) {
            pos += 1;
        }
    }
    Some(pos)
}

// ============================================================
//  Writing
// ============================================================

/// The compact form: no white space, members in their order. It is the
/// form that is signed (`canonical` sorts the members first).
pub fn to_string(value: &Value) -> String {
    let mut out = String::new();
    write(value, &mut out, None, 0);
    out
}

/// Indented by two spaces, for files people read.
pub fn to_string_pretty(value: &Value) -> String {
    let mut out = String::new();
    write(value, &mut out, Some(2), 0);
    out.push('\n');
    out
}

/// The canonical form of a document: compact, object members sorted by
/// name (bytewise). Two documents that mean the same thing give the same
/// bytes -- what a signature is computed over.
pub fn canonical(value: &Value) -> String {
    to_string(&sorted(value))
}

fn sorted(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(sorted).collect()),
        Value::Object(object) => {
            let mut members: Vec<(String, Value)> = object
                .members
                .iter()
                .map(|(key, item)| (key.clone(), sorted(item)))
                .collect();
            members.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
            Value::Object(Object { members })
        }
        other => other.clone(),
    }
}

fn write(value: &Value, out: &mut String, indent: Option<usize>, level: usize) {
    let newline = |out: &mut String, level: usize| {
        if let Some(step) = indent {
            out.push('\n');
            for _ in 0..step * level {
                out.push(' ');
            }
        }
    };
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(number) => out.push_str(&number.0),
        Value::String(text) => write_string(text, out),
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                newline(out, level + 1);
                write(item, out, indent, level + 1);
            }
            if !items.is_empty() {
                newline(out, level);
            }
            out.push(']');
        }
        Value::Object(object) => {
            out.push('{');
            for (index, (key, item)) in object.members.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                newline(out, level + 1);
                write_string(key, out);
                out.push(':');
                if indent.is_some() {
                    out.push(' ');
                }
                write(item, out, indent, level + 1);
            }
            if !object.members.is_empty() {
                newline(out, level);
            }
            out.push('}');
        }
    }
}

/// A string, escaped: quote, backslash and control characters; the rest
/// stays as it is (UTF-8 is JSON).
pub fn write_string(text: &str, out: &mut String) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&alloc::format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

// ============================================================
//  Self-test
// ============================================================

/// Cases of RFC 8259 and of the corners a hand-written parser gets
/// wrong. Returns (passed, failed); prints each failure.
pub fn selftest() -> (usize, usize) {
    let mut passed = 0;
    let mut failed = 0;
    let mut check = |label: &str, ok: bool| {
        if ok {
            passed += 1;
        } else {
            failed += 1;
            crate::serial_println!("[json] ECHEC : {}", label);
        }
    };

    // RFC 8259, section 13, first example.
    let image = br#"{
        "Image": {
            "Width":  800,
            "Height": 600,
            "Title":  "View from 15th Floor",
            "Thumbnail": {
                "Url":    "http://www.example.com/image/481989943",
                "Height": 125,
                "Width":  100
            },
            "Animated" : false,
            "IDs": [116, 943, 234, 38793]
        }
    }"#;
    let parsed = parse(image);
    check("exemple 1 de la RFC 8259 lu", parsed.is_ok());
    if let Ok(document) = &parsed {
        let image = document.get("Image");
        check("membre entier", image.and_then(|i| i.get("Width")).and_then(Value::as_i64) == Some(800));
        check(
            "objet imbrique",
            image.and_then(|i| i.get("Thumbnail")).and_then(|t| t.str_of("Url"))
                == Some("http://www.example.com/image/481989943"),
        );
        check(
            "tableau",
            image.and_then(|i| i.get("IDs")).and_then(Value::as_array).map(|a| a.len()) == Some(4),
        );
        check("booleen", image.and_then(|i| i.get("Animated")) == Some(&Value::Bool(false)));
        // Reading back what was written gives the same value.
        check("ecriture puis relecture", parse_str(&to_string(document)).as_ref() == Ok(document));
        check("forme indentee relue", parse_str(&to_string_pretty(document)).as_ref() == Ok(document));
    }

    // RFC 8259, section 13, second example: a top-level array, and
    // numbers with a fraction kept as written.
    let places = parse(br#"[{"Latitude": 37.7668, "Longitude": -122.3959, "Zip": "94107"}]"#);
    check(
        "nombre a virgule garde tel quel",
        places
            .as_ref()
            .ok()
            .and_then(|v| v.as_array())
            .and_then(|a| a.first())
            .and_then(|p| p.get("Longitude"))
            .map(|n| matches!(n, Value::Number(n) if n.as_text() == "-122.3959" && n.as_i64().is_none()))
            == Some(true),
    );

    check("valeur scalaire seule", parse(b" 42 ") == Ok(Value::Number(Number(String::from("42")))));
    check(
        "echappements",
        parse(br#""a\"b\\c\/d\n\u00e9\ud83d\ude00""#)
            == Ok(Value::String(String::from("a\"b\\c/d\n\u{e9}\u{1F600}"))),
    );

    let refused: [(&[u8], ErrorKind); 12] = [
        (b"[1, 2,]", ErrorKind::UnexpectedByte(b']')),
        (b"{\"a\":1,}", ErrorKind::UnexpectedByte(b'}')),
        (b"01", ErrorKind::TrailingData),
        (b"1.", ErrorKind::BadNumber),
        (b"-", ErrorKind::BadNumber),
        (b"\"\\x\"", ErrorKind::BadEscape),
        (b"\"\\ud800\"", ErrorKind::BadUnicode),
        (b"\"a\x01b\"", ErrorKind::ControlInString),
        (b"{\"a\":1,\"a\":2}", ErrorKind::DuplicateKey),
        (b"tru", ErrorKind::UnexpectedEnd),
        (b"[1] [2]", ErrorKind::TrailingData),
        (b"\"\xff\"", ErrorKind::BadUnicode),
    ];
    for (input, kind) in refused {
        let outcome = parse(input).map_err(|e| e.kind);
        check("entree invalide refusee", outcome == Err(kind));
    }

    // Depth: the limit is reached, then refused, without exhausting the
    // stack.
    let mut deep = Vec::new();
    deep.resize(MAX_DEPTH, b'[');
    deep.resize(2 * MAX_DEPTH, b']');
    check("imbrication a la limite acceptee", parse(&deep).is_ok());
    let mut deeper = Vec::new();
    deeper.resize(MAX_DEPTH + 1, b'[');
    deeper.resize(2 * MAX_DEPTH + 2, b']');
    check(
        "imbrication au-dela refusee",
        parse(&deeper).map_err(|e| e.kind) == Err(ErrorKind::TooDeep),
    );

    // Canonical form: member order does not change the bytes.
    let a = parse(br#"{"b":1,"a":{"d":[true,null],"c":"x"}}"#);
    let b = parse(br#"{ "a" : { "c" : "x", "d" : [ true, null ] }, "b" : 1 }"#);
    check(
        "forme canonique independante de l'ordre",
        matches!((&a, &b), (Ok(a), Ok(b)) if canonical(a) == canonical(b)),
    );
    check(
        "forme canonique exacte",
        a.as_ref().map(canonical).ok().as_deref() == Some(r#"{"a":{"c":"x","d":[true,null]},"b":1}"#),
    );
    check("grand entier", parse(b"18446744073709551615").ok().and_then(|v| v.as_u64()) == Some(u64::MAX));

    (passed, failed)
}
