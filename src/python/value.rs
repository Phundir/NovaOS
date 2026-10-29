//! Python's values and objects.
//!
//! `Value` is a small enum: immediate for `None`, booleans, ints that fit
//! (`Int::Small`) and floats; a reference-counted pointer for the rest.
//! Mutable containers sit behind `RefCell`: Python aliases freely
//! (`a = b = []`), and the interpreter is single-threaded.
//!
//! Every value has a class (`Vm::class_of`): the built-in types (`int`,
//! `str`, `list`...) are `Class` objects like the user's, whose
//! dictionaries hold native methods. Attribute lookup is therefore one
//! algorithm for both: the instance's own dictionary, then the classes of
//! the MRO.
//!
//! Reference cycles (an object holding itself, a closure capturing its
//! own function) are not collected while the program runs: the whole heap
//! of a run is freed at its end (`Vm::teardown` breaks the cycles), which
//! is the lifetime of a `dev interpret`.

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use alloc::rc::Weak;
use core::cell::{Cell, RefCell, UnsafeCell};

use super::code::Code;
use super::dict::{Dict, SetTable};
use super::int::Int;
use crate::web::js::num::Num;

#[derive(Clone)]
pub enum Value {
    None,
    NotImplemented,
    Ellipsis,
    Bool(bool),
    Int(Int),
    Float(Num),
    /// A complex number: real and imaginary parts.
    Complex(Num, Num),
    Str(Rc<str>),
    Bytes(Rc<[u8]>),
    /// `bytearray`: mutable bytes.
    ByteArray(Rc<RefCell<Vec<u8>>>),
    List(Rc<RefCell<Vec<Value>>>),
    Tuple(Rc<[Value]>),
    Dict(Rc<RefCell<Dict>>),
    Set(Rc<RefCell<SetTable>>),
    FrozenSet(Rc<SetTable>),
    /// `dict.keys()`, `values()`, `items()`: live views of a dict.
    View(Rc<View>),
    Range(Rc<Range>),
    Slice(Rc<[Value; 3]>),
    Function(Rc<Function>),
    Builtin(Rc<Builtin>),
    /// A function bound to its first argument.
    Method(Rc<(Value, Value)>),
    Class(Rc<Class>),
    Instance(Rc<Instance>),
    Iter(Rc<RefCell<Iter>>),
    Generator(Rc<RefCell<Generator>>),
    Module(Rc<Module>),
    Property(Rc<[Value; 3]>),
    StaticMethod(Rc<Value>),
    ClassMethod(Rc<Value>),
    /// `super(cls, obj)`: the class after which to look, the object.
    Super(Rc<(Rc<Class>, Value)>),
    File(Rc<RefCell<File>>),
    /// Internal: a compiled body (a constant of the code that makes the
    /// function), and a cell on its way into a closure.
    Code(Rc<Code>),
    Cell(Rc<RefCell<Option<Value>>>),
}

/// A view of a dict: 0 keys, 1 values, 2 items.
pub struct View {
    pub dict: Rc<RefCell<Dict>>,
    pub kind: u8,
}

/// `range(start, stop, step)`, with machine integers (a range of big
/// ints is refused).
pub struct Range {
    pub start: i64,
    pub stop: i64,
    pub step: i64,
}

impl Range {
    pub fn len(&self) -> i64 {
        if self.step > 0 && self.start < self.stop {
            (self.stop - self.start - 1) / self.step + 1
        } else if self.step < 0 && self.start > self.stop {
            (self.start - self.stop - 1) / (-self.step) + 1
        } else {
            0
        }
    }

    pub fn get(&self, index: i64) -> i64 {
        self.start + index * self.step
    }
}

/// A function made by `def` or `lambda`.
pub struct Function {
    pub code: Rc<Code>,
    pub globals: Rc<Module>,
    pub defaults: Vec<Value>,
    pub kwdefaults: Vec<(Rc<str>, Value)>,
    pub closure: Vec<Rc<RefCell<Option<Value>>>>,
    pub name: RefCell<Rc<str>>,
    pub qualname: RefCell<Rc<str>>,
    pub dict: RefCell<Dict>,
    pub doc: RefCell<Value>,
}

pub type NativeFn = fn(&mut super::vm::Vm, &[Value], &[(Rc<str>, Value)]) -> super::vm::PyResult<Value>;

/// A function written in Rust.
pub struct Builtin {
    pub name: &'static str,
    pub call: NativeFn,
    /// Native methods are looked up on a type: bound to `self` like a
    /// function; free builtins (`len`) are not.
    pub method: bool,
    /// Data a native function carries (a module's state, a partial's
    /// arguments).
    pub data: Value,
}

/// What a class's instances are made of, when it derives from a built-in
/// type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Base {
    Object,
    Int,
    Float,
    Str,
    Bytes,
    Bool,
    List,
    Tuple,
    Dict,
    Set,
    FrozenSet,
    Exception,
    Other,
}

pub struct Class {
    pub name: Rc<str>,
    pub qualname: RefCell<Rc<str>>,
    pub bases: Vec<Rc<Class>>,
    pub mro: RefCell<Vec<Rc<Class>>>,
    pub dict: RefCell<Dict>,
    /// The built-in type underneath, if any.
    pub base: Base,
    /// A unique number: `id`, and the order of classes in errors.
    pub id: usize,
    /// The class of this class when it is not `type`.
    pub metaclass: RefCell<Option<Rc<Class>>>,
    /// The classes derived directly from this one (`__subclasses__`).
    pub subclasses: RefCell<Vec<alloc::rc::Weak<Class>>>,
}

pub struct Instance {
    pub class: RefCell<Rc<Class>>,
    /// Shared, so that `obj.__dict__` is the live dictionary.
    pub dict: Rc<RefCell<Dict>>,
    /// For an instance of a subclass of a built-in type (or an exception:
    /// its traceback), the built-in value inside.
    pub inner: RefCell<Value>,
    /// Exceptions: the traceback lines gathered while unwinding, and the
    /// exception being handled when this one was raised.
    pub traceback: RefCell<Vec<(Rc<str>, u32, Rc<str>, super::code::Pos)>>,
    pub context: RefCell<Value>,
    pub cause: RefCell<Value>,
}

/// The iterators the runtime makes.
pub enum Iter {
    Seq { seq: Value, index: usize },
    Range { next: i64, stop: i64, step: i64 },
    Str { text: Rc<str>, byte: usize },
    /// A dict walked by raw position; `size` is its length at the start
    /// (a dict that grows or shrinks under its iterator is an error).
    Keys { dict: Value, index: usize, kind: u8, size: usize },
    Enumerate { inner: Value, count: Int },
    Zip { inners: Vec<Value>, strict: bool },
    Map { function: Value, inners: Vec<Value> },
    Filter { function: Value, inner: Value },
    Reversed { seq: Value, index: usize },
    /// `iter(callable, sentinel)`
    Callable { function: Value, sentinel: Value },
    /// A user object whose `__iter__` returned itself: calls `__next__`.
    Object(Value),
    Done,
}

/// A generator: a frame that yields.
pub struct Generator {
    pub frame: Option<super::vm::Frame>,
    pub running: bool,
    pub name: Rc<str>,
    /// 0 a generator, 1 a coroutine (`async def`), 2 an asynchronous
    /// generator (`async def` with `yield`).
    pub kind: u8,
}

pub struct Module {
    pub name: Rc<str>,
    pub dict: Rc<RefCell<Dict>>,
    pub file: Rc<str>,
}

/// An open file: its path, its content (read whole at `open`, written
/// back at `close`), a position.
pub struct File {
    pub path: Rc<str>,
    pub data: Vec<u8>,
    pub position: usize,
    pub readable: bool,
    pub writable: bool,
    /// Mode `a`: every write goes to the end.
    pub append: bool,
    pub binary: bool,
    pub closed: bool,
    pub dirty: bool,
}

// ------------------------------------------------------------
//  The registry of containers
// ------------------------------------------------------------

/// A container that can hold a reference back to itself, weakly.
enum Tracked {
    List(Weak<RefCell<Vec<Value>>>),
    Dict(Weak<RefCell<Dict>>),
    Set(Weak<RefCell<SetTable>>),
    Cell(Weak<RefCell<Option<Value>>>),
    Iter(Weak<RefCell<Iter>>),
    Generator(Weak<RefCell<Generator>>),
    Instance(Weak<Instance>),
    Function(Weak<Function>),
    Class(Weak<Class>),
}

impl Tracked {
    fn alive(&self) -> bool {
        match self {
            Tracked::List(w) => w.strong_count() > 0,
            Tracked::Dict(w) => w.strong_count() > 0,
            Tracked::Set(w) => w.strong_count() > 0,
            Tracked::Cell(w) => w.strong_count() > 0,
            Tracked::Iter(w) => w.strong_count() > 0,
            Tracked::Generator(w) => w.strong_count() > 0,
            Tracked::Instance(w) => w.strong_count() > 0,
            Tracked::Function(w) => w.strong_count() > 0,
            Tracked::Class(w) => w.strong_count() > 0,
        }
    }

    /// Empties the container: whatever cycle went through it is broken.
    fn release(&self) {
        match self {
            Tracked::List(w) => {
                if let Some(l) = w.upgrade() {
                    let items = core::mem::take(&mut *l.borrow_mut());
                    drop(items);
                }
            }
            Tracked::Dict(w) => {
                if let Some(d) = w.upgrade() {
                    let dict = core::mem::take(&mut *d.borrow_mut());
                    drop(dict);
                }
            }
            Tracked::Set(w) => {
                if let Some(s) = w.upgrade() {
                    let set = core::mem::take(&mut *s.borrow_mut());
                    drop(set);
                }
            }
            Tracked::Cell(w) => {
                if let Some(c) = w.upgrade() {
                    let value = c.borrow_mut().take();
                    drop(value);
                }
            }
            Tracked::Iter(w) => {
                if let Some(i) = w.upgrade() {
                    let state = core::mem::replace(&mut *i.borrow_mut(), Iter::Done);
                    drop(state);
                }
            }
            Tracked::Generator(w) => {
                if let Some(g) = w.upgrade() {
                    let frame = g.borrow_mut().frame.take();
                    drop(frame);
                }
            }
            Tracked::Instance(w) => {
                if let Some(i) = w.upgrade() {
                    let dict = core::mem::take(&mut *i.dict.borrow_mut());
                    let inner = core::mem::replace(&mut *i.inner.borrow_mut(), Value::None);
                    let context = core::mem::replace(&mut *i.context.borrow_mut(), Value::None);
                    let cause = core::mem::replace(&mut *i.cause.borrow_mut(), Value::None);
                    drop((dict, inner, context, cause));
                }
            }
            Tracked::Function(w) => {
                if let Some(f) = w.upgrade() {
                    let dict = core::mem::take(&mut *f.dict.borrow_mut());
                    let doc = core::mem::replace(&mut *f.doc.borrow_mut(), Value::None);
                    drop((dict, doc));
                }
            }
            Tracked::Class(w) => {
                if let Some(c) = w.upgrade() {
                    let dict = core::mem::take(&mut *c.dict.borrow_mut());
                    let mro = core::mem::take(&mut *c.mro.borrow_mut());
                    drop((dict, mro));
                }
            }
        }
    }
}

/// Every container a run made, weakly: what `release_all` empties at the
/// end, so that the cycles Python programs make (a class is in its own
/// MRO, an object may hold itself) do not stay in the kernel's heap.
/// Dead entries are pruned whenever the list doubles.
struct Registry {
    objects: UnsafeCell<Vec<Tracked>>,
    prune_at: Cell<usize>,
}

// SAFETY: one interpreter runs at a time (`sdk::python` holds `BUSY`),
// in one task: the registry is never touched by two cores at once.
unsafe impl Sync for Registry {}

static REGISTRY: Registry = Registry { objects: UnsafeCell::new(Vec::new()), prune_at: Cell::new(4096) };

fn track(object: Tracked) {
    // SAFETY: see `Registry`; no reference to the vector outlives this
    // call.
    let objects = unsafe { &mut *REGISTRY.objects.get() };
    objects.push(object);
    if objects.len() >= REGISTRY.prune_at.get() {
        objects.retain(Tracked::alive);
        REGISTRY.prune_at.set((objects.len() * 2).max(4096));
    }
}

/// Empties every container still alive and forgets them all: the end of
/// a run.
pub fn release_all() {
    // SAFETY: see `Registry`. The vector is moved out first: emptying a
    // container drops values, and dropping never registers anything.
    let objects = core::mem::take(unsafe { &mut *REGISTRY.objects.get() });
    for object in &objects {
        object.release();
    }
    drop(objects);
    REGISTRY.prune_at.set(4096);
}

/// What `shared` knows how to register.
pub trait Track: Sized {
    fn register(rc: &Rc<RefCell<Self>>);
}

impl Track for Vec<Value> {
    fn register(rc: &Rc<RefCell<Self>>) {
        track(Tracked::List(Rc::downgrade(rc)));
    }
}

impl Track for Dict {
    fn register(rc: &Rc<RefCell<Self>>) {
        track(Tracked::Dict(Rc::downgrade(rc)));
    }
}

impl Track for SetTable {
    fn register(rc: &Rc<RefCell<Self>>) {
        track(Tracked::Set(Rc::downgrade(rc)));
    }
}

impl Track for Option<Value> {
    fn register(rc: &Rc<RefCell<Self>>) {
        track(Tracked::Cell(Rc::downgrade(rc)));
    }
}

impl Track for Iter {
    fn register(rc: &Rc<RefCell<Self>>) {
        track(Tracked::Iter(Rc::downgrade(rc)));
    }
}

impl Track for Generator {
    fn register(rc: &Rc<RefCell<Self>>) {
        track(Tracked::Generator(Rc::downgrade(rc)));
    }
}

impl Track for File {
    fn register(_: &Rc<RefCell<Self>>) {}
}

/// A new mutable container, registered.
pub fn shared<T: Track>(value: T) -> Rc<RefCell<T>> {
    let rc = Rc::new(RefCell::new(value));
    T::register(&rc);
    rc
}

pub fn shared_dict(dict: Dict) -> Rc<RefCell<Dict>> {
    shared(dict)
}

pub fn new_cell(value: Option<Value>) -> Rc<RefCell<Option<Value>>> {
    shared(value)
}

/// A function, registered.
pub fn new_function(function: Function) -> Rc<Function> {
    let rc = Rc::new(function);
    track(Tracked::Function(Rc::downgrade(&rc)));
    rc
}

/// Unique ids for classes (and `id()` of everything else, from the
/// pointer).
pub static NEXT_CLASS_ID: SyncCounter = SyncCounter(Cell::new(1));

/// A counter for a single-threaded interpreter, usable in a static.
pub struct SyncCounter(pub Cell<usize>);

// SAFETY: the interpreter runs in one task at a time (`sdk::python`
// holds `BUSY`); the counter is never touched by two cores at once.
unsafe impl Sync for SyncCounter {}

impl SyncCounter {
    pub fn next(&self) -> usize {
        let value = self.0.get();
        self.0.set(value + 1);
        value
    }
}

impl Value {
    pub fn str(text: &str) -> Value {
        Value::Str(Rc::from(text))
    }

    pub fn int(value: i64) -> Value {
        Value::Int(Int::Small(value))
    }

    pub fn list(items: Vec<Value>) -> Value {
        Value::List(shared(items))
    }

    pub fn dict(dict: Dict) -> Value {
        Value::Dict(shared(dict))
    }

    pub fn set(table: SetTable) -> Value {
        Value::Set(shared(table))
    }

    pub fn iter(state: Iter) -> Value {
        Value::Iter(shared(state))
    }

    pub fn generator(generator: Generator) -> Value {
        Value::Generator(shared(generator))
    }

    pub fn tuple(items: Vec<Value>) -> Value {
        Value::Tuple(Rc::from(items))
    }

    pub fn is_none(&self) -> bool {
        matches!(self, Value::None)
    }

    /// Identity, as `is` sees it.
    pub fn is(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::None, Value::None) | (Value::NotImplemented, Value::NotImplemented) | (Value::Ellipsis, Value::Ellipsis) => true,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            // Small ints are shared in CPython (-5..256); identity of
            // equal ints is what programs that use `is` expect.
            (Value::Int(Int::Small(a)), Value::Int(Int::Small(b))) => a == b,
            (Value::Int(Int::Big(a)), Value::Int(Int::Big(b))) => Rc::ptr_eq(a, b),
            (Value::Float(a), Value::Float(b)) => a.0 == b.0,
            (Value::Str(a), Value::Str(b)) => Rc::ptr_eq(a, b) || (a.len() <= 1 && a == b),
            _ => match (self.pointer(), other.pointer()) {
                (Some(a), Some(b)) => a == b,
                _ => false,
            },
        }
    }

    /// The address behind a reference value, for `id` and `is`.
    pub fn pointer(&self) -> Option<usize> {
        Some(match self {
            Value::Str(s) => s.as_ptr() as usize,
            Value::Bytes(b) => b.as_ptr() as usize,
            Value::List(l) => Rc::as_ptr(l) as *const () as usize,
            Value::Tuple(t) => t.as_ptr() as usize,
            Value::Dict(d) => Rc::as_ptr(d) as *const () as usize,
            Value::Set(d) => Rc::as_ptr(d) as *const () as usize,
            Value::FrozenSet(d) => Rc::as_ptr(d) as *const () as usize,
            Value::View(v) => Rc::as_ptr(v) as *const () as usize,
            Value::Code(c) => Rc::as_ptr(c) as *const () as usize,
            Value::Cell(c) => Rc::as_ptr(c) as *const () as usize,
            Value::Range(r) => Rc::as_ptr(r) as *const () as usize,
            Value::Slice(s) => Rc::as_ptr(s) as *const () as usize,
            Value::Function(f) => Rc::as_ptr(f) as *const () as usize,
            Value::Builtin(b) => Rc::as_ptr(b) as *const () as usize,
            Value::Method(m) => Rc::as_ptr(m) as *const () as usize,
            Value::Class(c) => Rc::as_ptr(c) as *const () as usize,
            Value::Instance(i) => Rc::as_ptr(i) as *const () as usize,
            Value::Iter(i) => Rc::as_ptr(i) as *const () as usize,
            Value::Generator(g) => Rc::as_ptr(g) as *const () as usize,
            Value::Module(m) => Rc::as_ptr(m) as *const () as usize,
            Value::Property(p) => Rc::as_ptr(p) as *const () as usize,
            Value::StaticMethod(s) | Value::ClassMethod(s) => Rc::as_ptr(s) as *const () as usize,
            Value::Super(s) => Rc::as_ptr(s) as *const () as usize,
            Value::File(f) => Rc::as_ptr(f) as *const () as usize,
            _ => return None,
        })
    }
}

impl Class {
    pub fn new(name: &str, bases: Vec<Rc<Class>>, base: Base) -> Rc<Class> {
        let class = Rc::new(Class {
            name: Rc::from(name),
            qualname: RefCell::new(Rc::from(name)),
            bases,
            mro: RefCell::new(Vec::new()),
            dict: RefCell::new(Dict::new()),
            base,
            id: NEXT_CLASS_ID.next(),
            metaclass: RefCell::new(None),
            subclasses: RefCell::new(Vec::new()),
        });
        for base in class.bases.iter() {
            let mut list = base.subclasses.borrow_mut();
            list.retain(|w| w.strong_count() > 0);
            list.push(Rc::downgrade(&class));
        }
        track(Tracked::Class(Rc::downgrade(&class)));
        class
    }

    /// Is `self` `other` or derived from it?
    pub fn is_subclass(&self, other: &Rc<Class>) -> bool {
        self.id == other.id || self.mro.borrow().iter().any(|c| c.id == other.id)
    }

    /// An attribute found along the MRO (the class itself first).
    pub fn lookup(&self, name: &str) -> Option<Value> {
        for class in self.mro.borrow().iter() {
            if let Some(value) = class.dict.borrow().get_str(name) {
                return Some(value);
            }
        }
        None
    }
}

impl Instance {
    pub fn new(class: Rc<Class>) -> Rc<Instance> {
        let instance = Rc::new(Instance {
            class: RefCell::new(class),
            dict: shared(Dict::new()),
            inner: RefCell::new(Value::None),
            traceback: RefCell::new(Vec::new()),
            context: RefCell::new(Value::None),
            cause: RefCell::new(Value::None),
        });
        track(Tracked::Instance(Rc::downgrade(&instance)));
        instance
    }

    pub fn class(&self) -> Rc<Class> {
        self.class.borrow().clone()
    }
}

/// C3 linearization: the MRO of a class from its bases' MROs.
pub fn compute_mro(class: &Rc<Class>) -> Result<Vec<Rc<Class>>, String> {
    let mut sequences: Vec<Vec<Rc<Class>>> = class.bases.iter().map(|b| b.mro.borrow().clone()).collect();
    sequences.push(class.bases.clone());
    let mut out = alloc::vec![class.clone()];
    loop {
        sequences.retain(|s| !s.is_empty());
        if sequences.is_empty() {
            return Ok(out);
        }
        let mut chosen = None;
        for sequence in &sequences {
            let candidate = &sequence[0];
            let in_tail = sequences.iter().any(|s| s[1..].iter().any(|c| c.id == candidate.id));
            if !in_tail {
                chosen = Some(candidate.clone());
                break;
            }
        }
        let Some(next) = chosen else {
            return Err(String::from("Cannot create a consistent method resolution order (MRO)"));
        };
        for sequence in sequences.iter_mut() {
            if sequence[0].id == next.id {
                sequence.remove(0);
            }
        }
        out.push(next);
    }
}
