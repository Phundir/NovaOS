//! JavaScript source to tokens.
//!
//! Two things make a JavaScript lexer more than a tokenizer:
//!
//! * **A `/` depends on what precedes it** (see `DECISIONS.md`, "Un `/`
//!   en JavaScript se lit dans son contexte"): after a value it divides,
//!   elsewhere it opens a regular expression. The decision is made from
//!   the previous token, which is why the whole source is tokenized in
//!   one pass rather than on demand.
//! * **Line breaks matter** for automatic semicolon insertion: each token
//!   records whether a line terminator came before it, and the parser
//!   consults that flag instead of seeing newlines as tokens.
//!
//! Template literals are cut into their text parts and the source spans
//! of their `${...}` expressions; the parser reads those spans on its own.

use alloc::string::String;
use alloc::vec::Vec;

use super::num::Num;
use super::value::JsStr;

#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    /// Identifiers and keywords alike: the parser knows which words are
    /// keywords where.
    Ident(JsStr),
    /// `#name`, a private class member.
    Private(JsStr),
    Num(Num),
    Str(JsStr),
    /// Cooked strings (`None` where an escape is invalid), raw strings,
    /// and the byte spans of the substitutions.
    Template(Vec<Option<JsStr>>, Vec<JsStr>, Vec<(usize, usize)>),
    Regex(JsStr, JsStr),
    Punct(&'static str),
    Eof,
}

#[derive(Clone, Debug)]
pub struct Token {
    pub tok: Tok,
    /// A line terminator precedes this token.
    pub newline: bool,
    /// Byte offset in the source, for error positions.
    pub at: usize,
}

/// Longest first, so that the first match is the right one.
const PUNCTUATORS: &[&str] = &[
    ">>>=", "...", "===", "!==", "**=", "<<=", ">>=", ">>>", "&&=", "||=", "??=", "=>", "==", "!=", "<=", ">=", "&&",
    "||", "??", "?.", "++", "--", "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=", "<<", ">>", "**", "{", "}", "(",
    ")", "[", "]", ";", ",", "<", ">", "+", "-", "*", "/", "%", "&", "|", "^", "!", "~", "?", ":", "=", ".", "@",
];

/// Words after which a `/` starts a regular expression.
const REGEX_AFTER_WORDS: &[&str] = &[
    "return", "typeof", "case", "do", "else", "in", "instanceof", "new", "delete", "void", "throw", "yield", "await",
    "of",
];

pub struct Lexer<'a> {
    source: &'a str,
    bytes: &'a [u8],
    pos: usize,
    /// Offset added to positions: a template substitution is lexed from
    /// a slice of the whole source.
    base: usize,
}

pub type LexResult<T> = Result<T, (String, usize)>;

fn is_id_start(c: char) -> bool {
    c.is_alphabetic() || c == '$' || c == '_'
}

fn is_id_part(c: char) -> bool {
    c.is_alphanumeric() || c == '$' || c == '_' || c == '\u{200c}' || c == '\u{200d}'
}

fn is_line_terminator(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

impl<'a> Lexer<'a> {
    pub fn new(source: &'a str, base: usize) -> Lexer<'a> {
        Lexer { source, bytes: source.as_bytes(), pos: 0, base }
    }

    fn error<T>(&self, message: &str) -> LexResult<T> {
        Err((String::from(message), self.base + self.pos))
    }

    fn peek_char(&self) -> Option<char> {
        self.source[self.pos..].chars().next()
    }

    fn char_at(&self, offset: usize) -> Option<char> {
        self.source.get(self.pos + offset..).and_then(|rest| rest.chars().next())
    }

    /// Skips white space and comments; true when a line terminator was
    /// among them.
    fn skip_trivia(&mut self) -> LexResult<bool> {
        let mut newline = false;
        loop {
            let Some(c) = self.peek_char() else {
                return Ok(newline);
            };
            if is_line_terminator(c) {
                newline = true;
                self.pos += c.len_utf8();
            } else if c.is_whitespace() || c == '\u{feff}' {
                self.pos += c.len_utf8();
            } else if self.source[self.pos..].starts_with("//")
                || (self.source[self.pos..].starts_with("<!--"))
                || (newline && self.source[self.pos..].starts_with("-->"))
                || (self.pos == 0 && self.source.starts_with("#!"))
            {
                // Line comment -- the HTML-like forms included, which
                // old inline scripts still carry.
                while let Some(c) = self.peek_char() {
                    if is_line_terminator(c) {
                        break;
                    }
                    self.pos += c.len_utf8();
                }
            } else if self.source[self.pos..].starts_with("/*") {
                let Some(end) = self.source[self.pos + 2..].find("*/") else {
                    return self.error("commentaire non termine");
                };
                let comment = &self.source[self.pos..self.pos + 2 + end];
                if comment.chars().any(is_line_terminator) {
                    newline = true;
                }
                self.pos += end + 4;
            } else {
                return Ok(newline);
            }
        }
    }

    /// Tokenizes the whole source.
    pub fn tokenize(mut self) -> LexResult<Vec<Token>> {
        let mut tokens: Vec<Token> = Vec::new();
        loop {
            let newline = self.skip_trivia()?;
            let at = self.base + self.pos;
            if self.pos >= self.bytes.len() {
                tokens.push(Token { tok: Tok::Eof, newline: true, at });
                return Ok(tokens);
            }
            let regex_allowed = match tokens.last().map(|t| &t.tok) {
                None => true,
                Some(Tok::Punct(p)) => !matches!(*p, ")" | "]" | "}"),
                Some(Tok::Ident(word)) => REGEX_AFTER_WORDS.contains(&word.as_str()),
                Some(_) => false,
            };
            let tok = self.token(regex_allowed)?;
            tokens.push(Token { tok, newline, at });
        }
    }

    fn token(&mut self, regex_allowed: bool) -> LexResult<Tok> {
        let c = self.peek_char().unwrap_or('\0');
        if is_id_start(c) || c == '\\' {
            return Ok(Tok::Ident(JsStr::from(self.identifier()?.as_str())));
        }
        if c == '#' {
            self.pos += 1;
            return Ok(Tok::Private(JsStr::from(self.identifier()?.as_str())));
        }
        if c.is_ascii_digit() || (c == '.' && self.char_at(1).is_some_and(|d| d.is_ascii_digit())) {
            return self.number();
        }
        if c == '"' || c == '\'' {
            return self.string(c);
        }
        if c == '`' {
            return self.template();
        }
        if c == '/' && regex_allowed {
            return self.regex();
        }
        for punct in PUNCTUATORS {
            if self.source[self.pos..].starts_with(punct) {
                // `?.` followed by a digit is `?` then a number.
                if *punct == "?." && self.char_at(2).is_some_and(|d| d.is_ascii_digit()) {
                    continue;
                }
                self.pos += punct.len();
                return Ok(Tok::Punct(punct));
            }
        }
        self.error("caractere inattendu")
    }

    fn identifier(&mut self) -> LexResult<String> {
        let mut name = String::new();
        loop {
            let Some(c) = self.peek_char() else {
                break;
            };
            if c == '\\' {
                if self.char_at(1) != Some('u') {
                    return self.error("echappement invalide dans un identifiant");
                }
                self.pos += 2;
                let code = self.unicode_escape()?;
                name.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                continue;
            }
            let ok = if name.is_empty() { is_id_start(c) } else { is_id_part(c) };
            if !ok {
                break;
            }
            name.push(c);
            self.pos += c.len_utf8();
        }
        if name.is_empty() {
            return self.error("identifiant attendu");
        }
        Ok(name)
    }

    fn number(&mut self) -> LexResult<Tok> {
        let start = self.pos;
        let rest = &self.source[self.pos..];
        let radix = if rest.len() > 1 && rest.as_bytes()[0] == b'0' {
            match rest.as_bytes()[1] {
                b'x' | b'X' => 16,
                b'o' | b'O' => 8,
                b'b' | b'B' => 2,
                _ => 0,
            }
        } else {
            0
        };
        if radix != 0 {
            self.pos += 2;
            let mut value = Num::ZERO;
            let radix_num = Num::from_i64(radix as i64);
            let mut any = false;
            while let Some(c) = self.peek_char() {
                if c == '_' {
                    self.pos += 1;
                    continue;
                }
                let Some(digit) = c.to_digit(radix) else {
                    break;
                };
                value = value.mul(radix_num).add(Num::from_i64(digit as i64));
                any = true;
                self.pos += 1;
            }
            if !any {
                return self.error("nombre invalide");
            }
            if self.peek_char() == Some('n') {
                self.pos += 1; // BigInt: read as a number
            }
            return Ok(Tok::Num(value));
        }
        // Legacy octal: 0 followed only by octal digits.
        let bytes = rest.as_bytes();
        if bytes.len() > 1 && bytes[0] == b'0' && bytes[1].is_ascii_digit() {
            let end = bytes.iter().position(|b| !b.is_ascii_digit()).unwrap_or(bytes.len());
            let digits = &rest[..end];
            if digits.bytes().all(|b| b < b'8') {
                self.pos += end;
                let mut value = Num::ZERO;
                for digit in digits.bytes() {
                    value = value.mul(Num::from_i64(8)).add(Num::from_i64((digit - b'0') as i64));
                }
                return Ok(Tok::Num(value));
            }
        }
        let Some((value, used)) = Num::parse_prefix(rest) else {
            return self.error("nombre invalide");
        };
        self.pos = start + used;
        if self.peek_char() == Some('n') {
            self.pos += 1;
        }
        if self.peek_char().is_some_and(|c| is_id_start(c) || c.is_ascii_digit()) {
            return self.error("identifiant colle a un nombre");
        }
        Ok(Tok::Num(value))
    }

    /// After `\u`: four hex digits or `{...}`.
    fn unicode_escape(&mut self) -> LexResult<u32> {
        if self.peek_char() == Some('{') {
            let end = match self.source[self.pos..].find('}') {
                Some(end) => end,
                None => return self.error("echappement unicode invalide"),
            };
            let hex = &self.source[self.pos + 1..self.pos + end];
            self.pos += end + 1;
            return match u32::from_str_radix(hex, 16) {
                Ok(code) if code <= 0x10ffff => Ok(code),
                _ => self.error("echappement unicode invalide"),
            };
        }
        let hex = self.source.get(self.pos..self.pos + 4).unwrap_or("");
        match u32::from_str_radix(hex, 16) {
            Ok(code) if hex.len() == 4 => {
                self.pos += 4;
                Ok(code)
            }
            _ => self.error("echappement unicode invalide"),
        }
    }

    /// One escape after the backslash, appended to `out`. Surrogate pairs
    /// written as two `\u` escapes are joined.
    fn escape(&mut self, out: &mut String) -> LexResult<()> {
        let Some(c) = self.peek_char() else {
            return self.error("chaine non terminee");
        };
        self.pos += c.len_utf8();
        match c {
            'n' => out.push('\n'),
            't' => out.push('\t'),
            'r' => out.push('\r'),
            'b' => out.push('\u{8}'),
            'f' => out.push('\u{c}'),
            'v' => out.push('\u{b}'),
            '0' if !self.peek_char().is_some_and(|d| d.is_ascii_digit()) => out.push('\0'),
            '1'..='7' | '0' => {
                // Legacy octal escape.
                let mut value = c.to_digit(8).unwrap_or(0);
                for _ in 0..2 {
                    match self.peek_char().and_then(|d| d.to_digit(8)) {
                        Some(digit) if value * 8 + digit <= 255 => {
                            value = value * 8 + digit;
                            self.pos += 1;
                        }
                        _ => break,
                    }
                }
                out.push(char::from_u32(value).unwrap_or('\u{fffd}'));
            }
            'x' => {
                let hex = self.source.get(self.pos..self.pos + 2).unwrap_or("");
                match u32::from_str_radix(hex, 16) {
                    Ok(code) if hex.len() == 2 => {
                        self.pos += 2;
                        out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                    }
                    _ => return self.error("echappement \\x invalide"),
                }
            }
            'u' => {
                let code = self.unicode_escape()?;
                if (0xd800..0xdc00).contains(&code) && self.source[self.pos..].starts_with("\\u") {
                    let mark = self.pos;
                    self.pos += 2;
                    let low = self.unicode_escape()?;
                    if (0xdc00..0xe000).contains(&low) {
                        let joined = 0x10000 + ((code - 0xd800) << 10) + (low - 0xdc00);
                        out.push(char::from_u32(joined).unwrap_or('\u{fffd}'));
                        return Ok(());
                    }
                    self.pos = mark;
                }
                out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
            }
            '\r' => {
                if self.peek_char() == Some('\n') {
                    self.pos += 1;
                }
            }
            '\n' | '\u{2028}' | '\u{2029}' => {}
            other => out.push(other),
        }
        Ok(())
    }

    fn string(&mut self, quote: char) -> LexResult<Tok> {
        self.pos += 1;
        let mut out = String::new();
        loop {
            let Some(c) = self.peek_char() else {
                return self.error("chaine non terminee");
            };
            if c == quote {
                self.pos += 1;
                return Ok(Tok::Str(JsStr::from(out.as_str())));
            }
            if c == '\n' || c == '\r' {
                return self.error("chaine non terminee");
            }
            self.pos += c.len_utf8();
            if c == '\\' {
                self.escape(&mut out)?;
            } else {
                out.push(c);
            }
        }
    }

    fn template(&mut self) -> LexResult<Tok> {
        self.pos += 1;
        let mut cooked: Vec<Option<JsStr>> = Vec::new();
        let mut raws: Vec<JsStr> = Vec::new();
        let mut spans: Vec<(usize, usize)> = Vec::new();
        let mut text = String::new();
        let mut valid = true;
        let mut raw_start = self.pos;
        loop {
            let Some(c) = self.peek_char() else {
                return self.error("gabarit non termine");
            };
            if c == '`' {
                raws.push(JsStr::from(self.source[raw_start..self.pos].replace("\r\n", "\n").as_str()));
                cooked.push(if valid { Some(JsStr::from(text.as_str())) } else { None });
                self.pos += 1;
                return Ok(Tok::Template(cooked, raws, spans));
            }
            if c == '$' && self.char_at(1) == Some('{') {
                raws.push(JsStr::from(self.source[raw_start..self.pos].replace("\r\n", "\n").as_str()));
                cooked.push(if valid { Some(JsStr::from(text.as_str())) } else { None });
                text.clear();
                valid = true;
                self.pos += 2;
                let start = self.pos;
                let end = self.skip_substitution()?;
                spans.push((self.base + start, self.base + end));
                self.pos = end + 1;
                raw_start = self.pos;
                continue;
            }
            if c == '\\' {
                self.pos += 1;
                let mark = self.pos;
                if self.escape(&mut text).is_err() {
                    // Invalid escapes are allowed in tagged templates;
                    // the cooked value is then undefined.
                    valid = false;
                    self.pos = mark + self.peek_char().map_or(0, |c| c.len_utf8());
                }
                continue;
            }
            if c == '\r' {
                self.pos += 1;
                if self.peek_char() == Some('\n') {
                    self.pos += 1;
                }
                text.push('\n');
                continue;
            }
            text.push(c);
            self.pos += c.len_utf8();
        }
    }

    /// From just after `${`, finds the matching `}` by lexing: braces in
    /// strings, comments, regular expressions and nested templates do
    /// not count.
    fn skip_substitution(&mut self) -> LexResult<usize> {
        let mut depth = 0usize;
        let mut previous: Option<Tok> = None;
        loop {
            self.skip_trivia()?;
            if self.pos >= self.bytes.len() {
                return self.error("gabarit non termine");
            }
            let regex_allowed = match &previous {
                None => true,
                Some(Tok::Punct(p)) => !matches!(*p, ")" | "]" | "}"),
                Some(Tok::Ident(word)) => REGEX_AFTER_WORDS.contains(&word.as_str()),
                Some(_) => false,
            };
            let at = self.pos;
            let tok = self.token(regex_allowed)?;
            match tok {
                Tok::Punct("{") => depth += 1,
                Tok::Punct("}") => {
                    if depth == 0 {
                        return Ok(at);
                    }
                    depth -= 1;
                }
                _ => {}
            }
            previous = Some(tok);
        }
    }

    fn regex(&mut self) -> LexResult<Tok> {
        self.pos += 1;
        let start = self.pos;
        let mut in_class = false;
        loop {
            let Some(c) = self.peek_char() else {
                return self.error("expression reguliere non terminee");
            };
            if is_line_terminator(c) {
                return self.error("expression reguliere non terminee");
            }
            self.pos += c.len_utf8();
            match c {
                '\\' => {
                    if let Some(next) = self.peek_char() {
                        self.pos += next.len_utf8();
                    }
                }
                '[' => in_class = true,
                ']' => in_class = false,
                '/' if !in_class => break,
                _ => {}
            }
        }
        let pattern = &self.source[start..self.pos - 1];
        let flags_start = self.pos;
        while self.peek_char().is_some_and(is_id_part) {
            self.pos += 1;
        }
        let flags = &self.source[flags_start..self.pos];
        Ok(Tok::Regex(JsStr::from(pattern), JsStr::from(flags)))
    }
}

/// Line and column (1-based) of a byte offset, for messages.
pub fn position(source: &str, at: usize) -> (usize, usize) {
    let before = &source[..at.min(source.len())];
    let line = before.matches('\n').count() + 1;
    let column = before.rfind('\n').map_or(before.len(), |nl| before.len() - nl - 1) + 1;
    (line, column)
}
