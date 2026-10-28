//! 9.5.1 -- Lexer for the Rust subset.
//!
//! ## What it accepts, and what it rejects on sight
//!
//! The token set is Rust's, restricted to what the parser can build a
//! meaning for. A character the subset has no use for is refused **here**
//! rather than further on: a lexer that swallowed `'a` and let the parser
//! discover the lifetime would report the problem at the wrong place, and
//! the message would name a construct the programmer did not write.
//!
//! ## Line numbers are carried, not recomputed
//!
//! Every token remembers the line it came from. Recovering it afterwards
//! from an offset means re-scanning the source for newlines, and every
//! error message would pay for it. One `usize` per token is cheaper than
//! being unable to say where.
//!
//! ## Only printable ASCII in a string
//!
//! Same rule as `sdk::script` and `sdk::asm`, and for the same reason:
//! the VGA writer replaces anything outside 0x20..0x7E with a solid
//! square. A program that prints an accent would print a black block, and
//! nothing between here and the screen would say why. The escapes `\n`,
//! `\t`, `\\`, `\"` and `\0` are decoded, since those are not characters
//! the writer chokes on -- `\n` is what makes a multi-line message.

use alloc::string::String;
use alloc::vec::Vec;

/// Keywords of the subset.
///
/// The list is deliberately larger than what the parser handles: `struct`,
/// `impl`, `match` and their kind are lexed as keywords precisely so the
/// parser can refuse them **by name**. A word it did not know would come
/// through as an identifier and produce "unexpected token", which says
/// nothing.
pub const KEYWORDS: &[&str] = &[
    // handled
    "fn", "let", "mut", "const", "if", "else", "while", "loop", "break", "continue", "return",
    "true", "false", "as",
    // recognised in order to be refused with their own message
    "struct", "enum", "trait", "impl", "use", "mod", "match", "for", "in", "unsafe", "async",
    "await", "move", "ref", "static", "type", "where", "dyn", "pub", "crate", "self", "super",
    "Self", "box",
];

/// Symbols, longest first.
///
/// The order is the whole correctness of the scan: with `=` before `==`,
/// every equality test in every program would read as two assignments.
pub const SYMBOLS: &[&str] = &[
    "<<=", ">>=", "&&", "||", "==", "!=", "<=", ">=", "->", "=>", "::", "+=", "-=", "*=", "/=",
    "%=", "&=", "|=", "^=", "<<", ">>", "..", "+", "-", "*", "/", "%", "!", "&", "|", "^", "<",
    ">", "=", "(", ")", "{", "}", "[", "]", ",", ";", ":", ".", "?", "@", "#", "$", "~",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token {
    /// An integer literal, with the suffix it carried (`42i64` -> "i64").
    Int(u64, Option<String>),
    Text(String),
    Ident(String),
    Keyword(&'static str),
    Symbol(&'static str),
    /// `name!` -- a macro invocation. Kept distinct from an identifier so
    /// that `println` and `println!` cannot be confused.
    Macro(String),
    End,
}

impl Token {
    pub fn describe(&self) -> String {
        match self {
            Token::Int(value, _) => alloc::format!("{}", value),
            Token::Text(_) => String::from("chaine"),
            Token::Ident(name) => name.clone(),
            Token::Keyword(word) => String::from(*word),
            Token::Symbol(symbol) => String::from(*symbol),
            Token::Macro(name) => alloc::format!("{}!", name),
            Token::End => String::from("fin de fichier"),
        }
    }
}

/// A token and the line it was read on.
#[derive(Debug, Clone)]
pub struct Lexed {
    pub token: Token,
    pub line: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LexError {
    /// A character no token can start with.
    BadCharacter(usize, char),
    /// Unterminated string, or a byte outside the printable range.
    BadString(usize),
    /// An escape sequence the subset does not decode.
    BadEscape(usize, char),
    /// An integer literal that does not fit in 64 bits, or is malformed.
    BadInteger(usize, String),
    /// A character literal: the subset has no `char` type.
    CharLiteral(usize),
    /// A lifetime: the subset has no borrows to name.
    Lifetime(usize, String),
}

impl LexError {
    pub fn message(&self) -> String {
        match self {
            LexError::BadCharacter(line, found) => {
                alloc::format!("ligne {} : caractere invalide '{}'", line, found)
            }
            LexError::BadString(line) => alloc::format!(
                "ligne {} : chaine non terminee, ou caractere hors ASCII imprimable",
                line
            ),
            LexError::BadEscape(line, found) => {
                alloc::format!("ligne {} : echappement inconnu \"\\{}\"", line, found)
            }
            LexError::BadInteger(line, text) => {
                alloc::format!("ligne {} : entier invalide \"{}\"", line, text)
            }
            LexError::CharLiteral(line) => alloc::format!(
                "ligne {} : le litteral de caractere n'existe pas dans ce sous-ensemble - utiliser un entier ou une chaine",
                line
            ),
            LexError::Lifetime(line, name) => alloc::format!(
                "ligne {} : duree de vie '{} - ce sous-ensemble n'a pas d'emprunt a nommer",
                line, name
            ),
        }
    }
}

/// Turns a source into a token stream.
pub fn lex(source: &str) -> Result<Vec<Lexed>, LexError> {
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0;
    let mut line = 1;

    while index < bytes.len() {
        let byte = bytes[index];

        if byte == b'\n' {
            line += 1;
            index += 1;
            continue;
        }
        if byte.is_ascii_whitespace() {
            index += 1;
            continue;
        }

        // --- comments ---
        if byte == b'/' && index + 1 < bytes.len() {
            if bytes[index + 1] == b'/' {
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
                continue;
            }
            if bytes[index + 1] == b'*' {
                // Block comments nest in Rust, and the nesting is not a
                // detail: `/* /* */ */` closes once for a scanner that
                // stops at the first `*/`, and the rest of the file is
                // then read as code.
                let mut depth = 1;
                index += 2;
                while index < bytes.len() && depth != 0 {
                    if bytes[index] == b'\n' {
                        line += 1;
                    }
                    if index + 1 < bytes.len() && bytes[index] == b'/' && bytes[index + 1] == b'*' {
                        depth += 1;
                        index += 2;
                        continue;
                    }
                    if index + 1 < bytes.len() && bytes[index] == b'*' && bytes[index + 1] == b'/' {
                        depth -= 1;
                        index += 2;
                        continue;
                    }
                    index += 1;
                }
                continue;
            }
        }

        // --- attributes: #[...] and #![...] ---
        //
        // Skipped whole rather than tokenised. An attribute changes what
        // the compiler does, and this one does nothing with any of them:
        // reading them in would mean pretending to honour them.
        if byte == b'#' {
            let mut cursor = index + 1;
            if cursor < bytes.len() && bytes[cursor] == b'!' {
                cursor += 1;
            }
            if cursor < bytes.len() && bytes[cursor] == b'[' {
                let mut depth = 0;
                while cursor < bytes.len() {
                    match bytes[cursor] {
                        b'[' => depth += 1,
                        b']' => {
                            depth -= 1;
                            if depth == 0 {
                                cursor += 1;
                                break;
                            }
                        }
                        b'\n' => line += 1,
                        _ => {}
                    }
                    cursor += 1;
                }
                index = cursor;
                continue;
            }
        }

        // --- string literal ---
        if byte == b'"' {
            let (text, next) = scan_string(source, index, line)?;
            tokens.push(Lexed {
                token: Token::Text(text),
                line,
            });
            index = next;
            continue;
        }

        // --- character literal or lifetime ---
        if byte == b'\'' {
            // `'a` followed by anything other than a closing quote is a
            // lifetime; `'x'` is a character literal. Both are refused,
            // and they are told apart so that each gets its own message.
            let mut cursor = index + 1;
            while cursor < bytes.len() && (bytes[cursor].is_ascii_alphanumeric() || bytes[cursor] == b'_') {
                cursor += 1;
            }
            if cursor < bytes.len() && bytes[cursor] == b'\'' {
                return Err(LexError::CharLiteral(line));
            }
            let name = String::from(&source[index + 1..cursor]);
            return Err(LexError::Lifetime(line, name));
        }

        // --- integer literal ---
        if byte.is_ascii_digit() {
            let start = index;
            while index < bytes.len()
                && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_')
            {
                index += 1;
            }
            let text = &source[start..index];
            let (value, suffix) = scan_integer(text).ok_or_else(|| {
                LexError::BadInteger(line, String::from(text))
            })?;
            tokens.push(Lexed {
                token: Token::Int(value, suffix),
                line,
            });
            continue;
        }

        // --- identifier, keyword or macro ---
        if byte.is_ascii_alphabetic() || byte == b'_' {
            let start = index;
            while index < bytes.len()
                && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_')
            {
                index += 1;
            }
            let word = &source[start..index];

            // `name!` is one token. Splitting it would let `println` and
            // `println!` reach the parser as the same thing, and the
            // subset has no function named `println`.
            if index < bytes.len() && bytes[index] == b'!' {
                // `a != b` must not be read as a macro `a!`. The
                // difference is the character after the `!`.
                let is_macro = index + 1 >= bytes.len() || bytes[index + 1] != b'=';
                if is_macro {
                    index += 1;
                    tokens.push(Lexed {
                        token: Token::Macro(String::from(word)),
                        line,
                    });
                    continue;
                }
            }

            let token = match KEYWORDS.iter().find(|keyword| **keyword == word) {
                Some(keyword) => Token::Keyword(keyword),
                None => Token::Ident(String::from(word)),
            };
            tokens.push(Lexed { token, line });
            continue;
        }

        // --- symbol ---
        let rest = &source[index..];
        match SYMBOLS.iter().find(|symbol| rest.starts_with(**symbol)) {
            Some(symbol) => {
                tokens.push(Lexed {
                    token: Token::Symbol(symbol),
                    line,
                });
                index += symbol.len();
            }
            None => return Err(LexError::BadCharacter(line, byte as char)),
        }
    }

    tokens.push(Lexed {
        token: Token::End,
        line,
    });
    Ok(tokens)
}

/// Reads a string literal, escapes decoded. Returns the text and the
/// index just past the closing quote.
fn scan_string(source: &str, start: usize, line: usize) -> Result<(String, usize), LexError> {
    let bytes = source.as_bytes();
    let mut index = start + 1;
    let mut text = String::new();

    while index < bytes.len() {
        match bytes[index] {
            b'"' => return Ok((text, index + 1)),
            b'\n' => return Err(LexError::BadString(line)),
            b'\\' => {
                index += 1;
                if index >= bytes.len() {
                    return Err(LexError::BadString(line));
                }
                let decoded = match bytes[index] {
                    b'n' => '\n',
                    b't' => '\t',
                    b'r' => '\r',
                    b'0' => '\0',
                    b'\\' => '\\',
                    b'"' => '"',
                    other => return Err(LexError::BadEscape(line, other as char)),
                };
                text.push(decoded);
                index += 1;
            }
            other => {
                // The printable range, plus the escapes decoded above.
                // Everything else becomes a solid square on screen.
                if !(0x20..=0x7E).contains(&other) {
                    return Err(LexError::BadString(line));
                }
                text.push(other as char);
                index += 1;
            }
        }
    }
    Err(LexError::BadString(line))
}

/// Decodes an integer literal and its optional type suffix.
///
/// The separator `_` is dropped, `0x`, `0o` and `0b` are honoured. The
/// value is read as `u64`: a negative literal is a unary minus applied to
/// a positive one, which is what Rust does too -- and it is why
/// `-9223372036854775808` parses, whereas reading the digits straight
/// into an `i64` would overflow on the way in.
fn scan_integer(text: &str) -> Option<(u64, Option<String>)> {
    let mut digits = String::new();
    let mut suffix = String::new();
    let mut radix = 10;
    let mut cursor = text;

    if let Some(rest) = cursor.strip_prefix("0x") {
        radix = 16;
        cursor = rest;
    } else if let Some(rest) = cursor.strip_prefix("0o") {
        radix = 8;
        cursor = rest;
    } else if let Some(rest) = cursor.strip_prefix("0b") {
        radix = 2;
        cursor = rest;
    }

    for character in cursor.chars() {
        if character == '_' {
            continue;
        }
        if suffix.is_empty() && character.is_digit(radix) {
            digits.push(character);
        } else {
            suffix.push(character);
        }
    }

    if digits.is_empty() {
        return None;
    }
    let value = u64::from_str_radix(&digits, radix).ok()?;
    let suffix = if suffix.is_empty() {
        None
    } else {
        Some(suffix)
    };
    Some((value, suffix))
}
