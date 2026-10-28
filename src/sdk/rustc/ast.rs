//! 9.5.1 -- Abstract syntax tree of the Rust subset.
//!
//! ## One tree, two back-ends
//!
//! 13.2 gives Rust two columns: an interpreter and a compiler. They share
//! this tree and nothing else -- `codegen` walks it to emit assembly,
//! `interp` walks it to produce a value. That is what makes the two agree
//! by construction: a program that runs one way and not the other would
//! have to be two different programs.
//!
//! ## Types are resolved, not named
//!
//! A `Type` here is a decided thing, not a piece of source text. The
//! parser turns `i64` into `Type::I64` and refuses anything else on the
//! spot, so no later stage has to hold an opinion about what `Foo` might
//! be. A tree that carried type *names* would make `check` responsible
//! for a resolution the subset does not have.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

/// The five types of the subset.
///
/// `usize` and `i32` are deliberately absent rather than aliased: an
/// alias would make `let n: i32 = 5_000_000_000;` compile, which is a
/// silent truncation, and the whole point of a type is to refuse that.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Type {
    I64,
    U64,
    Bool,
    /// `&str`, at run time a pointer to a `{ len: u64, bytes }` block.
    Str,
    /// `()` -- what a function with no `->` returns.
    Unit,
}

impl Type {
    pub fn as_str(&self) -> &'static str {
        match self {
            Type::I64 => "i64",
            Type::U64 => "u64",
            Type::Bool => "bool",
            Type::Str => "&str",
            Type::Unit => "()",
        }
    }

    /// Is this one of the two integer types?
    pub fn is_integer(&self) -> bool {
        matches!(self, Type::I64 | Type::U64)
    }

    /// Is the type signed? Decides `idiv` against `div`, `jl` against
    /// `jb` -- the choices where an error gives a plausible wrong answer
    /// instead of a crash.
    pub fn is_signed(&self) -> bool {
        matches!(self, Type::I64)
    }
}

/// Binary operators of the subset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
}

impl BinOp {
    pub fn as_str(&self) -> &'static str {
        match self {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::Rem => "%",
            BinOp::Eq => "==",
            BinOp::Ne => "!=",
            BinOp::Lt => "<",
            BinOp::Le => "<=",
            BinOp::Gt => ">",
            BinOp::Ge => ">=",
            BinOp::And => "&&",
            BinOp::Or => "||",
            BinOp::BitAnd => "&",
            BinOp::BitOr => "|",
            BinOp::BitXor => "^",
            BinOp::Shl => "<<",
            BinOp::Shr => ">>",
        }
    }

    /// Does the operator yield a `bool` whatever its operands are?
    pub fn is_comparison(&self) -> bool {
        matches!(
            self,
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge
        )
    }

    /// Binding power. Higher binds tighter.
    ///
    /// The table is Rust's, and the two places it differs from C are the
    /// ones worth having: `&`, `|` and `^` bind **tighter** than the
    /// comparisons, so `a & b == c` is `(a & b) == c`, and the shifts sit
    /// above the additions.
    pub fn precedence(&self) -> u8 {
        match self {
            BinOp::Or => 1,
            BinOp::And => 2,
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => 3,
            BinOp::BitOr => 4,
            BinOp::BitXor => 5,
            BinOp::BitAnd => 6,
            BinOp::Shl | BinOp::Shr => 7,
            BinOp::Add | BinOp::Sub => 8,
            BinOp::Mul | BinOp::Div | BinOp::Rem => 9,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    /// `-x`
    Neg,
    /// `!x` -- logical on a `bool`, bitwise on an integer, as in Rust.
    Not,
}

/// A piece of a `println!` format string.
#[derive(Debug, Clone)]
pub enum FormatPart {
    /// Literal text, `{{` and `}}` already reduced.
    Literal(String),
    /// A `{}` hole, filled by the argument at this index.
    Hole(usize),
}

#[derive(Debug, Clone)]
pub enum Expr {
    /// An integer literal, with the type its suffix imposed if it had one.
    Int(u64, Option<Type>),
    Bool(bool),
    Str(String),
    /// `()` written as a value.
    Unit,
    /// A name: local, parameter or `const`.
    Path(String, usize),
    Unary(UnOp, Box<Expr>, usize),
    Binary(BinOp, Box<Expr>, Box<Expr>, usize),
    /// `expr as T`
    Cast(Box<Expr>, Type, usize),
    Call(String, Vec<Expr>, usize),
    /// `if c { a } else { b }` used as a value.
    If(Box<Expr>, Box<Block>, Option<Box<Block>>, usize),
    /// A block used as a value: its tail expression is its value.
    Block(Box<Block>),
}

impl Expr {
    /// The line the expression starts on, for error messages.
    pub fn line(&self) -> usize {
        match self {
            Expr::Int(_, _) | Expr::Bool(_) | Expr::Str(_) | Expr::Unit => 0,
            Expr::Path(_, line)
            | Expr::Unary(_, _, line)
            | Expr::Binary(_, _, _, line)
            | Expr::Cast(_, _, line)
            | Expr::Call(_, _, line)
            | Expr::If(_, _, _, line) => *line,
            Expr::Block(block) => block.line,
        }
    }
}

#[derive(Debug, Clone)]
pub enum Stmt {
    /// `let [mut] name [: T] = expr;`
    Let {
        name: String,
        mutable: bool,
        declared: Option<Type>,
        value: Expr,
        line: usize,
    },
    /// `name = expr;` and the compound forms, already desugared: `x += 1`
    /// arrives here as `Assign { op: Some(Add) }` rather than as
    /// `Assign { value: Binary(Add, Path(x), 1) }`. Keeping the operator
    /// lets the checker say "x += s" instead of blaming an addition the
    /// programmer did not write.
    Assign {
        name: String,
        op: Option<BinOp>,
        value: Expr,
        line: usize,
    },
    If(Expr, Block, Option<Block>, usize),
    While(Expr, Block, usize),
    /// `loop { ... }`
    Loop(Block, usize),
    Break(usize),
    Continue(usize),
    Return(Option<Expr>, usize),
    /// An expression evaluated for its effect; its value is dropped.
    Expr(Expr, usize),
    /// `println!(...)` / `print!(...)`
    Print {
        parts: Vec<FormatPart>,
        args: Vec<Expr>,
        newline: bool,
        line: usize,
    },
}

#[derive(Debug, Clone)]
pub struct Block {
    pub statements: Vec<Stmt>,
    /// Tail expression: a block's value, when it has one.
    pub tail: Option<Expr>,
    pub line: usize,
}

#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub ty: Type,
}

#[derive(Debug, Clone)]
pub struct Function {
    pub name: String,
    pub params: Vec<Param>,
    pub result: Type,
    pub body: Block,
    pub line: usize,
}

/// A `const` item. Its value is an expression the checker folds: a
/// constant that needed run-time evaluation would need a place to live,
/// and this subset has no static storage to give it.
#[derive(Debug, Clone)]
pub struct Const {
    pub name: String,
    pub ty: Type,
    pub value: Expr,
    pub line: usize,
}

#[derive(Debug, Clone)]
pub struct Program {
    pub functions: Vec<Function>,
    pub constants: Vec<Const>,
}

impl Program {
    pub fn function(&self, name: &str) -> Option<&Function> {
        self.functions.iter().find(|item| item.name == name)
    }
}
