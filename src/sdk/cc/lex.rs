//! Tokens of C, as translation phases 1 to 3 cut them.
//!
//! The lexer keeps the **spelling** of each token and nothing else: a
//! number stays `0x1Fu` until the parser converts it, a string keeps its
//! quotes and escapes. That is what the preprocessor needs (`#` and `##`
//! work on spellings, and a pp-number such as `1e+` is not a number yet),
//! and it means a token is never converted twice.

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use super::CcError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Ident,
    Punct,
    /// A preprocessing number: what the parser will read as an integer or
    /// floating constant.
    Number,
    /// A string literal, quotes and prefix included.
    Str,
    /// A character constant, quotes included.
    Char,
}

/// Where a token comes from: a file (index into the compiler's list of
/// names) and a line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Loc {
    pub file: u32,
    pub line: u32,
}

#[derive(Debug, Clone)]
pub struct Token {
    pub kind: Kind,
    pub text: Rc<str>,
    pub loc: Loc,
    /// First token of its line (for `#` directives).
    pub bol: bool,
    /// Preceded by blank space (for `#` stringizing and `(` of a
    /// function-like `#define`).
    pub space: bool,
    /// Names of the macros this token came out of: a macro is not
    /// expanded again inside its own expansion (C11 6.10.3.4).
    pub hide: Rc<Vec<Rc<str>>>,
}

impl Token {
    pub fn is(&self, text: &str) -> bool {
        self.kind != Kind::Str && self.kind != Kind::Char && &*self.text == text
    }

    pub fn hidden(&self, name: &str) -> bool {
        self.hide.iter().any(|h| &**h == name)
    }
}

/// Punctuators, longest first.
const PUNCTUATORS: &[&str] = &[
    "<<=", ">>=", "...", "==", "!=", "<=", ">=", "->", "+=", "-=", "*=", "/=", "++", "--", "%=",
    "&=", "|=", "^=", "&&", "||", "<<", ">>", "##",
];

fn ident_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_' || c == b'$'
}

fn ident_continue(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'$'
}

/// Splits a source file into tokens. `file` is its index for messages.
pub fn tokenize(source: &str, file: u32) -> Result<Vec<Token>, CcError> {
    let bytes = source.as_bytes();
    let empty: Rc<Vec<Rc<str>>> = Rc::new(Vec::new());
    let mut tokens = Vec::new();
    let mut i = 0;
    let mut line = 1u32;
    let mut bol = true;
    let mut space = false;
    let error = |line: u32, message: &str| CcError::at(Loc { file, line }, message);

    // Phase 2: a backslash-newline joins two lines. Done on the fly: the
    // line counter still advances, so messages name the physical line.
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'\\' && bytes.get(i + 1) == Some(&b'\n') {
            i += 2;
            line += 1;
            continue;
        }
        if c == b'\\' && bytes.get(i + 1) == Some(&b'\r') && bytes.get(i + 2) == Some(&b'\n') {
            i += 3;
            line += 1;
            continue;
        }
        if c == b'\n' {
            i += 1;
            line += 1;
            bol = true;
            space = false;
            continue;
        }
        if c == b' ' || c == b'\t' || c == b'\r' || c == 0x0B || c == 0x0C {
            i += 1;
            space = true;
            continue;
        }
        if c == b'/' && bytes.get(i + 1) == Some(&b'/') {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            space = true;
            continue;
        }
        if c == b'/' && bytes.get(i + 1) == Some(&b'*') {
            let start = line;
            i += 2;
            loop {
                if i + 1 >= bytes.len() {
                    return Err(error(start, "commentaire /* jamais ferme"));
                }
                if bytes[i] == b'*' && bytes[i + 1] == b'/' {
                    i += 2;
                    break;
                }
                if bytes[i] == b'\n' {
                    line += 1;
                }
                i += 1;
            }
            space = true;
            continue;
        }

        let start = i;
        let token_line = line;
        let kind;
        if c.is_ascii_digit() || (c == b'.' && bytes.get(i + 1).is_some_and(u8::is_ascii_digit)) {
            // A pp-number: digits, letters, `_`, `.`, and a sign right
            // after an exponent letter.
            i += 1;
            while i < bytes.len() {
                let d = bytes[i];
                if (d == b'+' || d == b'-') && matches!(bytes[i - 1], b'e' | b'E' | b'p' | b'P') {
                    i += 1;
                } else if ident_continue(d) || d == b'.' {
                    i += 1;
                } else {
                    break;
                }
            }
            kind = Kind::Number;
        } else if c == b'"'
            || c == b'\''
            || ((c == b'L' || c == b'u' || c == b'U') && matches!(bytes.get(i + 1), Some(b'"') | Some(b'\'')))
            || (c == b'u' && bytes.get(i + 1) == Some(&b'8') && bytes.get(i + 2) == Some(&b'"'))
        {
            while bytes[i] != b'"' && bytes[i] != b'\'' {
                i += 1;
            }
            let quote = bytes[i];
            i += 1;
            loop {
                match bytes.get(i) {
                    None | Some(b'\n') => {
                        return Err(error(token_line, if quote == b'"' { "chaine jamais fermee" } else { "caractere jamais ferme" }));
                    }
                    Some(b'\\') => i += 2,
                    Some(&d) if d == quote => {
                        i += 1;
                        break;
                    }
                    _ => i += 1,
                }
            }
            kind = if quote == b'"' { Kind::Str } else { Kind::Char };
        } else if ident_start(c) {
            i += 1;
            while i < bytes.len() && ident_continue(bytes[i]) {
                i += 1;
            }
            kind = Kind::Ident;
        } else if c.is_ascii_punctuation() {
            let rest = &source[i..];
            let length = PUNCTUATORS.iter().find(|p| rest.starts_with(**p)).map(|p| p.len()).unwrap_or(1);
            i += length;
            kind = Kind::Punct;
        } else {
            return Err(error(line, "caractere inattendu dans le source"));
        }

        tokens.push(Token {
            kind,
            text: Rc::from(&source[start..i]),
            loc: Loc { file, line: token_line },
            bol,
            space,
            hide: empty.clone(),
        });
        bol = false;
        space = false;
    }
    Ok(tokens)
}

/// Decodes the escapes of a string or character literal body (quotes
/// removed). Octal and hexadecimal escapes give one byte each.
pub fn unescape(body: &str, loc: Loc) -> Result<Vec<u8>, CcError> {
    let bytes = body.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\\' {
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        i += 1;
        let Some(&c) = bytes.get(i) else {
            return Err(CcError::at(loc, "echappement incomplet"));
        };
        i += 1;
        let byte = match c {
            b'n' => b'\n',
            b't' => b'\t',
            b'r' => b'\r',
            b'0'..=b'7' => {
                let mut value = (c - b'0') as u32;
                let mut count = 1;
                while count < 3 && i < bytes.len() && (b'0'..=b'7').contains(&bytes[i]) {
                    value = value * 8 + (bytes[i] - b'0') as u32;
                    i += 1;
                    count += 1;
                }
                value as u8
            }
            b'x' => {
                let mut value: u32 = 0;
                let start = i;
                while i < bytes.len() && bytes[i].is_ascii_hexdigit() {
                    value = value.wrapping_mul(16) + (bytes[i] as char).to_digit(16).unwrap_or(0);
                    i += 1;
                }
                if i == start {
                    return Err(CcError::at(loc, "\\x sans chiffre hexadecimal"));
                }
                value as u8
            }
            b'a' => 7,
            b'b' => 8,
            b'f' => 12,
            b'v' => 11,
            b'e' => 27,
            b'\\' | b'\'' | b'"' | b'?' => c,
            _ => return Err(CcError::at(loc, "sequence d'echappement inconnue")),
        };
        out.push(byte);
    }
    Ok(out)
}

/// The spelling of a run of tokens, as `#` stringizes it: one space
/// where the source had blank space, none elsewhere.
pub fn spell(tokens: &[Token]) -> String {
    let mut out = String::new();
    for (index, token) in tokens.iter().enumerate() {
        if index > 0 && token.space {
            out.push(' ');
        }
        out.push_str(&token.text);
    }
    out
}

/// A string literal token for `text` (quoted and escaped).
pub fn string_token(text: &str, like: &Token) -> Token {
    let mut quoted = String::from("\"");
    for c in text.chars() {
        if c == '"' || c == '\\' {
            quoted.push('\\');
        }
        quoted.push(c);
    }
    quoted.push('"');
    Token { kind: Kind::Str, text: Rc::from(quoted.as_str()), ..like.clone() }
}
