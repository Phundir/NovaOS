//! Python source to tokens.
//!
//! What makes Python's lexer more than a tokenizer is **indentation**:
//! the lexer keeps the stack of open indentation levels and emits
//! `Indent` and `Dedent` tokens, so that the parser sees blocks as
//! brackets. Inside parentheses, brackets and braces, and after a
//! backslash at the end of a line, line breaks do not count -- the
//! "implicit and explicit line joining" of the language reference.
//!
//! Strings come out cooked (escapes resolved, prefixes applied); an
//! f-string comes out as its literal parts and the source of each
//! replacement field, which the parser parses as an expression of its
//! own. Numbers come out as values: an `int` literal of any size, a
//! float read to the nearest double.

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use super::int::Int;
use crate::web::js::num::Num;

/// One replacement field of an f-string: `{expr!r:spec}`.
#[derive(Debug, Clone)]
pub struct Field {
    /// The expression's source text, and where it starts (for errors).
    pub source: String,
    pub line: u32,
    /// `!r`, `!s` or `!a`.
    pub conversion: Option<char>,
    /// The format spec, itself made of parts (`{x:{width}}`).
    pub spec: Option<Vec<FPart>>,
    /// `{x=}`: the text before the value.
    pub debug: Option<String>,
}

#[derive(Debug, Clone)]
pub enum FPart {
    Text(String),
    Field(Field),
}

#[derive(Debug, Clone)]
pub enum Tok {
    Name(Rc<str>),
    Int(Int),
    Float(Num),
    /// An imaginary literal: `2j`.
    Imag(Num),
    Str(String),
    Bytes(Vec<u8>),
    FString(Vec<FPart>),
    Op(&'static str),
    Newline,
    Indent,
    Dedent,
    Eof,
}

#[derive(Debug, Clone)]
pub struct Token {
    pub tok: Tok,
    pub line: u32,
    /// Columns in characters, 0-based; the end is exclusive and may sit
    /// on a later line (triple-quoted strings).
    pub col: u32,
    pub end_line: u32,
    pub end_col: u32,
}

/// Longest first.
const OPERATORS: &[&str] = &[
    "**=", "//=", ">>=", "<<=", "...", "->", ":=", "**", "//", "<<", ">>", "<=", ">=", "==", "!=", "+=", "-=", "*=", "/=",
    "%=", "&=", "|=", "^=", "@=", "+", "-", "*", "/", "%", "@", "&", "|", "^", "~", "<", ">", "(", ")", "[", "]", "{",
    "}", ",", ":", ".", ";", "=",
];

pub struct LexError {
    pub message: String,
    pub line: u32,
    /// 1-based column, 0 when unknown.
    pub offset: u32,
}

impl LexError {
    pub fn new(message: impl Into<String>, line: u32) -> LexError {
        LexError { message: message.into(), line, offset: 0 }
    }
}

type LResult<T> = Result<T, LexError>;

struct Lexer<'a> {
    src: &'a [u8],
    text: &'a str,
    pos: usize,
    line: u32,
    /// Open brackets: line breaks inside do not count.
    depth: usize,
    indents: Vec<usize>,
    out: Vec<Token>,
    /// At the start of a logical line.
    at_line_start: bool,
    /// Where the token being read starts.
    start_line: u32,
    start_col: u32,
}

/// CPython's message for a character no token starts with.
fn invalid_character(c: char) -> String {
    if super::modules::unicodedata::is_printable(c) {
        alloc::format!("invalid character '{}' (U+{:04X})", c, c as u32)
    } else {
        alloc::format!("invalid non-printable character U+{:04X}", c as u32)
    }
}

fn is_id_start(c: char) -> bool {
    c == '_' || c.is_alphabetic()
}

fn is_id_continue(c: char) -> bool {
    c == '_' || c.is_alphanumeric()
}

impl<'a> Lexer<'a> {
    fn error<T>(&self, message: &str) -> LResult<T> {
        let mut e = LexError::new(String::from(message), self.line);
        e.offset = self.col_of(self.pos) + 1;
        Err(e)
    }

    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }

    fn peek_at(&self, offset: usize) -> Option<u8> {
        self.src.get(self.pos + offset).copied()
    }

    fn push(&mut self, tok: Tok) {
        let end_col = self.col_of(self.pos);
        let (line, col) = if self.start_line == 0 { (self.line, end_col) } else { (self.start_line, self.start_col) };
        self.out.push(Token { tok, line, col, end_line: self.line, end_col });
        self.start_line = 0;
    }

    /// The column (in characters) of byte offset `pos` on its line.
    fn col_of(&self, pos: usize) -> u32 {
        let pos = pos.min(self.text.len());
        let start = self.text[..pos].rfind('\n').map_or(0, |i| i + 1);
        self.text[start..pos].chars().count() as u32
    }

    fn char_at(&self, pos: usize) -> Option<char> {
        self.text[pos..].chars().next()
    }

    /// Measures the indentation of the line starting at `pos`; None for a
    /// blank or comment-only line.
    fn indentation(&mut self) -> LResult<Option<usize>> {
        let mut width = 0;
        loop {
            match self.peek() {
                Some(b' ') => width += 1,
                Some(b'\t') => width = (width / 8 + 1) * 8,
                Some(b'\x0c') => width = 0,
                _ => break,
            }
            self.pos += 1;
        }
        match self.peek() {
            None => Ok(None),
            Some(b'\n') | Some(b'#') => Ok(None),
            Some(b'\r') if self.peek_at(1) == Some(b'\n') => Ok(None),
            Some(b'\\') if matches!(self.peek_at(1), Some(b'\n')) => Ok(None),
            _ => Ok(Some(width)),
        }
    }

    fn run(mut self) -> LResult<Vec<Token>> {
        loop {
            if self.at_line_start && self.depth == 0 {
                let start = self.pos;
                match self.indentation()? {
                    None => {
                        // Blank or comment line: skip to its end.
                        self.skip_comment();
                        match self.peek() {
                            None => break,
                            Some(b'\r') => {
                                self.pos += 2;
                                self.line += 1;
                            }
                            Some(b'\\') => {
                                self.pos += 2;
                                self.line += 1;
                            }
                            _ => {
                                self.pos += 1;
                                self.line += 1;
                            }
                        }
                        continue;
                    }
                    Some(width) => {
                        let _ = start;
                        let current = *self.indents.last().unwrap_or(&0);
                        if width > current {
                            self.indents.push(width);
                            self.push(Tok::Indent);
                        } else {
                            while width < *self.indents.last().unwrap_or(&0) {
                                self.indents.pop();
                                self.push(Tok::Dedent);
                            }
                            if width != *self.indents.last().unwrap_or(&0) {
                                return self.error("unindent does not match any outer indentation level");
                            }
                        }
                        self.at_line_start = false;
                    }
                }
            }
            let Some(byte) = self.peek() else {
                break;
            };
            if !matches!(byte, b' ' | b'\t' | b'\x0c' | b'\r' | b'#' | b'\n' | b'\\') {
                self.start_line = self.line;
                self.start_col = self.col_of(self.pos);
            }
            match byte {
                b' ' | b'\t' | b'\x0c' => self.pos += 1,
                b'\r' => self.pos += 1,
                b'#' => self.skip_comment(),
                b'\n' => {
                    self.pos += 1;
                    if self.depth == 0 {
                        self.push(Tok::Newline);
                        self.at_line_start = true;
                    }
                    self.line += 1;
                }
                b'\\' => {
                    // Explicit line joining.
                    if self.peek_at(1) == Some(b'\n') {
                        self.pos += 2;
                        self.line += 1;
                    } else if self.peek_at(1) == Some(b'\r') && self.peek_at(2) == Some(b'\n') {
                        self.pos += 3;
                        self.line += 1;
                    } else {
                        return self.error("unexpected character after line continuation character");
                    }
                }
                b'0'..=b'9' => self.number()?,
                b'.' if self.peek_at(1).is_some_and(|c| c.is_ascii_digit()) => self.number()?,
                b'"' | b'\'' => {
                    let tok = self.string("")?;
                    self.push(tok);
                }
                _ => {
                    let c = self.char_at(self.pos).unwrap_or('\0');
                    if is_id_start(c) {
                        self.name_or_string()?;
                    } else {
                        self.operator()?;
                    }
                }
            }
        }
        if self.depth > 0 {
            return self.error("unexpected EOF while parsing");
        }
        if !self.at_line_start {
            self.push(Tok::Newline);
        }
        while self.indents.len() > 1 {
            self.indents.pop();
            self.push(Tok::Dedent);
        }
        self.push(Tok::Eof);
        Ok(self.out)
    }

    fn skip_comment(&mut self) {
        if self.peek() == Some(b'#') {
            while let Some(c) = self.peek() {
                if c == b'\n' {
                    break;
                }
                self.pos += 1;
            }
        }
    }

    fn operator(&mut self) -> LResult<()> {
        let rest = &self.text[self.pos..];
        for op in OPERATORS {
            if rest.starts_with(op) {
                self.pos += op.len();
                match *op {
                    "(" | "[" | "{" => self.depth += 1,
                    ")" | "]" | "}" => self.depth = self.depth.saturating_sub(1),
                    _ => {}
                }
                self.push(Tok::Op(op));
                return Ok(());
            }
        }
        let c = self.char_at(self.pos).unwrap_or('?');
        self.error(&invalid_character(c))
    }

    fn name_or_string(&mut self) -> LResult<()> {
        let start = self.pos;
        while let Some(c) = self.char_at(self.pos) {
            if !is_id_continue(c) {
                break;
            }
            self.pos += c.len_utf8();
        }
        let word = &self.text[start..self.pos];
        // A string prefix: r, b, f, u, and their pairs.
        if matches!(self.peek(), Some(b'"') | Some(b'\'')) && word.len() <= 2 {
            let lower = word.to_ascii_lowercase();
            if matches!(lower.as_str(), "r" | "b" | "f" | "u" | "rb" | "br" | "fr" | "rf") {
                let tok = self.string(&lower)?;
                self.push(tok);
                return Ok(());
            }
        }
        self.push(Tok::Name(Rc::from(word)));
        Ok(())
    }

    fn number(&mut self) -> LResult<()> {
        let start = self.pos;
        let bytes = self.src;
        // 0x, 0o, 0b
        if bytes[start] == b'0' && matches!(self.peek_at(1), Some(b'x' | b'X' | b'o' | b'O' | b'b' | b'B')) {
            let radix = match bytes[start + 1] | 0x20 {
                b'x' => 16,
                b'o' => 8,
                _ => 2,
            };
            self.pos += 2;
            let digits_start = self.pos;
            while self.peek().is_some_and(|c| c.is_ascii_alphanumeric() || c == b'_') {
                self.pos += 1;
            }
            let digits: String = self.text[digits_start..self.pos].chars().filter(|c| *c != '_').collect();
            return match Int::parse_radix(&digits, radix) {
                Some(value) => {
                    self.push(Tok::Int(value));
                    Ok(())
                }
                None => self.error("invalid decimal literal"),
            };
        }
        let mut is_float = false;
        while self.peek().is_some_and(|c| c.is_ascii_digit() || c == b'_') {
            self.pos += 1;
        }
        if self.peek() == Some(b'.') {
            is_float = true;
            self.pos += 1;
            while self.peek().is_some_and(|c| c.is_ascii_digit() || c == b'_') {
                self.pos += 1;
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            let save = self.pos;
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            if self.peek().is_some_and(|c| c.is_ascii_digit()) {
                is_float = true;
                while self.peek().is_some_and(|c| c.is_ascii_digit() || c == b'_') {
                    self.pos += 1;
                }
            } else {
                self.pos = save;
            }
        }
        let text: String = self.text[start..self.pos].chars().filter(|c| *c != '_').collect();
        if matches!(self.peek(), Some(b'j' | b'J')) {
            self.pos += 1;
            self.push(Tok::Imag(Num::parse(&text)));
            return Ok(());
        }
        if is_float {
            self.push(Tok::Float(Num::parse(&text)));
        } else {
            if text.len() > 1 && text.starts_with('0') && text.bytes().any(|b| b != b'0') {
                return self.error("leading zeros in decimal integer literals are not permitted; use an 0o prefix for octal integers");
            }
            match Int::parse_radix(&text, 10) {
                Some(value) => self.push(Tok::Int(value)),
                None => return self.error("invalid decimal literal"),
            }
        }
        Ok(())
    }

    /// From the opening quote, with the prefix already read.
    fn string(&mut self, prefix: &str) -> LResult<Tok> {
        let raw = prefix.contains('r');
        let bytes = prefix.contains('b');
        let fstring = prefix.contains('f');
        let quote = self.src[self.pos];
        let triple = self.peek_at(1) == Some(quote) && self.peek_at(2) == Some(quote);
        self.pos += if triple { 3 } else { 1 };
        let start_line = self.line;
        // The raw body first: escapes are resolved afterwards, f-string
        // fields need the raw text.
        let body_start = self.pos;
        loop {
            let Some(c) = self.peek() else {
                return Err(LexError::new(String::from("unterminated string literal"), start_line));
            };
            if c == b'\\' {
                if self.peek_at(1) == Some(b'\n') {
                    self.line += 1;
                }
                self.pos += 2;
                continue;
            }
            if c == b'\n' {
                if !triple {
                    return Err(LexError::new(String::from("unterminated string literal"), start_line));
                }
                self.line += 1;
            }
            if c == quote {
                if !triple {
                    break;
                }
                if self.peek_at(1) == Some(quote) && self.peek_at(2) == Some(quote) {
                    break;
                }
            }
            self.pos += 1;
        }
        let body = &self.text[body_start..self.pos];
        self.pos += if triple { 3 } else { 1 };
        if fstring {
            let parts = fstring_parts(body, raw, start_line).map_err(|message| LexError::new(message, start_line))?;
            return Ok(Tok::FString(parts));
        }
        let cooked = if raw { String::from(body) } else { unescape(body, bytes).map_err(|message| LexError::new(message, start_line))? };
        if bytes {
            if cooked.chars().any(|c| c as u32 > 0xff) {
                return Err(LexError::new(String::from("bytes can only contain ASCII literal characters"), start_line));
            }
            return Ok(Tok::Bytes(cooked.chars().map(|c| c as u8).collect()));
        }
        Ok(Tok::Str(cooked))
    }
}

/// Resolves the escapes of a string body. In a bytes literal, `\xhh`
/// gives the byte (kept as the char of that code, below 256).
fn unescape(body: &str, bytes: bool) -> Result<String, String> {
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let Some(e) = chars.next() else {
            out.push('\\');
            break;
        };
        match e {
            '\n' => {}
            '\\' => out.push('\\'),
            '\'' => out.push('\''),
            '"' => out.push('"'),
            'a' => out.push('\x07'),
            'b' => out.push('\x08'),
            'f' => out.push('\x0c'),
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            'v' => out.push('\x0b'),
            '0'..='7' => {
                let mut value = e.to_digit(8).unwrap_or(0);
                for _ in 0..2 {
                    match chars.peek().and_then(|d| d.to_digit(8)) {
                        Some(d) => {
                            value = value * 8 + d;
                            chars.next();
                        }
                        None => break,
                    }
                }
                out.push(char::from_u32(value).unwrap_or('\u{fffd}'));
            }
            'x' => {
                let hex: String = (0..2).filter_map(|_| chars.next()).collect();
                let value = u32::from_str_radix(&hex, 16).map_err(|_| String::from("(unicode error) 'unicodeescape' codec can't decode bytes in position 0-1: truncated \\xXX escape"))?;
                out.push(char::from_u32(value).unwrap_or('\u{fffd}'));
            }
            'u' | 'U' if !bytes => {
                let count = if e == 'u' { 4 } else { 8 };
                let hex: String = (0..count).filter_map(|_| chars.next()).collect();
                let value = u32::from_str_radix(&hex, 16).map_err(|_| String::from("(unicode error) 'unicodeescape' codec can't decode bytes in position 0-5: truncated \\uXXXX escape"))?;
                out.push(char::from_u32(value).ok_or_else(|| String::from("(unicode error) 'unicodeescape' codec can't decode bytes in position 0-9: illegal Unicode character"))?);
            }
            'N' if !bytes => {
                if chars.peek() != Some(&'{') {
                    return Err(String::from("(unicode error) 'unicodeescape' codec can't decode bytes: malformed \\N character escape"));
                }
                chars.next();
                let mut name = String::new();
                loop {
                    match chars.next() {
                        Some('}') => break,
                        Some(c) => name.push(c),
                        None => return Err(String::from("(unicode error) 'unicodeescape' codec can't decode bytes: malformed \\N character escape")),
                    }
                }
                match super::modules::unicodedata::lookup(&name).and_then(char::from_u32) {
                    Some(c) => out.push(c),
                    None => return Err(String::from("(unicode error) 'unicodeescape' codec can't decode bytes: unknown Unicode character name")),
                }
            }
            other => {
                // Unknown escapes stay as they are.
                out.push('\\');
                out.push(other);
            }
        }
    }
    Ok(out)
}

/// Splits an f-string body into text and fields.
fn fstring_parts(body: &str, raw: bool, line: u32) -> Result<Vec<FPart>, String> {
    let chars: Vec<char> = body.chars().collect();
    let mut index = 0;
    parse_fparts(&chars, &mut index, raw, line, false)
}

/// Reads parts until the end (or, in a format spec, until the `}` that
/// closes the field).
fn parse_fparts(chars: &[char], index: &mut usize, raw: bool, line: u32, in_spec: bool) -> Result<Vec<FPart>, String> {
    let mut parts = Vec::new();
    let mut text = String::new();
    while *index < chars.len() {
        let c = chars[*index];
        match c {
            '{' if chars.get(*index + 1) == Some(&'{') && !in_spec => {
                text.push('{');
                *index += 2;
            }
            '}' if in_spec => break,
            '}' if chars.get(*index + 1) == Some(&'}') => {
                text.push('}');
                *index += 2;
            }
            '}' => return Err(String::from("f-string: single '}' is not allowed")),
            '{' => {
                if !text.is_empty() {
                    let cooked = if raw { core::mem::take(&mut text) } else { unescape(&core::mem::take(&mut text), false)? };
                    parts.push(FPart::Text(cooked));
                }
                *index += 1;
                parts.push(FPart::Field(parse_field(chars, index, raw, line)?));
            }
            _ => {
                text.push(c);
                *index += 1;
            }
        }
    }
    if !text.is_empty() {
        let cooked = if raw { text } else { unescape(&text, false)? };
        parts.push(FPart::Text(cooked));
    }
    Ok(parts)
}

/// From after `{`, to just after the matching `}`.
fn parse_field(chars: &[char], index: &mut usize, raw: bool, line: u32) -> Result<Field, String> {
    let start = *index;
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    // The expression ends at `!`, `:` or `}` at depth 0, outside strings.
    while *index < chars.len() {
        let c = chars[*index];
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
            *index += 1;
            continue;
        }
        match c {
            '\'' | '"' => quote = Some(c),
            '(' | '[' | '{' => depth += 1,
            ')' | ']' => depth = depth.saturating_sub(1),
            '}' if depth > 0 => depth -= 1,
            '}' => break,
            '!' if depth == 0 && chars.get(*index + 1) != Some(&'=') => break,
            ':' if depth == 0 => break,
            _ => {}
        }
        *index += 1;
    }
    let mut source: String = chars[start..*index].iter().collect();
    let mut debug = None;
    // `{x=}`: the text, then the value (repr by default).
    let trimmed = source.trim_end();
    if trimmed.ends_with('=') && !trimmed.ends_with("==") && !trimmed.ends_with("!=") && !trimmed.ends_with("<=") && !trimmed.ends_with(">=") {
        debug = Some(source.clone());
        let cut = trimmed.len() - 1;
        source.truncate(cut);
    }
    if source.trim().is_empty() {
        return Err(String::from("f-string: valid expression required before '}'"));
    }
    let mut conversion = None;
    if chars.get(*index) == Some(&'!') {
        conversion = chars.get(*index + 1).copied();
        if !matches!(conversion, Some('r' | 's' | 'a')) {
            return Err(String::from("f-string: invalid conversion character: expected 's', 'r', or 'a'"));
        }
        *index += 2;
    }
    let mut spec = None;
    if chars.get(*index) == Some(&':') {
        *index += 1;
        spec = Some(parse_fparts(chars, index, raw, line, true)?);
    }
    if chars.get(*index) != Some(&'}') {
        return Err(String::from("f-string: expecting '}'"));
    }
    *index += 1;
    if debug.is_some() && conversion.is_none() && spec.is_none() {
        conversion = Some('r');
    }
    Ok(Field { source, line, conversion, spec, debug })
}

/// Tokenizes a whole source.
pub fn tokenize(text: &str) -> Result<Vec<Token>, LexError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    Lexer { src: text.as_bytes(), text, pos: 0, line: 1, depth: 0, indents: alloc::vec![0], out: Vec::new(), at_line_start: true, start_line: 0, start_col: 0 }.run()
}
