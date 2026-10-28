//! The syntax tree the interpreter walks.
//!
//! Function bodies sit behind `Rc`: every closure made from the same
//! function literal shares one tree, and a function made a thousand times
//! in a loop does not copy its body a thousand times.

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::vec::Vec;

use super::num::Num;
use super::value::JsStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VarKind {
    Var,
    Let,
    Const,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Exp,
    Shl,
    Shr,
    UShr,
    BitAnd,
    BitOr,
    BitXor,
    Eq,
    Ne,
    StrictEq,
    StrictNe,
    Lt,
    Gt,
    Le,
    Ge,
    In,
    InstanceOf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogicOp {
    And,
    Or,
    Nullish,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Neg,
    Plus,
    Not,
    BitNot,
    TypeOf,
    Void,
    Delete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssignOp {
    Assign,
    Op(BinOp),
    Logic(LogicOp),
}

/// A property name in a literal or pattern.
#[derive(Debug, Clone)]
pub enum PropKey {
    Name(JsStr),
    Computed(Box<Expr>),
    Private(JsStr),
}

#[derive(Debug, Clone)]
pub enum PropKind {
    Value,
    Get,
    Set,
    /// A method: sees `super`.
    Method,
    /// `...expr`
    Spread,
}

#[derive(Debug, Clone)]
pub struct PropDef {
    pub key: PropKey,
    pub kind: PropKind,
    pub value: Expr,
}

#[derive(Debug, Clone)]
pub enum Pattern {
    Ident(JsStr),
    /// (key, target, default), then `...rest`.
    Object(Vec<(PropKey, Pattern, Option<Expr>)>, Option<Box<Pattern>>),
    /// Elements (holes are `None`), then `...rest`.
    Array(Vec<Option<(Pattern, Option<Expr>)>>, Option<Box<Pattern>>),
    /// An assignment target that is not a name: `a.b`, `a[i]`.
    Target(Box<Expr>),
}

#[derive(Debug, Clone)]
pub struct Func {
    pub name: Option<JsStr>,
    pub params: Vec<(Pattern, Option<Expr>)>,
    pub rest: Option<Pattern>,
    pub body: Vec<Stmt>,
    pub arrow: bool,
    pub is_async: bool,
    pub generator: bool,
    /// The body is `expr` (arrow with an expression body).
    pub expression: bool,
    /// Names declared with `var` anywhere in the body, and the names of
    /// the function declarations directly in it: hoisted on entry.
    pub vars: Vec<JsStr>,
    /// `arguments` appears in the body.
    pub uses_arguments: bool,
    /// Parameters are plain names without defaults: the fast path.
    pub simple_params: bool,
    /// Source text, for `Function.prototype.toString`.
    pub source: (usize, usize),
    pub strict: bool,
    /// A class constructor: `new` only.
    pub is_ctor: bool,
}

#[derive(Debug, Clone)]
pub struct ClassMember {
    pub key: PropKey,
    pub kind: PropKind,
    pub is_static: bool,
    /// Methods and accessors: a function; fields: an initializer.
    pub value: Option<Expr>,
    pub field: bool,
}

#[derive(Debug, Clone)]
pub struct Class {
    pub name: Option<JsStr>,
    pub extends: Option<Box<Expr>>,
    pub constructor: Option<Rc<Func>>,
    pub members: Vec<ClassMember>,
    /// `static { ... }` blocks.
    pub static_blocks: Vec<Rc<Func>>,
}

#[derive(Debug, Clone)]
pub enum Arg {
    Plain(Expr),
    Spread(Expr),
}

#[derive(Debug, Clone)]
pub enum Expr {
    Num(Num),
    Str(JsStr),
    /// Cooked parts and substitutions.
    Template(Vec<JsStr>, Vec<Expr>),
    /// Tag, cooked parts (maybe undefined), raw parts, substitutions.
    Tagged(Box<Expr>, Rc<(Vec<Option<JsStr>>, Vec<JsStr>)>, Vec<Expr>),
    Regex(JsStr, JsStr),
    Ident(JsStr),
    This,
    Null,
    Undefined,
    Bool(bool),
    Array(Vec<Option<Arg>>),
    Object(Vec<PropDef>),
    Function(Rc<Func>),
    Class(Rc<Class>),
    Unary(UnaryOp, Box<Expr>),
    /// ++/--: increment?, prefix?, target.
    Update(bool, bool, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    Logic(LogicOp, Box<Expr>, Box<Expr>),
    Assign(AssignOp, Box<Pattern>, Box<Expr>),
    Cond(Box<Expr>, Box<Expr>, Box<Expr>),
    /// Callee, arguments, optional call `f?.()`.
    Call(Box<Expr>, Vec<Arg>, bool),
    New(Box<Expr>, Vec<Arg>),
    /// Object, property name, optional `?.`.
    Member(Box<Expr>, JsStr, bool),
    Index(Box<Expr>, Box<Expr>, bool),
    /// `obj.#name`
    PrivateMember(Box<Expr>, JsStr),
    /// `#name in obj`
    PrivateIn(JsStr, Box<Expr>),
    /// The end of an optional chain: a `?.` inside short-circuits to
    /// here.
    Chain(Box<Expr>),
    Seq(Vec<Expr>),
    Await(Box<Expr>),
    /// Value, delegate (`yield*`).
    Yield(Option<Box<Expr>>, bool),
    SuperCall(Vec<Arg>),
    SuperMember(Box<Expr>),
    NewTarget,
    /// `import(...)`: a promise of the module's namespace, from the
    /// realm's module loader.
    Import(Box<Expr>),
    /// `import.meta`
    ImportMeta,
}

#[derive(Debug, Clone)]
pub enum ForInit {
    Var(VarKind, Vec<(Pattern, Option<Expr>)>),
    Expr(Expr),
}

#[derive(Debug, Clone)]
pub enum Stmt {
    Expr(Expr),
    Var(VarKind, Vec<(Pattern, Option<Expr>)>),
    Function(Rc<Func>),
    Class(JsStr, Rc<Class>),
    Return(Option<Expr>),
    If(Expr, Box<Stmt>, Option<Box<Stmt>>),
    Block(Vec<Stmt>),
    For(Option<ForInit>, Option<Expr>, Option<Expr>, Box<Stmt>),
    /// Declaration kind (None: an existing target), target, object, body.
    ForIn(Option<VarKind>, Pattern, Expr, Box<Stmt>),
    ForOf(Option<VarKind>, Pattern, Expr, Box<Stmt>),
    While(Expr, Box<Stmt>),
    DoWhile(Box<Stmt>, Expr),
    Break(Option<JsStr>),
    Continue(Option<JsStr>),
    Throw(Expr),
    /// Block, catch (binding, body), finally.
    Try(Vec<Stmt>, Option<(Option<Pattern>, Vec<Stmt>)>, Option<Vec<Stmt>>),
    Switch(Expr, Vec<(Option<Expr>, Vec<Stmt>)>),
    Labeled(JsStr, Box<Stmt>),
    Empty,
}

/// What an `import` binds a local name to.
#[derive(Debug, Clone)]
pub enum ImportName {
    /// `import x from`
    Default,
    /// `import * as x from`
    Namespace,
    /// `import { a as x } from`
    Named(JsStr),
}

/// `import ... from "source"`: the local names and what they read.
#[derive(Debug, Clone)]
pub struct ImportDecl {
    pub source: JsStr,
    pub bindings: Vec<(ImportName, JsStr)>,
}

/// One name a module exports.
#[derive(Debug, Clone)]
pub enum ExportEntry {
    /// Exported name, local binding: `export const a`, `export { a as b }`,
    /// `export default` (the binding is named `default`, which no source
    /// can name).
    Local(JsStr, JsStr),
    /// Exported name, source module, imported name (`None`: the whole
    /// namespace, `export * as ns from`).
    From(JsStr, JsStr, Option<JsStr>),
    /// `export * from "source"`
    All(JsStr),
}

/// The import and export declarations of a module, taken out of its body:
/// imports are resolved before the body runs, as the language wants.
#[derive(Debug, Clone, Default)]
pub struct ModuleInfo {
    pub imports: Vec<ImportDecl>,
    pub exports: Vec<ExportEntry>,
}

/// A parsed script.
pub struct Program {
    pub body: Vec<Stmt>,
    pub vars: Vec<JsStr>,
    pub strict: bool,
    /// A module: what it imports and exports.
    pub module: Option<ModuleInfo>,
}
