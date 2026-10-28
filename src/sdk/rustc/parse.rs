//! 9.5.1 -- Recursive-descent parser for the Rust subset.
//!
//! ## Refusing by name is the whole design
//!
//! A compiler for a subset has one duty beyond compiling: saying, of a
//! program it will not take, **which construct** it will not take. So
//! `struct`, `trait`, `match`, `for`, closures, generics and method calls
//! each have their own refusal, produced at the token that starts them.
//! The alternative -- a generic "unexpected token" -- makes the reader
//! hunt for what a Rust compiler would have accepted, which is exactly
//! the work the message exists to save.
//!
//! ## Two bounds, as in `sdk::script`
//!
//! The parser runs on a 32 KiB kernel stack. Nesting is bounded twice:
//! by a depth counter, which is cheap and approximate, and by the actual
//! stack pointer, which is exact and is the only one the compiler's frame
//! sizes cannot contradict. `sdk::script` learned that the hard way --
//! twenty-four levels of its interpreter wanted 200 KiB.
//!
//! ## Precedence climbing, not a ladder of functions
//!
//! Eighteen binary operators on nine levels would be nine mutually
//! recursive functions, hence nine stack frames for `1 + 1`. Precedence
//! climbing does it in one frame per level actually used, which on a
//! kernel stack is not a micro-optimisation.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

use super::ast::*;
use super::lex::{Lexed, Token};

/// Maximum nesting depth accepted, expressions and blocks alike.
const MAX_DEPTH: usize = 24;

/// Stack margin below which the parser refuses to descend, in bytes.
///
/// Same reasoning as `script::STACK_MARGIN`: `MAX_DEPTH` counts levels,
/// this counts the bytes that are actually left.
const STACK_MARGIN: u64 = 24 * 1024;

/// Maximum number of parameters a function may take.
///
/// Six, because six is what the argument registers hold. Passing the
/// seventh on the stack is a real calling convention and a real amount of
/// code; refusing it is honest and costs one message.
pub const MAX_PARAMS: usize = 6;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    Unexpected(usize, String, &'static str),
    /// A construct of the language that this subset does not implement.
    Unsupported(usize, &'static str, &'static str),
    /// A type name the subset does not have.
    UnknownType(usize, String),
    /// Nesting too deep for the kernel stack.
    TooDeep(usize),
    /// More than `MAX_PARAMS` parameters.
    TooManyParams(usize, String),
    /// A macro other than `print!` / `println!`.
    UnknownMacro(usize, String),
    /// A format string whose holes and arguments do not match.
    FormatMismatch(usize, usize, usize),
    /// A `{` or `}` alone in a format string.
    BadFormat(usize),
    /// Two items with the same name.
    Duplicate(usize, String),
    /// No `fn main`.
    NoMain,
}

impl ParseError {
    pub fn message(&self) -> String {
        match self {
            ParseError::Unexpected(line, found, expected) => alloc::format!(
                "ligne {} : \"{}\" inattendu, {} attendu",
                line,
                found,
                expected
            ),
            ParseError::Unsupported(line, what, advice) => alloc::format!(
                "ligne {} : {} n'est pas dans le sous-ensemble Rust de Nova OS - {}",
                line,
                what,
                advice
            ),
            ParseError::UnknownType(line, name) => alloc::format!(
                "ligne {} : type \"{}\" inconnu - le sous-ensemble a i64, u64, bool, &str et ()",
                line,
                name
            ),
            ParseError::TooDeep(line) => {
                alloc::format!("ligne {} : imbrication trop profonde", line)
            }
            ParseError::TooManyParams(line, name) => alloc::format!(
                "ligne {} : \"{}\" depasse {} parametres",
                line,
                name,
                MAX_PARAMS
            ),
            ParseError::UnknownMacro(line, name) => alloc::format!(
                "ligne {} : macro \"{}!\" inconnue - seules print! et println! existent ici",
                line,
                name
            ),
            ParseError::FormatMismatch(line, holes, args) => alloc::format!(
                "ligne {} : {} trou(s) dans le format, {} argument(s) fourni(s)",
                line,
                holes,
                args
            ),
            ParseError::BadFormat(line) => alloc::format!(
                "ligne {} : accolade isolee dans un format - ecrire {{{{ ou }}}} pour une accolade litterale",
                line
            ),
            ParseError::Duplicate(line, name) => {
                alloc::format!("ligne {} : \"{}\" est declare deux fois", line, name)
            }
            ParseError::NoMain => {
                String::from("aucune fonction main - un programme doit avoir un point d'entree")
            }
        }
    }
}

struct Parser {
    tokens: Vec<Lexed>,
    position: usize,
    depth: usize,
    /// Address below which one more level is forbidden. Zero outside a
    /// scheduled task, where the bound is unknown.
    stack_floor: u64,
}

impl Parser {
    fn new(tokens: Vec<Lexed>) -> Self {
        let bottom = crate::scheduler::current_stack_bottom();
        Parser {
            tokens,
            position: 0,
            depth: 0,
            stack_floor: if bottom == 0 { 0 } else { bottom + STACK_MARGIN },
        }
    }

    fn peek(&self) -> &Token {
        &self.tokens[self.position.min(self.tokens.len() - 1)].token
    }

    fn peek_at(&self, ahead: usize) -> &Token {
        let index = (self.position + ahead).min(self.tokens.len() - 1);
        &self.tokens[index].token
    }

    fn line(&self) -> usize {
        self.tokens[self.position.min(self.tokens.len() - 1)].line
    }

    fn advance(&mut self) -> Token {
        let token = self.peek().clone();
        if self.position + 1 < self.tokens.len() {
            self.position += 1;
        }
        token
    }

    fn eat_symbol(&mut self, symbol: &str) -> bool {
        if matches!(self.peek(), Token::Symbol(found) if *found == symbol) {
            self.advance();
            return true;
        }
        false
    }

    fn eat_keyword(&mut self, keyword: &str) -> bool {
        if matches!(self.peek(), Token::Keyword(found) if *found == keyword) {
            self.advance();
            return true;
        }
        false
    }

    fn expect_symbol(&mut self, symbol: &'static str) -> Result<(), ParseError> {
        if self.eat_symbol(symbol) {
            return Ok(());
        }
        Err(ParseError::Unexpected(
            self.line(),
            self.peek().describe(),
            symbol,
        ))
    }

    fn expect_ident(&mut self) -> Result<String, ParseError> {
        match self.advance() {
            Token::Ident(name) => Ok(name),
            other => Err(ParseError::Unexpected(
                self.line(),
                other.describe(),
                "un identifiant",
            )),
        }
    }

    fn enter(&mut self) -> Result<(), ParseError> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(ParseError::TooDeep(self.line()));
        }
        if self.stack_floor != 0 && crate::scheduler::stack_pointer() < self.stack_floor {
            return Err(ParseError::TooDeep(self.line()));
        }
        Ok(())
    }

    fn leave(&mut self) {
        self.depth -= 1;
    }
}

/// Parses a complete source into a program.
pub fn parse(source: &str, tokens: Vec<Lexed>) -> Result<Program, ParseError> {
    let _ = source;
    let mut parser = Parser::new(tokens);
    let mut program = Program {
        functions: Vec::new(),
        constants: Vec::new(),
    };

    loop {
        match parser.peek().clone() {
            Token::End => break,
            Token::Keyword("fn") => {
                let function = parse_function(&mut parser)?;
                if program.function(&function.name).is_some() {
                    return Err(ParseError::Duplicate(function.line, function.name));
                }
                program.functions.push(function);
            }
            Token::Keyword("const") | Token::Keyword("static") => {
                let item = parse_const(&mut parser)?;
                if program.constants.iter().any(|other| other.name == item.name) {
                    return Err(ParseError::Duplicate(item.line, item.name));
                }
                program.constants.push(item);
            }
            // `pub` before an item: accepted and ignored. There is one
            // compilation unit, so visibility has nothing to separate.
            Token::Keyword("pub") => {
                parser.advance();
            }
            other => return Err(refuse_item(parser.line(), &other)),
        }
    }

    if program.function("main").is_none() {
        return Err(ParseError::NoMain);
    }
    Ok(program)
}

/// The refusal for a top-level construct the subset does not have.
fn refuse_item(line: usize, token: &Token) -> ParseError {
    match token {
        Token::Keyword("struct") => ParseError::Unsupported(
            line,
            "struct",
            "les donnees se passent en parametres, un scalaire a la fois",
        ),
        Token::Keyword("enum") => ParseError::Unsupported(
            line,
            "enum",
            "utiliser des constantes entieres",
        ),
        Token::Keyword("trait") | Token::Keyword("impl") | Token::Keyword("dyn") => {
            ParseError::Unsupported(
                line,
                "trait / impl",
                "il n'y a ni methode ni repartition dynamique",
            )
        }
        Token::Keyword("use") | Token::Keyword("mod") | Token::Keyword("crate") => {
            ParseError::Unsupported(
                line,
                "use / mod",
                "un projet est une unite de compilation unique",
            )
        }
        Token::Keyword("type") => {
            ParseError::Unsupported(line, "type", "les cinq types du sous-ensemble sont fixes")
        }
        Token::Keyword("async") | Token::Keyword("await") => ParseError::Unsupported(
            line,
            "async / await",
            "il n'y a pas d'executeur ; voir sleep_ms et yield_now",
        ),
        Token::Keyword("unsafe") => ParseError::Unsupported(
            line,
            "unsafe",
            "aucun pointeur brut n'est manipulable depuis ce sous-ensemble",
        ),
        other => ParseError::Unexpected(line, other.describe(), "fn ou const"),
    }
}

fn parse_type(parser: &mut Parser) -> Result<Type, ParseError> {
    let line = parser.line();

    // `&str` is the only borrow the subset knows, and it is a type, not a
    // borrow: there is nothing else to point at.
    if parser.eat_symbol("&") {
        let name = match parser.advance() {
            Token::Ident(name) => name,
            other => {
                return Err(ParseError::Unexpected(line, other.describe(), "str"));
            }
        };
        if name == "str" {
            return Ok(Type::Str);
        }
        return Err(ParseError::Unsupported(
            line,
            "l'emprunt d'une valeur",
            "seul &str existe, et il designe une chaine litterale",
        ));
    }

    // `()`
    if parser.eat_symbol("(") {
        parser.expect_symbol(")")?;
        return Ok(Type::Unit);
    }

    let name = match parser.advance() {
        Token::Ident(name) => name,
        other => {
            return Err(ParseError::Unexpected(line, other.describe(), "un type"));
        }
    };

    // A generic argument is refused where it appears, rather than
    // producing "unexpected <".
    if matches!(parser.peek(), Token::Symbol("<")) {
        return Err(ParseError::Unsupported(
            line,
            "le generique",
            "ecrire une fonction par type concret",
        ));
    }

    match name.as_str() {
        "i64" => Ok(Type::I64),
        "u64" => Ok(Type::U64),
        "bool" => Ok(Type::Bool),
        "String" => Err(ParseError::Unsupported(
            line,
            "String",
            "les chaines sont des litteraux &str ; il n'y a pas de tas",
        )),
        "Vec" => Err(ParseError::Unsupported(
            line,
            "Vec",
            "il n'y a ni tas ni type parametre",
        )),
        _ => Err(ParseError::UnknownType(line, name)),
    }
}

fn parse_function(parser: &mut Parser) -> Result<Function, ParseError> {
    let line = parser.line();
    parser.advance(); // fn
    let name = parser.expect_ident()?;

    if matches!(parser.peek(), Token::Symbol("<")) {
        return Err(ParseError::Unsupported(
            line,
            "le generique",
            "ecrire une fonction par type concret",
        ));
    }

    parser.expect_symbol("(")?;
    let mut params = Vec::new();
    if !parser.eat_symbol(")") {
        loop {
            // `&self` / `self`: the message must name the method, not the
            // ampersand.
            if matches!(parser.peek(), Token::Keyword("self"))
                || matches!(
                    (parser.peek(), parser.peek_at(1)),
                    (Token::Symbol("&"), Token::Keyword("self"))
                )
            {
                return Err(ParseError::Unsupported(
                    parser.line(),
                    "la methode",
                    "ecrire une fonction libre et passer la valeur en parametre",
                ));
            }
            let param_name = parser.expect_ident()?;
            parser.expect_symbol(":")?;
            let ty = parse_type(parser)?;
            params.push(Param {
                name: param_name,
                ty,
            });
            if !parser.eat_symbol(",") {
                break;
            }
            if matches!(parser.peek(), Token::Symbol(")")) {
                break;
            }
        }
        parser.expect_symbol(")")?;
    }

    if params.len() > MAX_PARAMS {
        return Err(ParseError::TooManyParams(line, name));
    }

    let result = if parser.eat_symbol("->") {
        parse_type(parser)?
    } else {
        Type::Unit
    };

    if matches!(parser.peek(), Token::Keyword("where")) {
        return Err(ParseError::Unsupported(
            parser.line(),
            "la clause where",
            "il n'y a ni generique ni trait a contraindre",
        ));
    }

    let body = parse_block(parser)?;
    Ok(Function {
        name,
        params,
        result,
        body,
        line,
    })
}

fn parse_const(parser: &mut Parser) -> Result<Const, ParseError> {
    let line = parser.line();
    parser.advance(); // const / static
    parser.eat_keyword("mut");
    let name = parser.expect_ident()?;
    parser.expect_symbol(":")?;
    let ty = parse_type(parser)?;
    parser.expect_symbol("=")?;
    let value = parse_expr(parser)?;
    parser.expect_symbol(";")?;
    Ok(Const {
        name,
        ty,
        value,
        line,
    })
}

fn parse_block(parser: &mut Parser) -> Result<Block, ParseError> {
    parser.enter()?;
    let line = parser.line();
    parser.expect_symbol("{")?;

    let mut statements = Vec::new();
    let mut tail = None;

    loop {
        if parser.eat_symbol("}") {
            break;
        }
        if matches!(parser.peek(), Token::End) {
            parser.leave();
            return Err(ParseError::Unexpected(
                parser.line(),
                String::from("fin de fichier"),
                "}",
            ));
        }
        // A stray `;` is an empty statement, and Rust allows it.
        if parser.eat_symbol(";") {
            continue;
        }

        let (statement, is_tail) = parse_statement(parser)?;
        match (statement, is_tail) {
            (Stmt::Expr(expr, _), true) => {
                // A tail expression must be the last thing in the block.
                if !parser.eat_symbol("}") {
                    parser.leave();
                    return Err(ParseError::Unexpected(
                        parser.line(),
                        parser.peek().describe(),
                        "; ou }",
                    ));
                }
                tail = Some(expr);
                break;
            }
            (statement, _) => statements.push(statement),
        }
    }

    parser.leave();
    Ok(Block {
        statements,
        tail,
        line,
    })
}

/// Parses one statement. The flag says whether it was an expression with
/// no trailing `;` -- that is, a candidate tail value for its block.
fn parse_statement(parser: &mut Parser) -> Result<(Stmt, bool), ParseError> {
    let line = parser.line();

    match parser.peek().clone() {
        Token::Keyword("let") => {
            parser.advance();
            let mutable = parser.eat_keyword("mut");
            let name = parser.expect_ident()?;
            let declared = if parser.eat_symbol(":") {
                Some(parse_type(parser)?)
            } else {
                None
            };
            // `let x;` then `x = ...` needs a definite-assignment
            // analysis to be safe. Demanding an initialiser is one line
            // and removes the whole question.
            if !parser.eat_symbol("=") {
                return Err(ParseError::Unsupported(
                    line,
                    "let sans initialisation",
                    "donner une valeur des la declaration",
                ));
            }
            let value = parse_expr(parser)?;
            parser.expect_symbol(";")?;
            Ok((
                Stmt::Let {
                    name,
                    mutable,
                    declared,
                    value,
                    line,
                },
                false,
            ))
        }

        Token::Keyword("if") => {
            let (condition, then_block, else_block) = parse_if(parser)?;

            // An `if` is a statement, **except** when it is the last
            // thing in its block: there it is the block's value, and
            // `fn signe(n: i64) -> i64 { if n < 0 { -1 } else { 1 } }`
            // is the shape that makes the difference visible. Reading it
            // as a statement gave that function the type `()`, which the
            // checker then reported as a missing return -- an error
            // about the wrong thing entirely.
            //
            // The test is the token that follows: a `}` means the block
            // ends here, so this `if` is its tail.
            if matches!(parser.peek(), Token::Symbol("}")) {
                return Ok((
                    Stmt::Expr(
                        Expr::If(
                            Box::new(condition),
                            Box::new(then_block),
                            else_block.map(Box::new),
                            line,
                        ),
                        line,
                    ),
                    true,
                ));
            }
            Ok((Stmt::If(condition, then_block, else_block, line), false))
        }

        Token::Keyword("while") => {
            parser.advance();
            let condition = parse_expr_no_struct(parser)?;
            let body = parse_block(parser)?;
            Ok((Stmt::While(condition, body, line), false))
        }

        Token::Keyword("loop") => {
            parser.advance();
            let body = parse_block(parser)?;
            Ok((Stmt::Loop(body, line), false))
        }

        Token::Keyword("for") => Err(ParseError::Unsupported(
            line,
            "la boucle for",
            "il n'y a pas d'iterateur ; ecrire un while avec un compteur",
        )),

        Token::Keyword("match") => Err(ParseError::Unsupported(
            line,
            "match",
            "ecrire une chaine de if / else if",
        )),

        Token::Keyword("break") => {
            parser.advance();
            parser.expect_symbol(";")?;
            Ok((Stmt::Break(line), false))
        }

        Token::Keyword("continue") => {
            parser.advance();
            parser.expect_symbol(";")?;
            Ok((Stmt::Continue(line), false))
        }

        Token::Keyword("return") => {
            parser.advance();
            let value = if matches!(parser.peek(), Token::Symbol(";")) {
                None
            } else {
                Some(parse_expr(parser)?)
            };
            parser.expect_symbol(";")?;
            Ok((Stmt::Return(value, line), false))
        }

        Token::Macro(name) => {
            let statement = parse_print(parser, &name)?;
            parser.expect_symbol(";")?;
            Ok((statement, false))
        }

        // Assignment, or an expression statement. The two are told apart
        // by looking one token past the name: doing it by backtracking
        // would mean keeping a snapshot of the parser.
        Token::Ident(name) => {
            if let Some(op) = assignment_operator(parser.peek_at(1)) {
                parser.advance();
                parser.advance();
                let value = parse_expr(parser)?;
                parser.expect_symbol(";")?;
                return Ok((
                    Stmt::Assign {
                        name,
                        op,
                        value,
                        line,
                    },
                    false,
                ));
            }
            let expr = parse_expr(parser)?;
            let terminated = parser.eat_symbol(";");
            Ok((Stmt::Expr(expr, line), !terminated))
        }

        _ => {
            let expr = parse_expr(parser)?;
            let terminated = parser.eat_symbol(";");
            Ok((Stmt::Expr(expr, line), !terminated))
        }
    }
}

/// Reads an assignment operator, plain or compound.
///
/// Returns `Some(None)` for `=`, `Some(Some(op))` for `+=` and friends,
/// and `None` when the token is not an assignment at all. The double
/// option is doing real work: "this is an assignment" and "it carries an
/// operator" are two different facts.
fn assignment_operator(token: &Token) -> Option<Option<BinOp>> {
    let Token::Symbol(symbol) = token else {
        return None;
    };
    Some(match *symbol {
        "=" => None,
        "+=" => Some(BinOp::Add),
        "-=" => Some(BinOp::Sub),
        "*=" => Some(BinOp::Mul),
        "/=" => Some(BinOp::Div),
        "%=" => Some(BinOp::Rem),
        "&=" => Some(BinOp::BitAnd),
        "|=" => Some(BinOp::BitOr),
        "^=" => Some(BinOp::BitXor),
        "<<=" => Some(BinOp::Shl),
        ">>=" => Some(BinOp::Shr),
        _ => return None,
    })
}

fn parse_if(parser: &mut Parser) -> Result<(Expr, Block, Option<Block>), ParseError> {
    parser.advance(); // if
    let condition = parse_expr_no_struct(parser)?;
    let then_block = parse_block(parser)?;

    let else_block = if parser.eat_keyword("else") {
        if matches!(parser.peek(), Token::Keyword("if")) {
            // `else if` is an else branch holding one if. Flattening it
            // into a chain would make the tree shape depend on how it was
            // written.
            let line = parser.line();
            let (nested_condition, nested_then, nested_else) = parse_if(parser)?;
            Some(Block {
                statements: Vec::new(),
                tail: Some(Expr::If(
                    Box::new(nested_condition),
                    Box::new(nested_then),
                    nested_else.map(Box::new),
                    line,
                )),
                line,
            })
        } else {
            Some(parse_block(parser)?)
        }
    } else {
        None
    };

    Ok((condition, then_block, else_block))
}

/// `print!` / `println!`.
fn parse_print(parser: &mut Parser, name: &str) -> Result<Stmt, ParseError> {
    let line = parser.line();
    let newline = match name {
        "println" => true,
        "print" => false,
        _ => return Err(ParseError::UnknownMacro(line, String::from(name))),
    };
    parser.advance(); // the macro name

    // Rust accepts all three bracket shapes for a macro call.
    let close = match parser.advance() {
        Token::Symbol("(") => ")",
        Token::Symbol("[") => "]",
        Token::Symbol("{") => "}",
        other => {
            return Err(ParseError::Unexpected(line, other.describe(), "("));
        }
    };

    // `println!()` with no argument is an empty line.
    if matches!(parser.peek(), Token::Symbol(found) if *found == close) {
        parser.advance();
        return Ok(Stmt::Print {
            parts: Vec::new(),
            args: Vec::new(),
            newline,
            line,
        });
    }

    let format = match parser.advance() {
        Token::Text(text) => text,
        other => {
            return Err(ParseError::Unexpected(
                line,
                other.describe(),
                "une chaine de format",
            ));
        }
    };

    let mut args = Vec::new();
    while parser.eat_symbol(",") {
        if matches!(parser.peek(), Token::Symbol(found) if *found == close) {
            break;
        }
        args.push(parse_expr(parser)?);
    }
    if !matches!(parser.peek(), Token::Symbol(found) if *found == close) {
        return Err(ParseError::Unexpected(
            parser.line(),
            parser.peek().describe(),
            "la fin de la macro",
        ));
    }
    parser.advance();

    let parts = parse_format(&format, line)?;
    let holes = parts
        .iter()
        .filter(|part| matches!(part, FormatPart::Hole(_)))
        .count();
    if holes != args.len() {
        return Err(ParseError::FormatMismatch(line, holes, args.len()));
    }

    Ok(Stmt::Print {
        parts,
        args,
        newline,
        line,
    })
}

/// Splits a format string into literals and holes.
///
/// Only `{}` is a hole. `{:?}`, `{name}` and the width and precision
/// forms are refused rather than approximated: printing `{:>8}` as if it
/// were `{}` would produce a program that runs and lies about its
/// alignment.
fn parse_format(format: &str, line: usize) -> Result<Vec<FormatPart>, ParseError> {
    let mut parts = Vec::new();
    let mut literal = String::new();
    let mut index = 0;
    let bytes = format.as_bytes();

    while index < bytes.len() {
        match bytes[index] {
            b'{' => {
                if index + 1 < bytes.len() && bytes[index + 1] == b'{' {
                    literal.push('{');
                    index += 2;
                    continue;
                }
                if index + 1 >= bytes.len() || bytes[index + 1] != b'}' {
                    return Err(ParseError::BadFormat(line));
                }
                if !literal.is_empty() {
                    parts.push(FormatPart::Literal(core::mem::take(&mut literal)));
                }
                let position = parts
                    .iter()
                    .filter(|part| matches!(part, FormatPart::Hole(_)))
                    .count();
                parts.push(FormatPart::Hole(position));
                index += 2;
            }
            b'}' => {
                if index + 1 < bytes.len() && bytes[index + 1] == b'}' {
                    literal.push('}');
                    index += 2;
                    continue;
                }
                return Err(ParseError::BadFormat(line));
            }
            other => {
                literal.push(other as char);
                index += 1;
            }
        }
    }
    if !literal.is_empty() {
        parts.push(FormatPart::Literal(literal));
    }
    Ok(parts)
}

// ============================================================
//  Expressions
// ============================================================

/// Parses an expression in a position where a `{` starts a block, not a
/// struct literal.
///
/// `if x { ... }` and `while n < 10 { ... }` are the reason: without this
/// distinction the `{` would be read as the start of something belonging
/// to the condition. Rust's own parser carries the same flag.
fn parse_expr_no_struct(parser: &mut Parser) -> Result<Expr, ParseError> {
    parse_binary(parser, 0)
}

fn parse_expr(parser: &mut Parser) -> Result<Expr, ParseError> {
    parse_binary(parser, 0)
}

fn binary_operator(token: &Token) -> Option<BinOp> {
    let Token::Symbol(symbol) = token else {
        return None;
    };
    Some(match *symbol {
        "+" => BinOp::Add,
        "-" => BinOp::Sub,
        "*" => BinOp::Mul,
        "/" => BinOp::Div,
        "%" => BinOp::Rem,
        "==" => BinOp::Eq,
        "!=" => BinOp::Ne,
        "<" => BinOp::Lt,
        "<=" => BinOp::Le,
        ">" => BinOp::Gt,
        ">=" => BinOp::Ge,
        "&&" => BinOp::And,
        "||" => BinOp::Or,
        "&" => BinOp::BitAnd,
        "|" => BinOp::BitOr,
        "^" => BinOp::BitXor,
        "<<" => BinOp::Shl,
        ">>" => BinOp::Shr,
        _ => return None,
    })
}

/// Precedence climbing.
///
/// Every operator of the subset is left-associative, so the recursive
/// call is made with `precedence + 1`: with `precedence`, `a - b - c`
/// would group as `a - (b - c)` and subtraction would stop meaning
/// subtraction.
fn parse_binary(parser: &mut Parser, minimum: u8) -> Result<Expr, ParseError> {
    parser.enter()?;
    let mut left = parse_unary(parser)?;

    loop {
        let Some(operator) = binary_operator(parser.peek()) else {
            break;
        };
        let precedence = operator.precedence();
        if precedence < minimum {
            break;
        }
        let line = parser.line();
        parser.advance();
        let right = parse_binary(parser, precedence + 1)?;
        left = Expr::Binary(operator, Box::new(left), Box::new(right), line);
    }

    // `as` binds tighter than any binary operator and looser than a
    // unary one, which is where Rust puts it.
    parser.leave();
    Ok(left)
}

fn parse_unary(parser: &mut Parser) -> Result<Expr, ParseError> {
    parser.enter()?;
    let line = parser.line();

    let expr = if parser.eat_symbol("-") {
        let inner = parse_unary(parser)?;
        Expr::Unary(UnOp::Neg, Box::new(inner), line)
    } else if parser.eat_symbol("!") {
        let inner = parse_unary(parser)?;
        Expr::Unary(UnOp::Not, Box::new(inner), line)
    } else if matches!(parser.peek(), Token::Symbol("&")) {
        parser.leave();
        return Err(ParseError::Unsupported(
            line,
            "l'emprunt",
            "les valeurs se passent par copie ; &str designe deja une chaine",
        ));
    } else if matches!(parser.peek(), Token::Symbol("*")) {
        parser.leave();
        return Err(ParseError::Unsupported(
            line,
            "le dereferencement",
            "il n'y a pas de pointeur manipulable",
        ));
    } else {
        parse_postfix(parser)?
    };

    parser.leave();
    Ok(expr)
}

/// Suffixes: `as T`, and the refusals for `.` and `?`.
fn parse_postfix(parser: &mut Parser) -> Result<Expr, ParseError> {
    let mut expr = parse_primary(parser)?;

    loop {
        let line = parser.line();
        if parser.eat_keyword("as") {
            let ty = parse_type(parser)?;
            expr = Expr::Cast(Box::new(expr), ty, line);
            continue;
        }
        if matches!(parser.peek(), Token::Symbol(".")) {
            return Err(ParseError::Unsupported(
                line,
                "l'acces par point",
                "il n'y a ni champ ni methode",
            ));
        }
        if matches!(parser.peek(), Token::Symbol("?")) {
            return Err(ParseError::Unsupported(
                line,
                "l'operateur ?",
                "il n'y a ni Result ni Option",
            ));
        }
        if matches!(parser.peek(), Token::Symbol("[")) {
            return Err(ParseError::Unsupported(
                line,
                "l'indexation",
                "il n'y a ni tableau ni tranche",
            ));
        }
        if matches!(parser.peek(), Token::Symbol("::")) {
            return Err(ParseError::Unsupported(
                line,
                "le chemin qualifie",
                "un programme est une unite unique, les noms sont plats",
            ));
        }
        break;
    }
    Ok(expr)
}

fn parse_primary(parser: &mut Parser) -> Result<Expr, ParseError> {
    let line = parser.line();

    match parser.advance() {
        Token::Int(value, suffix) => {
            let ty = match suffix.as_deref() {
                None => None,
                Some("i64") => Some(Type::I64),
                Some("u64") => Some(Type::U64),
                Some(other) => {
                    return Err(ParseError::UnknownType(line, String::from(other)));
                }
            };
            Ok(Expr::Int(value, ty))
        }
        Token::Text(text) => Ok(Expr::Str(text)),
        Token::Keyword("true") => Ok(Expr::Bool(true)),
        Token::Keyword("false") => Ok(Expr::Bool(false)),

        Token::Keyword("if") => {
            // Step back onto the `if`: `parse_if` expects to consume it.
            parser.position -= 1;
            let (condition, then_block, else_block) = parse_if(parser)?;
            Ok(Expr::If(
                Box::new(condition),
                Box::new(then_block),
                else_block.map(Box::new),
                line,
            ))
        }

        Token::Symbol("(") => {
            // `()` as a value.
            if parser.eat_symbol(")") {
                return Ok(Expr::Unit);
            }
            let inner = parse_expr(parser)?;
            if matches!(parser.peek(), Token::Symbol(",")) {
                return Err(ParseError::Unsupported(
                    line,
                    "le tuple",
                    "retourner une seule valeur, ou utiliser plusieurs fonctions",
                ));
            }
            parser.expect_symbol(")")?;
            Ok(inner)
        }

        Token::Symbol("{") => {
            parser.position -= 1;
            let block = parse_block(parser)?;
            Ok(Expr::Block(Box::new(block)))
        }

        Token::Symbol("|") | Token::Symbol("||") => Err(ParseError::Unsupported(
            line,
            "la fermeture",
            "ecrire une fonction nommee",
        )),

        Token::Ident(name) => {
            if parser.eat_symbol("(") {
                let mut args = Vec::new();
                if !parser.eat_symbol(")") {
                    loop {
                        args.push(parse_expr(parser)?);
                        if !parser.eat_symbol(",") {
                            break;
                        }
                        if matches!(parser.peek(), Token::Symbol(")")) {
                            break;
                        }
                    }
                    parser.expect_symbol(")")?;
                }
                return Ok(Expr::Call(name, args, line));
            }
            // A `{` right after a name at expression position would be a
            // struct literal in Rust. There are no structs here, and the
            // `{` belongs to the enclosing block.
            Ok(Expr::Path(name, line))
        }

        Token::Macro(name) => Err(ParseError::UnknownMacro(line, name)),

        other => Err(refuse_item(line, &other)),
    }
}

/// Lexes and parses a source in one call.
pub fn program(source: &str) -> Result<Program, super::RustError> {
    let tokens = super::lex::lex(source).map_err(super::RustError::Lex)?;
    parse(source, tokens).map_err(super::RustError::Parse)
}
