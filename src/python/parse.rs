//! Tokens to syntax tree: recursive descent over the Python grammar.
//!
//! The grammar is the language reference's, for the subset of `mod.rs`.
//! Targets of assignments are parsed as expressions and checked after
//! the `=` is seen, as CPython's own parser does. Nesting is bounded:
//! every descent counts, and a source nested too deeply is a
//! `SyntaxError`, not a stack overflow (the parser runs on the kernel
//! stack; the program, later, does not).

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use super::ast::*;
use super::lex::{self, Tok, Token};

/// Nesting allowed in the source (CPython's own parser stops near 200).
const MAX_DEPTH: usize = 200;

pub struct SyntaxError {
    pub message: String,
    pub line: u32,
}

type PResult<T> = Result<T, SyntaxError>;

const KEYWORDS: &[&str] = &[
    "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif",
    "else", "except", "finally", "for", "from", "global", "if", "import", "in", "is", "lambda", "nonlocal", "not", "or",
    "pass", "raise", "return", "try", "while", "with", "yield",
];

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    depth: usize,
    /// Inside a function (yield allowed), a loop (break allowed).
    functions: usize,
    /// For each enclosing function, innermost last: is it `async def`?
    asyncs: Vec<bool>,
    /// Counter of the hidden names `async for` and `async with` use.
    hidden: usize,
}

impl Parser {
    fn peek(&self) -> &Tok {
        &self.tokens[self.pos].tok
    }

    fn peek_at(&self, offset: usize) -> &Tok {
        &self.tokens[(self.pos + offset).min(self.tokens.len() - 1)].tok
    }

    fn line(&self) -> u32 {
        self.tokens[self.pos].line
    }

    fn advance(&mut self) -> Tok {
        let tok = self.tokens[self.pos].tok.clone();
        if self.pos + 1 < self.tokens.len() {
            self.pos += 1;
        }
        tok
    }

    fn error<T>(&self, message: &str) -> PResult<T> {
        Err(SyntaxError { message: String::from(message), line: self.line() })
    }

    fn is_op(&self, op: &str) -> bool {
        matches!(self.peek(), Tok::Op(o) if *o == op)
    }

    fn is_kw(&self, word: &str) -> bool {
        matches!(self.peek(), Tok::Name(n) if &**n == word)
    }

    fn kw_at(&self, offset: usize, word: &str) -> bool {
        matches!(self.peek_at(offset), Tok::Name(n) if &**n == word)
    }

    fn eat_op(&mut self, op: &str) -> bool {
        if self.is_op(op) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn eat_kw(&mut self, word: &str) -> bool {
        if self.is_kw(word) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn expect_op(&mut self, op: &str) -> PResult<()> {
        if self.eat_op(op) {
            Ok(())
        } else {
            self.error(&alloc::format!("\"{}\" attendu", op))
        }
    }

    fn expect_kw(&mut self, word: &str) -> PResult<()> {
        if self.eat_kw(word) {
            Ok(())
        } else {
            self.error(&alloc::format!("\"{}\" attendu", word))
        }
    }

    fn name(&mut self) -> PResult<Rc<str>> {
        match self.peek().clone() {
            Tok::Name(n) if !KEYWORDS.contains(&&*n) => {
                self.advance();
                Ok(n)
            }
            _ => self.error("nom attendu"),
        }
    }

    fn enter(&mut self) -> PResult<()> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return self.error("imbrication trop profonde");
        }
        Ok(())
    }

    fn leave(&mut self) {
        self.depth -= 1;
    }

    fn expr_at(&self, kind: ExprKind, line: u32) -> Expr {
        Expr { kind, line }
    }

    // --------------------------------------------------------
    //  Statements
    // --------------------------------------------------------

    fn file(&mut self) -> PResult<Vec<Stmt>> {
        let mut body = Vec::new();
        while !matches!(self.peek(), Tok::Eof) {
            if matches!(self.peek(), Tok::Newline) {
                self.advance();
                continue;
            }
            body.extend(self.statement()?);
        }
        Ok(body)
    }

    fn statement(&mut self) -> PResult<Vec<Stmt>> {
        self.enter()?;
        let result = self.statement_inner();
        self.leave();
        result
    }

    fn statement_inner(&mut self) -> PResult<Vec<Stmt>> {
        let line = self.line();
        let compound = match self.peek() {
            Tok::Name(n) => match &**n {
                "if" => Some(self.if_stmt()?),
                "while" => Some(self.while_stmt()?),
                "for" => Some(self.for_stmt()?),
                "try" => Some(self.try_stmt()?),
                "with" => Some(self.with_stmt()?),
                "def" => Some(self.def_stmt(Vec::new(), false)?),
                "class" => Some(self.class_stmt(Vec::new())?),
                "async" => {
                    self.advance();
                    if self.is_kw("def") {
                        Some(self.def_stmt(Vec::new(), true)?)
                    } else if self.is_kw("for") || self.is_kw("with") {
                        if !self.asyncs.last().copied().unwrap_or(false) {
                            let what = if self.is_kw("for") { "for" } else { "with" };
                            return self.error(&alloc::format!("'async {}' outside async function", what));
                        }
                        return if self.is_kw("for") { self.async_for(line) } else { self.async_with(line) };
                    } else {
                        return self.error("syntaxe invalide apres async");
                    }
                }
                "match" if self.match_statement_ahead() => Some(self.match_stmt()?),
                _ => None,
            },
            Tok::Op("@") => Some(self.decorated()?),
            _ => None,
        };
        if let Some(kind) = compound {
            return Ok(alloc::vec![Stmt { kind, line }]);
        }
        self.simple_statements()
    }

    /// `match x:` at the start of a line (match is a soft keyword).
    fn match_statement_ahead(&self) -> bool {
        // match <something> ... ':' NEWLINE: crude but sufficient.
        let mut i = self.pos + 1;
        let mut depth = 0i32;
        while i < self.tokens.len() {
            match &self.tokens[i].tok {
                Tok::Op("(") | Tok::Op("[") | Tok::Op("{") => depth += 1,
                Tok::Op(")") | Tok::Op("]") | Tok::Op("}") => depth -= 1,
                Tok::Op(":") if depth == 0 => return matches!(self.tokens.get(i + 1).map(|t| &t.tok), Some(Tok::Newline)),
                Tok::Op("=") if depth == 0 => return false,
                Tok::Newline | Tok::Eof => return false,
                _ => {}
            }
            i += 1;
        }
        false
    }

    // --------------------------------------------------------
    //  async for, async with: rewritten with await
    // --------------------------------------------------------

    fn hidden_name(&mut self) -> Rc<str> {
        self.hidden += 1;
        Rc::from(alloc::format!("$a{}", self.hidden).as_str())
    }

    /// `async for target in iterable: body else: orelse` becomes
    ///
    /// ```text
    /// $it = (iterable).__aiter__()
    /// while True:
    ///     try:
    ///         $v = await $it.__anext__()
    ///     except StopAsyncIteration:
    ///         orelse
    ///         break
    ///     target = $v
    ///     body
    /// ```
    fn async_for(&mut self, line: u32) -> PResult<Vec<Stmt>> {
        self.advance(); // for
        let target = self.target_list()?;
        self.expect_kw("in")?;
        let iterable = self.star_expressions()?;
        let body = self.block()?;
        let orelse = if self.is_kw("else") {
            self.advance();
            self.block()?
        } else {
            Vec::new()
        };
        let it = self.hidden_name();
        let v = self.hidden_name();
        let e = |kind: ExprKind| Expr { kind, line };
        let st = |kind: StmtKind| Stmt { kind, line };
        let call = |object: Expr, method: &str| {
            e(ExprKind::Call(Box::new(e(ExprKind::Attribute(Box::new(object), Rc::from(method)))), Vec::new()))
        };
        let mut handler_body = orelse;
        handler_body.push(st(StmtKind::Break));
        let fetch = st(StmtKind::Try {
            body: alloc::vec![st(StmtKind::Assign(
                alloc::vec![e(ExprKind::Name(v.clone()))],
                e(ExprKind::Await(Box::new(call(e(ExprKind::Name(it.clone())), "__anext__")))),
            ))],
            handlers: alloc::vec![ExceptHandler {
                kind: Some(e(ExprKind::Name(Rc::from("StopAsyncIteration")))),
                name: None,
                body: handler_body,
                line,
            }],
            orelse: Vec::new(),
            finalbody: Rc::new(Vec::new()),
        });
        let mut loop_body = alloc::vec![fetch, st(StmtKind::Assign(alloc::vec![target], e(ExprKind::Name(v))))];
        loop_body.extend(body);
        Ok(alloc::vec![
            st(StmtKind::Assign(alloc::vec![e(ExprKind::Name(it))], call(iterable, "__aiter__"))),
            st(StmtKind::While(e(ExprKind::Const(Const::True)), loop_body, Vec::new())),
        ])
    }

    /// `async with ctx as target: body` becomes
    ///
    /// ```text
    /// $m = ctx
    /// target = await $m.__aenter__()
    /// $f = False
    /// try:
    ///     try:
    ///         body
    ///     except BaseException as $e:
    ///         $f = True
    ///         if not await $m.__aexit__($e.__class__, $e, None):
    ///             raise
    /// finally:
    ///     if not $f:
    ///         await $m.__aexit__(None, None, None)
    /// ```
    fn async_with(&mut self, line: u32) -> PResult<Vec<Stmt>> {
        self.advance(); // with
        let parenthesized = self.is_op("(") && self.with_items_parenthesized();
        if parenthesized {
            self.advance();
        }
        let mut items = Vec::new();
        loop {
            if parenthesized && self.is_op(")") {
                break;
            }
            let context = self.expression()?;
            let target = if self.eat_kw("as") {
                let target = self.star_target()?;
                self.check_target(&target, false)?;
                Some(target)
            } else {
                None
            };
            items.push((context, target));
            if !self.eat_op(",") {
                break;
            }
        }
        if parenthesized {
            self.expect_op(")")?;
        }
        let mut body = self.block()?;
        let e = |kind: ExprKind| Expr { kind, line };
        let st = |kind: StmtKind| Stmt { kind, line };
        for (context, target) in items.into_iter().rev() {
            let m = self.hidden_name();
            let f = self.hidden_name();
            let x = self.hidden_name();
            let name = |n: &Rc<str>| e(ExprKind::Name(n.clone()));
            let method = |object: Expr, method: &str, args: Vec<Expr>| {
                e(ExprKind::Call(
                    Box::new(e(ExprKind::Attribute(Box::new(object), Rc::from(method)))),
                    args.into_iter().map(Arg::Positional).collect(),
                ))
            };
            let none = || e(ExprKind::Const(Const::None));
            let enter = e(ExprKind::Await(Box::new(method(name(&m), "__aenter__", Vec::new()))));
            let mut out = alloc::vec![st(StmtKind::Assign(alloc::vec![name(&m)], context))];
            match target {
                Some(t) => out.push(st(StmtKind::Assign(alloc::vec![t], enter))),
                None => out.push(st(StmtKind::Expr(enter))),
            }
            out.push(st(StmtKind::Assign(alloc::vec![name(&f)], e(ExprKind::Const(Const::False)))));
            let exit_on_error = e(ExprKind::Await(Box::new(method(
                name(&m),
                "__aexit__",
                alloc::vec![e(ExprKind::Attribute(Box::new(name(&x)), Rc::from("__class__"))), name(&x), none()],
            ))));
            let handler = ExceptHandler {
                kind: Some(e(ExprKind::Name(Rc::from("BaseException")))),
                name: Some(x.clone()),
                body: alloc::vec![
                    st(StmtKind::Assign(alloc::vec![name(&f)], e(ExprKind::Const(Const::True)))),
                    st(StmtKind::If(
                        e(ExprKind::Unary(UnaryOp::Not, Box::new(exit_on_error))),
                        alloc::vec![st(StmtKind::Raise(None, None))],
                        Vec::new(),
                    )),
                ],
                line,
            };
            let inner = st(StmtKind::Try { body, handlers: alloc::vec![handler], orelse: Vec::new(), finalbody: Rc::new(Vec::new()) });
            let exit_clean = e(ExprKind::Await(Box::new(method(name(&m), "__aexit__", alloc::vec![none(), none(), none()]))));
            let finally = alloc::vec![st(StmtKind::If(
                e(ExprKind::Unary(UnaryOp::Not, Box::new(name(&f)))),
                alloc::vec![st(StmtKind::Expr(exit_clean))],
                Vec::new(),
            ))];
            out.push(st(StmtKind::Try { body: alloc::vec![inner], handlers: Vec::new(), orelse: Vec::new(), finalbody: Rc::new(finally) }));
            body = out;
        }
        Ok(body)
    }

    fn match_stmt(&mut self) -> PResult<StmtKind> {
        self.advance(); // match
        let subject = self.star_expressions()?;
        self.expect_op(":")?;
        if !matches!(self.peek(), Tok::Newline) {
            return self.error("match attend un bloc de case");
        }
        self.advance();
        if !matches!(self.peek(), Tok::Indent) {
            return self.error("bloc indente attendu (IndentationError)");
        }
        self.advance();
        let mut cases = Vec::new();
        while !matches!(self.peek(), Tok::Dedent | Tok::Eof) {
            if matches!(self.peek(), Tok::Newline) {
                self.advance();
                continue;
            }
            let line = self.line();
            if !self.eat_kw("case") {
                return self.error("case attendu");
            }
            let pattern = self.open_pattern()?;
            let guard = if self.eat_kw("if") { Some(self.named_expression()?) } else { None };
            let body = self.block()?;
            cases.push(MatchCase { pattern, guard, body, line });
        }
        if matches!(self.peek(), Tok::Dedent) {
            self.advance();
        }
        if cases.is_empty() {
            return self.error("match sans case");
        }
        // An irrefutable case other than the last makes the rest
        // unreachable: CPython refuses it.
        for case in &cases[..cases.len() - 1] {
            if case.guard.is_none() && irrefutable(&case.pattern) {
                return Err(SyntaxError {
                    message: String::from("wildcard makes remaining patterns unreachable"),
                    line: case.line,
                });
            }
        }
        Ok(StmtKind::Match(subject, cases))
    }

    /// `case a, b:` is a sequence without brackets.
    fn open_pattern(&mut self) -> PResult<Pattern> {
        let first = self.as_pattern()?;
        if !self.is_op(",") {
            return Ok(first);
        }
        let mut items = alloc::vec![first];
        while self.eat_op(",") {
            if self.is_op(":") || self.is_kw("if") {
                break;
            }
            items.push(self.as_pattern()?);
        }
        Ok(Pattern::Sequence(items))
    }

    fn as_pattern(&mut self) -> PResult<Pattern> {
        self.enter()?;
        let result = self.as_pattern_inner();
        self.leave();
        result
    }

    fn as_pattern_inner(&mut self) -> PResult<Pattern> {
        let mut alternatives = alloc::vec![self.closed_pattern()?];
        while self.eat_op("|") {
            alternatives.push(self.closed_pattern()?);
        }
        let pattern = if alternatives.len() == 1 { alternatives.pop().unwrap_or(Pattern::Wildcard) } else { Pattern::Or(alternatives) };
        if self.eat_kw("as") {
            let name = self.name()?;
            if &*name == "_" {
                return self.error("cannot use '_' as a target");
            }
            return Ok(Pattern::As(Box::new(pattern), name));
        }
        Ok(pattern)
    }

    fn closed_pattern(&mut self) -> PResult<Pattern> {
        let line = self.line();
        match self.peek().clone() {
            Tok::Op("*") => {
                self.advance();
                let name = self.name()?;
                Ok(Pattern::Star(if &*name == "_" { None } else { Some(name) }))
            }
            Tok::Op("(") => {
                self.advance();
                if self.eat_op(")") {
                    return Ok(Pattern::Sequence(Vec::new()));
                }
                let first = self.as_pattern()?;
                if self.eat_op(")") {
                    // A parenthesized pattern, unless it is `(*x)`.
                    return Ok(match first {
                        Pattern::Star(_) => Pattern::Sequence(alloc::vec![first]),
                        other => other,
                    });
                }
                let mut items = alloc::vec![first];
                while self.eat_op(",") {
                    if self.is_op(")") {
                        break;
                    }
                    items.push(self.as_pattern()?);
                }
                self.expect_op(")")?;
                Ok(Pattern::Sequence(items))
            }
            Tok::Op("[") => {
                self.advance();
                let mut items = Vec::new();
                while !self.eat_op("]") {
                    items.push(self.as_pattern()?);
                    if !self.eat_op(",") {
                        self.expect_op("]")?;
                        break;
                    }
                }
                Ok(Pattern::Sequence(items))
            }
            Tok::Op("{") => {
                self.advance();
                let mut items = Vec::new();
                let mut rest = None;
                while !self.eat_op("}") {
                    if self.eat_op("**") {
                        rest = Some(self.name()?);
                        self.eat_op(",");
                        self.expect_op("}")?;
                        break;
                    }
                    let key = self.pattern_value()?;
                    self.expect_op(":")?;
                    let value = self.as_pattern()?;
                    items.push((key, value));
                    if !self.eat_op(",") {
                        self.expect_op("}")?;
                        break;
                    }
                }
                Ok(Pattern::Mapping(items, rest))
            }
            Tok::Name(n) if &*n == "None" || &*n == "True" || &*n == "False" => {
                self.advance();
                Ok(Pattern::Singleton(match &*n {
                    "None" => Const::None,
                    "True" => Const::True,
                    _ => Const::False,
                }))
            }
            Tok::Name(n) if !KEYWORDS.contains(&&*n) => {
                // A capture, a dotted value, or a class pattern.
                if !matches!(self.peek_at(1), Tok::Op(".") | Tok::Op("(")) {
                    self.advance();
                    return Ok(if &*n == "_" { Pattern::Wildcard } else { Pattern::Capture(n) });
                }
                self.advance();
                let mut expr = self.expr_at(ExprKind::Name(n), line);
                while self.eat_op(".") {
                    let attr = self.name()?;
                    expr = self.expr_at(ExprKind::Attribute(Box::new(expr), attr), line);
                }
                if !self.eat_op("(") {
                    return Ok(Pattern::Value(expr));
                }
                let mut args = Vec::new();
                let mut kwargs: Vec<(Rc<str>, Pattern)> = Vec::new();
                while !self.eat_op(")") {
                    if matches!(self.peek(), Tok::Name(_)) && matches!(self.peek_at(1), Tok::Op("=")) {
                        let key = self.name()?;
                        self.advance();
                        if kwargs.iter().any(|(k, _)| *k == key) {
                            return self.error(&alloc::format!("attribute name repeated in class pattern: {}", key));
                        }
                        kwargs.push((key, self.as_pattern()?));
                    } else {
                        if !kwargs.is_empty() {
                            return self.error("positional patterns follow keyword patterns");
                        }
                        args.push(self.as_pattern()?);
                    }
                    if !self.eat_op(",") {
                        self.expect_op(")")?;
                        break;
                    }
                }
                Ok(Pattern::Class(expr, args, kwargs))
            }
            _ => Ok(Pattern::Value(self.pattern_value()?)),
        }
    }

    /// A literal of a pattern: a number (signed, or a complex `a + bj`),
    /// strings, or a dotted name.
    fn pattern_value(&mut self) -> PResult<Expr> {
        let line = self.line();
        match self.peek().clone() {
            Tok::Str(_) | Tok::Bytes(_) => self.strings(),
            Tok::FString(_) => self.error("patterns may only match literals and attribute lookups"),
            Tok::Name(n) if !KEYWORDS.contains(&&*n) => {
                self.advance();
                let mut expr = self.expr_at(ExprKind::Name(n), line);
                while self.eat_op(".") {
                    let attr = self.name()?;
                    expr = self.expr_at(ExprKind::Attribute(Box::new(expr), attr), line);
                }
                Ok(expr)
            }
            Tok::Name(n) if &*n == "None" || &*n == "True" || &*n == "False" => self.atom(),
            Tok::Op("-") | Tok::Int(_) | Tok::Float(_) | Tok::Imag(_) => {
                let value = self.factor()?;
                if self.is_op("+") || self.is_op("-") {
                    let op = if self.eat_op("+") { BinOp::Add } else {
                        self.advance();
                        BinOp::Sub
                    };
                    let imaginary = self.factor()?;
                    return Ok(self.expr_at(ExprKind::BinOp(op, Box::new(value), Box::new(imaginary)), line));
                }
                Ok(value)
            }
            _ => self.error("motif invalide"),
        }
    }

    fn block(&mut self) -> PResult<Vec<Stmt>> {
        self.expect_op(":")?;
        if matches!(self.peek(), Tok::Newline) {
            self.advance();
            if !matches!(self.peek(), Tok::Indent) {
                return self.error("bloc indente attendu (IndentationError)");
            }
            self.advance();
            let mut body = Vec::new();
            while !matches!(self.peek(), Tok::Dedent | Tok::Eof) {
                if matches!(self.peek(), Tok::Newline) {
                    self.advance();
                    continue;
                }
                body.extend(self.statement()?);
            }
            if matches!(self.peek(), Tok::Dedent) {
                self.advance();
            }
            Ok(body)
        } else {
            self.simple_statements()
        }
    }

    fn simple_statements(&mut self) -> PResult<Vec<Stmt>> {
        let mut out = Vec::new();
        loop {
            out.push(self.simple_statement()?);
            if !self.eat_op(";") {
                break;
            }
            if matches!(self.peek(), Tok::Newline | Tok::Eof) {
                break;
            }
        }
        match self.peek() {
            Tok::Newline => {
                self.advance();
            }
            Tok::Eof | Tok::Dedent => {}
            _ => return self.error("syntaxe invalide"),
        }
        Ok(out)
    }

    fn simple_statement(&mut self) -> PResult<Stmt> {
        let line = self.line();
        let kind = match self.peek().clone() {
            Tok::Name(n) => match &*n {
                "pass" => {
                    self.advance();
                    StmtKind::Pass
                }
                "break" => {
                    self.advance();
                    StmtKind::Break
                }
                "continue" => {
                    self.advance();
                    StmtKind::Continue
                }
                "return" => {
                    self.advance();
                    if self.functions == 0 {
                        return self.error("'return' hors d'une fonction");
                    }
                    if self.at_statement_end() {
                        StmtKind::Return(None)
                    } else {
                        StmtKind::Return(Some(self.star_expressions()?))
                    }
                }
                "raise" => {
                    self.advance();
                    if self.at_statement_end() {
                        StmtKind::Raise(None, None)
                    } else {
                        let exc = self.expression()?;
                        let cause = if self.eat_kw("from") { Some(self.expression()?) } else { None };
                        StmtKind::Raise(Some(exc), cause)
                    }
                }
                "global" | "nonlocal" => {
                    self.advance();
                    let mut names = alloc::vec![self.name()?];
                    while self.eat_op(",") {
                        names.push(self.name()?);
                    }
                    if &*n == "global" { StmtKind::Global(names) } else { StmtKind::Nonlocal(names) }
                }
                "del" => {
                    self.advance();
                    let mut targets = alloc::vec![self.bitwise_or()?];
                    while self.eat_op(",") {
                        if self.at_statement_end() {
                            break;
                        }
                        targets.push(self.bitwise_or()?);
                    }
                    for target in &targets {
                        self.check_target(target, true)?;
                    }
                    StmtKind::Delete(targets)
                }
                "assert" => {
                    self.advance();
                    let test = self.expression()?;
                    let message = if self.eat_op(",") { Some(self.expression()?) } else { None };
                    StmtKind::Assert(test, message)
                }
                "import" => {
                    self.advance();
                    let mut names = Vec::new();
                    loop {
                        let dotted = self.dotted_name()?;
                        let alias = if self.eat_kw("as") { Some(self.name()?) } else { None };
                        names.push((dotted, alias));
                        if !self.eat_op(",") {
                            break;
                        }
                    }
                    StmtKind::Import(names)
                }
                "from" => self.from_import()?,
                _ => return self.expression_statement(),
            },
            _ => return self.expression_statement(),
        };
        Ok(Stmt { kind, line })
    }

    fn at_statement_end(&self) -> bool {
        matches!(self.peek(), Tok::Newline | Tok::Eof | Tok::Dedent) || self.is_op(";")
    }

    fn dotted_name(&mut self) -> PResult<Rc<str>> {
        let mut name = String::from(&*self.name()?);
        while self.eat_op(".") {
            name.push('.');
            name.push_str(&self.name()?);
        }
        Ok(Rc::from(name.as_str()))
    }

    fn from_import(&mut self) -> PResult<StmtKind> {
        self.advance(); // from
        let mut level = 0;
        loop {
            if self.eat_op(".") {
                level += 1;
            } else if self.eat_op("...") {
                level += 3;
            } else {
                break;
            }
        }
        let module = if self.is_kw("import") { None } else { Some(self.dotted_name()?) };
        self.expect_kw("import")?;
        if self.eat_op("*") {
            return Ok(StmtKind::ImportFrom(module, level, alloc::vec![(Rc::from("*"), None)]));
        }
        let parenthesized = self.eat_op("(");
        let mut names = Vec::new();
        loop {
            if parenthesized && self.is_op(")") {
                break;
            }
            let name = self.name()?;
            let alias = if self.eat_kw("as") { Some(self.name()?) } else { None };
            names.push((name, alias));
            if !self.eat_op(",") {
                break;
            }
        }
        if parenthesized {
            self.expect_op(")")?;
        }
        Ok(StmtKind::ImportFrom(module, level, names))
    }

    fn expression_statement(&mut self) -> PResult<Stmt> {
        let line = self.line();
        let first = self.star_expressions_or_yield()?;
        // Annotated assignment: `x: int = 1`.
        if self.is_op(":") {
            self.advance();
            self.check_target(&first, false)?;
            let annotation = self.expression()?;
            let value = if self.eat_op("=") { Some(self.star_expressions_or_yield()?) } else { None };
            return Ok(Stmt { kind: StmtKind::AnnAssign(first, annotation, value), line });
        }
        // Augmented assignment.
        if let Tok::Op(op) = self.peek().clone() {
            let aug = match op {
                "+=" => Some(BinOp::Add),
                "-=" => Some(BinOp::Sub),
                "*=" => Some(BinOp::Mul),
                "@=" => Some(BinOp::MatMul),
                "/=" => Some(BinOp::Div),
                "//=" => Some(BinOp::FloorDiv),
                "%=" => Some(BinOp::Mod),
                "**=" => Some(BinOp::Pow),
                "<<=" => Some(BinOp::LShift),
                ">>=" => Some(BinOp::RShift),
                "|=" => Some(BinOp::BitOr),
                "^=" => Some(BinOp::BitXor),
                "&=" => Some(BinOp::BitAnd),
                _ => None,
            };
            if let Some(op) = aug {
                self.advance();
                if !matches!(first.kind, ExprKind::Name(_) | ExprKind::Attribute(..) | ExprKind::Subscript(..)) {
                    return self.error("cible invalide pour une affectation augmentee");
                }
                let value = self.star_expressions_or_yield()?;
                return Ok(Stmt { kind: StmtKind::AugAssign(first, op, value), line });
            }
        }
        if self.is_op("=") {
            let mut targets = alloc::vec![first];
            let mut value;
            loop {
                self.advance(); // =
                value = self.star_expressions_or_yield()?;
                if !self.is_op("=") {
                    break;
                }
                targets.push(value);
            }
            for target in &targets {
                self.check_target(target, false)?;
            }
            return Ok(Stmt { kind: StmtKind::Assign(targets, value), line });
        }
        Ok(Stmt { kind: StmtKind::Expr(first), line })
    }

    /// Is `expr` something one can assign to (or delete)?
    fn check_target(&self, expr: &Expr, delete: bool) -> PResult<()> {
        match &expr.kind {
            ExprKind::Name(_) | ExprKind::Attribute(..) | ExprKind::Subscript(..) => Ok(()),
            ExprKind::Tuple(items) | ExprKind::List(items) => {
                let mut starred = 0;
                for item in items {
                    if let ExprKind::Starred(inner) = &item.kind {
                        if delete {
                            return Err(SyntaxError { message: String::from("cible invalide"), line: expr.line });
                        }
                        starred += 1;
                        self.check_target(inner, delete)?;
                    } else {
                        self.check_target(item, delete)?;
                    }
                }
                if starred > 1 {
                    return Err(SyntaxError { message: String::from("plusieurs expressions etoilees dans une affectation"), line: expr.line });
                }
                Ok(())
            }
            _ => Err(SyntaxError {
                message: String::from(if delete { "impossible de supprimer cette expression" } else { "impossible d'affecter a cette expression" }),
                line: expr.line,
            }),
        }
    }

    fn if_stmt(&mut self) -> PResult<StmtKind> {
        self.advance(); // if / elif
        let test = self.named_expression()?;
        let body = self.block()?;
        let orelse = if self.is_kw("elif") {
            let line = self.line();
            alloc::vec![Stmt { kind: self.if_stmt()?, line }]
        } else if self.eat_kw("else") {
            self.block()?
        } else {
            Vec::new()
        };
        Ok(StmtKind::If(test, body, orelse))
    }

    fn while_stmt(&mut self) -> PResult<StmtKind> {
        self.advance();
        let test = self.named_expression()?;
        let body = self.block()?;
        let orelse = if self.eat_kw("else") { self.block()? } else { Vec::new() };
        Ok(StmtKind::While(test, body, orelse))
    }

    fn for_stmt(&mut self) -> PResult<StmtKind> {
        self.advance();
        let target = self.target_list()?;
        self.expect_kw("in")?;
        let iter = self.star_expressions()?;
        let body = self.block()?;
        let orelse = if self.eat_kw("else") { self.block()? } else { Vec::new() };
        Ok(StmtKind::For(target, iter, body, orelse))
    }

    /// The target of a `for` or a comprehension: `a`, `a, b`, `(a, *b)`.
    fn target_list(&mut self) -> PResult<Expr> {
        let line = self.line();
        let first = self.star_target()?;
        if !self.is_op(",") {
            self.check_target(&first, false)?;
            return Ok(first);
        }
        let mut items = alloc::vec![first];
        while self.eat_op(",") {
            if self.is_kw("in") || self.is_op("=") {
                break;
            }
            items.push(self.star_target()?);
        }
        let tuple = self.expr_at(ExprKind::Tuple(items), line);
        self.check_target(&tuple, false)?;
        Ok(tuple)
    }

    fn star_target(&mut self) -> PResult<Expr> {
        let line = self.line();
        if self.eat_op("*") {
            let inner = self.bitwise_or()?;
            return Ok(self.expr_at(ExprKind::Starred(Box::new(inner)), line));
        }
        self.bitwise_or()
    }

    fn try_stmt(&mut self) -> PResult<StmtKind> {
        self.advance();
        let body = self.block()?;
        let mut handlers = Vec::new();
        while self.is_kw("except") {
            let line = self.line();
            self.advance();
            if self.is_op("*") {
                return self.error("except* n'est pas pris en charge");
            }
            let (kind, name) = if self.is_op(":") {
                (None, None)
            } else {
                let kind = self.expression()?;
                let name = if self.eat_kw("as") { Some(self.name()?) } else { None };
                (Some(kind), name)
            };
            let body = self.block()?;
            handlers.push(ExceptHandler { kind, name, body, line });
        }
        let orelse = if self.eat_kw("else") { self.block()? } else { Vec::new() };
        let finalbody = if self.eat_kw("finally") { self.block()? } else { Vec::new() };
        if handlers.is_empty() && finalbody.is_empty() {
            return self.error("\"except\" ou \"finally\" attendu");
        }
        Ok(StmtKind::Try { body, handlers, orelse, finalbody: Rc::new(finalbody) })
    }

    fn with_stmt(&mut self) -> PResult<StmtKind> {
        self.advance();
        let parenthesized = self.is_op("(") && self.with_items_parenthesized();
        if parenthesized {
            self.advance();
        }
        let mut items = Vec::new();
        loop {
            if parenthesized && self.is_op(")") {
                break;
            }
            let context = self.expression()?;
            let target = if self.eat_kw("as") {
                let target = self.star_target()?;
                self.check_target(&target, false)?;
                Some(target)
            } else {
                None
            };
            items.push((context, target));
            if !self.eat_op(",") {
                break;
            }
        }
        if parenthesized {
            self.expect_op(")")?;
        }
        let body = self.block()?;
        Ok(StmtKind::With(items, body))
    }

    /// `with (a as b, c as d):` -- the parenthesis holds items, not a
    /// tuple: an `as` at depth 1 says so.
    fn with_items_parenthesized(&self) -> bool {
        let mut depth = 0i32;
        let mut i = self.pos;
        while i < self.tokens.len() {
            match &self.tokens[i].tok {
                Tok::Op("(") | Tok::Op("[") | Tok::Op("{") => depth += 1,
                Tok::Op(")") | Tok::Op("]") | Tok::Op("}") => {
                    depth -= 1;
                    if depth == 0 {
                        return matches!(self.tokens.get(i + 1).map(|t| &t.tok), Some(Tok::Op(":")));
                    }
                }
                Tok::Name(n) if &**n == "as" && depth == 1 => return true,
                Tok::Newline | Tok::Eof => return false,
                _ => {}
            }
            i += 1;
        }
        false
    }

    fn decorated(&mut self) -> PResult<StmtKind> {
        let mut decorators = Vec::new();
        while self.eat_op("@") {
            decorators.push(self.named_expression()?);
            if !matches!(self.peek(), Tok::Newline) {
                return self.error("fin de ligne attendue apres un decorateur");
            }
            self.advance();
        }
        if self.is_kw("def") {
            self.def_stmt(decorators, false)
        } else if self.is_kw("async") && self.kw_at(1, "def") {
            self.advance();
            self.def_stmt(decorators, true)
        } else if self.is_kw("class") {
            self.class_stmt(decorators)
        } else {
            self.error("def ou class attendu apres un decorateur")
        }
    }

    fn def_stmt(&mut self, decorators: Vec<Expr>, is_async: bool) -> PResult<StmtKind> {
        let line = self.line();
        self.advance(); // def
        let name = self.name()?;
        if self.is_op("[") {
            return self.error("les parametres de type (PEP 695) ne sont pas pris en charge");
        }
        self.expect_op("(")?;
        let params = self.parameters(")", true)?;
        self.expect_op(")")?;
        if self.eat_op("->") {
            self.expression()?;
        }
        self.functions += 1;
        self.asyncs.push(is_async);
        let body = self.block();
        self.asyncs.pop();
        self.functions -= 1;
        Ok(StmtKind::FunctionDef(Rc::new(FunctionDef { name, params: Rc::new(params), body: body?, decorators, line, is_async })))
    }

    fn class_stmt(&mut self, decorators: Vec<Expr>) -> PResult<StmtKind> {
        let line = self.line();
        self.advance(); // class
        let name = self.name()?;
        let mut bases = Vec::new();
        let mut keywords = Vec::new();
        if self.eat_op("(") {
            for arg in self.arguments()? {
                match arg {
                    Arg::Positional(e) => bases.push(e),
                    Arg::Keyword(k, e) => keywords.push((k, e)),
                    _ => return self.error("*args et **kwargs ne sont pas pris en charge dans une classe"),
                }
            }
        }
        // A class body is not a function: `return` stays forbidden there.
        let saved = core::mem::replace(&mut self.functions, 0);
        self.asyncs.push(false);
        let body = self.block();
        self.asyncs.pop();
        self.functions = saved;
        Ok(StmtKind::ClassDef(Rc::new(ClassDef { name, bases, keywords, body: body?, decorators, line })))
    }

    /// A parameter list, up to `close` (`)` for def, `:` for lambda).
    fn parameters(&mut self, close: &str, annotations: bool) -> PResult<Params> {
        let mut params = Params::default();
        let mut seen_default = false;
        let mut keyword_only = false;
        while !self.is_op(close) {
            if self.eat_op("/") {
                if keyword_only || !params.posonly.is_empty() {
                    return self.error("'/' mal place");
                }
                params.posonly = core::mem::take(&mut params.args);
            } else if self.eat_op("**") {
                params.kwarg = Some(self.name()?);
                if annotations && self.eat_op(":") {
                    self.expression()?;
                }
            } else if self.eat_op("*") {
                keyword_only = true;
                if !self.is_op(",") && !self.is_op(close) {
                    params.vararg = Some(self.name()?);
                    if annotations && self.eat_op(":") {
                        self.expression()?;
                    }
                }
            } else {
                let name = self.name()?;
                if annotations && self.eat_op(":") {
                    self.expression()?;
                }
                let default = if self.eat_op("=") { Some(self.expression()?) } else { None };
                if keyword_only {
                    params.kwonly.push(Param { name, default });
                } else {
                    if default.is_some() {
                        seen_default = true;
                    } else if seen_default {
                        return self.error("un parametre sans valeur par defaut suit un parametre qui en a une");
                    }
                    params.args.push(Param { name, default });
                }
            }
            if !self.eat_op(",") {
                break;
            }
        }
        Ok(params)
    }

    // --------------------------------------------------------
    //  Expressions
    // --------------------------------------------------------

    /// `a, b, *c` -> a tuple when there is a comma.
    fn star_expressions(&mut self) -> PResult<Expr> {
        let line = self.line();
        let first = self.star_expression()?;
        if !self.is_op(",") {
            return Ok(first);
        }
        let mut items = alloc::vec![first];
        while self.eat_op(",") {
            if self.expression_ends() {
                break;
            }
            items.push(self.star_expression()?);
        }
        Ok(self.expr_at(ExprKind::Tuple(items), line))
    }

    fn star_expressions_or_yield(&mut self) -> PResult<Expr> {
        if self.is_kw("yield") {
            return self.yield_expr();
        }
        self.star_expressions()
    }

    fn expression_ends(&self) -> bool {
        match self.peek() {
            Tok::Newline | Tok::Eof | Tok::Dedent => true,
            Tok::Op(op) => matches!(*op, ")" | "]" | "}" | "=" | ":" | ";"),
            Tok::Name(n) => matches!(&**n, "in" | "for" | "if"),
            _ => false,
        }
    }

    fn star_expression(&mut self) -> PResult<Expr> {
        let line = self.line();
        if self.eat_op("*") {
            let inner = self.bitwise_or()?;
            return Ok(self.expr_at(ExprKind::Starred(Box::new(inner)), line));
        }
        self.named_expression()
    }

    fn yield_expr(&mut self) -> PResult<Expr> {
        let line = self.line();
        self.advance(); // yield
        if self.functions == 0 {
            return self.error("'yield' hors d'une fonction");
        }
        if self.eat_kw("from") {
            let value = self.expression()?;
            return Ok(self.expr_at(ExprKind::YieldFrom(Box::new(value)), line));
        }
        if self.expression_ends() || self.is_op(")") {
            return Ok(self.expr_at(ExprKind::Yield(None), line));
        }
        let value = self.star_expressions()?;
        Ok(self.expr_at(ExprKind::Yield(Some(Box::new(value))), line))
    }

    fn named_expression(&mut self) -> PResult<Expr> {
        if let Tok::Name(name) = self.peek().clone() {
            if matches!(self.peek_at(1), Tok::Op(":=")) && !KEYWORDS.contains(&&*name) {
                let line = self.line();
                self.advance();
                self.advance();
                let value = self.expression()?;
                return Ok(self.expr_at(ExprKind::Named(name, Box::new(value)), line));
            }
        }
        self.expression()
    }

    pub fn expression(&mut self) -> PResult<Expr> {
        self.enter()?;
        let result = self.expression_inner();
        self.leave();
        result
    }

    fn expression_inner(&mut self) -> PResult<Expr> {
        let line = self.line();
        if self.is_kw("lambda") {
            self.advance();
            let params = self.parameters(":", false)?;
            self.expect_op(":")?;
            self.functions += 1;
            self.asyncs.push(false);
            let body = self.expression();
            self.asyncs.pop();
            self.functions -= 1;
            return Ok(self.expr_at(ExprKind::Lambda(Rc::new(params), Box::new(body?)), line));
        }
        let test = self.disjunction()?;
        if self.is_kw("if") && !self.kw_at(0, "else") {
            // Only a conditional when `else` follows: in a comprehension,
            // `if` belongs to the comprehension.
            let save = self.pos;
            self.advance();
            let condition = self.disjunction()?;
            if self.eat_kw("else") {
                let otherwise = self.expression()?;
                return Ok(self.expr_at(ExprKind::IfExp(Box::new(condition), Box::new(test), Box::new(otherwise)), line));
            }
            self.pos = save;
        }
        Ok(test)
    }

    fn disjunction(&mut self) -> PResult<Expr> {
        let line = self.line();
        let first = self.conjunction()?;
        if !self.is_kw("or") {
            return Ok(first);
        }
        let mut values = alloc::vec![first];
        while self.eat_kw("or") {
            values.push(self.conjunction()?);
        }
        Ok(self.expr_at(ExprKind::BoolOp(false, values), line))
    }

    fn conjunction(&mut self) -> PResult<Expr> {
        let line = self.line();
        let first = self.inversion()?;
        if !self.is_kw("and") {
            return Ok(first);
        }
        let mut values = alloc::vec![first];
        while self.eat_kw("and") {
            values.push(self.inversion()?);
        }
        Ok(self.expr_at(ExprKind::BoolOp(true, values), line))
    }

    fn inversion(&mut self) -> PResult<Expr> {
        let line = self.line();
        if self.eat_kw("not") {
            self.enter()?;
            let operand = self.inversion();
            self.leave();
            return Ok(self.expr_at(ExprKind::Unary(UnaryOp::Not, Box::new(operand?)), line));
        }
        self.comparison()
    }

    fn comparison_operator(&mut self) -> Option<CmpOp> {
        let op = match self.peek() {
            Tok::Op("==") => CmpOp::Eq,
            Tok::Op("!=") => CmpOp::NotEq,
            Tok::Op("<") => CmpOp::Lt,
            Tok::Op("<=") => CmpOp::LtE,
            Tok::Op(">") => CmpOp::Gt,
            Tok::Op(">=") => CmpOp::GtE,
            Tok::Name(n) if &**n == "in" => CmpOp::In,
            Tok::Name(n) if &**n == "is" => {
                if self.kw_at(1, "not") {
                    self.advance();
                    self.advance();
                    return Some(CmpOp::IsNot);
                }
                CmpOp::Is
            }
            Tok::Name(n) if &**n == "not" && self.kw_at(1, "in") => {
                self.advance();
                self.advance();
                return Some(CmpOp::NotIn);
            }
            _ => return None,
        };
        self.advance();
        Some(op)
    }

    fn comparison(&mut self) -> PResult<Expr> {
        let line = self.line();
        let first = self.bitwise_or()?;
        let mut rest = Vec::new();
        while let Some(op) = self.comparison_operator() {
            rest.push((op, self.bitwise_or()?));
        }
        if rest.is_empty() {
            return Ok(first);
        }
        Ok(self.expr_at(ExprKind::Compare(Box::new(first), rest), line))
    }

    fn binary_level(&mut self, level: u8) -> PResult<Expr> {
        if level == 6 {
            return self.factor();
        }
        self.enter()?;
        let line = self.line();
        let mut left = self.binary_level(level + 1)?;
        loop {
            let op = match (level, self.peek()) {
                (0, Tok::Op("|")) => BinOp::BitOr,
                (1, Tok::Op("^")) => BinOp::BitXor,
                (2, Tok::Op("&")) => BinOp::BitAnd,
                (3, Tok::Op("<<")) => BinOp::LShift,
                (3, Tok::Op(">>")) => BinOp::RShift,
                (4, Tok::Op("+")) => BinOp::Add,
                (4, Tok::Op("-")) => BinOp::Sub,
                (5, Tok::Op("*")) => BinOp::Mul,
                (5, Tok::Op("/")) => BinOp::Div,
                (5, Tok::Op("//")) => BinOp::FloorDiv,
                (5, Tok::Op("%")) => BinOp::Mod,
                (5, Tok::Op("@")) => BinOp::MatMul,
                _ => break,
            };
            self.advance();
            let right = self.binary_level(level + 1)?;
            left = self.expr_at(ExprKind::BinOp(op, Box::new(left), Box::new(right)), line);
        }
        self.leave();
        Ok(left)
    }

    fn bitwise_or(&mut self) -> PResult<Expr> {
        self.binary_level(0)
    }

    fn factor(&mut self) -> PResult<Expr> {
        let line = self.line();
        let op = match self.peek() {
            Tok::Op("-") => Some(UnaryOp::Neg),
            Tok::Op("+") => Some(UnaryOp::Pos),
            Tok::Op("~") => Some(UnaryOp::Invert),
            _ => None,
        };
        if let Some(op) = op {
            self.advance();
            self.enter()?;
            let operand = self.factor();
            self.leave();
            let operand = operand?;
            // `-1` is a constant, and `-9223372036854775808` fits.
            if op == UnaryOp::Neg {
                match &operand.kind {
                    ExprKind::Const(Const::Int(n)) => return Ok(self.expr_at(ExprKind::Const(Const::Int(n.neg())), line)),
                    ExprKind::Const(Const::Float(f)) => return Ok(self.expr_at(ExprKind::Const(Const::Float(f.neg())), line)),
                    _ => {}
                }
            }
            return Ok(self.expr_at(ExprKind::Unary(op, Box::new(operand)), line));
        }
        self.power()
    }

    fn power(&mut self) -> PResult<Expr> {
        let line = self.line();
        let base = if self.is_kw("await") {
            if self.asyncs.is_empty() {
                return self.error("'await' outside function");
            }
            if !self.asyncs.last().copied().unwrap_or(false) {
                return self.error("'await' outside async function");
            }
            self.advance();
            self.enter()?;
            let operand = self.primary();
            self.leave();
            self.expr_at(ExprKind::Await(Box::new(operand?)), line)
        } else {
            self.primary()?
        };
        if self.eat_op("**") {
            self.enter()?;
            let exponent = self.factor();
            self.leave();
            return Ok(self.expr_at(ExprKind::BinOp(BinOp::Pow, Box::new(base), Box::new(exponent?)), line));
        }
        Ok(base)
    }

    fn primary(&mut self) -> PResult<Expr> {
        let mut expr = self.atom()?;
        loop {
            let line = self.line();
            if self.eat_op(".") {
                let name = match self.advance() {
                    Tok::Name(n) => n,
                    _ => return self.error("nom d'attribut attendu"),
                };
                expr = self.expr_at(ExprKind::Attribute(Box::new(expr), name), line);
            } else if self.eat_op("(") {
                let args = self.arguments()?;
                expr = self.expr_at(ExprKind::Call(Box::new(expr), args), line);
            } else if self.eat_op("[") {
                let index = self.slices()?;
                self.expect_op("]")?;
                expr = self.expr_at(ExprKind::Subscript(Box::new(expr), Box::new(index)), line);
            } else {
                break;
            }
        }
        Ok(expr)
    }

    /// After `(`, to the `)` included.
    fn arguments(&mut self) -> PResult<Vec<Arg>> {
        let mut args = Vec::new();
        while !self.is_op(")") {
            if self.eat_op("*") {
                args.push(Arg::Starred(self.expression()?));
            } else if self.eat_op("**") {
                args.push(Arg::DoubleStarred(self.expression()?));
            } else if matches!(self.peek(), Tok::Name(_)) && matches!(self.peek_at(1), Tok::Op("=")) {
                let name = self.name()?;
                self.advance();
                args.push(Arg::Keyword(name, self.expression()?));
            } else {
                let line = self.line();
                let value = self.named_expression()?;
                // `f(x for x in y)`: a generator as the only argument.
                if self.comprehension_ahead() {
                    let generators = self.comprehension_clauses()?;
                    args.push(Arg::Positional(self.expr_at(ExprKind::Generator(Box::new(value), generators), line)));
                } else {
                    args.push(Arg::Positional(value));
                }
            }
            if !self.eat_op(",") {
                break;
            }
        }
        self.expect_op(")")?;
        Ok(args)
    }

    /// Inside `[...]` of a subscript: an index, a slice, or a tuple of
    /// them.
    fn slices(&mut self) -> PResult<Expr> {
        let line = self.line();
        let first = self.slice()?;
        if !self.is_op(",") {
            return Ok(first);
        }
        let mut items = alloc::vec![first];
        while self.eat_op(",") {
            if self.is_op("]") {
                break;
            }
            items.push(self.slice()?);
        }
        Ok(self.expr_at(ExprKind::Tuple(items), line))
    }

    fn slice(&mut self) -> PResult<Expr> {
        let line = self.line();
        let lower = if self.is_op(":") { None } else { Some(Box::new(self.star_expression()?)) };
        if !self.eat_op(":") {
            return lower.map(|b| *b).ok_or_else(|| SyntaxError { message: String::from("indice attendu"), line });
        }
        let upper = if self.is_op(":") || self.is_op("]") || self.is_op(",") { None } else { Some(Box::new(self.expression()?)) };
        let step = if self.eat_op(":") {
            if self.is_op("]") || self.is_op(",") { None } else { Some(Box::new(self.expression()?)) }
        } else {
            None
        };
        Ok(self.expr_at(ExprKind::Slice(lower, upper, step), line))
    }

    /// `for` or `async for`: a comprehension clause starts here.
    fn comprehension_ahead(&self) -> bool {
        self.is_kw("for") || (self.is_kw("async") && self.kw_at(1, "for"))
    }

    fn comprehension_clauses(&mut self) -> PResult<Vec<Comprehension>> {
        let mut out = Vec::new();
        while self.comprehension_ahead() {
            let is_async = self.eat_kw("async");
            if is_async && !self.asyncs.last().copied().unwrap_or(false) {
                return self.error("asynchronous comprehension outside of an asynchronous function");
            }
            self.advance();
            let target = self.target_list()?;
            self.expect_kw("in")?;
            let iter = self.disjunction()?;
            let mut ifs = Vec::new();
            while self.is_kw("if") {
                self.advance();
                ifs.push(self.disjunction()?);
            }
            out.push(Comprehension { target, iter, ifs, is_async });
        }
        Ok(out)
    }

    fn atom(&mut self) -> PResult<Expr> {
        self.enter()?;
        let result = self.atom_inner();
        self.leave();
        result
    }

    fn atom_inner(&mut self) -> PResult<Expr> {
        let line = self.line();
        match self.peek().clone() {
            Tok::Name(n) => {
                self.advance();
                let kind = match &*n {
                    "True" => ExprKind::Const(Const::True),
                    "False" => ExprKind::Const(Const::False),
                    "None" => ExprKind::Const(Const::None),
                    _ if KEYWORDS.contains(&&*n) => {
                        return Err(SyntaxError { message: alloc::format!("syntaxe invalide pres de '{}'", n), line });
                    }
                    _ => ExprKind::Name(n),
                };
                Ok(self.expr_at(kind, line))
            }
            Tok::Int(i) => {
                self.advance();
                Ok(self.expr_at(ExprKind::Const(Const::Int(i)), line))
            }
            Tok::Float(f) => {
                self.advance();
                Ok(self.expr_at(ExprKind::Const(Const::Float(f)), line))
            }
            Tok::Imag(f) => {
                self.advance();
                Ok(self.expr_at(ExprKind::Const(Const::Imag(f)), line))
            }
            Tok::Str(_) | Tok::FString(_) | Tok::Bytes(_) => self.strings(),
            Tok::Op("...") => {
                self.advance();
                Ok(self.expr_at(ExprKind::Const(Const::Ellipsis), line))
            }
            Tok::Op("(") => {
                self.advance();
                if self.eat_op(")") {
                    return Ok(self.expr_at(ExprKind::Tuple(Vec::new()), line));
                }
                if self.is_kw("yield") {
                    let value = self.yield_expr()?;
                    self.expect_op(")")?;
                    return Ok(value);
                }
                let first = self.star_expression()?;
                if self.comprehension_ahead() {
                    let generators = self.comprehension_clauses()?;
                    self.expect_op(")")?;
                    return Ok(self.expr_at(ExprKind::Generator(Box::new(first), generators), line));
                }
                if self.eat_op(")") {
                    if matches!(first.kind, ExprKind::Starred(_)) {
                        return self.error("expression etoilee hors d'un contexte qui l'accepte");
                    }
                    return Ok(first);
                }
                let mut items = alloc::vec![first];
                while self.eat_op(",") {
                    if self.is_op(")") {
                        break;
                    }
                    items.push(self.star_expression()?);
                }
                self.expect_op(")")?;
                Ok(self.expr_at(ExprKind::Tuple(items), line))
            }
            Tok::Op("[") => {
                self.advance();
                if self.eat_op("]") {
                    return Ok(self.expr_at(ExprKind::List(Vec::new()), line));
                }
                let first = self.star_expression()?;
                if self.comprehension_ahead() {
                    let generators = self.comprehension_clauses()?;
                    self.expect_op("]")?;
                    return Ok(self.expr_at(ExprKind::ListComp(Box::new(first), generators), line));
                }
                let mut items = alloc::vec![first];
                while self.eat_op(",") {
                    if self.is_op("]") {
                        break;
                    }
                    items.push(self.star_expression()?);
                }
                self.expect_op("]")?;
                Ok(self.expr_at(ExprKind::List(items), line))
            }
            Tok::Op("{") => self.dict_or_set(),
            Tok::Op(op) => Err(SyntaxError { message: alloc::format!("syntaxe invalide pres de '{}'", op), line }),
            Tok::Newline | Tok::Eof => self.error("fin de ligne inattendue"),
            Tok::Indent => self.error("indentation inattendue (IndentationError)"),
            Tok::Dedent => self.error("syntaxe invalide"),
        }
    }

    fn dict_or_set(&mut self) -> PResult<Expr> {
        let line = self.line();
        self.advance(); // {
        if self.eat_op("}") {
            return Ok(self.expr_at(ExprKind::Dict(Vec::new()), line));
        }
        // `**mapping` first: a dict.
        if self.eat_op("**") {
            let mapping = self.bitwise_or()?;
            return self.dict_rest(alloc::vec![(None, mapping)], line);
        }
        let first = self.star_expression()?;
        if self.eat_op(":") {
            let value = self.expression()?;
            if self.comprehension_ahead() {
                let generators = self.comprehension_clauses()?;
                self.expect_op("}")?;
                return Ok(self.expr_at(ExprKind::DictComp(Box::new(first), Box::new(value), generators), line));
            }
            return self.dict_rest(alloc::vec![(Some(first), value)], line);
        }
        if self.comprehension_ahead() {
            let generators = self.comprehension_clauses()?;
            self.expect_op("}")?;
            return Ok(self.expr_at(ExprKind::SetComp(Box::new(first), generators), line));
        }
        let mut items = alloc::vec![first];
        while self.eat_op(",") {
            if self.is_op("}") {
                break;
            }
            items.push(self.star_expression()?);
        }
        self.expect_op("}")?;
        Ok(self.expr_at(ExprKind::Set(items), line))
    }

    fn dict_rest(&mut self, mut pairs: Vec<(Option<Expr>, Expr)>, line: u32) -> PResult<Expr> {
        while self.eat_op(",") {
            if self.is_op("}") {
                break;
            }
            if self.eat_op("**") {
                pairs.push((None, self.bitwise_or()?));
            } else {
                let key = self.expression()?;
                self.expect_op(":")?;
                pairs.push((Some(key), self.expression()?));
            }
        }
        self.expect_op("}")?;
        Ok(self.expr_at(ExprKind::Dict(pairs), line))
    }

    /// Adjacent string literals, concatenated; f-strings among them make
    /// the whole an f-string.
    fn strings(&mut self) -> PResult<Expr> {
        let line = self.line();
        let mut parts: Vec<FPart> = Vec::new();
        let mut bytes: Option<Vec<u8>> = None;
        let mut any_f = false;
        let mut text = String::new();
        loop {
            match self.peek().clone() {
                Tok::Str(s) => {
                    if bytes.is_some() {
                        return self.error("melange de chaines et d'octets");
                    }
                    text.push_str(&s);
                }
                Tok::Bytes(b) => {
                    if any_f || !text.is_empty() || !parts.is_empty() {
                        return self.error("melange de chaines et d'octets");
                    }
                    bytes.get_or_insert_with(Vec::new).extend_from_slice(&b);
                }
                Tok::FString(fparts) => {
                    if bytes.is_some() {
                        return self.error("melange de chaines et d'octets");
                    }
                    any_f = true;
                    for part in fparts {
                        match part {
                            lex::FPart::Text(t) => text.push_str(&t),
                            lex::FPart::Field(field) => {
                                if !text.is_empty() {
                                    parts.push(FPart::Text(Rc::from(core::mem::take(&mut text).as_str())));
                                }
                                parts.push(self.fstring_field(field)?);
                            }
                        }
                    }
                }
                _ => break,
            }
            self.advance();
        }
        if let Some(b) = bytes {
            return Ok(self.expr_at(ExprKind::Const(Const::Bytes(Rc::from(b.as_slice()))), line));
        }
        if !any_f {
            return Ok(self.expr_at(ExprKind::Const(Const::Str(Rc::from(text.as_str()))), line));
        }
        if !text.is_empty() {
            parts.push(FPart::Text(Rc::from(text.as_str())));
        }
        Ok(self.expr_at(ExprKind::FString(parts), line))
    }

    fn fstring_field(&mut self, field: lex::Field) -> PResult<FPart> {
        let value = parse_expression_source(&field.source, field.line, self.functions > 0)?;
        let spec = match field.spec {
            None => None,
            Some(spec_parts) => {
                let mut out = Vec::new();
                for part in spec_parts {
                    match part {
                        lex::FPart::Text(t) => out.push(FPart::Text(Rc::from(t.as_str()))),
                        lex::FPart::Field(inner) => out.push(self.fstring_field(inner)?),
                    }
                }
                Some(out)
            }
        };
        Ok(FPart::Field {
            value: Box::new(value),
            conversion: field.conversion,
            spec,
            debug: field.debug.map(|d| Rc::from(d.as_str())),
        })
    }
}

/// Parses the source of an f-string field as one expression.
fn parse_expression_source(source: &str, line: u32, in_function: bool) -> PResult<Expr> {
    let wrapped = alloc::format!("({})", source.trim());
    let tokens = lex::tokenize(&wrapped).map_err(|e| SyntaxError { message: e.message, line })?;
    let tokens: Vec<Token> = tokens.into_iter().map(|t| Token { tok: t.tok, line }).collect();
    let mut parser = Parser {
        tokens,
        pos: 0,
        depth: 0,
        functions: usize::from(in_function),
        asyncs: if in_function { alloc::vec![true] } else { Vec::new() },
        hidden: 0,
    };
    let expr = parser.star_expressions()?;
    while matches!(parser.peek(), Tok::Newline) {
        parser.advance();
    }
    if !matches!(parser.peek(), Tok::Eof) {
        return Err(SyntaxError { message: String::from("f-string : expression invalide"), line });
    }
    Ok(expr)
}

/// Parses a module.
/// Does this pattern match anything (a capture or `_`, maybe in an
/// or-pattern)?
fn irrefutable(pattern: &Pattern) -> bool {
    match pattern {
        Pattern::Wildcard | Pattern::Capture(_) => true,
        Pattern::As(inner, _) => irrefutable(inner),
        Pattern::Or(alternatives) => alternatives.iter().any(irrefutable),
        _ => false,
    }
}

pub fn parse(source: &str) -> Result<Vec<Stmt>, SyntaxError> {
    let tokens = lex::tokenize(source).map_err(|e| SyntaxError { message: e.message, line: e.line })?;
    let mut parser = Parser { tokens, pos: 0, depth: 0, functions: 0, asyncs: Vec::new(), hidden: 0 };
    parser.file()
}

/// Parses one expression (`eval`).
pub fn parse_eval(source: &str) -> Result<Expr, SyntaxError> {
    parse_expression_source(source, 1, false)
}
