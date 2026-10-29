//! The syntax tree of the Python subset, as the parser builds it and the
//! compiler reads it. Each node carries its line, for tracebacks.

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use super::int::Int;
use crate::web::js::num::Num;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    MatMul,
    Div,
    FloorDiv,
    Mod,
    Pow,
    LShift,
    RShift,
    BitOr,
    BitXor,
    BitAnd,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Neg,
    Pos,
    Not,
    Invert,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    NotEq,
    Lt,
    LtE,
    Gt,
    GtE,
    Is,
    IsNot,
    In,
    NotIn,
}

#[derive(Debug, Clone)]
pub enum Const {
    None,
    True,
    False,
    Ellipsis,
    Int(Int),
    Float(Num),
    /// An imaginary literal.
    Imag(Num),
    Str(Rc<str>),
    Bytes(Rc<[u8]>),
}

/// A part of an f-string, its fields parsed.
#[derive(Debug, Clone)]
pub enum FPart {
    Text(Rc<str>),
    Field { value: Box<Expr>, conversion: Option<char>, spec: Option<Vec<FPart>>, debug: Option<Rc<str>> },
}

#[derive(Debug, Clone)]
pub enum Arg {
    Positional(Expr),
    /// `*iterable`
    Starred(Expr),
    Keyword(Rc<str>, Expr),
    /// `**mapping`
    DoubleStarred(Expr),
}

#[derive(Debug, Clone)]
pub struct Comprehension {
    pub target: Expr,
    pub iter: Expr,
    pub ifs: Vec<Expr>,
    /// `async for`.
    pub is_async: bool,
}

#[derive(Debug, Clone)]
pub struct Param {
    pub name: Rc<str>,
    pub default: Option<Expr>,
}

/// A parameter list: `(a, b=1, /, c, *args, d, e=2, **kwargs)`.
#[derive(Debug, Clone, Default)]
pub struct Params {
    pub posonly: Vec<Param>,
    pub args: Vec<Param>,
    pub vararg: Option<Rc<str>>,
    pub kwonly: Vec<Param>,
    pub kwarg: Option<Rc<str>>,
}

#[derive(Debug, Clone)]
pub struct Expr {
    pub kind: ExprKind,
    pub line: u32,
    /// Where the expression sits (columns in characters, end exclusive),
    /// for the carets of tracebacks; `end_line` 0 when unknown.
    pub col: u32,
    pub end_line: u32,
    pub end_col: u32,
    /// The part tracebacks underline with `^` (a binary operator, the
    /// parentheses of a call, the brackets of a subscript), on `line`.
    pub anchor: (u32, u32),
}

#[derive(Debug, Clone)]
pub enum ExprKind {
    Name(Rc<str>),
    Const(Const),
    FString(Vec<FPart>),
    BinOp(BinOp, Box<Expr>, Box<Expr>),
    Unary(UnaryOp, Box<Expr>),
    /// `and` (true) or `or` (false), two or more operands.
    BoolOp(bool, Vec<Expr>),
    /// `a < b <= c`: the first operand, then each operator and operand.
    Compare(Box<Expr>, Vec<(CmpOp, Expr)>),
    Call(Box<Expr>, Vec<Arg>),
    Attribute(Box<Expr>, Rc<str>),
    Subscript(Box<Expr>, Box<Expr>),
    Slice(Option<Box<Expr>>, Option<Box<Expr>>, Option<Box<Expr>>),
    List(Vec<Expr>),
    Tuple(Vec<Expr>),
    Set(Vec<Expr>),
    /// Keys are `None` for `**mapping`.
    Dict(Vec<(Option<Expr>, Expr)>),
    ListComp(Box<Expr>, Vec<Comprehension>),
    SetComp(Box<Expr>, Vec<Comprehension>),
    DictComp(Box<Expr>, Box<Expr>, Vec<Comprehension>),
    Generator(Box<Expr>, Vec<Comprehension>),
    Lambda(Rc<Params>, Box<Expr>),
    IfExp(Box<Expr>, Box<Expr>, Box<Expr>),
    Starred(Box<Expr>),
    /// `name := value`
    Named(Rc<str>, Box<Expr>),
    Yield(Option<Box<Expr>>),
    YieldFrom(Box<Expr>),
    /// `await value`
    Await(Box<Expr>),
}

#[derive(Debug, Clone)]
pub struct ExceptHandler {
    pub kind: Option<Expr>,
    pub name: Option<Rc<str>>,
    pub body: Vec<Stmt>,
    pub line: u32,
}

#[derive(Debug, Clone)]
pub struct FunctionDef {
    pub name: Rc<str>,
    pub params: Rc<Params>,
    pub body: Vec<Stmt>,
    pub decorators: Vec<Expr>,
    pub line: u32,
    /// `async def`.
    pub is_async: bool,
}

#[derive(Debug, Clone)]
pub struct ClassDef {
    pub name: Rc<str>,
    pub bases: Vec<Expr>,
    pub keywords: Vec<(Rc<str>, Expr)>,
    pub body: Vec<Stmt>,
    pub decorators: Vec<Expr>,
    pub line: u32,
}

#[derive(Debug, Clone)]
pub struct Stmt {
    pub kind: StmtKind,
    pub line: u32,
}

#[derive(Debug, Clone)]
pub enum StmtKind {
    Expr(Expr),
    /// `a = b = value`: every target, then the value.
    Assign(Vec<Expr>, Expr),
    AugAssign(Expr, BinOp, Expr),
    /// `x: int = 1` (the annotation is read and dropped).
    /// Target, annotation, value.
    AnnAssign(Expr, Expr, Option<Expr>),
    If(Expr, Vec<Stmt>, Vec<Stmt>),
    While(Expr, Vec<Stmt>, Vec<Stmt>),
    For(Expr, Expr, Vec<Stmt>, Vec<Stmt>),
    Break,
    Continue,
    Pass,
    Return(Option<Expr>),
    FunctionDef(Rc<FunctionDef>),
    ClassDef(Rc<ClassDef>),
    Try { body: Vec<Stmt>, handlers: Vec<ExceptHandler>, orelse: Vec<Stmt>, finalbody: Rc<Vec<Stmt>> },
    Raise(Option<Expr>, Option<Expr>),
    With(Vec<(Expr, Option<Expr>)>, Vec<Stmt>),
    /// `import a.b as c`: the dotted name, the alias.
    Import(Vec<(Rc<str>, Option<Rc<str>>)>),
    /// `from .m import a as b`: module, level (dots), names (`*` alone
    /// for a star import).
    ImportFrom(Option<Rc<str>>, usize, Vec<(Rc<str>, Option<Rc<str>>)>),
    Global(Vec<Rc<str>>),
    Nonlocal(Vec<Rc<str>>),
    Delete(Vec<Expr>),
    Assert(Expr, Option<Expr>),
    /// `match subject:` and its cases.
    Match(Expr, Vec<MatchCase>),
}

/// A pattern of `case` (PEP 634).
#[derive(Debug, Clone)]
pub enum Pattern {
    /// `_`
    Wildcard,
    /// `name`
    Capture(Rc<str>),
    /// A literal or a dotted name, compared with `==`.
    Value(Expr),
    /// `None`, `True`, `False`, compared with `is`.
    Singleton(Const),
    /// `[a, *rest, b]` or `(a, b)`.
    Sequence(Vec<Pattern>),
    /// `*name` (or `*_`) inside a sequence.
    Star(Option<Rc<str>>),
    /// `{key: pattern, **rest}`.
    Mapping(Vec<(Expr, Pattern)>, Option<Rc<str>>),
    /// `Cls(p, q, attr=r)`.
    Class(Expr, Vec<Pattern>, Vec<(Rc<str>, Pattern)>),
    Or(Vec<Pattern>),
    As(Box<Pattern>, Rc<str>),
}

#[derive(Debug, Clone)]
pub struct MatchCase {
    pub pattern: Pattern,
    pub guard: Option<Expr>,
    pub body: Vec<Stmt>,
    pub line: u32,
}

impl Pattern {
    /// The names a pattern binds, in order.
    pub fn names(&self, out: &mut Vec<Rc<str>>) {
        match self {
            Pattern::Capture(n) => out.push(n.clone()),
            Pattern::Star(Some(n)) => out.push(n.clone()),
            Pattern::Sequence(items) => items.iter().for_each(|p| p.names(out)),
            Pattern::Mapping(items, rest) => {
                items.iter().for_each(|(_, p)| p.names(out));
                if let Some(r) = rest {
                    out.push(r.clone());
                }
            }
            Pattern::Class(_, args, kwargs) => {
                args.iter().for_each(|p| p.names(out));
                kwargs.iter().for_each(|(_, p)| p.names(out));
            }
            Pattern::Or(alternatives) => {
                if let Some(first) = alternatives.first() {
                    first.names(out);
                }
            }
            Pattern::As(inner, n) => {
                inner.names(out);
                out.push(n.clone());
            }
            _ => {}
        }
    }
}

/// A parsed module.
#[derive(Debug, Clone)]
pub struct Module {
    pub body: Vec<Stmt>,
    /// The source, for tracebacks.
    pub source: Rc<str>,
    pub filename: Rc<str>,
}

impl Expr {
    /// An expression whose columns are not known (made by the parser
    /// for a desugaring, or by the compiler).
    pub fn new(kind: ExprKind, line: u32) -> Expr {
        Expr { kind, line, col: 0, end_line: 0, end_col: 0, anchor: (0, 0) }
    }
}

pub fn name_of(name: &str) -> Rc<str> {
    Rc::from(name)
}

pub fn display(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::MatMul => "@",
        BinOp::Div => "/",
        BinOp::FloorDiv => "//",
        BinOp::Mod => "%",
        BinOp::Pow => "**",
        BinOp::LShift => "<<",
        BinOp::RShift => ">>",
        BinOp::BitOr => "|",
        BinOp::BitXor => "^",
        BinOp::BitAnd => "&",
    }
}

pub fn unused(_: String) {}
