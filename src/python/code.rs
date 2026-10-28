//! Bytecode: what the compiler produces and the virtual machine runs.
//!
//! A stack machine in the manner of CPython's (3.8 era, block stack
//! included), with instructions as a Rust enum rather than bytes: the
//! operands are typed, and a `match` over them is the dispatch. Each
//! function body is a `Code`; its constants, names and nested codes are
//! referred to by index.

use alloc::rc::Rc;
use alloc::vec::Vec;

use super::ast::{BinOp, CmpOp, UnaryOp};
use super::value::Value;

#[derive(Debug, Clone, Copy)]
pub enum Op {
    Nop,
    Pop,
    Dup,
    DupTwo,
    Rot2,
    Rot3,
    Rot4,
    LoadConst(u32),
    /// Locals of a function, by slot.
    LoadFast(u32),
    StoreFast(u32),
    DeleteFast(u32),
    /// Cells (variables shared with inner functions): the function's own
    /// first, then the free ones.
    LoadDeref(u32),
    StoreDeref(u32),
    DeleteDeref(u32),
    /// Pushes the cell itself, to build a closure.
    LoadClosure(u32),
    /// In a class body, a free variable: the class namespace first.
    LoadClassDeref(u32),
    LoadGlobal(u32),
    StoreGlobal(u32),
    DeleteGlobal(u32),
    /// Module level and class bodies: the namespace dict, then globals,
    /// then builtins.
    LoadName(u32),
    StoreName(u32),
    DeleteName(u32),
    LoadAttr(u32),
    StoreAttr(u32),
    DeleteAttr(u32),
    Subscr,
    StoreSubscr,
    DeleteSubscr,
    Binary(BinOp),
    Inplace(BinOp),
    Unary(UnaryOp),
    /// `==`, `!=`, `<`... (identity and membership have their own).
    Compare(CmpOp),
    Is(bool),
    /// `in` (false) or `not in` (true).
    Contains(bool),
    BuildTuple(u32),
    BuildList(u32),
    BuildSet(u32),
    /// A set display of more than two constants (CPython goes through a
    /// frozenset constant).
    BuildConstSet(u32),
    BuildMap(u32),
    /// Slice of 2 or 3 values.
    BuildSlice(u32),
    /// Concatenates n strings (f-strings).
    BuildString(u32),
    ListAppend(u32),
    SetAdd(u32),
    MapAdd(u32),
    ListExtend(u32),
    SetUpdate(u32),
    DictUpdate(u32),
    /// `f(**a, **b)`: like DictUpdate, a repeated key is a TypeError.
    DictMerge(u32),
    ListToTuple,
    /// Conversion (0 none, b's', b'r', b'a'), and whether a spec is on
    /// the stack above the value.
    FormatValue(u8, bool),
    UnpackSequence(u32),
    /// `a, *b, c`: counts before and after the starred target.
    UnpackEx(u16, u16),
    Jump(u32),
    PopJumpIfFalse(u32),
    PopJumpIfTrue(u32),
    JumpIfFalseOrPop(u32),
    JumpIfTrueOrPop(u32),
    GetIter,
    /// Pushes the next item, or pops the iterator and jumps.
    ForIter(u32),
    /// Positional arguments only.
    Call(u32),
    /// Arguments then a tuple of keyword names (the last ones).
    CallKw(u32),
    /// Function, a tuple of positional arguments, and a dict if the flag
    /// says so.
    CallEx(bool),
    /// Builds a function from the code at this constant index; the flags
    /// say what is on the stack below: 1 defaults (tuple), 2 keyword
    /// defaults (dict), 8 closure (tuple of cells). Qualified name on top.
    MakeFunction(u32, u8),
    Return,
    Yield,
    GetYieldFromIter,
    /// Delegates to the iterator under the sent value.
    YieldFrom,
    /// Pushes a block: an exception raised before the matching PopBlock
    /// jumps to the handler, with the exception on the stack.
    SetupFinally(u32),
    PopBlock,
    /// Leaves an except handler: the exception handled before comes back.
    PopExcept,
    /// Raises the exception on the stack again.
    Reraise,
    /// `raise` (0), `raise e` (1), `raise e from c` (2).
    Raise(u32),
    /// Stack: exception, class(es). Pops both; jumps if no match.
    JumpIfNotExcMatch(u32),
    /// `with`: calls `__enter__`, pushes `__exit__` and the result, and a
    /// block for the handler.
    SetupWith(u32),
    /// In the handler of a `with`: calls `__exit__(type, exc, tb)`.
    WithExceptStart,
    /// Builds a class: the body function and the name on the stack, then
    /// the bases (count), then keyword arguments (count).
    BuildClass(u32, u32),
    /// `import name` (name index, level); the from-list is on the stack.
    ImportName(u32, u32),
    ImportFrom(u32),
    ImportStar,
    LoadAssertionError,
    /// Class bodies: `__annotations__ = {}`.
    SetupAnnotations,
}

/// A compiled function body (or module, or class body).
#[derive(Debug)]
pub struct Code {
    pub name: Rc<str>,
    pub qualname: Rc<str>,
    pub filename: Rc<str>,
    pub ops: Vec<Op>,
    /// The source line of each op, for tracebacks.
    pub lines: Vec<u32>,
    pub consts: Vec<Value>,
    pub names: Vec<Rc<str>>,
    /// Local variables, parameters first.
    pub varnames: Vec<Rc<str>>,
    /// Cells this code creates (its locals that inner functions use).
    pub cellvars: Vec<Rc<str>>,
    /// Cells it receives from the enclosing function.
    pub freevars: Vec<Rc<str>>,
    /// For each cell variable that is also a parameter, its slot.
    pub cell_params: Vec<(u32, u32)>,
    pub argcount: u32,
    pub posonly: u32,
    pub kwonly: u32,
    pub varargs: bool,
    pub varkw: bool,
    pub generator: bool,
    /// Module and class bodies use a namespace dict for their names.
    pub uses_namespace: bool,
    pub first_line: u32,
    pub doc: Option<Rc<str>>,
}

impl core::fmt::Debug for Value {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("<valeur>")
    }
}
