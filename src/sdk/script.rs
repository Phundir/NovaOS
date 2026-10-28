//! 9.1.4 -- `dev interpret`: the built-in interpreter.
//!
//! ## Why one more language
//!
//! 15.2 shows `dev interpret main.py`, so the SDK must be able to run a
//! source without compiling it. All eight languages of 13.2 assume an
//! interpreter of several megabytes -- CPython, V8, a JVM. None of them
//! can live here.
//!
//! Nova Script is the honest answer: a complete but tiny language,
//! written entirely in this file, whose interpreter does what an
//! interpreter does -- parse, evaluate, return a result or a located
//! error. It is declared outside 13.2 in `sdk::runtime`, precisely so
//! that nobody takes it for a line of the specification.
//!
//! ## Two bounds that are not negotiable
//!
//! The interpreter runs on a **32 KiB kernel stack**
//! (`process::STACK_SIZE`) and inside a scheduled task. Two consequences:
//!
//! * **A step budget.** A `while true` loop in a script would make its
//!   core unavailable until reboot. `Interpreter::steps` counts, and
//!   returns `Budget` exceeded rather than never returning at all.
//! * **A bounded depth.** At parse time -- an expression parenthesised a
//!   thousand times would overflow the stack during parsing -- as much as
//!   at evaluation time, where a recursive function would do the same.
//!   Both limits are checked, not hoped for.
//!
//! These are the two mistakes an interpreter written in user space can
//! afford to ignore and a kernel interpreter cannot.
//!
//! ## Grammar
//!
//! ```text
//!   program    := statement*
//!   statement  := "let" name "=" expr
//!               | name "=" expr
//!               | "if" expr block ("else" block)?
//!               | "while" expr block
//!               | "fn" name "(" params ")" block
//!               | "return" expr?
//!               | expr
//!   block      := "{" statement* "}"
//! ```
//!
//! The keywords are in English, like the identifiers of the rest of the
//! project. Values are integers, text or booleans: there is **no float**,
//! and that is not an oversight -- the kernel runs soft-float, an `f64` in
//! a script would become an `f64` in the kernel.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

/// Maximum nesting depth, at parse time as at evaluation time.
const MAX_DEPTH: usize = 24;

/// Kernel stack margin below which the interpreter refuses to descend.
///
/// `MAX_DEPTH` alone protects nothing: it counts levels, whereas what
/// runs out is bytes, and it is the compiler that decides how many a
/// level consumes. In a debug build, one level of script call costs the
/// sum of the frames of `call` (1784 B), `statement` (2664 B), `binary`
/// (2520 B), `eval` (1352 B) and `block` (360 B) -- close to 9 KiB.
/// Twenty-four levels demanded more than 200 KiB on a stack of 32: the
/// kernel crushed the stacks of the neighbouring tasks without a single
/// fault, and died much later on a corrupted return address.
///
/// 32 KiB of margin cover three complete levels -- enough to walk the
/// whole error chain back, which needs stack of its own.
const STACK_MARGIN: u64 = 32 * 1024;

/// Default number of evaluation steps.
pub const DEFAULT_BUDGET: u64 = 200_000;

/// Maximum length of a string produced by a script.
const MAX_TEXT: usize = 4096;

// ============================================================
//  Values
// ============================================================

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Nil,
    Int(i64),
    Text(String),
    Bool(bool),
}

impl Value {
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Nil => "nil",
            Value::Int(_) => "entier",
            Value::Text(_) => "texte",
            Value::Bool(_) => "booleen",
        }
    }

    /// Returns the value as a `print` would display it.
    pub fn display(&self) -> String {
        match self {
            Value::Nil => String::from("nil"),
            Value::Int(value) => alloc::format!("{}", value),
            Value::Text(text) => text.clone(),
            Value::Bool(true) => String::from("true"),
            Value::Bool(false) => String::from("false"),
        }
    }

    /// Truth of a value in an `if` or a `while`.
    ///
    /// Only a boolean is accepted. Treating zero as false would let
    /// `if text` through with nothing to flag it, and that is the class
    /// of bug a four-hundred-line language has no reason to import.
    fn truth(&self) -> Option<bool> {
        match self {
            Value::Bool(value) => Some(*value),
            _ => None,
        }
    }
}

// ============================================================
//  Errors
// ============================================================

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScriptError {
    /// Character the lexer does not recognise.
    BadCharacter(usize, char),
    /// Unterminated string, or byte outside the printable range.
    BadString(usize),
    /// Unexpected token at parse time.
    Unexpected(usize, String),
    /// Nesting too deep for the kernel stack.
    TooDeep(usize),
    /// Variable read before being defined.
    Undefined(String),
    /// Function called but never declared.
    UnknownFunction(String),
    /// The argument count does not match.
    BadArity(String, usize, usize),
    /// Operation between incompatible types.
    TypeMismatch(&'static str, &'static str),
    /// Division or modulo by zero.
    DivisionByZero,
    /// Signed integer overflow.
    Overflow,
    /// Step budget exhausted: the script does not terminate.
    Budget,
    /// String built beyond the allowed size.
    TextTooLong,
}

impl ScriptError {
    pub fn message(&self) -> String {
        match self {
            ScriptError::BadCharacter(line, character) => {
                alloc::format!("ligne {} : caractere invalide '{}'", line, character)
            }
            ScriptError::BadString(line) => alloc::format!("ligne {} : chaine invalide", line),
            ScriptError::Unexpected(line, found) => {
                alloc::format!("ligne {} : jeton inattendu \"{}\"", line, found)
            }
            ScriptError::TooDeep(line) => {
                alloc::format!("ligne {} : imbrication trop profonde", line)
            }
            ScriptError::Undefined(name) => alloc::format!("variable inconnue : {}", name),
            ScriptError::UnknownFunction(name) => alloc::format!("fonction inconnue : {}", name),
            ScriptError::BadArity(name, expected, found) => alloc::format!(
                "{} attend {} argument(s), {} fourni(s)",
                name,
                expected,
                found
            ),
            ScriptError::TypeMismatch(left, right) => {
                alloc::format!("operation impossible entre {} et {}", left, right)
            }
            ScriptError::DivisionByZero => String::from("division par zero"),
            ScriptError::Overflow => String::from("debordement entier"),
            ScriptError::Budget => String::from("budget de pas epuise - le script ne termine pas"),
            ScriptError::TextTooLong => String::from("chaine trop longue"),
        }
    }
}

// ============================================================
//  Lexer
// ============================================================

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Int(i64),
    Text(String),
    Ident(String),
    Keyword(&'static str),
    Symbol(&'static str),
    End,
}

impl Token {
    fn describe(&self) -> String {
        match self {
            Token::Int(value) => alloc::format!("{}", value),
            Token::Text(_) => String::from("chaine"),
            Token::Ident(name) => name.clone(),
            Token::Keyword(word) => String::from(*word),
            Token::Symbol(symbol) => String::from(*symbol),
            Token::End => String::from("fin de fichier"),
        }
    }
}

const KEYWORDS: &[&str] = &[
    "let", "if", "else", "while", "fn", "return", "true", "false", "nil", "and", "or", "not",
];

/// Symbols recognised, longest first: without that order, `==` would be
/// read as two `=` and the parser would see an assignment.
const SYMBOLS: &[&str] = &[
    "==", "!=", "<=", ">=", "+", "-", "*", "/", "%", "<", ">", "=", "(", ")", "{", "}", ",",
];

struct Lexed {
    token: Token,
    line: usize,
}

fn lex(source: &str) -> Result<Vec<Lexed>, ScriptError> {
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
        if byte == b'#' {
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
            continue;
        }

        if byte == b'"' {
            let start = index + 1;
            let mut end = start;
            while end < bytes.len() && bytes[end] != b'"' {
                if bytes[end] == b'\n' {
                    return Err(ScriptError::BadString(line));
                }
                end += 1;
            }
            if end >= bytes.len() {
                return Err(ScriptError::BadString(line));
            }
            let text = &source[start..end];
            // The console and the VGA writer admit printable ASCII only:
            // refusing here avoids a solid square on screen.
            if !text.bytes().all(|byte| (0x20..=0x7E).contains(&byte)) {
                return Err(ScriptError::BadString(line));
            }
            tokens.push(Lexed {
                token: Token::Text(String::from(text)),
                line,
            });
            index = end + 1;
            continue;
        }

        if byte.is_ascii_digit() {
            let start = index;
            while index < bytes.len() && bytes[index].is_ascii_alphanumeric() {
                index += 1;
            }
            let text = &source[start..index];
            let value = if let Some(rest) = text.strip_prefix("0x") {
                i64::from_str_radix(rest, 16).ok()
            } else {
                text.parse::<i64>().ok()
            };
            let Some(value) = value else {
                return Err(ScriptError::BadCharacter(line, byte as char));
            };
            tokens.push(Lexed {
                token: Token::Int(value),
                line,
            });
            continue;
        }

        if byte.is_ascii_alphabetic() || byte == b'_' {
            let start = index;
            while index < bytes.len()
                && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_')
            {
                index += 1;
            }
            let text = &source[start..index];
            match KEYWORDS.iter().find(|word| **word == text) {
                Some(word) => tokens.push(Lexed {
                    token: Token::Keyword(word),
                    line,
                }),
                None => tokens.push(Lexed {
                    token: Token::Ident(String::from(text)),
                    line,
                }),
            }
            continue;
        }

        let rest = &source[index..];
        match SYMBOLS.iter().find(|symbol| rest.starts_with(**symbol)) {
            Some(symbol) => {
                tokens.push(Lexed {
                    token: Token::Symbol(symbol),
                    line,
                });
                index += symbol.len();
            }
            None => return Err(ScriptError::BadCharacter(line, byte as char)),
        }
    }

    tokens.push(Lexed {
        token: Token::End,
        line,
    });
    Ok(tokens)
}

// ============================================================
//  Tree
// ============================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Equal,
    NotEqual,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    And,
    Or,
}

#[derive(Debug, Clone)]
pub enum Expr {
    Literal(Value),
    Var(String),
    Negate(Box<Expr>),
    Not(Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    Call(String, Vec<Expr>),
}

#[derive(Debug, Clone)]
pub enum Stmt {
    Let(String, Expr),
    Assign(String, Expr),
    If(Expr, Vec<Stmt>, Vec<Stmt>),
    While(Expr, Vec<Stmt>),
    Function(String, Vec<String>, Vec<Stmt>),
    Return(Option<Expr>),
    Discard(Expr),
}

// ============================================================
//  Parser
// ============================================================

struct Parser {
    tokens: Vec<Lexed>,
    position: usize,
    depth: usize,
}

impl Parser {
    fn peek(&self) -> &Token {
        &self.tokens[self.position].token
    }

    fn line(&self) -> usize {
        self.tokens[self.position].line
    }

    fn advance(&mut self) -> Token {
        let token = self.tokens[self.position].token.clone();
        if self.position + 1 < self.tokens.len() {
            self.position += 1;
        }
        token
    }

    fn accept_symbol(&mut self, symbol: &str) -> bool {
        if matches!(self.peek(), Token::Symbol(found) if *found == symbol) {
            self.advance();
            return true;
        }
        false
    }

    fn accept_keyword(&mut self, keyword: &str) -> bool {
        if matches!(self.peek(), Token::Keyword(found) if *found == keyword) {
            self.advance();
            return true;
        }
        false
    }

    fn expect_symbol(&mut self, symbol: &str) -> Result<(), ScriptError> {
        if self.accept_symbol(symbol) {
            return Ok(());
        }
        Err(ScriptError::Unexpected(self.line(), self.peek().describe()))
    }

    fn expect_ident(&mut self) -> Result<String, ScriptError> {
        let line = self.line();
        match self.advance() {
            Token::Ident(name) => Ok(name),
            other => Err(ScriptError::Unexpected(line, other.describe())),
        }
    }

    fn enter(&mut self) -> Result<(), ScriptError> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(ScriptError::TooDeep(self.line()));
        }
        Ok(())
    }

    fn leave(&mut self) {
        self.depth -= 1;
    }

    // --- statements ---

    fn block(&mut self) -> Result<Vec<Stmt>, ScriptError> {
        self.enter()?;
        self.expect_symbol("{")?;
        let mut body = Vec::new();
        while !self.accept_symbol("}") {
            if matches!(self.peek(), Token::End) {
                self.leave();
                return Err(ScriptError::Unexpected(self.line(), String::from("}")));
            }
            body.push(self.statement()?);
        }
        self.leave();
        Ok(body)
    }

    fn statement(&mut self) -> Result<Stmt, ScriptError> {
        if self.accept_keyword("let") {
            let name = self.expect_ident()?;
            self.expect_symbol("=")?;
            let value = self.expression(0)?;
            return Ok(Stmt::Let(name, value));
        }
        if self.accept_keyword("if") {
            let condition = self.expression(0)?;
            let then = self.block()?;
            let otherwise = if self.accept_keyword("else") {
                self.block()?
            } else {
                Vec::new()
            };
            return Ok(Stmt::If(condition, then, otherwise));
        }
        if self.accept_keyword("while") {
            let condition = self.expression(0)?;
            let body = self.block()?;
            return Ok(Stmt::While(condition, body));
        }
        if self.accept_keyword("fn") {
            let name = self.expect_ident()?;
            self.expect_symbol("(")?;
            let mut parameters = Vec::new();
            if !self.accept_symbol(")") {
                loop {
                    parameters.push(self.expect_ident()?);
                    if self.accept_symbol(")") {
                        break;
                    }
                    self.expect_symbol(",")?;
                }
            }
            let body = self.block()?;
            return Ok(Stmt::Function(name, parameters, body));
        }
        if self.accept_keyword("return") {
            // A bare `return` is legal: the function returns `nil`. It is
            // recognised by what follows, without needing a semicolon the
            // language does not have.
            if matches!(self.peek(), Token::Symbol("}")) || matches!(self.peek(), Token::End) {
                return Ok(Stmt::Return(None));
            }
            return Ok(Stmt::Return(Some(self.expression(0)?)));
        }

        // Assignment or expression: the distinction is made on the token
        // after the identifier, which takes one token of lookahead.
        if let Token::Ident(name) = self.peek().clone()
            && matches!(
                self.tokens.get(self.position + 1).map(|item| &item.token),
                Some(Token::Symbol("="))
            )
        {
            self.advance();
            self.advance();
            let value = self.expression(0)?;
            return Ok(Stmt::Assign(name, value));
        }

        Ok(Stmt::Discard(self.expression(0)?))
    }

    // --- expressions, by increasing precedence ---

    fn binding_power(token: &Token) -> Option<(BinOp, u8)> {
        Some(match token {
            Token::Keyword("or") => (BinOp::Or, 1),
            Token::Keyword("and") => (BinOp::And, 2),
            Token::Symbol("==") => (BinOp::Equal, 3),
            Token::Symbol("!=") => (BinOp::NotEqual, 3),
            Token::Symbol("<") => (BinOp::Less, 3),
            Token::Symbol("<=") => (BinOp::LessEqual, 3),
            Token::Symbol(">") => (BinOp::Greater, 3),
            Token::Symbol(">=") => (BinOp::GreaterEqual, 3),
            Token::Symbol("+") => (BinOp::Add, 4),
            Token::Symbol("-") => (BinOp::Sub, 4),
            Token::Symbol("*") => (BinOp::Mul, 5),
            Token::Symbol("/") => (BinOp::Div, 5),
            Token::Symbol("%") => (BinOp::Rem, 5),
            _ => return None,
        })
    }

    fn expression(&mut self, minimum: u8) -> Result<Expr, ScriptError> {
        self.enter()?;
        let mut left = self.unary()?;

        while let Some((operator, power)) = Self::binding_power(self.peek()) {
            if power < minimum {
                break;
            }
            self.advance();
            // Every operator is left-associative: the right-hand side is
            // read with a strictly higher binding power.
            let right = self.expression(power + 1)?;
            left = Expr::Binary(operator, Box::new(left), Box::new(right));
        }

        self.leave();
        Ok(left)
    }

    fn unary(&mut self) -> Result<Expr, ScriptError> {
        if self.accept_symbol("-") {
            self.enter()?;
            let inner = self.unary()?;
            self.leave();
            return Ok(Expr::Negate(Box::new(inner)));
        }
        if self.accept_keyword("not") {
            self.enter()?;
            let inner = self.unary()?;
            self.leave();
            return Ok(Expr::Not(Box::new(inner)));
        }
        self.primary()
    }

    fn primary(&mut self) -> Result<Expr, ScriptError> {
        let line = self.line();
        match self.advance() {
            Token::Int(value) => Ok(Expr::Literal(Value::Int(value))),
            Token::Text(text) => Ok(Expr::Literal(Value::Text(text))),
            Token::Keyword("true") => Ok(Expr::Literal(Value::Bool(true))),
            Token::Keyword("false") => Ok(Expr::Literal(Value::Bool(false))),
            Token::Keyword("nil") => Ok(Expr::Literal(Value::Nil)),
            Token::Symbol("(") => {
                self.enter()?;
                let inner = self.expression(0)?;
                self.leave();
                self.expect_symbol(")")?;
                Ok(inner)
            }
            Token::Ident(name) => {
                if self.accept_symbol("(") {
                    let mut arguments = Vec::new();
                    if !self.accept_symbol(")") {
                        loop {
                            arguments.push(self.expression(0)?);
                            if self.accept_symbol(")") {
                                break;
                            }
                            self.expect_symbol(",")?;
                        }
                    }
                    return Ok(Expr::Call(name, arguments));
                }
                Ok(Expr::Var(name))
            }
            other => Err(ScriptError::Unexpected(line, other.describe())),
        }
    }
}

/// Parses a complete source.
pub fn parse(source: &str) -> Result<Vec<Stmt>, ScriptError> {
    let tokens = lex(source)?;
    let mut parser = Parser {
        tokens,
        position: 0,
        depth: 0,
    };
    let mut program = Vec::new();
    while !matches!(parser.peek(), Token::End) {
        program.push(parser.statement()?);
    }
    Ok(program)
}

// ============================================================
//  Evaluation
// ============================================================

struct Function {
    parameters: Vec<String>,
    body: Vec<Stmt>,
}

/// What a statement does to the flow.
enum Flow {
    Next,
    Return(Value),
}

/// The state of one execution.
pub struct Interpreter {
    scopes: Vec<Vec<(String, Value)>>,
    functions: Vec<(String, Function)>,
    /// What the script printed. Captured rather than written directly:
    /// the sandbox must be able to show the output without the script
    /// having touched the console.
    pub output: Vec<String>,
    steps: u64,
    budget: u64,
    depth: usize,
    /// Address below which descending one more level becomes forbidden.
    /// Zero when the bound is unknown -- outside a task.
    stack_floor: u64,
}

impl Interpreter {
    pub fn new(budget: u64) -> Self {
        // The bound is read once, on entry: the stack of a task does not
        // change during its life, and asking the scheduler at every step
        // would cost one MMIO read of the APIC per step.
        let bottom = crate::scheduler::current_stack_bottom();
        let stack_floor = if bottom == 0 { 0 } else { bottom + STACK_MARGIN };

        Interpreter {
            scopes: alloc::vec![Vec::new()],
            functions: Vec::new(),
            output: Vec::new(),
            steps: 0,
            budget,
            depth: 0,
            stack_floor,
        }
    }

    pub fn steps_used(&self) -> u64 {
        self.steps
    }

    fn tick(&mut self) -> Result<(), ScriptError> {
        self.steps += 1;
        if self.steps > self.budget {
            return Err(ScriptError::Budget);
        }
        // The physical control on recursion. `MAX_DEPTH` counts levels;
        // this one counts the bytes that remain, and it is the only one
        // of the two the compiler cannot contradict.
        if self.stack_floor != 0 && crate::scheduler::stack_pointer() < self.stack_floor {
            return Err(ScriptError::TooDeep(0));
        }
        Ok(())
    }

    fn lookup(&self, name: &str) -> Option<&Value> {
        // Scopes searched from the nearest to the furthest: a local
        // variable masks a global of the same name.
        for scope in self.scopes.iter().rev() {
            if let Some((_, value)) = scope.iter().find(|(key, _)| key == name) {
                return Some(value);
            }
        }
        None
    }

    fn assign(&mut self, name: &str, value: Value) -> Result<(), ScriptError> {
        for scope in self.scopes.iter_mut().rev() {
            if let Some(slot) = scope.iter_mut().find(|(key, _)| key == name) {
                slot.1 = value;
                return Ok(());
            }
        }
        Err(ScriptError::Undefined(String::from(name)))
    }

    fn declare(&mut self, name: &str, value: Value) {
        // `let` in the current scope: a redeclaration replaces, it does
        // not push. Pushing would grow the scope on every turn of a
        // loop.
        let scope = self
            .scopes
            .last_mut()
            .expect("une portee existe toujours - la globale");
        match scope.iter_mut().find(|(key, _)| key == name) {
            Some(slot) => slot.1 = value,
            None => scope.push((String::from(name), value)),
        }
    }

    /// Runs a complete program.
    pub fn run(&mut self, program: &[Stmt]) -> Result<Value, ScriptError> {
        // Function declarations are collected first: a script can thus
        // call a function defined further down, which a strictly
        // sequential walk would forbid.
        for statement in program {
            if let Stmt::Function(name, parameters, body) = statement {
                self.functions.retain(|(key, _)| key != name);
                self.functions.push((
                    name.clone(),
                    Function {
                        parameters: parameters.clone(),
                        body: body.clone(),
                    },
                ));
            }
        }
        match self.block(program)? {
            Flow::Return(value) => Ok(value),
            Flow::Next => Ok(Value::Nil),
        }
    }

    fn block(&mut self, body: &[Stmt]) -> Result<Flow, ScriptError> {
        for statement in body {
            match self.statement(statement)? {
                Flow::Next => {}
                Flow::Return(value) => return Ok(Flow::Return(value)),
            }
        }
        Ok(Flow::Next)
    }

    fn statement(&mut self, statement: &Stmt) -> Result<Flow, ScriptError> {
        self.tick()?;
        match statement {
            Stmt::Let(name, expression) => {
                let value = self.eval(expression)?;
                self.declare(name, value);
                Ok(Flow::Next)
            }
            Stmt::Assign(name, expression) => {
                let value = self.eval(expression)?;
                self.assign(name, value)?;
                Ok(Flow::Next)
            }
            Stmt::If(condition, then, otherwise) => {
                let value = self.eval(condition)?;
                let Some(truth) = value.truth() else {
                    return Err(ScriptError::TypeMismatch(value.type_name(), "booleen"));
                };
                self.scopes.push(Vec::new());
                let flow = if truth {
                    self.block(then)
                } else {
                    self.block(otherwise)
                };
                self.scopes.pop();
                flow
            }
            Stmt::While(condition, body) => {
                loop {
                    self.tick()?;
                    let value = self.eval(condition)?;
                    let Some(truth) = value.truth() else {
                        return Err(ScriptError::TypeMismatch(value.type_name(), "booleen"));
                    };
                    if !truth {
                        break;
                    }
                    self.scopes.push(Vec::new());
                    let flow = self.block(body);
                    self.scopes.pop();
                    if let Flow::Return(value) = flow? {
                        return Ok(Flow::Return(value));
                    }
                }
                Ok(Flow::Next)
            }
            // Already collected before execution: seeing it again is a no-op.
            Stmt::Function(_, _, _) => Ok(Flow::Next),
            Stmt::Return(expression) => {
                let value = match expression {
                    Some(expression) => self.eval(expression)?,
                    None => Value::Nil,
                };
                Ok(Flow::Return(value))
            }
            Stmt::Discard(expression) => {
                self.eval(expression)?;
                Ok(Flow::Next)
            }
        }
    }

    fn eval(&mut self, expression: &Expr) -> Result<Value, ScriptError> {
        self.tick()?;
        match expression {
            Expr::Literal(value) => Ok(value.clone()),
            Expr::Var(name) => self
                .lookup(name)
                .cloned()
                .ok_or_else(|| ScriptError::Undefined(name.clone())),
            Expr::Negate(inner) => match self.eval(inner)? {
                Value::Int(value) => value
                    .checked_neg()
                    .map(Value::Int)
                    .ok_or(ScriptError::Overflow),
                other => Err(ScriptError::TypeMismatch(other.type_name(), "entier")),
            },
            Expr::Not(inner) => {
                let value = self.eval(inner)?;
                match value.truth() {
                    Some(truth) => Ok(Value::Bool(!truth)),
                    None => Err(ScriptError::TypeMismatch(value.type_name(), "booleen")),
                }
            }
            Expr::Binary(operator, left, right) => self.binary(*operator, left, right),
            Expr::Call(name, arguments) => {
                let mut values = Vec::new();
                for argument in arguments {
                    values.push(self.eval(argument)?);
                }
                self.call(name, values)
            }
        }
    }

    fn binary(&mut self, operator: BinOp, left: &Expr, right: &Expr) -> Result<Value, ScriptError> {
        // `and` and `or` short-circuit: evaluating the right-hand side of
        // an `and` whose left side is false would change the semantics,
        // and would burn budget for nothing.
        if matches!(operator, BinOp::And | BinOp::Or) {
            let left_value = self.eval(left)?;
            let Some(truth) = left_value.truth() else {
                return Err(ScriptError::TypeMismatch(left_value.type_name(), "booleen"));
            };
            if (operator == BinOp::And && !truth) || (operator == BinOp::Or && truth) {
                return Ok(Value::Bool(truth));
            }
            let right_value = self.eval(right)?;
            let Some(truth) = right_value.truth() else {
                return Err(ScriptError::TypeMismatch(
                    right_value.type_name(),
                    "booleen",
                ));
            };
            return Ok(Value::Bool(truth));
        }

        let left = self.eval(left)?;
        let right = self.eval(right)?;

        if matches!(operator, BinOp::Equal | BinOp::NotEqual) {
            let equal = left == right;
            return Ok(Value::Bool(if operator == BinOp::Equal {
                equal
            } else {
                !equal
            }));
        }

        // Concatenation: `+` accepts two texts. Mixing text and integer
        // is refused -- implicit conversion is the commonest source of
        // surprising results, and `text(n)` exists.
        if let (Value::Text(left), Value::Text(right)) = (&left, &right)
            && operator == BinOp::Add
        {
            if left.len() + right.len() > MAX_TEXT {
                return Err(ScriptError::TextTooLong);
            }
            let mut joined = left.clone();
            joined.push_str(right);
            return Ok(Value::Text(joined));
        }

        let (Value::Int(a), Value::Int(b)) = (&left, &right) else {
            return Err(ScriptError::TypeMismatch(
                left.type_name(),
                right.type_name(),
            ));
        };
        let (a, b) = (*a, *b);

        Ok(match operator {
            BinOp::Add => Value::Int(a.checked_add(b).ok_or(ScriptError::Overflow)?),
            BinOp::Sub => Value::Int(a.checked_sub(b).ok_or(ScriptError::Overflow)?),
            BinOp::Mul => Value::Int(a.checked_mul(b).ok_or(ScriptError::Overflow)?),
            BinOp::Div => {
                if b == 0 {
                    return Err(ScriptError::DivisionByZero);
                }
                Value::Int(a.checked_div(b).ok_or(ScriptError::Overflow)?)
            }
            BinOp::Rem => {
                if b == 0 {
                    return Err(ScriptError::DivisionByZero);
                }
                Value::Int(a.checked_rem(b).ok_or(ScriptError::Overflow)?)
            }
            BinOp::Less => Value::Bool(a < b),
            BinOp::LessEqual => Value::Bool(a <= b),
            BinOp::Greater => Value::Bool(a > b),
            BinOp::GreaterEqual => Value::Bool(a >= b),
            // Handled above.
            BinOp::Equal | BinOp::NotEqual | BinOp::And | BinOp::Or => Value::Nil,
        })
    }

    fn call(&mut self, name: &str, arguments: Vec<Value>) -> Result<Value, ScriptError> {
        // --- built-in functions ---
        match name {
            "print" => {
                let mut line = String::new();
                for (index, value) in arguments.iter().enumerate() {
                    if index > 0 {
                        line.push(' ');
                    }
                    line.push_str(&value.display());
                }
                if line.len() > MAX_TEXT {
                    return Err(ScriptError::TextTooLong);
                }
                self.output.push(line);
                return Ok(Value::Nil);
            }
            "len" => {
                let [value] = arguments.as_slice() else {
                    return Err(ScriptError::BadArity(
                        String::from("len"),
                        1,
                        arguments.len(),
                    ));
                };
                return match value {
                    Value::Text(text) => Ok(Value::Int(text.len() as i64)),
                    other => Err(ScriptError::TypeMismatch(other.type_name(), "texte")),
                };
            }
            "text" => {
                let [value] = arguments.as_slice() else {
                    return Err(ScriptError::BadArity(
                        String::from("text"),
                        1,
                        arguments.len(),
                    ));
                };
                return Ok(Value::Text(value.display()));
            }
            "abs" => {
                let [value] = arguments.as_slice() else {
                    return Err(ScriptError::BadArity(
                        String::from("abs"),
                        1,
                        arguments.len(),
                    ));
                };
                return match value {
                    Value::Int(number) => number
                        .checked_abs()
                        .map(Value::Int)
                        .ok_or(ScriptError::Overflow),
                    other => Err(ScriptError::TypeMismatch(other.type_name(), "entier")),
                };
            }
            _ => {}
        }

        // --- script functions ---
        let Some((_, function)) = self.functions.iter().find(|(key, _)| key == name) else {
            return Err(ScriptError::UnknownFunction(String::from(name)));
        };
        if function.parameters.len() != arguments.len() {
            return Err(ScriptError::BadArity(
                String::from(name),
                function.parameters.len(),
                arguments.len(),
            ));
        }

        // The kernel stack is 32 KiB: unbounded recursion would overflow
        // it, and a kernel stack overflow does not produce an error but a
        // triple fault.
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            self.depth -= 1;
            return Err(ScriptError::TooDeep(0));
        }

        let parameters = function.parameters.clone();
        let body = function.body.clone();

        let mut frame = Vec::new();
        for (parameter, value) in parameters.iter().zip(arguments) {
            frame.push((parameter.clone(), value));
        }
        self.scopes.push(frame);

        let flow = self.block(&body);

        self.scopes.pop();
        self.depth -= 1;

        match flow? {
            Flow::Return(value) => Ok(value),
            Flow::Next => Ok(Value::Nil),
        }
    }
}

// ============================================================
//  Entry point
// ============================================================

/// Result of an execution.
pub struct Run {
    pub value: Value,
    pub output: Vec<String>,
    pub steps: u64,
}

/// Parses and runs a source, default budget.
pub fn execute(source: &str) -> Result<Run, ScriptError> {
    execute_with_budget(source, DEFAULT_BUDGET)
}

/// Parses and runs a source with an explicit step budget.
pub fn execute_with_budget(source: &str, budget: u64) -> Result<Run, ScriptError> {
    let program = parse(source)?;
    let mut interpreter = Interpreter::new(budget);
    let value = interpreter.run(&program)?;
    Ok(Run {
        value,
        output: interpreter.output,
        steps: interpreter.steps,
    })
}
