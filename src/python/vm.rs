//! The virtual machine: frames, the evaluation loop, calls, exceptions.
//!
//! ## Frames on the heap
//!
//! A Python call does not recurse in Rust: `Call` pushes a `Frame` on
//! `Vm::frames` and the loop goes on with it; `Return` pops it and hands
//! the value to the frame below. Python recursion is therefore bounded
//! by `sys.getrecursionlimit()` (frames are counted), not by the kernel
//! stack. Rust recursion happens only where native code calls back into
//! Python (`sorted(key=...)`, `__eq__` from a dict lookup, `next()` on a
//! generator): each such entry runs a nested loop over the frames it
//! pushed (`run_from`), and the stack headroom is checked there.
//!
//! ## Generators
//!
//! A generator owns a suspended frame. Resuming it pushes the frame back
//! and runs the loop until the frame yields (it is taken out again) or
//! returns -- `yield` is a frame leaving the stack with its state intact,
//! which a tree walker cannot do.
//!
//! ## Exceptions
//!
//! An exception is a value: an instance of a class deriving from
//! `BaseException`. Raising one unwinds the block stack of the frame (a
//! `SetupFinally` or `SetupWith` block sends it to its handler with the
//! exception on the stack), then the frames, recording a traceback line
//! for each. `Halt` is not an exception: fuel, memory or the kernel ended
//! the run, and nothing in the program can catch it.

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

use super::code::{Code, Op};
use super::dict::Dict;
use super::value::*;

/// What stops evaluation.
#[derive(Clone)]
pub enum PyErr {
    /// A Python exception (an instance of a BaseException subclass).
    Exc(Value),
    /// The run is over (fuel, memory): not catchable.
    Halt(String),
}

pub type PyResult<T> = Result<T, PyErr>;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BlockKind {
    /// `SetupFinally` and `SetupWith`: the exception goes to `handler`.
    Finally,
    /// An exception is being handled: leaving restores the previous one.
    ExceptHandler,
}

#[derive(Clone, Copy)]
pub struct FBlock {
    pub kind: BlockKind,
    pub handler: usize,
    /// Stack height to come back to.
    pub level: usize,
    /// Depth of `Vm::handling` to come back to.
    pub handling: usize,
}

/// What a frame does with its result.
#[derive(Clone)]
pub enum OnReturn {
    Value,
    /// `__init__` of this instance: the caller gets the instance.
    Instance(Value),
    /// A class body: the namespace becomes a class.
    ClassBody(Rc<RefCell<Dict>>, Rc<str>, Vec<Rc<Class>>, Vec<(Rc<str>, Value)>),
    /// A class body with a metaclass other than `type`: the metaclass is
    /// called with the name, the bases and the namespace.
    MetaClassBody(Rc<RefCell<Dict>>, Rc<str>, Value, Vec<(Rc<str>, Value)>, Rc<Class>),
}

pub struct Frame {
    pub code: Rc<Code>,
    pub ip: usize,
    pub stack: Vec<Value>,
    pub fast: Vec<Option<Value>>,
    pub cells: Vec<Rc<RefCell<Option<Value>>>>,
    pub blocks: Vec<FBlock>,
    pub globals: Rc<Module>,
    /// Module and class bodies: where names are stored.
    pub namespace: Option<Rc<RefCell<Dict>>>,
    pub on_return: OnReturn,
    /// The op being executed, for tracebacks.
    pub last: usize,
}

/// How a run of frames ended.
pub enum Exit {
    Return(Value),
    Yield(Value),
}

/// Default limit of nested Python calls (CPython's).
pub const RECURSION_LIMIT: usize = 1000;
/// Native re-entries into the loop allowed (sorted's key calling a
/// function that sorts...).
const NATIVE_DEPTH: usize = 200;

pub struct Vm {
    pub frames: Vec<Frame>,
    pub types: super::types::Types,
    pub builtins: Rc<Module>,
    /// `sys.modules`.
    pub modules: Vec<(Rc<str>, Value)>,
    /// Exceptions being handled, innermost last (`sys.exc_info`).
    pub handling: Vec<Value>,
    pub host: alloc::boxed::Box<dyn super::PyHost>,
    pub fuel: u64,
    pub recursion_limit: usize,
    native_depth: usize,
    pub headroom: &'static dyn Fn() -> usize,
    pub argv: Vec<String>,
    /// Directory of the main script: where `import x` looks.
    pub path: Vec<String>,
    /// Generators of `random`: the module's own first, then those of
    /// `random.Random` instances.
    pub random: Vec<super::modules::random::Mt>,
    /// Objects being printed: `[...]` for a list inside itself.
    pub repr_stack: Vec<usize>,
    pub exit_code: Option<i64>,
    /// Sources compiled so far, for the lines of tracebacks.
    pub sources: Vec<(Rc<str>, Rc<str>)>,
    /// Files opened: flushed at the end of the run if left open, as
    /// CPython does when it finalizes them.
    pub open_files: Vec<Rc<RefCell<File>>>,
    /// Patterns compiled by `re`.
    pub regexes: Vec<super::modules::re::Compiled>,
    /// The object an asynchronous generator's `yield` is wrapped with.
    pub async_marker: Option<Value>,
    /// `sys.modules` and `sys.path` once sys is imported: the program
    /// may change them, and imports follow.
    pub sys_modules: Option<Rc<RefCell<Dict>>>,
    pub sys_path: Option<Rc<RefCell<Vec<Value>>>>,
}

impl Vm {
    /// A machine with the built-in types and the builtins module.
    pub fn new(host: alloc::boxed::Box<dyn super::PyHost>, headroom: &'static dyn Fn() -> usize, fuel: u64) -> Vm {
        let seed = host.time_ns() ^ host.monotonic_ns().rotate_left(32);
        let placeholder = Rc::new(Module { name: Rc::from("builtins"), dict: shared_dict(Dict::new()), file: Rc::from("") });
        let mut vm = Vm {
            frames: Vec::new(),
            types: super::types::Types::new(),
            builtins: placeholder,
            modules: Vec::new(),
            handling: Vec::new(),
            host,
            fuel,
            recursion_limit: RECURSION_LIMIT,
            native_depth: 0,
            headroom,
            argv: Vec::new(),
            path: Vec::new(),
            random: alloc::vec![super::modules::random::Mt::from_u64(seed)],
            repr_stack: Vec::new(),
            exit_code: None,
            sources: Vec::new(),
            open_files: Vec::new(),
            regexes: Vec::new(),
            async_marker: None,
            sys_modules: None,
            sys_path: None,
        };
        super::types::install(&mut vm);
        let builtins = super::builtins::module(&mut vm);
        vm.builtins = builtins.clone();
        vm.modules.push((Rc::from("builtins"), Value::Module(builtins)));
        vm
    }

    /// Keeps a source for tracebacks.
    pub fn remember_source(&mut self, filename: &str, source: &str) {
        if !self.sources.iter().any(|(f, _)| &**f == filename) {
            self.sources.push((Rc::from(filename), Rc::from(source)));
        }
    }

    /// Line `line` (from 1) of a source compiled during the run.
    pub fn source_line(&self, filename: &str, line: u32) -> Option<String> {
        let (_, source) = self.sources.iter().find(|(f, _)| &**f == filename)?;
        source.lines().nth((line as usize).checked_sub(1)?).map(String::from)
    }

    /// The end of a run: every container emptied, so that no cycle of
    /// references outlives it.
    pub fn teardown(&mut self) {
        self.frames.clear();
        self.handling.clear();
        self.open_files.clear();
        self.sys_path = None;
        if let Some(d) = self.sys_modules.take() {
            // Modules the program put in sys.modules itself.
            let all = core::mem::take(&mut *d.borrow_mut());
            for entry in all.iter() {
                if let Value::Module(m) = &entry.value {
                    drop(core::mem::take(&mut *m.dict.borrow_mut()));
                }
            }
        }
        for (_, module) in core::mem::take(&mut self.modules) {
            if let Value::Module(m) = module {
                let dict = core::mem::take(&mut *m.dict.borrow_mut());
                drop(dict);
            }
        }
        let dict = core::mem::take(&mut *self.builtins.dict.borrow_mut());
        drop(dict);
        release_all();
    }

    // --------------------------------------------------------
    //  Errors
    // --------------------------------------------------------

    /// A new exception of `class` with `message`.
    pub fn error(&mut self, class: &Rc<Class>, message: &str) -> PyErr {
        let instance = Instance::new(class.clone());
        instance.dict.borrow_mut().set_str("args", Value::tuple(alloc::vec![Value::str(message)]));
        PyErr::Exc(Value::Instance(instance))
    }

    pub fn type_error<T>(&mut self, message: &str) -> PyResult<T> {
        let class = self.types.type_error.clone();
        Err(self.error(&class, message))
    }

    pub fn value_error<T>(&mut self, message: &str) -> PyResult<T> {
        let class = self.types.value_error.clone();
        Err(self.error(&class, message))
    }

    pub fn index_error<T>(&mut self, message: &str) -> PyResult<T> {
        let class = self.types.index_error.clone();
        Err(self.error(&class, message))
    }

    pub fn key_error<T>(&mut self, key: Value) -> PyResult<T> {
        let instance = Instance::new(self.types.key_error.clone());
        instance.dict.borrow_mut().set_str("args", Value::tuple(alloc::vec![key]));
        Err(PyErr::Exc(Value::Instance(instance)))
    }

    pub fn attribute_error<T>(&mut self, message: &str) -> PyResult<T> {
        let class = self.types.attribute_error.clone();
        Err(self.error(&class, message))
    }

    pub fn name_error<T>(&mut self, name: &str) -> PyResult<T> {
        let class = self.types.name_error.clone();
        let message = alloc::format!("name '{}' is not defined", name);
        let err = self.error(&class, &message);
        if let PyErr::Exc(Value::Instance(i)) = &err {
            i.dict.borrow_mut().set_str("name", Value::str(name));
        }
        Err(err)
    }

    pub fn zero_division<T>(&mut self, message: &str) -> PyResult<T> {
        let class = self.types.zero_division_error.clone();
        Err(self.error(&class, message))
    }

    pub fn overflow<T>(&mut self, message: &str) -> PyResult<T> {
        let class = self.types.overflow_error.clone();
        Err(self.error(&class, message))
    }

    pub fn stop_iteration(&mut self, value: Value) -> PyErr {
        let instance = Instance::new(self.types.stop_iteration.clone());
        let args = if value.is_none() { Vec::new() } else { alloc::vec![value.clone()] };
        instance.dict.borrow_mut().set_str("args", Value::tuple(args));
        instance.dict.borrow_mut().set_str("value", value);
        PyErr::Exc(Value::Instance(instance))
    }

    /// Is `err` an exception of `class` (or a subclass)?
    pub fn is_instance_of(&self, err: &PyErr, class: &Rc<Class>) -> bool {
        match err {
            PyErr::Exc(Value::Instance(i)) => i.class().is_subclass(class),
            _ => false,
        }
    }

    // --------------------------------------------------------
    //  Budget
    // --------------------------------------------------------

    fn charge(&mut self) -> PyResult<()> {
        if self.fuel == 0 {
            return Err(PyErr::Halt(String::from("budget d'execution epuise (boucle sans fin ?)")));
        }
        self.fuel -= 1;
        if self.fuel & 0xffff == 0 && !self.host.memory_ok() {
            let class = self.types.memory_error.clone();
            return Err(self.error(&class, "memoire du noyau insuffisante"));
        }
        Ok(())
    }

    /// Before a native re-entry into the loop.
    pub fn enter_native(&mut self) -> PyResult<()> {
        self.native_depth += 1;
        if self.native_depth > NATIVE_DEPTH || (self.headroom)() < 64 * 1024 {
            self.native_depth -= 1;
            let class = self.types.recursion_error.clone();
            return Err(self.error(&class, "maximum recursion depth exceeded"));
        }
        Ok(())
    }

    pub fn leave_native(&mut self) {
        self.native_depth -= 1;
    }

    // --------------------------------------------------------
    //  Frames
    // --------------------------------------------------------

    /// A frame for `function` called with `args`/`kwargs`, arguments
    /// bound to parameters as Python binds them.
    pub fn new_frame(&mut self, function: &Rc<Function>, args: &[Value], kwargs: &[(Rc<str>, Value)]) -> PyResult<Frame> {
        let code = function.code.clone();
        let mut fast: Vec<Option<Value>> = alloc::vec![None; code.varnames.len()];
        let argcount = code.argcount as usize;
        let kwonly = code.kwonly as usize;
        // CPython names the function by its qualified name: `A.m()`.
        let name = function.qualname.borrow().clone();
        // Positional.
        let given = args.len();
        for (i, value) in args.iter().take(argcount).enumerate() {
            fast[i] = Some(value.clone());
        }
        let mut slot = argcount + kwonly;
        if code.varargs {
            let extra: Vec<Value> = if given > argcount { args[argcount..].to_vec() } else { Vec::new() };
            fast[slot] = Some(Value::tuple(extra));
            slot += 1;
        } else if given > argcount {
            let message = alloc::format!(
                "{}() takes {} positional argument{} but {} {} given",
                name,
                argcount,
                if argcount == 1 { "" } else { "s" },
                given,
                if given == 1 { "was" } else { "were" }
            );
            return self.type_error(&message);
        }
        let mut extra_kw = Dict::new();
        let mut positional_only: Vec<Rc<str>> = Vec::new();
        for (key, value) in kwargs {
            let position = code.varnames[..argcount + kwonly].iter().position(|v| v == key);
            match position {
                Some(p) if p >= code.posonly as usize => {
                    if fast[p].is_some() {
                        let message = alloc::format!("{}() got multiple values for argument '{}'", name, key);
                        return self.type_error(&message);
                    }
                    fast[p] = Some(value.clone());
                }
                _ => {
                    if code.varkw {
                        extra_kw.set_str(key, value.clone());
                    } else if position.is_some() {
                        positional_only.push(key.clone());
                    } else {
                        let message = alloc::format!("{}() got an unexpected keyword argument '{}'", name, key);
                        return self.type_error(&message);
                    }
                }
            }
        }
        if !positional_only.is_empty() {
            let names: Vec<&str> = positional_only.iter().map(|n| &**n).collect();
            let message = alloc::format!("{}() got some positional-only arguments passed as keyword arguments: '{}'", name, names.join(", "));
            return self.type_error(&message);
        }
        if code.varkw {
            fast[slot] = Some(Value::dict(extra_kw));
        }
        // Defaults.
        let defaults = &function.defaults;
        let first_default = argcount - defaults.len();
        let mut missing = Vec::new();
        for i in 0..argcount {
            if fast[i].is_none() {
                if i >= first_default {
                    fast[i] = Some(defaults[i - first_default].clone());
                } else {
                    missing.push(code.varnames[i].clone());
                }
            }
        }
        if !missing.is_empty() {
            let names: Vec<String> = missing.iter().map(|m| alloc::format!("'{}'", m)).collect();
            let joined = match names.len() {
                1 => names[0].clone(),
                2 => alloc::format!("{} and {}", names[0], names[1]),
                _ => alloc::format!("{}, and {}", names[..names.len() - 1].join(", "), names[names.len() - 1]),
            };
            let message = alloc::format!(
                "{}() missing {} required positional argument{}: {}",
                name,
                missing.len(),
                if missing.len() == 1 { "" } else { "s" },
                joined
            );
            return self.type_error(&message);
        }
        for i in argcount..argcount + kwonly {
            if fast[i].is_none() {
                let key = &code.varnames[i];
                match function.kwdefaults.iter().find(|(k, _)| k == key) {
                    Some((_, v)) => fast[i] = Some(v.clone()),
                    None => {
                        let message = alloc::format!("{}() missing 1 required keyword-only argument: '{}'", name, key);
                        return self.type_error(&message);
                    }
                }
            }
        }
        // Cells: the code's own, filled from parameters, then the free
        // ones of the closure.
        let mut cells: Vec<Rc<RefCell<Option<Value>>>> = code.cellvars.iter().map(|_| new_cell(None)).collect();
        for (slot, cell) in &code.cell_params {
            *cells[*cell as usize].borrow_mut() = fast[*slot as usize].take();
        }
        cells.extend(function.closure.iter().cloned());
        Ok(Frame {
            code,
            ip: 0,
            stack: Vec::with_capacity(8),
            fast,
            cells,
            blocks: Vec::new(),
            globals: function.globals.clone(),
            namespace: None,
            on_return: OnReturn::Value,
            last: 0,
        })
    }

    /// A frame for a module's code.
    pub fn module_frame(&self, code: Rc<Code>, module: Rc<Module>) -> Frame {
        let namespace = module.dict.clone();
        Frame {
            fast: alloc::vec![None; code.varnames.len()],
            cells: code.cellvars.iter().map(|_| new_cell(None)).collect(),
            code,
            ip: 0,
            stack: Vec::new(),
            blocks: Vec::new(),
            globals: module,
            namespace: Some(namespace),
            on_return: OnReturn::Value,
            last: 0,
        }
    }

    // --------------------------------------------------------
    //  Calls
    // --------------------------------------------------------

    /// Calls anything callable, from native code: runs to completion.
    pub fn call(&mut self, callable: &Value, args: &[Value], kwargs: &[(Rc<str>, Value)]) -> PyResult<Value> {
        match self.prepare_call(callable, args, kwargs)? {
            Prepared::Done(value) => Ok(value),
            Prepared::Frame(frame) => {
                self.enter_native()?;
                let base = self.frames.len();
                self.push_frame(frame)?;
                let result = self.run_from(base);
                self.leave_native();
                match result? {
                    Exit::Return(v) => Ok(v),
                    Exit::Yield(_) => self.type_error("yield inattendu"),
                }
            }
        }
    }

    pub fn push_frame(&mut self, frame: Frame) -> PyResult<()> {
        // The library's frames (`functools.lru_cache`'s wrapper...) stand
        // for CPython's C functions: they do not count.
        if self.frames.len() >= self.recursion_limit {
            let visible = self.frames.iter().filter(|f| !is_library(&f.code)).count();
            if visible >= self.recursion_limit || self.frames.len() >= 4 * self.recursion_limit {
                let class = self.types.recursion_error.clone();
                return Err(self.error(&class, "maximum recursion depth exceeded"));
            }
        }
        self.frames.push(frame);
        Ok(())
    }

    /// Either the result, or a frame to run (Python functions).
    pub fn prepare_call(&mut self, callable: &Value, args: &[Value], kwargs: &[(Rc<str>, Value)]) -> PyResult<Prepared> {
        match callable {
            Value::Function(function) => {
                let frame = self.new_frame(function, args, kwargs)?;
                if function.code.generator || function.code.coroutine {
                    let kind = match (function.code.coroutine, function.code.generator) {
                        (false, _) => 0,
                        (true, false) => 1,
                        (true, true) => 2,
                    };
                    let generator = Generator { frame: Some(frame), running: false, name: function.name.borrow().clone(), kind };
                    return Ok(Prepared::Done(Value::generator(generator)));
                }
                Ok(Prepared::Frame(frame))
            }
            Value::Builtin(builtin) => {
                let call = builtin.call;
                let result = if builtin.data.is_none() {
                    call(self, args, kwargs)?
                } else {
                    // Data-carrying natives see their data first.
                    let mut full = Vec::with_capacity(args.len() + 1);
                    full.push(builtin.data.clone());
                    full.extend_from_slice(args);
                    call(self, &full, kwargs)?
                };
                Ok(Prepared::Done(result))
            }
            Value::Method(bound) => {
                let (receiver, function) = (&bound.0, &bound.1);
                let mut full = Vec::with_capacity(args.len() + 1);
                full.push(receiver.clone());
                full.extend_from_slice(args);
                let function = function.clone();
                self.prepare_call(&function, &full, kwargs)
            }
            Value::Class(class) => {
                // A metaclass with its own `__call__` decides.
                let metaclass = class.metaclass.borrow().clone();
                if let Some(meta) = metaclass {
                    if let Some(call @ Value::Function(_)) = meta.lookup("__call__") {
                        let mut full = alloc::vec![callable.clone()];
                        full.extend_from_slice(args);
                        return self.prepare_call(&call, &full, kwargs);
                    }
                }
                self.instantiate(class, args, kwargs)
            }
            Value::Instance(instance) => {
                let class = instance.class();
                match class.lookup("__call__") {
                    Some(method) => {
                        let mut full = alloc::vec![callable.clone()];
                        full.extend_from_slice(args);
                        self.prepare_call(&method, &full, kwargs)
                    }
                    None => {
                        let message = alloc::format!("'{}' object is not callable", class.name);
                        self.type_error(&message)
                    }
                }
            }
            Value::StaticMethod(inner) => {
                let inner = (**inner).clone();
                self.prepare_call(&inner, args, kwargs)
            }
            other => {
                let name = self.type_name(other);
                let message = alloc::format!("'{}' object is not callable", name);
                self.type_error(&message)
            }
        }
    }

    /// `Class(...)`: `__new__`, then `__init__` on what it made, as
    /// CPython's `type.__call__`.
    fn instantiate(&mut self, class: &Rc<Class>, args: &[Value], kwargs: &[(Rc<str>, Value)]) -> PyResult<Prepared> {
        // The built-in types construct natively (`int("5")`, `list(x)`).
        if class.base != Base::Exception && super::types::is_builtin(self, class) {
            if let Some(constructor) = super::types::constructor(self, class) {
                return Ok(Prepared::Done(constructor(self, class, args, kwargs)?));
            }
        }
        let user_new = class.lookup("__new__").filter(|n| matches!(n, Value::Function(_) | Value::StaticMethod(_)));
        let made = match user_new {
            Some(new) => {
                let mut full = alloc::vec![Value::Class(class.clone())];
                full.extend_from_slice(args);
                let made = self.call(&new, &full, kwargs)?;
                // `__init__` runs only on an instance of the class.
                let of_class = match &made {
                    Value::Instance(i) => i.class().is_subclass(class),
                    Value::Class(_) => self.class_of(&made).is_subclass(class),
                    _ => false,
                };
                if !of_class {
                    return Ok(Prepared::Done(made));
                }
                made
            }
            // A metaclass: its instances are classes.
            None if class.is_subclass(&self.types.type_) && class.id != self.types.type_.id => {
                super::types::object::new_type(self, class, args, kwargs)?
            }
            None => super::types::object::allocate(self, class, args, kwargs)?,
        };
        self.init(made, args, kwargs)
    }

    /// `type.__call__`: makes an instance without asking the metaclass
    /// again.
    pub fn construct(&mut self, class: &Rc<Class>, args: &[Value], kwargs: &[(Rc<str>, Value)]) -> PyResult<Value> {
        match self.instantiate(class, args, kwargs)? {
            Prepared::Done(value) => Ok(value),
            Prepared::Frame(frame) => {
                self.enter_native()?;
                let base = self.frames.len();
                self.push_frame(frame)?;
                let result = self.run_from(base);
                self.leave_native();
                match result? {
                    Exit::Return(v) => Ok(v),
                    Exit::Yield(_) => self.type_error("yield inattendu"),
                }
            }
        }
    }

    fn init(&mut self, value: Value, args: &[Value], kwargs: &[(Rc<str>, Value)]) -> PyResult<Prepared> {
        let class = match &value {
            Value::Instance(i) => i.class(),
            // A class made by a metaclass: the metaclass's `__init__`.
            Value::Class(c) => {
                let meta = c.metaclass.borrow().clone();
                let init = meta.and_then(|m| m.lookup("__init__")).filter(|i| matches!(i, Value::Function(_)));
                if let Some(init) = init {
                    let mut full = alloc::vec![value.clone()];
                    full.extend_from_slice(args);
                    self.call(&init, &full, kwargs)?;
                }
                return Ok(Prepared::Done(value));
            }
            _ => return Ok(Prepared::Done(value)),
        };
        match class.lookup("__init__") {
            Some(Value::Function(init)) => {
                let mut full = alloc::vec![value.clone()];
                full.extend_from_slice(args);
                let mut frame = self.new_frame(&init, &full, kwargs)?;
                frame.on_return = OnReturn::Instance(value);
                Ok(Prepared::Frame(frame))
            }
            Some(other) => {
                let mut full = alloc::vec![value.clone()];
                full.extend_from_slice(args);
                let result = self.call(&other, &full, kwargs)?;
                if !result.is_none() {
                    return self.type_error("__init__() should return None");
                }
                Ok(Prepared::Done(value))
            }
            None => Ok(Prepared::Done(value)),
        }
    }

    // --------------------------------------------------------
    //  Generators
    // --------------------------------------------------------

    /// Resumes a generator with `sent` (or throws `thrown` into it).
    /// `Ok(Some(v))`: it yielded v; `Ok(None)` with the return value in
    /// `StopIteration` semantics is given as Err(StopIteration).
    pub fn resume(&mut self, generator: &Rc<RefCell<Generator>>, sent: Value, thrown: Option<PyErr>) -> PyResult<Value> {
        let frame = {
            let mut g = generator.borrow_mut();
            if g.running {
                drop(g);
                return self.value_error("generator already executing");
            }
            match g.frame.take() {
                Some(frame) => {
                    g.running = true;
                    frame
                }
                None => {
                    drop(g);
                    return match thrown {
                        Some(err) => Err(err),
                        None => Err(self.stop_iteration(Value::None)),
                    };
                }
            }
        };
        let started = frame.ip > 0;
        if !started && !sent.is_none() && thrown.is_none() {
            generator.borrow_mut().frame = Some(frame);
            generator.borrow_mut().running = false;
            return self.type_error("can't send non-None value to a just-started generator");
        }
        // A generator suspended in `yield from` (or `await`) hands a
        // thrown exception to what it delegates to, as CPython does: the
        // cancellation of an asyncio task reaches the innermost await.
        let mut frame = frame;
        let mut thrown = thrown;
        let mut push_sent = started;
        if started {
            if let Some(PyErr::Exc(exc)) = &thrown {
                if matches!(frame.code.ops.get(frame.ip), Some(Op::YieldFrom)) {
                    let exc = exc.clone();
                    let receiver = frame.stack.last().cloned().unwrap_or(Value::None);
                    let exit_class = self.types.generator_exit.clone();
                    let is_exit = matches!(&exc, Value::Instance(i) if i.class().is_subclass(&exit_class));
                    if is_exit {
                        if let Ok(close) = self.get_attr(&receiver, "close") {
                            if let Err(e) = self.call(&close, &[], &[]) {
                                thrown = Some(e);
                            }
                        }
                    } else {
                        let outcome = match &receiver {
                            Value::Generator(g) => Some(self.resume(g, Value::None, Some(PyErr::Exc(exc.clone())))),
                            other => match self.get_attr(other, "throw") {
                                Ok(method) => Some(self.call(&method, &[exc.clone()], &[])),
                                Err(_) => None,
                            },
                        };
                        match outcome {
                            Some(Ok(value)) => {
                                // The delegate yielded: still suspended here.
                                generator.borrow_mut().frame = Some(frame);
                                generator.borrow_mut().running = false;
                                return Ok(value);
                            }
                            Some(Err(e)) if self.is_instance_of(&e, &self.types.stop_iteration.clone()) => {
                                let value = match &e {
                                    PyErr::Exc(Value::Instance(i)) => i.dict.borrow().get_str("value").unwrap_or(Value::None),
                                    _ => Value::None,
                                };
                                frame.stack.pop();
                                frame.stack.push(value);
                                frame.ip += 1;
                                thrown = None;
                                push_sent = false;
                            }
                            Some(Err(e)) => thrown = Some(e),
                            None => {}
                        }
                    }
                }
            }
        }
        self.enter_native()?;
        let base = self.frames.len();
        if let Err(e) = self.push_frame(frame) {
            self.leave_native();
            generator.borrow_mut().running = false;
            return Err(e);
        }
        let result = match thrown {
            Some(PyErr::Exc(exc)) => {
                // Raised at the yield the generator is suspended on.
                match self.raise_in_top(exc, base) {
                    Ok(()) => self.run_from(base),
                    Err(e) => Err(e),
                }
            }
            Some(halt) => Err(halt),
            None => {
                if push_sent {
                    if let Some(top) = self.frames.last_mut() {
                        top.stack.push(sent);
                    }
                }
                self.run_from(base)
            }
        };
        self.leave_native();
        generator.borrow_mut().running = false;
        match result {
            Ok(Exit::Yield(value)) => {
                let frame = self.frames.pop();
                generator.borrow_mut().frame = frame;
                Ok(value)
            }
            Ok(Exit::Return(value)) => Err(self.stop_iteration(value)),
            Err(e) => {
                // A StopIteration escaping a generator is a RuntimeError.
                if self.is_instance_of(&e, &self.types.stop_iteration.clone()) {
                    let class = self.types.runtime_error.clone();
                    return Err(self.error(&class, "generator raised StopIteration"));
                }
                Err(e)
            }
        }
    }

    // --------------------------------------------------------
    //  The loop
    // --------------------------------------------------------

    /// Runs the frames from `base` up until the frame at `base` returns
    /// or yields.
    pub fn run_from(&mut self, base: usize) -> PyResult<Exit> {
        loop {
            match self.step(base) {
                Ok(Some(exit)) => return Ok(exit),
                Ok(None) => {}
                Err(PyErr::Exc(exc)) => {
                    self.raise_in_top(exc, base)?;
                }
                Err(halt) => {
                    self.frames.truncate(base);
                    return Err(halt);
                }
            }
        }
    }

    /// Sends `exc` into the top frame: to its innermost handler, or out
    /// of it, frame after frame, down to `base`.
    fn raise_in_top(&mut self, exc: Value, base: usize) -> PyResult<()> {
        let mut first = true;
        loop {
            let Some(frame) = self.frames.last_mut() else {
                return Err(PyErr::Exc(exc));
            };
            // A traceback entry for each frame the exception reaches, the
            // one that catches it included (as CPython's tb chain), but
            // not for a bare `raise` or the end of a `finally`.
            let reraise = first && matches!(frame.code.ops.get(frame.last), Some(Op::Reraise) | Some(Op::Raise(0)));
            first = false;
            if !reraise && !is_library(&frame.code) {
                if let Value::Instance(i) = &exc {
                    let line = frame.code.lines.get(frame.last).copied().unwrap_or(frame.code.first_line);
                    let pos = frame.code.positions.get(frame.last).copied().unwrap_or_default();
                    i.traceback.borrow_mut().push((frame.code.filename.clone(), line, frame.code.name.clone(), pos));
                }
            }
            while let Some(block) = frame.blocks.pop() {
                match block.kind {
                    BlockKind::ExceptHandler => {
                        self.handling.truncate(block.handling);
                    }
                    BlockKind::Finally => {
                        frame.stack.truncate(block.level);
                        frame.blocks.push(FBlock { kind: BlockKind::ExceptHandler, handler: 0, level: block.level, handling: self.handling.len() });
                        // The exception being handled becomes its context.
                        if let Value::Instance(i) = &exc {
                            if i.context.borrow().is_none() {
                                if let Some(previous) = self.handling.last() {
                                    if !previous.is(&exc) {
                                        *i.context.borrow_mut() = previous.clone();
                                    }
                                }
                            }
                        }
                        self.handling.push(exc.clone());
                        frame.stack.push(exc);
                        frame.ip = block.handler;
                        return Ok(());
                    }
                }
            }
            // No handler here: the frame goes.
            if self.frames.len() - 1 == base {
                self.frames.pop();
                return Err(PyErr::Exc(exc));
            }
            self.frames.pop();
        }
    }

    fn top(&mut self) -> &mut Frame {
        let last = self.frames.len() - 1;
        &mut self.frames[last]
    }

    fn pop(&mut self) -> Value {
        self.top().stack.pop().unwrap_or(Value::None)
    }

    fn push(&mut self, value: Value) {
        self.top().stack.push(value);
    }

    fn peek(&mut self, depth: usize) -> Value {
        let stack = &self.top().stack;
        stack.get(stack.len().wrapping_sub(depth)).cloned().unwrap_or(Value::None)
    }

    fn name_at(&mut self, index: u32) -> Rc<str> {
        self.top().code.names[index as usize].clone()
    }

    /// The return of the top frame: to the frame below, or out of the
    /// loop if it was the base.
    fn finish_frame(&mut self, value: Value, base: usize) -> PyResult<Option<Exit>> {
        let frame = self.frames.pop().expect("frame");
        let value = match frame.on_return {
            OnReturn::Value => value,
            OnReturn::Instance(instance) => {
                if !value.is_none() {
                    return self.type_error("__init__() should return None");
                }
                instance
            }
            OnReturn::ClassBody(namespace, name, bases, keywords) => super::types::make_class(self, name, bases, namespace, value, keywords)?,
            OnReturn::MetaClassBody(namespace, name, bases, keywords, meta) => {
                let made = self.call(&Value::Class(meta), &[Value::Str(name), bases, Value::Dict(namespace)], &keywords)?;
                if let (Value::Cell(cell), Value::Class(_)) = (&value, &made) {
                    *cell.borrow_mut() = Some(made.clone());
                }
                made
            }
        };
        if self.frames.len() == base {
            // The base frame is gone: give its value out, and leave a
            // place holder the caller pops.
            return Ok(Some(Exit::Return(value)));
        }
        self.push(value);
        Ok(None)
    }

    /// Runs `prepared` for the current frame: a frame is pushed, a value
    /// is pushed.
    fn continue_with(&mut self, prepared: Prepared) -> PyResult<()> {
        match prepared {
            Prepared::Done(value) => {
                self.push(value);
                Ok(())
            }
            Prepared::Frame(frame) => self.push_frame(frame),
        }
    }

    /// One instruction of the top frame.
    fn step(&mut self, base: usize) -> PyResult<Option<Exit>> {
        self.charge()?;
        let (op, ip) = {
            let frame = self.top();
            let ip = frame.ip;
            frame.last = ip;
            frame.ip = ip + 1;
            (frame.code.ops[ip], ip)
        };
        let _ = ip;
        match op {
            Op::Nop => {}
            Op::Pop => {
                self.pop();
            }
            Op::Dup => {
                let v = self.peek(1);
                self.push(v);
            }
            Op::DupTwo => {
                let a = self.peek(2);
                let b = self.peek(1);
                self.push(a);
                self.push(b);
            }
            Op::Rot2 => {
                let stack = &mut self.top().stack;
                let n = stack.len();
                stack.swap(n - 1, n - 2);
            }
            Op::Rot3 => {
                let stack = &mut self.top().stack;
                let top = stack.pop().unwrap_or(Value::None);
                let n = stack.len();
                stack.insert(n - 2, top);
            }
            Op::Rot4 => {
                let stack = &mut self.top().stack;
                let top = stack.pop().unwrap_or(Value::None);
                let n = stack.len();
                stack.insert(n - 3, top);
            }
            Op::LoadConst(i) => {
                let v = self.top().code.consts[i as usize].clone();
                self.push(v);
            }
            Op::LoadFast(i) => {
                let v = self.top().fast[i as usize].clone();
                match v {
                    Some(v) => self.push(v),
                    None => {
                        let name = self.top().code.varnames[i as usize].clone();
                        let class = self.types.unbound_local_error.clone();
                        let message = alloc::format!("cannot access local variable '{}' where it is not associated with a value", name);
                        return Err(self.error(&class, &message));
                    }
                }
            }
            Op::StoreFast(i) => {
                let v = self.pop();
                self.top().fast[i as usize] = Some(v);
            }
            Op::DeleteFast(i) => {
                if self.top().fast[i as usize].take().is_none() {
                    let name = self.top().code.varnames[i as usize].clone();
                    return self.name_error(&name);
                }
            }
            Op::LoadClosure(i) => {
                // The cell itself, for MakeFunction.
                let cell = self.top().cells[i as usize].clone();
                self.push(Value::Cell(cell));
            }
            Op::LoadDeref(i) | Op::LoadClassDeref(i) => {
                if let Op::LoadClassDeref(_) = op {
                    let name = self.cell_name(i);
                    if let Some(ns) = self.top().namespace.clone() {
                        if let Some(v) = ns.borrow().get_str(&name) {
                            self.push(v);
                            return Ok(None);
                        }
                    }
                }
                let cell = self.top().cells[i as usize].clone();
                let value = cell.borrow().clone();
                match value {
                    Some(v) => self.push(v),
                    None => {
                        let name = self.cell_name(i);
                        let class = self.types.name_error.clone();
                        let message = alloc::format!("cannot access free variable '{}' where it is not associated with a value in enclosing scope", name);
                        return Err(self.error(&class, &message));
                    }
                }
            }
            Op::StoreDeref(i) => {
                let v = self.pop();
                let cell = self.top().cells[i as usize].clone();
                *cell.borrow_mut() = Some(v);
            }
            Op::DeleteDeref(i) => {
                let cell = self.top().cells[i as usize].clone();
                *cell.borrow_mut() = None;
            }
            Op::LoadGlobal(i) => {
                let name = self.name_at(i);
                let value = self.load_global(&name)?;
                self.push(value);
            }
            Op::StoreGlobal(i) => {
                let name = self.name_at(i);
                let v = self.pop();
                let globals = self.top().globals.clone();
                globals.dict.borrow_mut().set_str(&name, v);
            }
            Op::DeleteGlobal(i) => {
                let name = self.name_at(i);
                let globals = self.top().globals.clone();
                if globals.dict.borrow_mut().remove_str(&name).is_none() {
                    return self.name_error(&name);
                }
            }
            Op::LoadName(i) => {
                let name = self.name_at(i);
                if let Some(ns) = self.top().namespace.clone() {
                    if let Some(v) = ns.borrow().get_str(&name) {
                        self.push(v);
                        return Ok(None);
                    }
                }
                let value = self.load_global(&name)?;
                self.push(value);
            }
            Op::StoreName(i) => {
                let name = self.name_at(i);
                let v = self.pop();
                let ns = self.top().namespace.clone();
                match ns {
                    Some(ns) => ns.borrow_mut().set_str(&name, v),
                    None => {
                        let globals = self.top().globals.clone();
                        globals.dict.borrow_mut().set_str(&name, v);
                    }
                }
            }
            Op::DeleteName(i) => {
                let name = self.name_at(i);
                let ns = self.top().namespace.clone();
                let removed = match ns {
                    Some(ns) => ns.borrow_mut().remove_str(&name),
                    None => None,
                };
                if removed.is_none() {
                    return self.name_error(&name);
                }
            }
            Op::LoadAttr(i) => {
                let name = self.name_at(i);
                let object = self.pop();
                let value = self.get_attr(&object, &name)?;
                self.push(value);
            }
            Op::StoreAttr(i) => {
                let name = self.name_at(i);
                let object = self.pop();
                let value = self.pop();
                self.set_attr(&object, &name, value)?;
            }
            Op::DeleteAttr(i) => {
                let name = self.name_at(i);
                let object = self.pop();
                self.del_attr(&object, &name)?;
            }
            Op::Subscr => {
                let index = self.pop();
                let object = self.pop();
                let value = self.get_item(&object, &index)?;
                self.push(value);
            }
            Op::StoreSubscr => {
                let index = self.pop();
                let object = self.pop();
                let value = self.pop();
                self.set_item(&object, &index, value)?;
            }
            Op::DeleteSubscr => {
                let index = self.pop();
                let object = self.pop();
                self.del_item(&object, &index)?;
            }
            Op::Binary(op) => {
                let b = self.pop();
                let a = self.pop();
                let r = self.binary(op, &a, &b, false)?;
                self.push(r);
            }
            Op::Inplace(op) => {
                let b = self.pop();
                let a = self.pop();
                let r = self.binary(op, &a, &b, true)?;
                self.push(r);
            }
            Op::Unary(op) => {
                let a = self.pop();
                let r = self.unary(op, &a)?;
                self.push(r);
            }
            Op::Compare(op) => {
                let b = self.pop();
                let a = self.pop();
                let r = self.compare(op, &a, &b)?;
                self.push(r);
            }
            Op::Is(negate) => {
                let b = self.pop();
                let a = self.pop();
                self.push(Value::Bool(a.is(&b) != negate));
            }
            Op::Contains(negate) => {
                let container = self.pop();
                let item = self.pop();
                let found = self.contains(&container, &item)?;
                self.push(Value::Bool(found != negate));
            }
            Op::BuildTuple(n) => {
                let items = self.pop_n(n as usize);
                self.push(Value::tuple(items));
            }
            Op::BuildList(n) => {
                let items = self.pop_n(n as usize);
                self.push(Value::list(items));
            }
            Op::BuildSet(n) => {
                let items = self.pop_n(n as usize);
                let set = self.new_set(&items)?;
                self.push(set);
            }
            Op::BuildMap(n) => {
                let items = self.pop_n(2 * n as usize);
                let mut dict = Dict::new();
                for pair in items.chunks(2) {
                    self.dict_set(&mut dict, pair[0].clone(), pair[1].clone())?;
                }
                self.push(Value::dict(dict));
            }
            Op::BuildSlice(n) => {
                let step = if n == 3 { self.pop() } else { Value::None };
                let stop = self.pop();
                let start = self.pop();
                self.push(Value::Slice(Rc::new([start, stop, step])));
            }
            Op::BuildString(n) => {
                let parts = self.pop_n(n as usize);
                let mut out = String::new();
                for p in parts {
                    if let Value::Str(s) = p {
                        out.push_str(&s);
                    }
                }
                self.push(Value::str(&out));
            }
            Op::ListAppend(depth) => {
                let v = self.pop();
                let target = self.peek(depth as usize);
                if let Value::List(l) = target {
                    l.borrow_mut().push(v);
                }
            }
            Op::SetAdd(depth) => {
                let v = self.pop();
                let target = self.peek(depth as usize);
                self.set_add(&target, v)?;
            }
            Op::MapAdd(depth) => {
                let value = self.pop();
                let key = self.pop();
                let target = self.peek(depth as usize);
                if let Value::Dict(d) = target {
                    let mut dict = core::mem::take(&mut *d.borrow_mut());
                    let r = self.dict_set(&mut dict, key, value);
                    *d.borrow_mut() = dict;
                    r?;
                }
            }
            Op::ListExtend(depth) => {
                let iterable = self.pop();
                let items = self.collect(&iterable)?;
                if let Value::List(l) = self.peek(depth as usize) {
                    l.borrow_mut().extend(items);
                }
            }
            Op::SetUpdate(depth) => {
                let iterable = self.pop();
                if let Value::Set(target) = self.peek(depth as usize) {
                    let mut table = target.borrow().clone();
                    super::types::set_update_from(self, &mut table, &iterable)?;
                    *target.borrow_mut() = table;
                }
            }
            Op::BuildConstSet(n) => {
                // CPython builds a frozenset constant, then merges it into
                // a new set: the table of a merge, not of n additions.
                let items = self.pop_n(n as usize);
                let mut frozen = super::dict::SetTable::new();
                for item in items {
                    let hash = self.hash(&item)?;
                    super::types::table_add(self, &mut frozen, hash, item)?;
                }
                let table = super::types::set_copy(self, &frozen)?;
                self.push(Value::set(table));
            }
            Op::DictUpdate(depth) | Op::DictMerge(depth) => {
                let other = self.pop();
                let target = self.peek(depth as usize);
                let pairs = self.mapping_items(&other)?;
                if let Value::Dict(d) = target {
                    for (k, v) in pairs {
                        let mut dict = core::mem::take(&mut *d.borrow_mut());
                        let previous = self.dict_set(&mut dict, k.clone(), v);
                        *d.borrow_mut() = dict;
                        if matches!(op, Op::DictMerge(_)) {
                            if let Ok(Some(_)) = previous {
                                let key = self.str_of(&k)?;
                                let message = alloc::format!("got multiple values for keyword argument '{}'", key);
                                return self.type_error(&message);
                            }
                        }
                        previous?;
                    }
                }
            }
            Op::ListToTuple => {
                let v = self.pop();
                if let Value::List(l) = v {
                    let items = l.borrow().clone();
                    self.push(Value::tuple(items));
                }
            }
            Op::FormatValue(conversion, has_spec) => {
                let spec = if has_spec { self.pop() } else { Value::str("") };
                let value = self.pop();
                let value = match conversion {
                    b's' => Value::str(&self.str_of(&value)?),
                    b'r' => Value::str(&self.repr(&value)?),
                    b'a' => Value::str(&super::format::ascii(&self.repr(&value)?)),
                    _ => value,
                };
                let spec = match spec {
                    Value::Str(s) => s,
                    _ => Rc::from(""),
                };
                let text = self.format(&value, &spec)?;
                self.push(Value::str(&text));
            }
            Op::UnpackSequence(n) => {
                let v = self.pop();
                let items = self.collect(&v)?;
                if items.len() != n as usize {
                    let message = if items.len() > n as usize {
                        alloc::format!("too many values to unpack (expected {})", n)
                    } else {
                        alloc::format!("not enough values to unpack (expected {}, got {})", n, items.len())
                    };
                    return self.value_error(&message);
                }
                for item in items.into_iter().rev() {
                    self.push(item);
                }
            }
            Op::UnpackEx(before, after) => {
                let v = self.pop();
                let items = self.collect(&v)?;
                let (before, after) = (before as usize, after as usize);
                if items.len() < before + after {
                    let message = alloc::format!("not enough values to unpack (expected at least {}, got {})", before + after, items.len());
                    return self.value_error(&message);
                }
                let middle = items[before..items.len() - after].to_vec();
                for item in items[items.len() - after..].iter().rev() {
                    self.push(item.clone());
                }
                self.push(Value::list(middle));
                for item in items[..before].iter().rev() {
                    self.push(item.clone());
                }
            }
            Op::Jump(t) => self.top().ip = t as usize,
            Op::PopJumpIfFalse(t) => {
                let v = self.pop();
                if !self.truthy(&v)? {
                    self.top().ip = t as usize;
                }
            }
            Op::PopJumpIfTrue(t) => {
                let v = self.pop();
                if self.truthy(&v)? {
                    self.top().ip = t as usize;
                }
            }
            Op::JumpIfFalseOrPop(t) => {
                let v = self.peek(1);
                if !self.truthy(&v)? {
                    self.top().ip = t as usize;
                } else {
                    self.pop();
                }
            }
            Op::JumpIfTrueOrPop(t) => {
                let v = self.peek(1);
                if self.truthy(&v)? {
                    self.top().ip = t as usize;
                } else {
                    self.pop();
                }
            }
            Op::GetIter => {
                let v = self.pop();
                let it = self.get_iter(&v)?;
                self.push(it);
            }
            Op::ForIter(t) => {
                let iterator = self.peek(1);
                match self.next(&iterator)? {
                    Some(v) => self.push(v),
                    None => {
                        self.pop();
                        self.top().ip = t as usize;
                    }
                }
            }
            Op::Call(n) => {
                let args = self.pop_n(n as usize);
                let function = self.pop();
                let prepared = self.prepare_call(&function, &args, &[])?;
                self.continue_with(prepared)?;
            }
            Op::CallKw(n) => {
                let names = self.pop();
                let mut args = self.pop_n(n as usize);
                let function = self.pop();
                let names: Vec<Rc<str>> = match names {
                    Value::Tuple(t) => t.iter().filter_map(|n| if let Value::Str(s) = n { Some(s.clone()) } else { None }).collect(),
                    _ => Vec::new(),
                };
                let split = args.len() - names.len();
                let kwargs: Vec<(Rc<str>, Value)> = names.into_iter().zip(args.drain(split..)).collect();
                let prepared = self.prepare_call(&function, &args, &kwargs)?;
                self.continue_with(prepared)?;
            }
            Op::CallEx(has_kwargs) => {
                let kwargs = if has_kwargs { self.pop() } else { Value::None };
                let args = self.pop();
                let function = self.pop();
                let args = match args {
                    Value::Tuple(t) => t.to_vec(),
                    other => self.collect(&other)?,
                };
                let mut kw = Vec::new();
                if let Value::Dict(d) = &kwargs {
                    for entry in d.borrow().iter() {
                        match &entry.key {
                            Value::Str(s) => kw.push((s.clone(), entry.value.clone())),
                            _ => return self.type_error("keywords must be strings"),
                        }
                    }
                }
                let prepared = self.prepare_call(&function, &args, &kw)?;
                self.continue_with(prepared)?;
            }
            Op::MakeFunction(_, flags) => {
                let qualname = self.pop();
                let code = self.pop();
                let closure = if flags & 8 != 0 { self.pop() } else { Value::None };
                let kwdefaults = if flags & 2 != 0 { self.pop() } else { Value::None };
                let defaults = if flags & 1 != 0 { self.pop() } else { Value::None };
                let Value::Code(code) = code else {
                    return self.type_error("code attendu");
                };
                let cells = match closure {
                    Value::Tuple(t) => t.iter().filter_map(cell_of).collect(),
                    _ => Vec::new(),
                };
                let kwdefaults = match kwdefaults {
                    Value::Dict(d) => d.borrow().iter().filter_map(|e| if let Value::Str(k) = &e.key { Some((k.clone(), e.value.clone())) } else { None }).collect(),
                    _ => Vec::new(),
                };
                let defaults = match defaults {
                    Value::Tuple(t) => t.to_vec(),
                    _ => Vec::new(),
                };
                let qualname = match qualname {
                    Value::Str(s) => s,
                    _ => code.qualname.clone(),
                };
                let doc = code.doc.clone().map(Value::Str).unwrap_or(Value::None);
                let function = Function {
                    name: RefCell::new(code.name.clone()),
                    qualname: RefCell::new(qualname),
                    globals: self.top().globals.clone(),
                    code,
                    defaults,
                    kwdefaults,
                    closure: cells,
                    dict: RefCell::new(Dict::new()),
                    doc: RefCell::new(doc),
                };
                self.push(Value::Function(new_function(function)));
            }
            Op::Return => {
                let value = self.pop();
                return self.finish_frame(value, base);
            }
            Op::Yield => {
                let value = self.pop();
                if self.frames.len() - 1 == base {
                    return Ok(Some(Exit::Yield(value)));
                }
                return self.type_error("yield hors d'un generateur");
            }
            Op::GetYieldFromIter => {
                let v = self.pop();
                let it = match &v {
                    Value::Generator(_) => v,
                    other => self.get_iter(other)?,
                };
                self.push(it);
            }
            Op::YieldFrom => {
                let sent = self.pop();
                let receiver = self.peek(1);
                let step = match &receiver {
                    Value::Generator(g) => self.resume(g, sent, None).map(Some),
                    other => {
                        if sent.is_none() {
                            self.next(other)
                        } else {
                            let send = self.get_attr(other, "send")?;
                            self.call(&send, &[sent], &[]).map(Some)
                        }
                    }
                };
                match step {
                    Ok(Some(value)) => {
                        // Yields it, and comes back here when resumed.
                        self.top().ip -= 1;
                        if self.frames.len() - 1 == base {
                            return Ok(Some(Exit::Yield(value)));
                        }
                        return self.type_error("yield from hors d'un generateur");
                    }
                    Ok(None) => {
                        self.pop();
                        self.push(Value::None);
                    }
                    Err(e) => {
                        if self.is_instance_of(&e, &self.types.stop_iteration.clone()) {
                            let value = match &e {
                                PyErr::Exc(Value::Instance(i)) => i.dict.borrow().get_str("value").unwrap_or(Value::None),
                                _ => Value::None,
                            };
                            self.pop();
                            self.push(value);
                        } else {
                            return Err(e);
                        }
                    }
                }
            }
            Op::SetupFinally(handler) | Op::SetupWith(handler) => {
                if let Op::SetupWith(_) = op {
                    let manager = self.pop();
                    let exit = self.get_attr(&manager, "__exit__").map_err(|_| ()).or_else(|_| {
                        let name = self.type_name(&manager);
                        let message = alloc::format!("'{}' object does not support the context manager protocol", name);
                        self.type_error(&message)
                    })?;
                    let enter = self.get_attr(&manager, "__enter__")?;
                    self.push(exit);
                    let level = self.top().stack.len();
                    let handling = self.handling.len();
                    self.top().blocks.push(FBlock { kind: BlockKind::Finally, handler: handler as usize, level, handling });
                    let entered = self.call(&enter, &[], &[])?;
                    self.push(entered);
                } else {
                    let level = self.top().stack.len();
                    let handling = self.handling.len();
                    self.top().blocks.push(FBlock { kind: BlockKind::Finally, handler: handler as usize, level, handling });
                }
            }
            Op::PopBlock => {
                self.top().blocks.pop();
            }
            Op::PopExcept => {
                if let Some(block) = self.top().blocks.pop() {
                    self.handling.truncate(block.handling);
                }
            }
            Op::Reraise => {
                let exc = self.pop();
                return Err(PyErr::Exc(exc));
            }
            Op::Raise(n) => {
                let cause = if n == 2 { Some(self.pop()) } else { None };
                let exc = if n >= 1 { Some(self.pop()) } else { None };
                return Err(self.raise(exc, cause)?);
            }
            Op::JumpIfNotExcMatch(t) => {
                let kind = self.pop();
                let exc = self.pop();
                if !self.exception_matches(&exc, &kind)? {
                    self.top().ip = t as usize;
                }
            }
            Op::WithExceptStart => {
                let exc = self.peek(1);
                let exit = self.peek(2);
                let kind = match &exc {
                    Value::Instance(i) => Value::Class(i.class()),
                    _ => Value::None,
                };
                let result = self.call(&exit, &[kind, exc, Value::None], &[])?;
                self.push(result);
            }
            Op::BuildClass(nbases, nkw) => {
                let keyword_values = self.pop_n(2 * nkw as usize);
                let bases = self.pop_n(nbases as usize);
                let name = self.pop();
                let body = self.pop();
                let Value::Str(name) = name else {
                    return self.type_error("nom de classe attendu");
                };
                let mut base_classes = Vec::new();
                let original = Value::tuple(bases.clone());
                for b in bases {
                    match b {
                        Value::Class(c) => base_classes.push(c),
                        other => {
                            // `class Stack(Generic[T])`: what the object stands for.
                            if let Ok(entries) = self.get_attr(&other, "__mro_entries__") {
                                let replaced = self.call(&entries, core::slice::from_ref(&original), &[])?;
                                for c in self.collect(&replaced)? {
                                    if let Value::Class(c) = c {
                                        base_classes.push(c);
                                    }
                                }
                                continue;
                            }
                            let n = self.type_name(&other);
                            let message = alloc::format!("bases must be types, not {}", n);
                            return self.type_error(&message);
                        }
                    }
                }
                let keywords: Vec<(Rc<str>, Value)> = keyword_values
                    .chunks(2)
                    .filter_map(|p| if let Value::Str(k) = &p[0] { Some((k.clone(), p[1].clone())) } else { None })
                    .collect();
                let Value::Function(body) = body else {
                    return self.type_error("corps de classe attendu");
                };
                // The metaclass: the one asked for, or the most derived of
                // the bases'.
                let explicit = keywords.iter().find(|(k, _)| &**k == "metaclass").map(|(_, v)| v.clone());
                let keywords: Vec<(Rc<str>, Value)> = keywords.into_iter().filter(|(k, _)| &**k != "metaclass").collect();
                let mut meta = match &explicit {
                    Some(Value::Class(m)) => Some(m.clone()),
                    _ => None,
                };
                for base in base_classes.iter() {
                    let base_meta = base.metaclass.borrow().clone();
                    if let Some(bm) = base_meta {
                        match &meta {
                            None => meta = Some(bm),
                            Some(m) if bm.is_subclass(m) => meta = Some(bm),
                            Some(m) if m.is_subclass(&bm) || m.id == self.types.type_.id => {
                                if m.id == self.types.type_.id {
                                    meta = Some(bm);
                                }
                            }
                            Some(_) => {
                                return self.type_error("metaclass conflict: the metaclass of a derived class must be a (non-strict) subclass of the metaclasses of all its bases");
                            }
                        }
                    }
                }
                let meta = meta.filter(|m| m.id != self.types.type_.id);
                if let Some(meta) = meta {
                    let bases_tuple = Value::tuple(base_classes.iter().map(|c| Value::Class(c.clone())).collect());
                    let namespace = match meta.lookup("__prepare__") {
                        Some(_) => {
                            let prepare = self.get_attr(&Value::Class(meta.clone()), "__prepare__")?;
                            match self.call(&prepare, &[Value::Str(name.clone()), bases_tuple.clone()], &keywords)? {
                                Value::Dict(d) => d,
                                Value::Instance(i) => match &*i.inner.borrow() {
                                    Value::Dict(d) => d.clone(),
                                    _ => return self.type_error("__prepare__() must return a mapping"),
                                },
                                _ => return self.type_error("__prepare__() must return a mapping"),
                            }
                        }
                        None => shared_dict(Dict::new()),
                    };
                    let mut frame = self.new_frame(&body, &[], &[])?;
                    frame.namespace = Some(namespace.clone());
                    frame.on_return = OnReturn::MetaClassBody(namespace, name, bases_tuple, keywords, meta);
                    self.push_frame(frame)?;
                    return Ok(None);
                }
                let namespace = shared_dict(Dict::new());
                let mut frame = self.new_frame(&body, &[], &[])?;
                frame.namespace = Some(namespace.clone());
                frame.on_return = OnReturn::ClassBody(namespace, name, base_classes, keywords);
                self.push_frame(frame)?;
            }
            Op::ImportName(i, level) => {
                let name = self.name_at(i);
                let fromlist = self.pop();
                let module = super::modules::import(self, &name, level as usize, &fromlist)?;
                self.push(module);
            }
            Op::ImportFrom(i) => {
                let name = self.name_at(i);
                let module = self.peek(1);
                let value = match self.get_attr(&module, &name) {
                    Ok(v) => v,
                    Err(_) => {
                        let module_name = match &module {
                            Value::Module(m) => m.name.clone(),
                            _ => Rc::from("?"),
                        };
                        let class = self.types.import_error.clone();
                        let message = alloc::format!("cannot import name '{}' from '{}'", name, module_name);
                        return Err(self.error(&class, &message));
                    }
                };
                self.push(value);
            }
            Op::ImportStar => {
                let module = self.pop();
                if let Value::Module(m) = module {
                    let entries: Vec<(Value, Value)> = m.dict.borrow().iter().map(|e| (e.key.clone(), e.value.clone())).collect();
                    let ns = self.top().namespace.clone();
                    for (k, v) in entries {
                        if let Value::Str(name) = &k {
                            if !name.starts_with('_') {
                                if let Some(ns) = &ns {
                                    ns.borrow_mut().set_str(name, v);
                                }
                            }
                        }
                    }
                }
            }
            Op::LoadAssertionError => {
                let class = self.types.assertion_error.clone();
                self.push(Value::Class(class));
            }
            Op::MatchKind(kind) => {
                let value = self.pop();
                let result = self.match_kind(&value, kind)?;
                self.push(Value::Bool(result));
            }
            Op::GetLen => {
                let value = self.pop();
                let n = self.len(&value)?;
                self.push(Value::Int(super::int::Int::from_i64(n as i64)));
            }
            Op::MatchClass(positional) => {
                let names = self.pop();
                let class = self.pop();
                let subject = self.pop();
                let result = self.match_class(&subject, &class, positional as usize, &names)?;
                self.push(result);
            }
            Op::MatchKeys => {
                let keys = self.pop();
                let subject = self.pop();
                let result = self.match_keys(&subject, &keys)?;
                self.push(result);
            }
            Op::CopyDictWithout => {
                let keys = self.pop();
                let subject = self.pop();
                let result = self.copy_dict_without(&subject, &keys)?;
                self.push(result);
            }
            Op::GetAwaitable => {
                let value = self.pop();
                let awaitable = self.awaitable(value)?;
                self.push(awaitable);
            }
            Op::AsyncGenWrap => {
                let value = self.pop();
                let marker = self.async_marker();
                self.push(Value::tuple(alloc::vec![marker, value]));
            }
            Op::SetupAnnotations => {
                if let Some(ns) = self.top().namespace.clone() {
                    if ns.borrow().get_str("__annotations__").is_none() {
                        ns.borrow_mut().set_str("__annotations__", Value::dict(Dict::new()));
                    }
                }
            }
        }
        Ok(None)
    }

    fn cell_name(&mut self, i: u32) -> Rc<str> {
        let code = self.top().code.clone();
        let cells = code.cellvars.len();
        if (i as usize) < cells {
            code.cellvars[i as usize].clone()
        } else {
            code.freevars.get(i as usize - cells).cloned().unwrap_or_else(|| Rc::from("?"))
        }
    }

    fn pop_n(&mut self, n: usize) -> Vec<Value> {
        let stack = &mut self.top().stack;
        let at = stack.len().saturating_sub(n);
        stack.split_off(at)
    }

    pub fn load_global(&mut self, name: &str) -> PyResult<Value> {
        let globals = self.top().globals.clone();
        if let Some(v) = globals.dict.borrow().get_str(name) {
            return Ok(v);
        }
        if let Some(v) = self.builtins.dict.borrow().get_str(name) {
            return Ok(v);
        }
        if let Some(v) = self.lazy_builtin(name)? {
            return Ok(v);
        }
        self.name_error(name)
    }

    /// The built-in names written in Python (`ExceptionGroup`,
    /// `memoryview`, the helpers of `except*` and `type`): their module is
    /// loaded on first use, and they are then ordinary builtins.
    pub fn lazy_builtin(&mut self, name: &str) -> PyResult<Option<Value>> {
        const LAZY: &[&str] = &[
            "BaseExceptionGroup", "ExceptionGroup", "memoryview", "_nova_excstar_prepare", "_nova_excstar_split",
            "_nova_excstar_reraise", "_nova_type_alias", "_nova_typevar", "_nova_generic", "_nova_generic_alias", "_nova_traceback",
        ];
        if !LAZY.contains(&name) {
            return Ok(None);
        }
        let module = super::modules::import(self, "_builtins_extra", 0, &Value::None)?;
        let Value::Module(module) = module else {
            return Ok(None);
        };
        for n in LAZY {
            if let Some(v) = module.dict.borrow().get_str(n) {
                self.builtins.dict.borrow_mut().set_str(n, v);
            }
        }
        Ok(self.builtins.dict.borrow().get_str(name))
    }

    /// `raise`, `raise e`, `raise e from c`: the exception to unwind with.
    fn raise(&mut self, exc: Option<Value>, cause: Option<Value>) -> PyResult<PyErr> {
        let Some(exc) = exc else {
            return match self.handling.last() {
                Some(current) => Ok(PyErr::Exc(current.clone())),
                None => {
                    let class = self.types.runtime_error.clone();
                    Ok(self.error(&class, "No active exception to reraise"))
                }
            };
        };
        let instance = match exc {
            Value::Class(class) if class.is_subclass(&self.types.base_exception) => self.call(&Value::Class(class), &[], &[])?,
            Value::Instance(ref i) if i.class().is_subclass(&self.types.base_exception) => exc.clone(),
            _ => return Ok(self.type_error::<()>("exceptions must derive from BaseException").err().unwrap_or(PyErr::Halt(String::new()))),
        };
        if let Value::Instance(i) = &instance {
            i.traceback.borrow_mut().clear();
            if let Some(cause) = cause {
                let cause = match cause {
                    Value::Class(c) => self.call(&Value::Class(c), &[], &[])?,
                    other => other,
                };
                *i.cause.borrow_mut() = cause;
                i.dict.borrow_mut().set_str("__suppress_context__", Value::Bool(true));
            }
            if let Some(previous) = self.handling.last() {
                if !previous.is(&instance) {
                    *i.context.borrow_mut() = previous.clone();
                }
            }
        }
        Ok(PyErr::Exc(instance))
    }

    fn exception_matches(&mut self, exc: &Value, kind: &Value) -> PyResult<bool> {
        let Value::Instance(instance) = exc else {
            return Ok(false);
        };
        let class = instance.class();
        match kind {
            Value::Class(k) => Ok(class.is_subclass(k)),
            Value::Tuple(items) => {
                for item in items.iter() {
                    if self.exception_matches(exc, item)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
            _ => self.type_error("catching classes that do not inherit from BaseException is not allowed"),
        }
    }
}

/// Code of the modules shipped as Python source (`<functools>`): the
/// standard library, which CPython writes in C and does not show.
pub fn is_library(code: &Code) -> bool {
    let f = &*code.filename;
    f.starts_with('<') && f.ends_with('>') && f != "<string>" && f != "<stdin>"
}

/// Either the result of a call, or a frame to run for it.
pub enum Prepared {
    Done(Value),
    Frame(Frame),
}

fn cell_of(value: &Value) -> Option<Rc<RefCell<Option<Value>>>> {
    match value {
        Value::Cell(c) => Some(c.clone()),
        _ => None,
    }
}
