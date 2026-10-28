//! Tokens to syntax tree: a recursive-descent parser for ECMAScript 2022
//! scripts.
//!
//! What it covers: declarations (`var`/`let`/`const` with destructuring,
//! functions, classes with fields, accessors, statics and private
//! members), every statement, arrow functions, async functions and
//! generators, default and rest parameters, spread, template literals
//! (tagged too), optional chaining, `??`, `**` and the logical
//! assignments. Modules are not: `import` and `export` declarations are a
//! syntax error in a classic script, which is what a page's scripts are
//! unless they say `type="module"` -- and those the browser skips.
//!
//! **Depth is bounded.** Nesting in the source becomes recursion here, and
//! the source comes from the network: every descent checks the stack
//! headroom and a depth counter, and a script nested too deeply is a
//! syntax error rather than a stack overflow.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use super::ast::*;
use super::lex::{Lexer, Tok, Token};
use super::num::Num;
use super::value::JsStr;

/// Nesting depth allowed in the source.
const MAX_DEPTH: usize = 300;
/// Stack kept free while parsing.
const MIN_HEADROOM: usize = 48 * 1024;

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
        self.functions.push(FunctionState { vars: Vec::new(), uses_arguments: false, arrow: false, is_async: false, generator: false });
        let strict = self.directive_strict();
        let mut body = Vec::new();
        while !matches!(self.peek(), Tok::Eof) {
            body.push(self.statement()?);
        }
        let state = self.functions.pop().unwrap_or(FunctionState {
            vars: Vec::new(),
            uses_arguments: false,
            arrow: false,
            is_async: false,
            generator: false,
        });
        Ok(Program { body, vars: state.vars, strict })
    }

    fn directive_strict(&self) -> bool {
        matches!(self.peek(), Tok::Str(s) if s.as_str() == "use strict")
    }

    // --------------------------------------------------------
    //  Statements
    // --------------------------------------------------------

    fn statement(&mut self) -> PResult<Stmt> {
        self.enter()?;
        let result = self.statement_inner();
        self.leave();
        result
    }

    fn statement_inner(&mut self) -> PResult<Stmt> {
        match self.peek().clone() {
            Tok::Punct("{") => {
                self.advance();
                Ok(Stmt::Block(self.block_body()?))
            }
            Tok::Punct(";") => {
                self.advance();
                Ok(Stmt::Empty)
            }
            Tok::Ident(word) => match word.as_str() {
                "var" => {
                    self.advance();
                    let declarations = self.declarations(VarKind::Var)?;
                    self.semicolon()?;
                    Ok(Stmt::Var(VarKind::Var, declarations))
                }
                "const" => {
                    self.advance();
                    let declarations = self.declarations(VarKind::Const)?;
                    self.semicolon()?;
                    Ok(Stmt::Var(VarKind::Const, declarations))
                }
                "let" if matches!(self.peek_at(1), Tok::Ident(_) | Tok::Punct("[") | Tok::Punct("{")) => {
                    self.advance();
                    let declarations = self.declarations(VarKind::Let)?;
                    self.semicolon()?;
                    Ok(Stmt::Var(VarKind::Let, declarations))
                }
                "function" => {
                    let func = self.function(false, true)?;
                    if let Some(name) = &func.name {
                        let name = name.clone();
                        self.declare_var(&name);
                    }
                    Ok(Stmt::Function(func))
                }
                "async" if self.word_at(1, "function") && !self.tokens[self.pos + 1].newline => {
                    self.advance();
                    let func = self.function(true, true)?;
                    if let Some(name) = &func.name {
                        let name = name.clone();
                        self.declare_var(&name);
                    }
                    Ok(Stmt::Function(func))
                }
                "class" => {
                    let class = self.class(true)?;
                    let name = class.name.clone().unwrap_or_else(|| JsStr::from("default"));
                    Ok(Stmt::Class(name, class))
                }
                "if" => {
                    self.advance();
                    self.expect("(")?;
                    let test = self.expression()?;
                    self.expect(")")?;
                    let then = Box::new(self.statement()?);
                    let otherwise = if self.eat_word("else") { Some(Box::new(self.statement()?)) } else { None };
                    Ok(Stmt::If(test, then, otherwise))
                }
                "for" => self.for_statement(),
                "while" => {
                    self.advance();
                    self.expect("(")?;
                    let test = self.expression()?;
                    self.expect(")")?;
                    Ok(Stmt::While(test, Box::new(self.statement()?)))
                }
                "do" => {
                    self.advance();
                    let body = Box::new(self.statement()?);
                    if !self.eat_word("while") {
                        return self.error("\"while\" attendu");
                    }
                    self.expect("(")?;
                    let test = self.expression()?;
                    self.expect(")")?;
                    self.eat(";");
                    Ok(Stmt::DoWhile(body, test))
                }
                "return" => {
                    self.advance();
                    let value = if self.is(";") || self.is("}") || self.newline_before() || matches!(self.peek(), Tok::Eof) {
                        None
                    } else {
                        Some(self.expression()?)
                    };
                    self.semicolon()?;
                    Ok(Stmt::Return(value))
                }
                "break" | "continue" => {
                    self.advance();
                    let label = match self.peek().clone() {
                        Tok::Ident(name) if !self.newline_before() && !is_reserved(&name) => {
                            self.advance();
                            Some(self.intern(&name))
                        }
                        _ => None,
                    };
                    self.semicolon()?;
                    Ok(if word.as_str() == "break" { Stmt::Break(label) } else { Stmt::Continue(label) })
                }
                "throw" => {
                    self.advance();
                    if self.newline_before() {
                        return self.error("retour a la ligne apres throw");
                    }
                    let value = self.expression()?;
                    self.semicolon()?;
                    Ok(Stmt::Throw(value))
                }
                "try" => self.try_statement(),
                "switch" => self.switch_statement(),
                "debugger" => {
                    self.advance();
                    self.semicolon()?;
                    Ok(Stmt::Empty)
                }
                "with" => self.error("\"with\" n'est pas pris en charge"),
                "import" if !self.punct_at(1, "(") && !self.punct_at(1, ".") => {
                    self.error("\"import\" hors d'un module")
                }
                "export" => self.error("\"export\" hors d'un module"),
                _ if self.punct_at(1, ":") && !is_reserved(&word) => {
                    self.advance();
                    self.advance();
                    let label = self.intern(&word);
                    // A function declaration may be labeled (Annex B).
                    Ok(Stmt::Labeled(label, Box::new(self.statement()?)))
                }
                _ => self.expression_statement(),
            },
            _ => self.expression_statement(),
        }
    }

    fn expression_statement(&mut self) -> PResult<Stmt> {
        let expr = self.expression()?;
        self.semicolon()?;
        Ok(Stmt::Expr(expr))
    }

    fn block_body(&mut self) -> PResult<Vec<Stmt>> {
        let mut body = Vec::new();
        while !self.eat("}") {
            if matches!(self.peek(), Tok::Eof) {
                return self.error("\"}\" attendu");
            }
            body.push(self.statement()?);
        }
        Ok(body)
    }

    fn declarations(&mut self, kind: VarKind) -> PResult<Vec<(Pattern, Option<Expr>)>> {
        let mut out = Vec::new();
        loop {
            let target = self.binding_target()?;
            if kind == VarKind::Var {
                self.declare_pattern_vars(&target);
            }
            let init = if self.eat("=") { Some(self.assignment()?) } else { None };
            out.push((target, init));
            if !self.eat(",") {
                return Ok(out);
            }
        }
    }

    fn for_statement(&mut self) -> PResult<Stmt> {
        self.advance();
        self.eat_word("await");
        self.expect("(")?;
        let kind = if self.is_word("var") {
            Some(VarKind::Var)
        } else if self.is_word("const") {
            Some(VarKind::Const)
        } else if self.is_word("let") && matches!(self.peek_at(1), Tok::Ident(_) | Tok::Punct("[") | Tok::Punct("{")) {
            Some(VarKind::Let)
        } else {
            None
        };
        let init = if self.is(";") {
            None
        } else if let Some(kind) = kind {
            self.advance();
            let target = self.binding_target()?;
            if kind == VarKind::Var {
                self.declare_pattern_vars(&target);
            }
            if self.eat_word("of") {
                let object = self.assignment()?;
                self.expect(")")?;
                return Ok(Stmt::ForOf(Some(kind), target, object, Box::new(self.statement()?)));
            }
            if self.eat_word("in") {
                let object = self.expression()?;
                self.expect(")")?;
                return Ok(Stmt::ForIn(Some(kind), target, object, Box::new(self.statement()?)));
            }
            let saved = self.no_in;
            self.no_in = true;
            let first_init = if self.eat("=") { Some(self.assignment()?) } else { None };
            let mut declarations = alloc::vec![(target, first_init)];
            while self.eat(",") {
                let target = self.binding_target()?;
                if kind == VarKind::Var {
                    self.declare_pattern_vars(&target);
                }
                let init = if self.eat("=") { Some(self.assignment()?) } else { None };
                declarations.push((target, init));
            }
            self.no_in = saved;
            Some(ForInit::Var(kind, declarations))
        } else {
            let saved = self.no_in;
            self.no_in = true;
            let expr = self.left_hand_or_expression()?;
            self.no_in = saved;
            if self.eat_word("of") {
                let target = self.to_pattern(expr)?;
                let object = self.assignment()?;
                self.expect(")")?;
                return Ok(Stmt::ForOf(None, target, object, Box::new(self.statement()?)));
            }
            if self.eat_word("in") {
                let target = self.to_pattern(expr)?;
                let object = self.expression()?;
                self.expect(")")?;
                return Ok(Stmt::ForIn(None, target, object, Box::new(self.statement()?)));
            }
            Some(ForInit::Expr(expr))
        };
        self.expect(";")?;
        let test = if self.is(";") { None } else { Some(self.expression()?) };
        self.expect(";")?;
        let update = if self.is(")") { None } else { Some(self.expression()?) };
        self.expect(")")?;
        Ok(Stmt::For(init, test, update, Box::new(self.statement()?)))
    }

    /// In a `for` head without declaration: an expression, parsed so that
    /// `in` is not taken as the operator.
    fn left_hand_or_expression(&mut self) -> PResult<Expr> {
        self.expression()
    }

    fn try_statement(&mut self) -> PResult<Stmt> {
        self.advance();
        self.expect("{")?;
        let block = self.block_body()?;
        let catch = if self.eat_word("catch") {
            let binding = if self.eat("(") {
                let target = self.binding_target()?;
                self.expect(")")?;
                Some(target)
            } else {
                None
            };
            self.expect("{")?;
            Some((binding, self.block_body()?))
        } else {
            None
        };
        let finally = if self.eat_word("finally") {
            self.expect("{")?;
            Some(self.block_body()?)
        } else {
            None
        };
        if catch.is_none() && finally.is_none() {
            return self.error("\"catch\" ou \"finally\" attendu");
        }
        Ok(Stmt::Try(block, catch, finally))
    }

    fn switch_statement(&mut self) -> PResult<Stmt> {
        self.advance();
        self.expect("(")?;
        let discriminant = self.expression()?;
        self.expect(")")?;
        self.expect("{")?;
        let mut cases = Vec::new();
        while !self.eat("}") {
            let test = if self.eat_word("case") {
                Some(self.expression()?)
            } else if self.eat_word("default") {
                None
            } else {
                return self.error("\"case\" attendu");
            };
            self.expect(":")?;
            let mut body = Vec::new();
            while !self.is_word("case") && !self.is_word("default") && !self.is("}") {
                if matches!(self.peek(), Tok::Eof) {
                    return self.error("\"}\" attendu");
                }
                body.push(self.statement()?);
            }
            cases.push((test, body));
        }
        Ok(Stmt::Switch(discriminant, cases))
    }

    // --------------------------------------------------------
    //  Patterns
    // --------------------------------------------------------

    fn binding_target(&mut self) -> PResult<Pattern> {
        self.enter()?;
        let result = self.binding_target_inner();
        self.leave();
        result
    }

    fn binding_target_inner(&mut self) -> PResult<Pattern> {
        if self.eat("[") {
            let mut elements = Vec::new();
            let mut rest = None;
            loop {
                if self.eat("]") {
                    break;
                }
                if self.eat(",") {
                    elements.push(None);
                    continue;
                }
                if self.eat("...") {
                    rest = Some(Box::new(self.binding_target()?));
                    self.eat(",");
                    self.expect("]")?;
                    break;
                }
                let target = self.binding_target()?;
                let default = if self.eat("=") { Some(self.assignment()?) } else { None };
                elements.push(Some((target, default)));
                if !self.is("]") {
                    self.expect(",")?;
                }
            }
            return Ok(Pattern::Array(elements, rest));
        }
        if self.eat("{") {
            let mut properties = Vec::new();
            let mut rest = None;
            loop {
                if self.eat("}") {
                    break;
                }
                if self.eat("...") {
                    rest = Some(Box::new(self.binding_target()?));
                    self.eat(",");
                    self.expect("}")?;
                    break;
                }
                let key = self.property_key()?;
                let target = if self.eat(":") {
                    self.binding_target()?
                } else {
                    match &key {
                        PropKey::Name(name) => Pattern::Ident(self.intern(name)),
                        _ => return self.error("\":\" attendu"),
                    }
                };
                let default = if self.eat("=") { Some(self.assignment()?) } else { None };
                properties.push((key, target, default));
                if !self.is("}") {
                    self.expect(",")?;
                }
            }
            return Ok(Pattern::Object(properties, rest));
        }
        Ok(Pattern::Ident(self.identifier()?))
    }

    /// An expression parsed before knowing it was a target (`[a, b] =
    /// ...`, `for (x.y of ...)`) becomes a pattern.
    fn to_pattern(&self, expr: Expr) -> PResult<Pattern> {
        Ok(match expr {
            Expr::Ident(name) => Pattern::Ident(name),
            Expr::Member(..) | Expr::Index(..) | Expr::PrivateMember(..) | Expr::SuperMember(..) => {
                Pattern::Target(Box::new(expr))
            }
            Expr::Array(elements) => {
                let mut out = Vec::new();
                let mut rest = None;
                let count = elements.len();
                for (index, element) in elements.into_iter().enumerate() {
                    match element {
                        None => out.push(None),
                        Some(Arg::Spread(inner)) if index + 1 == count => rest = Some(Box::new(self.to_pattern(inner)?)),
                        Some(Arg::Spread(_)) => return self.error("\"...\" doit etre le dernier element"),
                        Some(Arg::Plain(Expr::Assign(AssignOp::Assign, target, default))) => {
                            out.push(Some((*target, Some(*default))))
                        }
                        Some(Arg::Plain(inner)) => out.push(Some((self.to_pattern(inner)?, None))),
                    }
                }
                Pattern::Array(out, rest)
            }
            Expr::Object(properties) => {
                let mut out = Vec::new();
                let mut rest = None;
                for property in properties {
                    match property.kind {
                        PropKind::Spread => rest = Some(Box::new(self.to_pattern(property.value)?)),
                        PropKind::Value => match property.value {
                            Expr::Assign(AssignOp::Assign, target, default) => out.push((property.key, *target, Some(*default))),
                            value => out.push((property.key, self.to_pattern(value)?, None)),
                        },
                        _ => return self.error("cible d'affectation invalide"),
                    }
                }
                Pattern::Object(out, rest)
            }
            Expr::Assign(AssignOp::Assign, target, _) => *target,
            Expr::Chain(inner) => return self.to_pattern(*inner),
            _ => return self.error("cible d'affectation invalide"),
        })
    }

    // --------------------------------------------------------
    //  Expressions
    // --------------------------------------------------------

    pub fn expression(&mut self) -> PResult<Expr> {
        let first = self.assignment()?;
        if !self.is(",") {
            return Ok(first);
        }
        let mut list = alloc::vec![first];
        while self.eat(",") {
            list.push(self.assignment()?);
        }
        Ok(Expr::Seq(list))
    }

    fn assignment(&mut self) -> PResult<Expr> {
        self.enter()?;
        let result = self.assignment_inner();
        self.leave();
        result
    }

    /// Is the token at `self.pos` the `(` of an arrow's parameters?
    fn arrow_ahead(&self, open: usize) -> bool {
        let mut depth = 0usize;
        let mut index = open;
        while index < self.tokens.len() {
            match &self.tokens[index].tok {
                Tok::Punct("(") | Tok::Punct("[") | Tok::Punct("{") => depth += 1,
                Tok::Punct(")") | Tok::Punct("]") | Tok::Punct("}") => {
                    depth -= 1;
                    if depth == 0 {
                        return index + 1 < self.tokens.len()
                            && matches!(self.tokens[index + 1].tok, Tok::Punct("=>"))
                            && !self.tokens[index + 1].newline;
                    }
                }
                Tok::Eof => return false,
                _ => {}
            }
            index += 1;
        }
        false
    }

    fn assignment_inner(&mut self) -> PResult<Expr> {
        // Arrow functions.
        match self.peek().clone() {
            Tok::Ident(word) if word.as_str() == "async" && !self.tokens[self.pos + 1].newline => {
                if let Tok::Ident(param) = self.peek_at(1).clone() {
                    if self.punct_at(2, "=>") && !is_reserved(&param) {
                        self.advance();
                        let name = self.identifier()?;
                        return self.arrow(alloc::vec![(Pattern::Ident(name), None)], None, true);
                    }
                }
                if self.punct_at(1, "(") && self.arrow_ahead(self.pos + 1) {
                    self.advance();
                    let (params, rest) = self.parameters()?;
                    return self.arrow(params, rest, true);
                }
            }
            Tok::Ident(word) if !is_reserved(&word) && self.punct_at(1, "=>") && !self.tokens[self.pos + 1].newline => {
                let name = self.identifier()?;
                return self.arrow(alloc::vec![(Pattern::Ident(name), None)], None, false);
            }
            Tok::Punct("(") if self.arrow_ahead(self.pos) => {
                let (params, rest) = self.parameters()?;
                return self.arrow(params, rest, false);
            }
            Tok::Ident(word) if word.as_str() == "yield" && self.in_generator() => {
                self.advance();
                let delegate = self.eat("*");
                let value = if self.newline_before()
                    || matches!(self.peek(), Tok::Punct(")") | Tok::Punct("]") | Tok::Punct("}") | Tok::Punct(",") | Tok::Punct(";") | Tok::Punct(":") | Tok::Eof)
                {
                    None
                } else {
                    Some(Box::new(self.assignment()?))
                };
                return Ok(Expr::Yield(value, delegate));
            }
            _ => {}
        }
        let left = self.conditional()?;
        if let Tok::Punct(punct) = self.peek().clone() {
            if let Some(op) = assign_op(punct) {
                self.advance();
                let target = if op == AssignOp::Assign {
                    self.to_pattern(left)?
                } else {
                    match left {
                        Expr::Ident(_) | Expr::Member(..) | Expr::Index(..) | Expr::PrivateMember(..) | Expr::SuperMember(..) => {
                            self.to_pattern(left)?
                        }
                        _ => return self.error("cible d'affectation invalide"),
                    }
                };
                let value = self.assignment()?;
                return Ok(Expr::Assign(op, Box::new(target), Box::new(value)));
            }
        }
        Ok(left)
    }

    fn conditional(&mut self) -> PResult<Expr> {
        let test = self.binary(0)?;
        if !self.eat("?") {
            return Ok(test);
        }
        let saved = self.no_in;
        self.no_in = false;
        let then = self.assignment()?;
        self.no_in = saved;
        self.expect(":")?;
        let otherwise = self.assignment()?;
        Ok(Expr::Cond(Box::new(test), Box::new(then), Box::new(otherwise)))
    }

    fn binary(&mut self, min: u8) -> PResult<Expr> {
        self.enter()?;
        let result = self.binary_inner(min);
        self.leave();
        result
    }

    fn binary_inner(&mut self, min: u8) -> PResult<Expr> {
        // `#x in obj`
        let mut left = if let Tok::Private(name) = self.peek().clone() {
            if self.word_at(1, "in") {
                self.advance();
                self.advance();
                let object = self.binary(9)?;
                Expr::PrivateIn(name, Box::new(object))
            } else {
                return self.error("nom prive inattendu");
            }
        } else {
            self.unary()?
        };
        loop {
            let operator = match self.peek() {
                Tok::Punct(p) => binary_precedence(p, self.no_in),
                Tok::Ident(w) if w.as_str() == "instanceof" || w.as_str() == "in" => binary_precedence(w, self.no_in),
                _ => None,
            };
            let Some((precedence, binary, logic)) = operator else {
                return Ok(left);
            };
            if precedence <= min && !(precedence == 12 && min == 12) {
                return Ok(left);
            }
            self.advance();
            // `**` is right-associative.
            let right = if precedence == 12 { self.binary(11)? } else { self.binary(precedence)? };
            left = match (binary, logic) {
                (Some(op), _) => Expr::Binary(op, Box::new(left), Box::new(right)),
                (None, Some(op)) => Expr::Logic(op, Box::new(left), Box::new(right)),
                _ => return self.error("operateur inconnu"),
            };
        }
    }

    fn unary(&mut self) -> PResult<Expr> {
        self.enter()?;
        let result = self.unary_inner();
        self.leave();
        result
    }

    fn unary_inner(&mut self) -> PResult<Expr> {
        let op = match self.peek() {
            Tok::Punct("!") => Some(UnaryOp::Not),
            Tok::Punct("-") => Some(UnaryOp::Neg),
            Tok::Punct("+") => Some(UnaryOp::Plus),
            Tok::Punct("~") => Some(UnaryOp::BitNot),
            Tok::Ident(w) if w.as_str() == "typeof" => Some(UnaryOp::TypeOf),
            Tok::Ident(w) if w.as_str() == "void" => Some(UnaryOp::Void),
            Tok::Ident(w) if w.as_str() == "delete" => Some(UnaryOp::Delete),
            _ => None,
        };
        if let Some(op) = op {
            self.advance();
            let operand = self.unary()?;
            // Constant folding of negative literals keeps `-1` a number.
            if let (UnaryOp::Neg, Expr::Num(n)) = (op, &operand) {
                return Ok(Expr::Num(n.neg()));
            }
            return Ok(Expr::Unary(op, Box::new(operand)));
        }
        if self.is("++") || self.is("--") {
            let increment = self.is("++");
            self.advance();
            let target = self.unary()?;
            return Ok(Expr::Update(increment, true, Box::new(target)));
        }
        if self.is_word("await") && (self.in_async() || self.functions.len() <= 1) {
            // Top-level await in a classic script is an identifier; treat
            // it as await only when followed by something to await.
            if self.in_async() || !matches!(self.peek_at(1), Tok::Punct(_) | Tok::Eof) || self.punct_at(1, "(") || self.punct_at(1, "[") {
                if self.in_async() {
                    self.advance();
                    let operand = self.unary()?;
                    return Ok(Expr::Await(Box::new(operand)));
                }
            }
        }
        let expr = self.postfix()?;
        Ok(expr)
    }

    fn postfix(&mut self) -> PResult<Expr> {
        let expr = self.call_member(false)?;
        if (self.is("++") || self.is("--")) && !self.newline_before() {
            let increment = self.is("++");
            self.advance();
            return Ok(Expr::Update(increment, false, Box::new(expr)));
        }
        Ok(expr)
    }

    fn arguments(&mut self) -> PResult<Vec<Arg>> {
        let mut out = Vec::new();
        let saved = self.no_in;
        self.no_in = false;
        while !self.eat(")") {
            if self.eat("...") {
                out.push(Arg::Spread(self.assignment()?));
            } else {
                out.push(Arg::Plain(self.assignment()?));
            }
            if !self.is(")") {
                self.expect(",")?;
            }
        }
        self.no_in = saved;
        Ok(out)
    }

    /// Member accesses, calls and `new`, with optional chains.
    fn call_member(&mut self, in_new: bool) -> PResult<Expr> {
        let mut expr = if self.is_word("new") {
            self.advance();
            if self.eat(".") {
                if !self.eat_word("target") {
                    return self.error("\"new.target\" attendu");
                }
                Expr::NewTarget
            } else {
                self.enter()?;
                let callee = self.call_member(true);
                self.leave();
                let callee = callee?;
                let args = if self.eat("(") { self.arguments()? } else { Vec::new() };
                Expr::New(Box::new(callee), args)
            }
        } else {
            self.primary()?
        };
        let mut chained = false;
        loop {
            if self.eat(".") {
                match self.advance() {
                    Tok::Ident(name) => {
                        let name = self.intern(&name);
                        expr = Expr::Member(Box::new(expr), name, false);
                    }
                    Tok::Private(name) => expr = Expr::PrivateMember(Box::new(expr), name),
                    _ => return self.error("nom de propriete attendu"),
                }
            } else if self.is("?.") {
                if in_new {
                    return self.error("chaine optionnelle dans new");
                }
                self.advance();
                chained = true;
                if self.eat("(") {
                    let args = self.arguments()?;
                    expr = Expr::Call(Box::new(expr), args, true);
                } else if self.eat("[") {
                    let index = self.expression()?;
                    self.expect("]")?;
                    expr = Expr::Index(Box::new(expr), Box::new(index), true);
                } else {
                    match self.advance() {
                        Tok::Ident(name) => {
                            let name = self.intern(&name);
                            expr = Expr::Member(Box::new(expr), name, true);
                        }
                        Tok::Private(name) => expr = Expr::PrivateMember(Box::new(expr), name),
                        _ => return self.error("nom de propriete attendu"),
                    }
                }
            } else if self.is("[") {
                self.advance();
                let saved = self.no_in;
                self.no_in = false;
                let index = self.expression()?;
                self.no_in = saved;
                self.expect("]")?;
                expr = Expr::Index(Box::new(expr), Box::new(index), false);
            } else if self.is("(") && !in_new {
                self.advance();
                let args = self.arguments()?;
                expr = Expr::Call(Box::new(expr), args, false);
            } else if let Tok::Template(cooked, raw, spans) = self.peek().clone() {
                if chained {
                    return self.error("gabarit dans une chaine optionnelle");
                }
                self.advance();
                let substitutions = self.substitutions(&spans)?;
                expr = Expr::Tagged(Box::new(expr), Rc::new((cooked, raw)), substitutions);
            } else {
                break;
            }
        }
        Ok(if chained { Expr::Chain(Box::new(expr)) } else { expr })
    }

    fn substitutions(&mut self, spans: &[(usize, usize)]) -> PResult<Vec<Expr>> {
        let mut out = Vec::new();
        for (start, end) in spans {
            let tokens = Lexer::new(&self.source[*start..*end], *start)
                .tokenize()
                .map_err(|(message, at)| ParseError { message, at })?;
            let saved_tokens = core::mem::replace(&mut self.tokens, tokens);
            let saved_pos = core::mem::replace(&mut self.pos, 0);
            let saved_in = core::mem::replace(&mut self.no_in, false);
            let result = self.expression();
            let finished = matches!(self.peek(), Tok::Eof);
            self.tokens = saved_tokens;
            self.pos = saved_pos;
            self.no_in = saved_in;
            let expr = result?;
            if !finished {
                return Err(ParseError { message: String::from("\"}\" attendu dans le gabarit"), at: *start });
            }
            out.push(expr);
        }
        Ok(out)
    }

    fn primary(&mut self) -> PResult<Expr> {
        let at = self.at();
        match self.advance() {
            Tok::Num(n) => Ok(Expr::Num(n)),
            Tok::Str(s) => Ok(Expr::Str(s)),
            Tok::Regex(pattern, flags) => Ok(Expr::Regex(pattern, flags)),
            Tok::Template(cooked, _, spans) => {
                let substitutions = self.substitutions(&spans)?;
                let parts = cooked.into_iter().map(|part| part.unwrap_or_else(|| JsStr::from(""))).collect();
                Ok(Expr::Template(parts, substitutions))
            }
            Tok::Punct("(") => {
                let saved = self.no_in;
                self.no_in = false;
                let expr = self.expression()?;
                self.no_in = saved;
                self.expect(")")?;
                Ok(expr)
            }
            Tok::Punct("[") => {
                let mut elements = Vec::new();
                let saved = self.no_in;
                self.no_in = false;
                loop {
                    if self.eat("]") {
                        break;
                    }
                    if self.eat(",") {
                        elements.push(None);
                        continue;
                    }
                    if self.eat("...") {
                        elements.push(Some(Arg::Spread(self.assignment()?)));
                    } else {
                        elements.push(Some(Arg::Plain(self.assignment()?)));
                    }
                    if !self.is("]") {
                        self.expect(",")?;
                    }
                }
                self.no_in = saved;
                Ok(Expr::Array(elements))
            }
            Tok::Punct("{") => self.object_literal(),
            Tok::Ident(word) => match word.as_str() {
                "this" => Ok(Expr::This),
                "null" => Ok(Expr::Null),
                "true" => Ok(Expr::Bool(true)),
                "false" => Ok(Expr::Bool(false)),
                "function" => {
                    self.pos -= 1;
                    Ok(Expr::Function(self.function(false, false)?))
                }
                "async" if self.is_word("function") && !self.newline_before() => {
                    Ok(Expr::Function(self.function(true, false)?))
                }
                "class" => {
                    self.pos -= 1;
                    Ok(Expr::Class(self.class(false)?))
                }
                "super" => {
                    if self.eat("(") {
                        let args = self.arguments()?;
                        Ok(Expr::SuperCall(args))
                    } else if self.eat(".") {
                        match self.advance() {
                            Tok::Ident(name) => Ok(Expr::SuperMember(Box::new(Expr::Str(name)))),
                            _ => self.error("nom de propriete attendu"),
                        }
                    } else if self.eat("[") {
                        let key = self.expression()?;
                        self.expect("]")?;
                        Ok(Expr::SuperMember(Box::new(key)))
                    } else {
                        self.error("\"super\" inattendu")
                    }
                }
                "import" => {
                    if self.eat(".") {
                        // import.meta
                        self.advance();
                        return Ok(Expr::Object(Vec::new()));
                    }
                    self.expect("(")?;
                    let specifier = self.assignment()?;
                    self.eat(",");
                    if !self.is(")") {
                        self.assignment()?;
                    }
                    self.expect(")")?;
                    Ok(Expr::Import(Box::new(specifier)))
                }
                "new" => {
                    self.pos -= 1;
                    self.call_member(false)
                }
                _ if is_reserved(&word) => Err(ParseError { message: alloc::format!("\"{}\" inattendu", word.as_str()), at }),
                _ => {
                    if word.as_str() == "arguments" {
                        for state in self.functions.iter_mut().rev() {
                            state.uses_arguments = true;
                            if !state.arrow {
                                break;
                            }
                        }
                    }
                    if word.as_str() == "undefined" {
                        return Ok(Expr::Ident(self.intern("undefined")));
                    }
                    Ok(Expr::Ident(self.intern(&word)))
                }
            },
            Tok::Eof => Err(ParseError { message: String::from("fin inattendue du script"), at }),
            _ => Err(ParseError { message: String::from("expression attendue"), at }),
        }
    }

    fn property_key(&mut self) -> PResult<PropKey> {
        match self.advance() {
            Tok::Ident(name) => Ok(PropKey::Name(self.intern(&name))),
            Tok::Str(s) => Ok(PropKey::Name(s)),
            Tok::Num(n) => Ok(PropKey::Name(JsStr::from(n.to_js_string()))),
            Tok::Private(name) => Ok(PropKey::Private(name)),
            Tok::Punct("[") => {
                let saved = self.no_in;
                self.no_in = false;
                let key = self.assignment()?;
                self.no_in = saved;
                self.expect("]")?;
                Ok(PropKey::Computed(Box::new(key)))
            }
            _ => self.error("nom de propriete attendu"),
        }
    }

    /// After `get`/`set`/`async`/`static`, is the next token a property
    /// key (so the word is a modifier) rather than the end of a member?
    fn modifier_applies(&self) -> bool {
        !matches!(self.peek_at(1), Tok::Punct("(") | Tok::Punct(",") | Tok::Punct(":") | Tok::Punct("}") | Tok::Punct("=") | Tok::Punct(";"))
            && !self.tokens[self.pos + 1].newline
    }

    fn object_literal(&mut self) -> PResult<Expr> {
        let mut properties = Vec::new();
        let saved = self.no_in;
        self.no_in = false;
        loop {
            if self.eat("}") {
                break;
            }
            if self.eat("...") {
                let value = self.assignment()?;
                properties.push(PropDef { key: PropKey::Name(JsStr::from("")), kind: PropKind::Spread, value });
            } else {
                let mut is_async = false;
                let mut generator = false;
                let mut accessor = None;
                if self.is_word("async") && self.modifier_applies() {
                    self.advance();
                    is_async = true;
                }
                if self.eat("*") {
                    generator = true;
                }
                if (self.is_word("get") || self.is_word("set")) && self.modifier_applies() && !is_async && !generator {
                    accessor = Some(self.is_word("get"));
                    self.advance();
                }
                let key = self.property_key()?;
                if self.is("(") {
                    let func = self.method_function(&key, is_async, generator)?;
                    let kind = match accessor {
                        Some(true) => PropKind::Get,
                        Some(false) => PropKind::Set,
                        None => PropKind::Method,
                    };
                    properties.push(PropDef { key, kind, value: Expr::Function(func) });
                } else if self.eat(":") {
                    let value = self.assignment()?;
                    properties.push(PropDef { key, kind: PropKind::Value, value });
                } else {
                    // Shorthand `{a}` or, in a pattern, `{a = 1}`.
                    let PropKey::Name(name) = &key else {
                        return self.error("\":\" attendu");
                    };
                    let name = self.intern(name);
                    let value = if self.eat("=") {
                        let default = self.assignment()?;
                        Expr::Assign(AssignOp::Assign, Box::new(Pattern::Ident(name.clone())), Box::new(default))
                    } else {
                        Expr::Ident(name)
                    };
                    properties.push(PropDef { key, kind: PropKind::Value, value });
                }
            }
            if !self.is("}") {
                self.expect(",")?;
            }
        }
        self.no_in = saved;
        Ok(Expr::Object(properties))
    }

    // --------------------------------------------------------
    //  Functions and classes
    // --------------------------------------------------------

    fn parameters(&mut self) -> PResult<(Vec<(Pattern, Option<Expr>)>, Option<Pattern>)> {
        self.expect("(")?;
        let mut params = Vec::new();
        let mut rest = None;
        let saved = self.no_in;
        self.no_in = false;
        while !self.eat(")") {
            if self.eat("...") {
                rest = Some(self.binding_target()?);
                self.eat(",");
                self.expect(")")?;
                break;
            }
            let target = self.binding_target()?;
            let default = if self.eat("=") { Some(self.assignment()?) } else { None };
            params.push((target, default));
            if !self.is(")") {
                self.expect(",")?;
            }
        }
        self.no_in = saved;
        Ok((params, rest))
    }

    fn function_body(
        &mut self,
        name: Option<JsStr>,
        params: Vec<(Pattern, Option<Expr>)>,
        rest: Option<Pattern>,
        start: usize,
        is_async: bool,
        generator: bool,
        is_ctor: bool,
    ) -> PResult<Rc<Func>> {
        self.expect("{")?;
        let strict = self.directive_strict();
        let body = self.block_body()?;
        let state = self.functions.pop().unwrap_or(FunctionState { vars: Vec::new(), uses_arguments: false, arrow: false, is_async, generator });
        let end = self.tokens[self.pos.saturating_sub(1)].at + 1;
        let simple_params = rest.is_none() && params.iter().all(|(p, d)| matches!(p, Pattern::Ident(_)) && d.is_none());
        Ok(Rc::new(Func {
            name,
            params,
            rest,
            body,
            arrow: false,
            is_async,
            generator,
            expression: false,
            vars: state.vars,
            uses_arguments: state.uses_arguments,
            simple_params,
            source: (start, end),
            strict,
            is_ctor,
        }))
    }

    /// `function name(...) {...}` from the `function` keyword.
    fn function(&mut self, is_async: bool, declaration: bool) -> PResult<Rc<Func>> {
        let start = self.at();
        self.advance(); // function
        let generator = self.eat("*");
        let name = if matches!(self.peek(), Tok::Ident(_)) && !self.is("(") {
            Some(self.identifier()?)
        } else if declaration {
            return self.error("nom de fonction attendu");
        } else {
            None
        };
        self.functions.push(FunctionState { vars: Vec::new(), uses_arguments: false, arrow: false, is_async, generator });
        let (params, rest) = match self.parameters() {
            Ok(parsed) => parsed,
            Err(error) => {
                self.functions.pop();
                return Err(error);
            }
        };
        self.function_body(name, params, rest, start, is_async, generator, false)
    }

    fn method_function(&mut self, key: &PropKey, is_async: bool, generator: bool) -> PResult<Rc<Func>> {
        let start = self.at();
        let name = match key {
            PropKey::Name(name) => Some(name.clone()),
            _ => None,
        };
        self.functions.push(FunctionState { vars: Vec::new(), uses_arguments: false, arrow: false, is_async, generator });
        let (params, rest) = match self.parameters() {
            Ok(parsed) => parsed,
            Err(error) => {
                self.functions.pop();
                return Err(error);
            }
        };
        self.function_body(name, params, rest, start, is_async, generator, false)
    }

    fn arrow(&mut self, params: Vec<(Pattern, Option<Expr>)>, rest: Option<Pattern>, is_async: bool) -> PResult<Expr> {
        let start = self.at();
        self.expect("=>")?;
        self.functions.push(FunctionState { vars: Vec::new(), uses_arguments: false, arrow: true, is_async, generator: false });
        let simple_params = rest.is_none() && params.iter().all(|(p, d)| matches!(p, Pattern::Ident(_)) && d.is_none());
        let (body, expression) = if self.eat("{") {
            match self.block_body() {
                Ok(body) => (body, false),
                Err(error) => {
                    self.functions.pop();
                    return Err(error);
                }
            }
        } else {
            match self.assignment() {
                Ok(expr) => (alloc::vec![Stmt::Return(Some(expr))], true),
                Err(error) => {
                    self.functions.pop();
                    return Err(error);
                }
            }
        };
        let state = self.functions.pop().unwrap_or(FunctionState { vars: Vec::new(), uses_arguments: false, arrow: true, is_async, generator: false });
        if state.uses_arguments {
            if let Some(outer) = self.functions.last_mut() {
                outer.uses_arguments = true;
            }
        }
        let end = self.tokens[self.pos.saturating_sub(1)].at + 1;
        Ok(Expr::Function(Rc::new(Func {
            name: None,
            params,
            rest,
            body,
            arrow: true,
            is_async,
            generator: false,
            expression,
            vars: state.vars,
            uses_arguments: false,
            simple_params,
            source: (start, end),
            strict: false,
            is_ctor: false,
        })))
    }

    fn class(&mut self, declaration: bool) -> PResult<Rc<Class>> {
        self.advance(); // class
        let name = if matches!(self.peek(), Tok::Ident(w) if w.as_str() != "extends") {
            Some(self.identifier()?)
        } else if declaration {
            return self.error("nom de classe attendu");
        } else {
            None
        };
        let extends = if self.eat_word("extends") { Some(Box::new(self.call_member(false)?)) } else { None };
        self.expect("{")?;
        let mut constructor = None;
        let mut members = Vec::new();
        let mut static_blocks = Vec::new();
        while !self.eat("}") {
            if self.eat(";") {
                continue;
            }
            let mut is_static = false;
            if self.is_word("static") && self.modifier_applies() {
                self.advance();
                is_static = true;
                if self.is("{") {
                    let start = self.at();
                    self.functions.push(FunctionState { vars: Vec::new(), uses_arguments: false, arrow: false, is_async: false, generator: false });
                    let func = self.function_body(None, Vec::new(), None, start, false, false, false)?;
                    static_blocks.push(func);
                    continue;
                }
            }
            let mut is_async = false;
            let mut generator = false;
            let mut accessor = None;
            if self.is_word("async") && self.modifier_applies() {
                self.advance();
                is_async = true;
            }
            if self.eat("*") {
                generator = true;
            }
            if (self.is_word("get") || self.is_word("set")) && self.modifier_applies() && !is_async && !generator {
                accessor = Some(self.is_word("get"));
                self.advance();
            }
            let key = self.property_key()?;
            if self.is("(") {
                let is_constructor = !is_static && matches!(&key, PropKey::Name(n) if n.as_str() == "constructor");
                let start = self.at();
                let name = match &key {
                    PropKey::Name(name) => Some(name.clone()),
                    _ => None,
                };
                self.functions.push(FunctionState { vars: Vec::new(), uses_arguments: false, arrow: false, is_async, generator });
                let (params, rest) = self.parameters()?;
                let func = self.function_body(name, params, rest, start, is_async, generator, is_constructor)?;
                if is_constructor {
                    constructor = Some(func);
                    continue;
                }
                let kind = match accessor {
                    Some(true) => PropKind::Get,
                    Some(false) => PropKind::Set,
                    None => PropKind::Method,
                };
                members.push(ClassMember { key, kind, is_static, value: Some(Expr::Function(func)), field: false });
            } else {
                // A field, with its initializer evaluated per instance as
                // the body of a method (so `this` is the instance).
                let value = if self.eat("=") {
                    let start = self.at();
                    self.functions.push(FunctionState { vars: Vec::new(), uses_arguments: false, arrow: true, is_async: false, generator: false });
                    let init = self.assignment();
                    let state = self.functions.pop();
                    let init = init?;
                    let end = self.tokens[self.pos.saturating_sub(1)].at + 1;
                    Some(Expr::Function(Rc::new(Func {
                        name: None,
                        params: Vec::new(),
                        rest: None,
                        body: alloc::vec![Stmt::Return(Some(init))],
                        arrow: false,
                        is_async: false,
                        generator: false,
                        expression: true,
                        vars: state.map(|s| s.vars).unwrap_or_default(),
                        uses_arguments: false,
                        simple_params: true,
                        source: (start, end),
                        strict: true,
                        is_ctor: false,
                    })))
                } else {
                    None
                };
                self.semicolon()?;
                members.push(ClassMember { key, kind: PropKind::Value, is_static, value, field: true });
            }
        }
        Ok(Rc::new(Class { name, extends, constructor, members, static_blocks }))
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

/// A number literal as JavaScript prints it, for keys.
pub fn number_key(n: Num) -> JsStr {
    JsStr::from(n.to_js_string())
}
