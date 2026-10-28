//! The typed tree the parser hands to the code generator.
//!
//! Every expression node carries its C type, decided when the node is
//! built: the conversions C makes implicitly (integer promotions, the
//! usual arithmetic conversions, scaling a pointer offset, converting an
//! argument to its parameter's type) are explicit `Cast` and `Mul` nodes
//! by the time code is generated. The generator never has to ask what C
//! means, only how to compute it.

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::vec::Vec;

use super::lex::Loc;
use super::ty::Type;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    Eq,
    Ne,
    Lt,
    Le,
}

#[derive(Debug, Clone)]
pub enum NodeKind {
    /// Empty statement.
    Null,
    Num(i64),
    /// A `double` (or `float`, widened), by its bit pattern.
    Float(u64),
    Local(usize),
    /// A global by its assembly label.
    Global(Rc<str>),
    /// `lhs.member`, by offset; the node's type is the member's.
    Member(Box<Node>, i64),
    Addr(Box<Node>),
    Deref(Box<Node>),
    Neg(Box<Node>),
    Not(Box<Node>),
    BitNot(Box<Node>),
    Cast(Box<Node>),
    Binary(Op, Box<Node>, Box<Node>),
    LogAnd(Box<Node>, Box<Node>),
    LogOr(Box<Node>, Box<Node>),
    Cond(Box<Node>, Box<Node>, Box<Node>),
    Comma(Box<Node>, Box<Node>),
    Assign(Box<Node>, Box<Node>),
    /// `callee(args)`; `buffer` is the local that receives a returned
    /// structure.
    Call { callee: Box<Node>, args: Vec<Node>, function: Rc<Type>, buffer: Option<usize> },
    /// Clears a local (before a partial initializer).
    Zero(usize),
    /// `__builtin_syscall(n, a, b, ...)`.
    Syscall(Vec<Node>),
    /// Address of the first variadic argument of the current function.
    VaStart,
    /// `__builtin_sqrt(x)`: `sqrtsd`, correctly rounded.
    Sqrt(Box<Node>),

    Return(Option<Box<Node>>),
    If(Box<Node>, Box<Node>, Option<Box<Node>>),
    /// `for` and `while`: labels for `break` and `continue`.
    Loop { init: Option<Box<Node>>, cond: Option<Box<Node>>, step: Option<Box<Node>>, body: Box<Node>, exit: u32, next: u32 },
    DoWhile { body: Box<Node>, cond: Box<Node>, exit: u32, next: u32 },
    Switch { cond: Box<Node>, body: Box<Node>, cases: Vec<(i64, u32)>, default: Option<u32>, exit: u32 },
    Block(Vec<Node>),
    Goto(u32),
    Label(u32, Box<Node>),
    Expr(Box<Node>),
}

#[derive(Debug, Clone)]
pub struct Node {
    pub kind: NodeKind,
    pub ty: Rc<Type>,
    pub loc: Loc,
}

impl Node {
    pub fn new(kind: NodeKind, ty: Rc<Type>, loc: Loc) -> Node {
        Node { kind, ty, loc }
    }

    pub fn statement(kind: NodeKind, loc: Loc) -> Node {
        Node { kind, ty: Type::void(), loc }
    }
}

#[derive(Debug, Clone)]
pub struct Local {
    pub name: Rc<str>,
    pub ty: Rc<Type>,
    /// From `rbp`: negative for locals (decided by the generator),
    /// positive for parameters (decided by the parser).
    pub offset: i64,
    pub param: bool,
}

/// A pointer stored in initialised data, filled at startup: Nova
/// binaries carry no relocation (`sdk::emit`).
#[derive(Debug, Clone)]
pub struct Reloc {
    pub offset: i64,
    pub label: Rc<str>,
    pub addend: i64,
}

#[derive(Debug, Clone)]
pub struct Global {
    pub label: Rc<str>,
    pub ty: Rc<Type>,
    /// `None`: zero-filled (`.bss`).
    pub data: Option<Vec<u8>>,
    pub relocs: Vec<Reloc>,
    pub defined: bool,
}

#[derive(Debug, Clone)]
pub struct Function {
    pub label: Rc<str>,
    pub name: Rc<str>,
    pub ty: Rc<Type>,
    pub locals: Vec<Local>,
    pub body: Node,
    /// Offset from `rbp` of the hidden pointer a structure is returned
    /// through, if the function returns one.
    pub return_buffer: Option<i64>,
    /// Offset from `rbp` of the first variadic argument.
    pub va_offset: i64,
    /// Labels of the functions and data this body names: what keeps a
    /// function of the library in the binary.
    pub references: Vec<Rc<str>>,
}

#[derive(Debug, Default)]
pub struct Program {
    pub globals: Vec<Global>,
    pub functions: Vec<Function>,
}
