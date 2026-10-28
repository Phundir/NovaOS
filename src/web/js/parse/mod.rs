//! Tokens to syntax tree: a recursive-descent parser for ECMAScript 2022
//! scripts and modules, and for TypeScript with its types erased.
//!
//! What it covers: declarations (`var`/`let`/`const` with destructuring,
//! functions, classes with fields, accessors, statics and private
//! members), every statement, arrow functions, async functions and
//! generators, default and rest parameters, spread, template literals
//! (tagged too), optional chaining, `??`, `**` and the logical
//! assignments.
//!
//! **Modules** (`Options::module`): `import` and `export` at the top
//! level are taken out of the body into `Program::module`, which the
//! interpreter links before the body runs (`module.rs` here). In a
//! classic script they stay a syntax error.
//!
//! **TypeScript** (`Options::typescript`): types are read and dropped,
//! as `tsc --isolatedModules` or Node's type stripping do -- annotations,
//! `as`, `!`, generics, `interface`, `type`, `declare`, overloads; what
//! has a value at run time is translated: `enum`, parameter properties
//! (`ts.rs`). No type is checked.
//!
//! **Depth is bounded.** Nesting in the source becomes recursion here, and
//! the source comes from the network: every descent checks the stack
//! headroom and a depth counter, and a script nested too deeply is a
//! syntax error rather than a stack overflow.
//!
//! The parser is split by grammar: statements and patterns (`stmt.rs`),
//! expressions (`expr.rs`), functions and classes (`func.rs`), module
//! declarations (`module.rs`), TypeScript (`ts.rs`).

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

mod expr;
mod func;
mod module;
mod stmt;
mod ts;

use super::ast::*;
use super::lex::{Lexer, Tok, Token};
use super::num::Num;
use super::value::JsStr;

/// Nesting depth allowed in the source.
const MAX_DEPTH: usize = 300;
/// Stack kept free while parsing.
const MIN_HEADROOM: usize = 48 * 1024;

/// What kind of source is parsed.
#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    /// A module: `import`/`export` allowed at the top level, strict mode,
    /// top-level `await`.
    pub module: bool,
    /// TypeScript: types read and erased.
    pub typescript: bool,
}

impl Options {
    /// A classic script, as pages have.
    pub const SCRIPT: Options = Options { module: false, typescript: false };
}

pub struct ParseError {
    pub message: String,
    pub at: usize,
}

type PResult<T> = Result<T, ParseError>;

/// Per-function bookkeeping while its body is parsed.
struct FunctionState {
    vars: Vec<JsStr>,
    uses_arguments: bool,
    arrow: bool,
    is_async: bool,
    generator: bool,
}

pub struct Parser<'a> {
    source: &'a str,
    tokens: Vec<Token>,
    pos: usize,
    depth: usize,
    functions: Vec<FunctionState>,
    /// Identifier names shared: scope lookups compare pointers first.
    names: BTreeMap<String, JsStr>,
    headroom: &'a dyn Fn() -> usize,
    /// `in` is not an operator here (the head of a `for`).
    no_in: bool,
    options: Options,
    /// The module's declarations, gathered while its body is parsed.
    module: ModuleInfo,
    /// TypeScript parameter properties (`constructor(private x)`) of the
    /// parameter list just parsed.
    param_properties: Vec<JsStr>,
    /// TypeScript: the function just parsed was a signature without a
    /// body (an overload, `abstract`, `declare`).
    signature: bool,
    /// Tokens cut in two by a type (`>>` closing two type argument
    /// lists): position and former token, to undo a guess that failed.
    splits: Vec<(usize, Tok)>,
}

fn is_reserved(word: &str) -> bool {
    matches!(
        word,
        "break" | "case" | "catch" | "class" | "const" | "continue" | "debugger" | "default" | "delete" | "do"
            | "else" | "export" | "extends" | "finally" | "for" | "function" | "if" | "import" | "in"
            | "instanceof" | "new" | "return" | "super" | "switch" | "this" | "throw" | "try" | "typeof"
            | "var" | "void" | "while" | "with" | "null" | "true" | "false" | "enum"
    )
}

fn assign_op(punct: &str) -> Option<AssignOp> {
    Some(match punct {
        "=" => AssignOp::Assign,
        "+=" => AssignOp::Op(BinOp::Add),
        "-=" => AssignOp::Op(BinOp::Sub),
        "*=" => AssignOp::Op(BinOp::Mul),
        "/=" => AssignOp::Op(BinOp::Div),
        "%=" => AssignOp::Op(BinOp::Rem),
        "**=" => AssignOp::Op(BinOp::Exp),
        "<<=" => AssignOp::Op(BinOp::Shl),
        ">>=" => AssignOp::Op(BinOp::Shr),
        ">>>=" => AssignOp::Op(BinOp::UShr),
        "&=" => AssignOp::Op(BinOp::BitAnd),
        "|=" => AssignOp::Op(BinOp::BitOr),
        "^=" => AssignOp::Op(BinOp::BitXor),
        "&&=" => AssignOp::Logic(LogicOp::And),
        "||=" => AssignOp::Logic(LogicOp::Or),
        "??=" => AssignOp::Logic(LogicOp::Nullish),
        _ => return None,
    })
}

/// Binary operators by precedence, loosest first.
fn binary_precedence(punct: &str, no_in: bool) -> Option<(u8, Option<BinOp>, Option<LogicOp>)> {
    Some(match punct {
        "??" => (1, None, Some(LogicOp::Nullish)),
        "||" => (2, None, Some(LogicOp::Or)),
        "&&" => (3, None, Some(LogicOp::And)),
        "|" => (4, Some(BinOp::BitOr), None),
        "^" => (5, Some(BinOp::BitXor), None),
        "&" => (6, Some(BinOp::BitAnd), None),
        "==" => (7, Some(BinOp::Eq), None),
        "!=" => (7, Some(BinOp::Ne), None),
        "===" => (7, Some(BinOp::StrictEq), None),
        "!==" => (7, Some(BinOp::StrictNe), None),
        "<" => (8, Some(BinOp::Lt), None),
        ">" => (8, Some(BinOp::Gt), None),
        "<=" => (8, Some(BinOp::Le), None),
        ">=" => (8, Some(BinOp::Ge), None),
        "instanceof" => (8, Some(BinOp::InstanceOf), None),
        "in" if !no_in => (8, Some(BinOp::In), None),
        "<<" => (9, Some(BinOp::Shl), None),
        ">>" => (9, Some(BinOp::Shr), None),
        ">>>" => (9, Some(BinOp::UShr), None),
        "+" => (10, Some(BinOp::Add), None),
        "-" => (10, Some(BinOp::Sub), None),
        "*" => (11, Some(BinOp::Mul), None),
        "/" => (11, Some(BinOp::Div), None),
        "%" => (11, Some(BinOp::Rem), None),
        "**" => (12, Some(BinOp::Exp), None),
        _ => return None,
    })
}

impl<'a> Parser<'a> {
    pub fn new(source: &'a str, headroom: &'a dyn Fn() -> usize) -> PResult<Parser<'a>> {
        Parser::with_options(source, headroom, Options::SCRIPT)
    }

    pub fn with_options(source: &'a str, headroom: &'a dyn Fn() -> usize, options: Options) -> PResult<Parser<'a>> {
        let tokens = Lexer::new(source, 0).tokenize().map_err(|(message, at)| ParseError { message, at })?;
        Ok(Parser {
            source,
            tokens,
            pos: 0,
            depth: 0,
            functions: Vec::new(),
            names: BTreeMap::new(),
            headroom,
            no_in: false,
            options,
            module: ModuleInfo::default(),
            param_properties: Vec::new(),
            signature: false,
            splits: Vec::new(),
        })
    }

    // --------------------------------------------------------
    //  Token access
    // --------------------------------------------------------

    fn peek(&self) -> &Tok {
        &self.tokens[self.pos].tok
    }

    fn peek_at(&self, offset: usize) -> &Tok {
        let index = (self.pos + offset).min(self.tokens.len() - 1);
        &self.tokens[index].tok
    }

    fn newline_before(&self) -> bool {
        self.tokens[self.pos].newline
    }

    fn at(&self) -> usize {
        self.tokens[self.pos].at
    }

    fn advance(&mut self) -> Tok {
        let tok = self.tokens[self.pos].tok.clone();
        if self.pos + 1 < self.tokens.len() {
            self.pos += 1;
        }
        tok
    }

    fn error<T>(&self, message: &str) -> PResult<T> {
        Err(ParseError { message: String::from(message), at: self.at() })
    }

    fn is(&self, punct: &str) -> bool {
        matches!(self.peek(), Tok::Punct(p) if *p == punct)
    }

    fn is_word(&self, word: &str) -> bool {
        matches!(self.peek(), Tok::Ident(w) if w.as_str() == word)
    }

    fn word_at(&self, offset: usize, word: &str) -> bool {
        matches!(self.peek_at(offset), Tok::Ident(w) if w.as_str() == word)
    }

    fn punct_at(&self, offset: usize, punct: &str) -> bool {
        matches!(self.peek_at(offset), Tok::Punct(p) if *p == punct)
    }

    fn eat(&mut self, punct: &str) -> bool {
        if self.is(punct) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn eat_word(&mut self, word: &str) -> bool {
        if self.is_word(word) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, punct: &str) -> PResult<()> {
        if self.eat(punct) {
            Ok(())
        } else {
            let message = alloc::format!("\"{}\" attendu", punct);
            self.error(&message)
        }
    }

    /// Automatic semicolon insertion.
    fn semicolon(&mut self) -> PResult<()> {
        if self.eat(";") {
            return Ok(());
        }
        if self.is("}") || matches!(self.peek(), Tok::Eof) || self.newline_before() {
            return Ok(());
        }
        self.error("\";\" attendu")
    }

    fn intern(&mut self, name: &str) -> JsStr {
        if let Some(existing) = self.names.get(name) {
            return existing.clone();
        }
        let interned = JsStr::from(name);
        self.names.insert(String::from(name), interned.clone());
        interned
    }

    fn identifier(&mut self) -> PResult<JsStr> {
        match self.peek().clone() {
            Tok::Ident(name) if !is_reserved(&name) => {
                self.advance();
                Ok(self.intern(&name))
            }
            _ => self.error("identifiant attendu"),
        }
    }

    fn enter(&mut self) -> PResult<()> {
        self.depth += 1;
        if self.depth > MAX_DEPTH || (self.headroom)() < MIN_HEADROOM {
            return self.error("imbrication trop profonde");
        }
        Ok(())
    }

    fn leave(&mut self) {
        self.depth -= 1;
    }

    fn declare_var(&mut self, name: &JsStr) {
        if let Some(state) = self.functions.last_mut() {
            if !state.vars.iter().any(|n| n == name) {
                state.vars.push(name.clone());
            }
        }
    }

    fn declare_pattern_vars(&mut self, pattern: &Pattern) {
        let mut names = Vec::new();
        pattern_names(pattern, &mut names);
        for name in names {
            self.declare_var(&name);
        }
    }

    fn in_async(&self) -> bool {
        self.functions.last().is_some_and(|f| f.is_async)
    }

    fn in_generator(&self) -> bool {
        self.functions.last().is_some_and(|f| f.generator)
    }

    // --------------------------------------------------------
    //  Program
    // --------------------------------------------------------

    pub fn program(mut self) -> PResult<Program> {
        // A module's top level is an async context: `await` works there.
        let is_module = self.options.module;
        self.functions.push(FunctionState { vars: Vec::new(), uses_arguments: false, arrow: false, is_async: is_module, generator: false });
        let strict = is_module || self.directive_strict();
        let mut body = Vec::new();
        while !matches!(self.peek(), Tok::Eof) {
            if is_module && self.is_word("import") && !self.punct_at(1, "(") && !self.punct_at(1, ".") {
                body.extend(self.import_declaration()?);
            } else if is_module && self.is_word("export") {
                body.extend(self.export_declaration()?);
            } else {
                body.push(self.statement()?);
            }
        }
        let state = self.functions.pop().unwrap_or(FunctionState {
            vars: Vec::new(),
            uses_arguments: false,
            arrow: false,
            is_async: false,
            generator: false,
        });
        let module = if is_module { Some(core::mem::take(&mut self.module)) } else { None };
        Ok(Program { body, vars: state.vars, strict, module })
    }

    fn directive_strict(&self) -> bool {
        matches!(self.peek(), Tok::Str(s) if s.as_str() == "use strict")
    }
}

/// The names a pattern binds.
pub fn pattern_names(pattern: &Pattern, out: &mut Vec<JsStr>) {
    match pattern {
        Pattern::Ident(name) => out.push(name.clone()),
        Pattern::Object(properties, rest) => {
            for (_, target, _) in properties {
                pattern_names(target, out);
            }
            if let Some(rest) = rest {
                pattern_names(rest, out);
            }
        }
        Pattern::Array(elements, rest) => {
            for (target, _) in elements.iter().flatten() {
                pattern_names(target, out);
            }
            if let Some(rest) = rest {
                pattern_names(rest, out);
            }
        }
        Pattern::Target(_) => {}
    }
}

/// Parses a script.
pub fn parse(source: &str, headroom: &dyn Fn() -> usize) -> Result<Program, ParseError> {
    Parser::new(source, headroom)?.program()
}

/// Parses a script or a module, JavaScript or TypeScript.
pub fn parse_with(source: &str, headroom: &dyn Fn() -> usize, options: Options) -> Result<Program, ParseError> {
    Parser::with_options(source, headroom, options)?.program()
}

/// A number literal as JavaScript prints it, for keys.
pub fn number_key(n: Num) -> JsStr {
    JsStr::from(n.to_js_string())
}