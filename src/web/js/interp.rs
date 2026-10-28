//! The evaluator: walks the syntax tree.
//!
//! ## Why a tree walker
//!
//! A bytecode compiler would be faster; it would also be a second
//! program to get right. A page's scripts mostly wire events and build a
//! few elements: the walk is fast enough for that, and each construct is
//! evaluated in one place that reads like the specification's own steps.
//!
//! ## Bounds that are not negotiable
//!
//! Scripts come from the network and run on a kernel stack (see
//! `sdk::script` and `DECISIONS.md` for the same rule):
//!
//! * **fuel** -- every statement and call burns one unit; an empty budget
//!   halts the script (`Ctrl::Halt`), which nothing in the script can
//!   catch. A `while (true) {}` ends there instead of freezing the core.
//! * **stack** -- every call and nested evaluation checks the headroom;
//!   below a margin the script gets a catchable `RangeError` ("Maximum call
//!   stack size exceeded"), well below it a halt.
//! * **memory** -- object creation asks the host, every few thousand
//!   objects, whether memory remains; a page that eats the heap is halted
//!   before the kernel allocator fails.
//!
//! ## What is simplified, and how
//!
//! * **Generators run eagerly**: calling one runs its body to the end
//!   and collects what it yields (up to `GENERATOR_LIMIT` values); the
//!   iterator then hands them out. Finite generators, which are nearly
//!   all of them, behave the same; values sent through `next(value)` are
//!   not seen by the body.
//! * **`await` waits in place**: it runs the job queue -- and, if that
//!   empties, the earliest timer, fast-forwarding the clock -- until the
//!   awaited promise settles. The order of interleaved logs can differ
//!   from a browser's; the values do not.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::rc::{Rc, Weak};
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

use super::ast::*;
use super::num::Num;
use super::ops;
use super::value::*;

/// What a statement or expression can do besides produce a value.
pub enum Ctrl {
    Throw(Value),
    Break(Option<JsStr>),
    Continue(Option<JsStr>),
    Return(Value),
    /// The generator collected enough values.
    GeneratorFull,
    /// Stops everything: fuel, stack or memory exhausted.
    Halt(String),
}

pub type R<T> = Result<T, Ctrl>;

/// Values a generator collects before it is stopped.
const GENERATOR_LIMIT: usize = 10_000;
/// Headroom under which a call throws a RangeError, and under which
/// evaluation halts outright.
const SOFT_STACK: usize = 96 * 1024;
const HARD_STACK: usize = 40 * 1024;
/// Nested calls, whatever the stack says.
const MAX_CALL_DEPTH: usize = 400;

// ============================================================
//  Scopes
// ============================================================

/// A binding's state.
const MUTABLE: u8 = 0;
const CONSTANT: u8 = 1;
/// `let`/`const`/`class` before its declaration ran: reading it throws.
const UNINITIALIZED: u8 = 2;

pub struct Binding {
    pub name: JsStr,
    pub value: Value,
    pub state: u8,
}

/// What a non-arrow function's scope knows about its call.
pub struct Frame {
    /// `None` in a derived constructor before `super()`.
    pub this: RefCell<Option<Value>>,
    pub function: Option<Obj>,
    pub new_target: Value,
    pub home: Option<Obj>,
    pub arguments: Vec<Value>,
}

pub struct ScopeData {
    pub vars: Vec<Binding>,
    pub parent: Option<Scope>,
    pub frame: Option<Rc<Frame>>,
}

#[derive(Clone)]
pub struct Scope(pub Rc<RefCell<ScopeData>>);

impl Scope {
    pub fn new(parent: Option<Scope>, frame: Option<Rc<Frame>>) -> Scope {
        Scope(Rc::new(RefCell::new(ScopeData { vars: Vec::new(), parent, frame })))
    }

    fn declare(&self, name: JsStr, value: Value, state: u8) {
        let mut data = self.0.borrow_mut();
        if let Some(binding) = data.vars.iter_mut().find(|b| b.name.ptr_eq(&name) || b.name == name) {
            binding.value = value;
            binding.state = state;
            return;
        }
        data.vars.push(Binding { name, value, state });
    }

    fn has_own(&self, name: &JsStr) -> bool {
        self.0.borrow().vars.iter().any(|b| b.name.ptr_eq(name) || b.name == *name)
    }

    fn frame(&self) -> Option<Rc<Frame>> {
        let mut scope = self.clone();
        loop {
            let next = {
                let data = scope.0.borrow();
                if let Some(frame) = &data.frame {
                    return Some(frame.clone());
                }
                data.parent.clone()
            };
            scope = next?;
        }
    }

    /// Empties the scope: breaks the cycles through it (page teardown).
    pub fn clear(&self) {
        let mut data = self.0.borrow_mut();
        data.vars.clear();
        data.parent = None;
        data.frame = None;
    }
}

// ============================================================
//  Realm
// ============================================================

/// The prototypes of the built-in objects.
pub struct Protos {
    pub object: Obj,
    pub function: Obj,
    pub array: Obj,
    pub string: Obj,
    pub number: Obj,
    pub boolean: Obj,
    pub symbol: Obj,
    pub error: Obj,
    pub type_error: Obj,
    pub range_error: Obj,
    pub syntax_error: Obj,
    pub reference_error: Obj,
    pub eval_error: Obj,
    pub uri_error: Obj,
    pub date: Obj,
    pub regexp: Obj,
    pub map: Obj,
    pub set: Obj,
    pub weak_map: Obj,
    pub weak_set: Obj,
    pub promise: Obj,
    pub iterator: Obj,
    pub generator: Obj,
    pub event_target: Obj,
    pub node: Obj,
    pub element: Obj,
    pub document: Obj,
    pub text: Obj,
    pub event: Obj,
    pub dom_token_list: Obj,
    pub style: Obj,
    pub node_list: Obj,
}

pub enum Job {
    /// A promise reaction: handler (or undefined), argument, whether the
    /// settlement was a rejection, and the derived promise's resolving
    /// functions.
    Reaction(Value, Value, bool, Value, Value),
    /// `thenable.then(resolve, reject)`, for a promise resolved with a
    /// thenable.
    Thenable(Value, Value, Value, Value),
    /// A plain callback (queueMicrotask).
    Call(Value, Vec<Value>),
}

pub struct Timer {
    pub id: u32,
    pub due: i64,
    pub interval: Option<i64>,
    pub callback: Value,
    pub args: Vec<Value>,
}

/// Well-known symbols.
pub struct Symbols {
    pub iterator: JsStr,
    pub async_iterator: JsStr,
    pub has_instance: JsStr,
    pub to_primitive: JsStr,
    pub to_string_tag: JsStr,
    pub species: JsStr,
    pub unscopables: JsStr,
}

pub struct Realm {
    pub global: Obj,
    pub global_scope: Scope,
    pub protos: Protos,
    pub symbols: Symbols,
    pub next_symbol: usize,
    /// Every object made, weakly: emptied at teardown.
    objects: Vec<Weak<RefCell<Object>>>,
    scopes: Vec<Weak<RefCell<ScopeData>>>,
    pruned_at: usize,
    pub jobs: VecDeque<Job>,
    pub timers: Vec<Timer>,
    pub next_timer: u32,
    /// Virtual clock offset added by `await` fast-forwarding timers.
    pub clock_skew: i64,
    pub dom: super::dom::DomState,
    /// Console output, for the browser's console page.
    pub console: Vec<String>,
    /// Unhandled rejections and uncaught errors, reported once.
    pub errors: Vec<String>,
    /// Promises rejected with no handler yet: reported when the job
    /// queue runs dry if still unhandled then.
    pub unhandled: Vec<Obj>,
    /// Sources of the scripts, for `Function.prototype.toString`.
    pub sources: Vec<Rc<str>>,
    pub halted: Option<String>,
}

/// What the page's scripts reach outside the engine.
pub trait Host {
    /// Milliseconds since the epoch.
    fn now_ms(&mut self) -> i64;
    /// Offset of local time from UTC, in minutes (east positive). Nova's
    /// zones are fixed offsets (`i18n::tz`): one number covers every date.
    fn tz_offset_minutes(&mut self) -> i32 {
        0
    }
    fn random(&mut self) -> u64;
    /// Is there memory left for the page to grow?
    fn memory_ok(&mut self) -> bool {
        true
    }
    /// Can a single allocation of `bytes` be made for the page? Asked
    /// before large strings and arrays (`ops::reserve`).
    fn can_allocate(&mut self, bytes: usize) -> bool {
        let _ = bytes;
        true
    }
    /// A synchronous request: (status, status text, headers, body, final
    /// URL).
    fn fetch(
        &mut self,
        method: &str,
        url: &str,
        headers: &[(String, String)],
        body: Option<Vec<u8>>,
    ) -> Result<(u16, String, Vec<(String, String)>, Vec<u8>, String), String>;
    fn cookies(&mut self, url: &str) -> String;
    fn set_cookie(&mut self, url: &str, line: &str);
    fn storage(&mut self, origin: &str, session: bool) -> Vec<(String, String)>;
    fn set_storage(&mut self, origin: &str, session: bool, items: &[(String, String)]);
    /// The page asks to go to `url`.
    fn navigate(&mut self, url: &str, replace: bool);
    fn log(&mut self, line: &str);
    /// `alert`/`confirm`/`prompt`: shown, not waited on.
    fn dialog(&mut self, kind: &str, message: &str) -> Option<String> {
        let _ = (kind, message);
        None
    }
}

impl Realm {
    pub fn assemble(global: Obj, protos: Protos, symbols: Symbols) -> Realm {
        let global_scope = Scope::new(None, None);
        let mut realm = Realm {
            global,
            global_scope,
            protos,
            symbols,
            next_symbol: 100,
            objects: Vec::new(),
            scopes: Vec::new(),
            pruned_at: 0,
            jobs: VecDeque::new(),
            timers: Vec::new(),
            next_timer: 1,
            clock_skew: 0,
            dom: super::dom::DomState::default(),
            console: Vec::new(),
            errors: Vec::new(),
            unhandled: Vec::new(),
            sources: Vec::new(),
            halted: None,
        };
        let scope = realm.global_scope.clone();
        realm.track_scope(&scope);
        realm
    }

    pub fn track(&mut self, object: &Obj) {
        self.objects.push(object.downgrade());
    }

    pub fn track_scope(&mut self, scope: &Scope) {
        self.scopes.push(Rc::downgrade(&scope.0));
    }

    /// Objects made since the list was last pruned: for the memory check.
    fn prune(&mut self) {
        self.objects.retain(|weak| weak.strong_count() > 0);
        self.scopes.retain(|weak| weak.strong_count() > 0);
        self.pruned_at = self.objects.len() + self.scopes.len();
    }

    /// Breaks every cycle, so that dropping the realm frees everything.
    pub fn teardown(&mut self) {
        for weak in self.objects.drain(..) {
            if let Some(object) = weak.upgrade() {
                if let Ok(mut object) = object.try_borrow_mut() {
                    object.props.clear();
                    object.proto = None;
                    object.kind = Kind::Ordinary;
                }
            }
        }
        for weak in self.scopes.drain(..) {
            if let Some(scope) = weak.upgrade() {
                if let Ok(mut data) = scope.try_borrow_mut() {
                    data.vars.clear();
                    data.parent = None;
                    data.frame = None;
                }
            }
        }
        self.global_scope.clear();
        self.global.borrow_mut().props.clear();
        self.jobs.clear();
        self.timers.clear();
        self.unhandled.clear();
        self.dom.clear();
    }

    pub fn object_count(&self) -> usize {
        self.objects.len()
    }
}

impl Drop for Realm {
    fn drop(&mut self) {
        self.teardown();
    }
}

// ============================================================
//  Context
// ============================================================

/// A realm at work: the realm, and what a run borrows from its caller.
pub struct Ctx<'a> {
    pub realm: &'a mut Realm,
    pub host: &'a mut dyn Host,
    pub doc: &'a mut super::super::html::Document,
    pub headroom: &'a dyn Fn() -> usize,
    pub fuel: u64,
    pub depth: usize,
    /// Values collected by the generators being run.
    generators: Vec<Vec<Value>>,
    /// The `new.target` for a native constructor being called.
    pub new_target: Option<Value>,
    /// Allocations since the last memory check.
    allocations: usize,
    /// Nested `await` loops.
    await_depth: usize,
    /// The DOM changed: the page must be laid out again.
    pub dirty: bool,
    /// The base URL of the page.
    pub base_url: String,
    /// Viewport width and height, for matchMedia and innerWidth.
    pub viewport: (i32, i32),
    /// Largest single allocation the host last agreed to, doubled: below
    /// it `ops::reserve` does not ask again (a string grown one piece at
    /// a time would otherwise ask at every piece).
    pub reserved_up_to: usize,
}

impl<'a> Ctx<'a> {
    pub fn new(
        realm: &'a mut Realm,
        host: &'a mut dyn Host,
        doc: &'a mut super::super::html::Document,
        headroom: &'a dyn Fn() -> usize,
        fuel: u64,
        base_url: String,
        viewport: (i32, i32),
    ) -> Ctx<'a> {
        Ctx {
            realm,
            host,
            doc,
            headroom,
            fuel,
            depth: 0,
            generators: Vec::new(),
            new_target: None,
            allocations: 0,
            await_depth: 0,
            dirty: false,
            base_url,
            viewport,
            reserved_up_to: 0,
        }
    }

    // --------------------------------------------------------
    //  Object creation
    // --------------------------------------------------------

    pub fn alloc(&mut self, kind: Kind, proto: Option<Obj>, class: &'static str) -> Obj {
        let object = Obj::new(kind, proto, class);
        self.realm.track(&object);
        self.allocations += 1;
        if self.allocations >= 4096 {
            self.allocations = 0;
            if self.realm.objects.len() + self.realm.scopes.len() > self.realm.pruned_at * 2 + 4096 {
                self.realm.prune();
            }
            if !self.host.memory_ok() && self.realm.halted.is_none() {
                self.realm.halted = Some(String::from("memoire epuisee par les scripts de la page"));
            }
        }
        object
    }

    pub fn new_object(&mut self) -> Obj {
        let proto = self.realm.protos.object.clone();
        self.alloc(Kind::Ordinary, Some(proto), "Object")
    }

    pub fn new_array(&mut self, values: Vec<Value>) -> Obj {
        let proto = self.realm.protos.array.clone();
        self.alloc(Kind::Array(values), Some(proto), "Array")
    }

    pub fn new_scope(&mut self, parent: Option<Scope>, frame: Option<Rc<Frame>>) -> Scope {
        let scope = Scope::new(parent, frame);
        self.realm.track_scope(&scope);
        scope
    }

    /// A native function object.
    pub fn native(&mut self, name: &str, length: usize, function: NativeFn) -> Obj {
        self.native_with(name, length, function, Value::Undefined)
    }

    pub fn native_with(&mut self, name: &str, length: usize, function: NativeFn, data: Value) -> Obj {
        let proto = self.realm.protos.function.clone();
        let object = self.alloc(Kind::Native(function, data, JsStr::from(name)), Some(proto), "Function");
        object.borrow_mut().props.set(JsStr::from("length"), Slot::Data(Value::num(length as i64), CONFIGURABLE));
        object.borrow_mut().props.set(JsStr::from("name"), Slot::Data(Value::str(name), CONFIGURABLE));
        object
    }

    /// Defines a native method on `target`, not enumerable.
    pub fn method(&mut self, target: &Obj, name: &str, length: usize, function: NativeFn) {
        let native = self.native(name, length, function);
        target.put_hidden(name, Value::Obj(native));
    }

    /// Defines a native accessor on `target`.
    pub fn accessor(&mut self, target: &Obj, name: &str, getter: Option<NativeFn>, setter: Option<NativeFn>) {
        let get = getter.map(|g| self.native(name, 0, g));
        let set = setter.map(|s| self.native(name, 1, s));
        target.borrow_mut().props.set(JsStr::from(name), Slot::Accessor(get, set, CONFIGURABLE));
    }

    // --------------------------------------------------------
    //  Errors
    // --------------------------------------------------------

    pub fn error_object(&mut self, proto: Obj, name: &str, message: &str) -> Obj {
        let error = self.alloc(Kind::Error, Some(proto), "Error");
        error.put_hidden("message", Value::str(message));
        error.put_hidden("stack", Value::Str(JsStr::from(alloc::format!("{}: {}\n    at <script>", name, message))));
        error
    }

    pub fn throw_error<T>(&mut self, kind: &str, message: &str) -> R<T> {
        let proto = match kind {
            "TypeError" => self.realm.protos.type_error.clone(),
            "RangeError" => self.realm.protos.range_error.clone(),
            "SyntaxError" => self.realm.protos.syntax_error.clone(),
            "ReferenceError" => self.realm.protos.reference_error.clone(),
            "URIError" => self.realm.protos.uri_error.clone(),
            _ => self.realm.protos.error.clone(),
        };
        let error = self.error_object(proto, kind, message);
        Err(Ctrl::Throw(Value::Obj(error)))
    }

    pub fn throw_type<T>(&mut self, message: &str) -> R<T> {
        self.throw_error("TypeError", message)
    }

    pub fn throw_range<T>(&mut self, message: &str) -> R<T> {
        self.throw_error("RangeError", message)
    }

    /// Burns one unit of fuel.
    #[inline]
    pub fn tick(&mut self) -> R<()> {
        if self.fuel == 0 {
            return Err(Ctrl::Halt(String::from("script trop long : budget d'execution epuise")));
        }
        self.fuel -= 1;
        if let Some(reason) = &self.realm.halted {
            return Err(Ctrl::Halt(reason.clone()));
        }
        // Growth that allocates no object (a loop pushing numbers) is
        // seen here: every 64 Ki steps the host is asked about memory.
        if self.fuel & 0xFFFF == 0 && !self.host.memory_ok() {
            self.realm.halted = Some(String::from("memoire epuisee par les scripts de la page"));
            return Err(Ctrl::Halt(String::from("memoire epuisee par les scripts de la page")));
        }
        Ok(())
    }

    fn check_stack(&mut self) -> R<()> {
        let room = (self.headroom)();
        if room < HARD_STACK {
            return Err(Ctrl::Halt(String::from("pile epuisee")));
        }
        if room < SOFT_STACK || self.depth > MAX_CALL_DEPTH {
            return self.throw_range("Maximum call stack size exceeded");
        }
        Ok(())
    }

    /// Current time in milliseconds, virtual clock included.
    pub fn now(&mut self) -> i64 {
        self.host.now_ms() + self.realm.clock_skew
    }

    // --------------------------------------------------------
    //  Scripts
    // --------------------------------------------------------

    /// Runs a script in the global scope. Errors come back as text.
    pub fn run_script(&mut self, source: &str) -> Result<Value, String> {
        match self.eval_global(source) {
            Ok(value) => Ok(value),
            Err(Ctrl::Throw(value)) => Err(self.describe_error(&value)),
            Err(Ctrl::Halt(reason)) => Err(reason),
            Err(_) => Ok(Value::Undefined),
        }
    }

    /// Parses and runs source text in the global scope: scripts, `eval`
    /// and `new Function`. A syntax error is thrown as a SyntaxError.
    pub fn eval_global(&mut self, source: &str) -> R<Value> {
        let headroom = self.headroom;
        let program = match super::parse::parse(source, headroom) {
            Ok(program) => program,
            Err(error) => {
                let (line, column) = super::lex::position(source, error.at);
                let message = alloc::format!("{} (ligne {}, colonne {})", error.message, line, column);
                return self.throw_error("SyntaxError", &message);
            }
        };
        self.realm.sources.push(Rc::from(source));
        let scope = self.realm.global_scope.clone();
        // `var` and function declarations at top level are properties of
        // the global object.
        for name in &program.vars {
            let exists = self.realm.global.borrow().props.get(name).is_some();
            if !exists {
                self.realm.global.put_key(name.clone(), Value::Undefined, WRITABLE | ENUMERABLE);
            }
        }
        self.declare_lexical(&program.body, &scope);
        self.hoist_functions(&program.body, &scope, true)?;
        let mut last = Value::Undefined;
        for statement in &program.body {
            if let Stmt::Expr(expr) = statement {
                last = self.eval(expr, &scope)?;
            } else {
                self.exec(statement, &scope)?;
            }
        }
        Ok(last)
    }

    /// A thrown value as a line of text.
    pub fn describe_error(&mut self, value: &Value) -> String {
        if let Value::Obj(object) = value {
            if matches!(object.borrow().kind, Kind::Error) {
                let name = ops::get(self, value, "name").ok().map(|v| ops::to_display(&v)).unwrap_or_default();
                let message = ops::get(self, value, "message").ok().map(|v| ops::to_display(&v)).unwrap_or_default();
                return alloc::format!("{}: {}", name, message);
            }
        }
        alloc::format!("Uncaught {}", ops::to_display(value))
    }

    // --------------------------------------------------------
    //  Jobs and timers
    // --------------------------------------------------------

    pub fn run_jobs(&mut self) -> Result<(), String> {
        let mut count = 0;
        while let Some(job) = self.realm.jobs.pop_front() {
            count += 1;
            if count > 100_000 {
                self.realm.jobs.clear();
                return Err(String::from("file de taches sans fin"));
            }
            match self.run_job(job) {
                Ok(()) => {}
                Err(Ctrl::Throw(value)) => {
                    let text = self.describe_error(&value);
                    self.report(text);
                }
                Err(Ctrl::Halt(reason)) => return Err(reason),
                Err(_) => {}
            }
        }
        super::builtins::promise::report_unhandled(self);
        Ok(())
    }

    fn run_job(&mut self, job: Job) -> R<()> {
        match job {
            Job::Reaction(handler, argument, rejected, resolve, reject) => {
                if handler.as_obj().is_some_and(|h| h.is_callable()) {
                    match self.call(&handler, Value::Undefined, &[argument]) {
                        Ok(value) => {
                            if !resolve.is_undefined() {
                                self.call(&resolve, Value::Undefined, &[value])?;
                            }
                        }
                        Err(Ctrl::Throw(error)) => {
                            if !reject.is_undefined() {
                                self.call(&reject, Value::Undefined, &[error])?;
                            }
                        }
                        Err(other) => return Err(other),
                    }
                } else if rejected {
                    if !reject.is_undefined() {
                        self.call(&reject, Value::Undefined, &[argument])?;
                    }
                } else if !resolve.is_undefined() {
                    self.call(&resolve, Value::Undefined, &[argument])?;
                }
                Ok(())
            }
            Job::Thenable(thenable, then, resolve, reject) => {
                if let Err(Ctrl::Throw(error)) = self.call(&then, thenable, &[resolve, reject.clone()]) {
                    self.call(&reject, Value::Undefined, &[error])?;
                }
                Ok(())
            }
            Job::Call(callback, args) => {
                self.call(&callback, Value::Undefined, &args)?;
                Ok(())
            }
        }
    }

    pub fn report(&mut self, text: String) {
        self.host.log(&text);
        if self.realm.errors.len() < 200 {
            self.realm.errors.push(text.clone());
        }
        if self.realm.console.len() < 2000 {
            self.realm.console.push(alloc::format!("[erreur] {}", text));
        }
    }

    /// Runs the timers due at `now`, then the jobs after each.
    pub fn run_timers(&mut self, now: i64) -> Result<usize, String> {
        let mut ran = 0;
        for _ in 0..1000 {
            let Some(position) = self
                .realm
                .timers
                .iter()
                .enumerate()
                .filter(|(_, t)| t.due <= now)
                .min_by_key(|(_, t)| (t.due, t.id))
                .map(|(i, _)| i)
            else {
                break;
            };
            self.fire_timer(position)?;
            ran += 1;
        }
        Ok(ran)
    }

    fn fire_timer(&mut self, position: usize) -> Result<(), String> {
        let timer = &mut self.realm.timers[position];
        let callback = timer.callback.clone();
        let args = timer.args.clone();
        let id = timer.id;
        match timer.interval {
            Some(interval) => timer.due += interval.max(4),
            None => {
                self.realm.timers.remove(position);
            }
        }
        let _ = id;
        match self.call(&callback, Value::Undefined, &args) {
            Ok(_) => {}
            Err(Ctrl::Throw(value)) => {
                let text = self.describe_error(&value);
                self.report(text);
            }
            Err(Ctrl::Halt(reason)) => return Err(reason),
            Err(_) => {}
        }
        self.run_jobs()
    }

    // --------------------------------------------------------
    //  Variables
    // --------------------------------------------------------

    pub fn lookup(&mut self, name: &JsStr, scope: &Scope) -> R<Value> {
        let mut current = scope.clone();
        loop {
            let next = {
                let data = current.0.borrow();
                for binding in data.vars.iter() {
                    if binding.name.ptr_eq(name) || binding.name == *name {
                        if binding.state == UNINITIALIZED {
                            drop(data);
                            let message = alloc::format!("Cannot access '{}' before initialization", name.as_str());
                            return self.throw_error("ReferenceError", &message);
                        }
                        return Ok(binding.value.clone());
                    }
                }
                data.parent.clone()
            };
            match next {
                Some(parent) => current = parent,
                None => break,
            }
        }
        // The global object.
        let global = self.realm.global.clone();
        if let Some(value) = ops::get_if_present(self, &global, name)? {
            return Ok(value);
        }
        if name.as_str() == "undefined" {
            return Ok(Value::Undefined);
        }
        let message = alloc::format!("{} is not defined", name.as_str());
        self.throw_error("ReferenceError", &message)
    }

    /// `typeof name` does not throw for an undeclared name.
    fn lookup_for_typeof(&mut self, name: &JsStr, scope: &Scope) -> R<Value> {
        match self.lookup(name, scope) {
            Err(Ctrl::Throw(error)) => {
                let is_reference = error.as_obj().is_some_and(|o| {
                    o.proto().is_some_and(|p| p.ptr_eq(&self.realm.protos.reference_error))
                });
                let declared = self.is_declared(name, scope);
                if is_reference && !declared {
                    Ok(Value::Undefined)
                } else {
                    Err(Ctrl::Throw(error))
                }
            }
            other => other,
        }
    }

    fn is_declared(&self, name: &JsStr, scope: &Scope) -> bool {
        let mut current = Some(scope.clone());
        while let Some(scope) = current {
            if scope.has_own(name) {
                return true;
            }
            current = scope.0.borrow().parent.clone();
        }
        false
    }

    pub fn assign(&mut self, name: &JsStr, value: Value, scope: &Scope) -> R<()> {
        let mut current = scope.clone();
        loop {
            let next = {
                let mut data = current.0.borrow_mut();
                let mut state = None;
                for binding in data.vars.iter_mut() {
                    if binding.name.ptr_eq(name) || binding.name == *name {
                        state = Some(binding.state);
                        if binding.state == MUTABLE {
                            binding.value = value.clone();
                        }
                        break;
                    }
                }
                match state {
                    Some(MUTABLE) => return Ok(()),
                    Some(CONSTANT) => {
                        drop(data);
                        return self.throw_type("Assignment to constant variable.");
                    }
                    Some(_) => {
                        drop(data);
                        let message = alloc::format!("Cannot access '{}' before initialization", name.as_str());
                        return self.throw_error("ReferenceError", &message);
                    }
                    None => data.parent.clone(),
                }
            };
            match next {
                Some(parent) => current = parent,
                None => break,
            }
        }
        let global = Value::Obj(self.realm.global.clone());
        ops::set(self, &global, name.clone(), value)?;
        Ok(())
    }

    fn this_value(&mut self, scope: &Scope) -> R<Value> {
        match scope.frame() {
            Some(frame) => match frame.this.borrow().clone() {
                Some(this) => Ok(this),
                None => self.throw_error("ReferenceError", "Must call super constructor before accessing 'this'"),
            },
            None => Ok(Value::Obj(self.realm.global.clone())),
        }
    }

    // --------------------------------------------------------
    //  Statements
    // --------------------------------------------------------

    /// Function declarations of a statement list are made on entry.
    fn hoist_functions(&mut self, body: &[Stmt], scope: &Scope, top: bool) -> R<()> {
        for statement in body {
            let target = match statement {
                Stmt::Function(func) => func,
                Stmt::Labeled(_, inner) => match inner.as_ref() {
                    Stmt::Function(func) => func,
                    _ => continue,
                },
                _ => continue,
            };
            let Some(name) = &target.name else {
                continue;
            };
            let closure = self.make_function(target.clone(), scope, None)?;
            if top && scope.0.borrow().parent.is_none() {
                // Global function declarations are global properties.
                self.realm.global.put_key(name.clone(), closure, WRITABLE | ENUMERABLE);
            } else {
                scope.declare(name.clone(), closure, MUTABLE);
            }
        }
        Ok(())
    }

    /// Declares the `let`/`const`/`class` names of a block, uninitialized.
    fn declare_lexical(&mut self, body: &[Stmt], scope: &Scope) {
        for statement in body {
            match statement {
                Stmt::Var(kind, declarations) if *kind != VarKind::Var => {
                    for (pattern, _) in declarations {
                        let mut names = Vec::new();
                        super::parse::pattern_names(pattern, &mut names);
                        for name in names {
                            scope.declare(name, Value::Undefined, UNINITIALIZED);
                        }
                    }
                }
                Stmt::Class(name, _) => scope.declare(name.clone(), Value::Undefined, UNINITIALIZED),
                _ => {}
            }
        }
    }

    fn needs_scope(body: &[Stmt]) -> bool {
        body.iter().any(|s| matches!(s, Stmt::Var(VarKind::Let | VarKind::Const, _) | Stmt::Class(..) | Stmt::Function(_)))
    }

    pub fn exec_block(&mut self, body: &[Stmt], scope: &Scope) -> R<()> {
        let scope = if Self::needs_scope(body) {
            let inner = self.new_scope(Some(scope.clone()), None);
            self.declare_lexical(body, &inner);
            self.hoist_functions(body, &inner, false)?;
            // Annex B: a function declared in a block is also a var of
            // the enclosing function.
            for statement in body {
                if let Stmt::Function(func) = statement {
                    if let Some(name) = &func.name {
                        let value = self.lookup(name, &inner)?;
                        let _ = self.assign_var_scope(name, value, scope);
                    }
                }
            }
            inner
        } else {
            scope.clone()
        };
        for statement in body {
            self.exec(statement, &scope)?;
        }
        Ok(())
    }

    /// Sets a var-scoped name found in an enclosing scope, without
    /// creating a global.
    fn assign_var_scope(&mut self, name: &JsStr, value: Value, scope: &Scope) -> R<()> {
        if self.is_declared(name, scope) {
            self.assign(name, value, scope)
        } else {
            Ok(())
        }
    }

    pub fn exec(&mut self, statement: &Stmt, scope: &Scope) -> R<()> {
        self.tick()?;
        match statement {
            Stmt::Expr(expr) => {
                self.eval(expr, scope)?;
                Ok(())
            }
            Stmt::Var(kind, declarations) => {
                for (pattern, init) in declarations {
                    match init {
                        Some(init) => {
                            let value = self.eval_named(init, scope, pattern)?;
                            self.bind_pattern(pattern, value, scope, Some(*kind))?;
                        }
                        None => {
                            if *kind == VarKind::Var {
                                // `var x;` does not reset x.
                                continue;
                            }
                            self.bind_pattern(pattern, Value::Undefined, scope, Some(*kind))?;
                        }
                    }
                }
                Ok(())
            }
            Stmt::Function(_) => Ok(()),
            Stmt::Class(name, class) => {
                let value = self.eval_class(class, scope)?;
                scope.declare(name.clone(), value, MUTABLE);
                Ok(())
            }
            Stmt::Return(value) => {
                let value = match value {
                    Some(expr) => self.eval(expr, scope)?,
                    None => Value::Undefined,
                };
                Err(Ctrl::Return(value))
            }
            Stmt::If(test, then, otherwise) => {
                if self.eval(test, scope)?.truthy() {
                    self.exec(then, scope)
                } else if let Some(otherwise) = otherwise {
                    self.exec(otherwise, scope)
                } else {
                    Ok(())
                }
            }
            Stmt::Block(body) => self.exec_block(body, scope),
            Stmt::Labeled(label, body) => match self.exec_loop(body, scope, Some(label)) {
                Err(Ctrl::Break(Some(target))) if target == *label => Ok(()),
                other => other,
            },
            Stmt::For(..) | Stmt::ForIn(..) | Stmt::ForOf(..) | Stmt::While(..) | Stmt::DoWhile(..) => {
                self.exec_loop(statement, scope, None)
            }
            Stmt::Break(label) => Err(Ctrl::Break(label.clone())),
            Stmt::Continue(label) => Err(Ctrl::Continue(label.clone())),
            Stmt::Throw(expr) => {
                let value = self.eval(expr, scope)?;
                Err(Ctrl::Throw(value))
            }
            Stmt::Try(block, catch, finally) => {
                let mut result = self.exec_block(block, scope);
                if let (Err(Ctrl::Throw(error)), Some((binding, body))) = (&result, catch) {
                    let error = error.clone();
                    let catch_scope = self.new_scope(Some(scope.clone()), None);
                    result = match binding {
                        Some(pattern) => match self.bind_pattern(pattern, error, &catch_scope, Some(VarKind::Let)) {
                            Ok(()) => self.exec_block(body, &catch_scope),
                            Err(e) => Err(e),
                        },
                        None => self.exec_block(body, &catch_scope),
                    };
                }
                if let Some(finally) = finally {
                    // A halt goes through finally blocks untouched.
                    if matches!(result, Err(Ctrl::Halt(_))) {
                        return result;
                    }
                    self.exec_block(finally, scope)?;
                }
                result
            }
            Stmt::Switch(discriminant, cases) => {
                let value = self.eval(discriminant, scope)?;
                let inner = self.new_scope(Some(scope.clone()), None);
                let all: Vec<Stmt> = cases.iter().flat_map(|(_, body)| body.iter().cloned()).collect();
                self.declare_lexical(&all, &inner);
                self.hoist_functions(&all, &inner, false)?;
                let mut matched = None;
                for (index, (test, _)) in cases.iter().enumerate() {
                    if let Some(test) = test {
                        let candidate = self.eval(test, &inner)?;
                        if value.strict_eq(&candidate) {
                            matched = Some(index);
                            break;
                        }
                    }
                }
                if matched.is_none() {
                    matched = cases.iter().position(|(test, _)| test.is_none());
                }
                let Some(start) = matched else {
                    return Ok(());
                };
                for (_, body) in &cases[start..] {
                    for statement in body {
                        match self.exec(statement, &inner) {
                            Err(Ctrl::Break(None)) => return Ok(()),
                            other => other?,
                        }
                    }
                }
                Ok(())
            }
            Stmt::Empty => Ok(()),
        }
    }

    /// Runs a loop body once: Ok(true) to go on, Ok(false) to stop.
    fn loop_body(&mut self, body: &Stmt, scope: &Scope, label: Option<&JsStr>) -> R<bool> {
        match self.exec(body, scope) {
            Ok(()) => Ok(true),
            Err(Ctrl::Break(None)) => Ok(false),
            Err(Ctrl::Continue(None)) => Ok(true),
            Err(Ctrl::Continue(Some(target))) if label.is_some_and(|l| *l == target) => Ok(true),
            Err(other) => Err(other),
        }
    }

    fn exec_loop(&mut self, statement: &Stmt, scope: &Scope, label: Option<&JsStr>) -> R<()> {
        match statement {
            Stmt::While(test, body) => {
                while self.eval(test, scope)?.truthy() {
                    self.tick()?;
                    if !self.loop_body(body, scope, label)? {
                        break;
                    }
                }
                Ok(())
            }
            Stmt::DoWhile(body, test) => {
                loop {
                    self.tick()?;
                    if !self.loop_body(body, scope, label)? {
                        break;
                    }
                    if !self.eval(test, scope)?.truthy() {
                        break;
                    }
                }
                Ok(())
            }
            Stmt::For(init, test, update, body) => {
                // `let` in the head: one scope per iteration, copied from
                // the previous, so that closures keep their own `i`.
                let per_iteration = matches!(init, Some(ForInit::Var(VarKind::Let | VarKind::Const, _)));
                let mut loop_scope = if per_iteration { self.new_scope(Some(scope.clone()), None) } else { scope.clone() };
                match init {
                    Some(ForInit::Var(kind, declarations)) => {
                        for (pattern, value) in declarations {
                            let value = match value {
                                Some(expr) => self.eval_named(expr, &loop_scope, pattern)?,
                                None => {
                                    if *kind == VarKind::Var {
                                        continue;
                                    }
                                    Value::Undefined
                                }
                            };
                            self.bind_pattern(pattern, value, &loop_scope, Some(*kind))?;
                        }
                    }
                    Some(ForInit::Expr(expr)) => {
                        self.eval(expr, scope)?;
                    }
                    None => {}
                }
                loop {
                    self.tick()?;
                    if let Some(test) = test {
                        if !self.eval(test, &loop_scope)?.truthy() {
                            break;
                        }
                    }
                    if !self.loop_body(body, &loop_scope, label)? {
                        break;
                    }
                    if per_iteration {
                        let copy = self.new_scope(Some(scope.clone()), None);
                        for binding in loop_scope.0.borrow().vars.iter() {
                            copy.declare(binding.name.clone(), binding.value.clone(), binding.state);
                        }
                        loop_scope = copy;
                    }
                    if let Some(update) = update {
                        self.eval(update, &loop_scope)?;
                    }
                }
                Ok(())
            }
            Stmt::ForIn(kind, pattern, object, body) => {
                let object = self.eval(object, scope)?;
                if object.is_nullish() {
                    return Ok(());
                }
                let keys = ops::enumerable_keys_with_protos(self, &object)?;
                for key in keys {
                    self.tick()?;
                    // Keys deleted during the loop are skipped.
                    if !ops::has_property(self, &object, &key)? {
                        continue;
                    }
                    let iteration = if kind.is_some_and(|k| k != VarKind::Var) {
                        self.new_scope(Some(scope.clone()), None)
                    } else {
                        scope.clone()
                    };
                    self.bind_pattern(pattern, Value::Str(key), &iteration, *kind)?;
                    if !self.loop_body(body, &iteration, label)? {
                        break;
                    }
                }
                Ok(())
            }
            Stmt::ForOf(kind, pattern, iterable, body) => {
                let iterable = self.eval(iterable, scope)?;
                let mut iterator = ops::get_iterator(self, &iterable)?;
                loop {
                    self.tick()?;
                    let Some(value) = ops::iterator_step(self, &mut iterator)? else {
                        break;
                    };
                    let iteration = if kind.is_some_and(|k| k != VarKind::Var) {
                        self.new_scope(Some(scope.clone()), None)
                    } else {
                        scope.clone()
                    };
                    if let Err(error) = self.bind_pattern(pattern, value, &iteration, *kind) {
                        ops::iterator_close(self, &iterator);
                        return Err(error);
                    }
                    match self.loop_body(body, &iteration, label) {
                        Ok(true) => {}
                        Ok(false) => {
                            ops::iterator_close(self, &iterator);
                            break;
                        }
                        Err(error) => {
                            if !matches!(error, Ctrl::Halt(_)) {
                                ops::iterator_close(self, &iterator);
                            }
                            return Err(error);
                        }
                    }
                }
                Ok(())
            }
            other => self.exec(other, scope),
        }
    }

    // --------------------------------------------------------
    //  Patterns
    // --------------------------------------------------------

    /// Binds a value to a pattern: declares (`kind` given) or assigns.
    pub fn bind_pattern(&mut self, pattern: &Pattern, value: Value, scope: &Scope, kind: Option<VarKind>) -> R<()> {
        match pattern {
            Pattern::Ident(name) => match kind {
                Some(VarKind::Var) => {
                    if self.is_declared(name, scope) {
                        self.assign(name, value, scope)
                    } else {
                        let global = Value::Obj(self.realm.global.clone());
                        ops::set(self, &global, name.clone(), value)?;
                        Ok(())
                    }
                }
                Some(VarKind::Let) => {
                    scope.declare(name.clone(), value, MUTABLE);
                    Ok(())
                }
                Some(VarKind::Const) => {
                    scope.declare(name.clone(), value, CONSTANT);
                    Ok(())
                }
                None => self.assign(name, value, scope),
            },
            Pattern::Target(target) => self.assign_target(target, value, scope),
            Pattern::Object(properties, rest) => {
                if value.is_nullish() {
                    return self.throw_type("Cannot destructure 'undefined' or 'null'.");
                }
                let mut used: Vec<JsStr> = Vec::new();
                for (key, target, default) in properties {
                    let key = match key {
                        PropKey::Name(name) => name.clone(),
                        PropKey::Computed(expr) => {
                            let key = self.eval(expr, scope)?;
                            ops::to_property_key(self, &key)?
                        }
                        PropKey::Private(name) => private_key(name),
                    };
                    let mut element = ops::get_key(self, &value, &key)?;
                    if element.is_undefined() {
                        if let Some(default) = default {
                            element = self.eval_named(default, scope, target)?;
                        }
                    }
                    used.push(key);
                    self.bind_pattern(target, element, scope, kind)?;
                }
                if let Some(rest) = rest {
                    let copy = self.new_object();
                    for key in ops::own_enumerable_keys(self, &value)? {
                        if used.iter().any(|u| *u == key) {
                            continue;
                        }
                        let element = ops::get_key(self, &value, &key)?;
                        copy.put_key(key, element, DEFAULT);
                    }
                    self.bind_pattern(rest, Value::Obj(copy), scope, kind)?;
                }
                Ok(())
            }
            Pattern::Array(elements, rest) => {
                let mut iterator = ops::get_iterator(self, &value)?;
                let mut done = false;
                for element in elements {
                    let item = if done {
                        Value::Undefined
                    } else {
                        match ops::iterator_step(self, &mut iterator)? {
                            Some(item) => item,
                            None => {
                                done = true;
                                Value::Undefined
                            }
                        }
                    };
                    if let Some((target, default)) = element {
                        let item = match (item.is_undefined(), default) {
                            (true, Some(default)) => self.eval_named(default, scope, target)?,
                            _ => item,
                        };
                        self.bind_pattern(target, item, scope, kind)?;
                    }
                }
                if let Some(rest) = rest {
                    let mut remaining = Vec::new();
                    if !done {
                        while let Some(item) = ops::iterator_step(self, &mut iterator)? {
                            self.tick()?;
                            remaining.push(item);
                        }
                    }
                    let array = self.new_array(remaining);
                    self.bind_pattern(rest, Value::Obj(array), scope, kind)?;
                } else if !done {
                    ops::iterator_close(self, &iterator);
                }
                Ok(())
            }
        }
    }

    fn assign_target(&mut self, target: &Expr, value: Value, scope: &Scope) -> R<()> {
        match target {
            Expr::Member(object, name, _) => {
                let object = self.eval(object, scope)?;
                ops::set(self, &object, name.clone(), value)?;
                Ok(())
            }
            Expr::Index(object, index, _) => {
                let object = self.eval(object, scope)?;
                let index = self.eval(index, scope)?;
                ops::set_value_key(self, &object, &index, value)?;
                Ok(())
            }
            Expr::PrivateMember(object, name) => {
                let object = self.eval(object, scope)?;
                ops::set(self, &object, private_key(name), value)?;
                Ok(())
            }
            Expr::SuperMember(key) => {
                let key = self.eval(key, scope)?;
                let key = ops::to_property_key(self, &key)?;
                let this = self.this_value(scope)?;
                ops::set(self, &this, key, value)?;
                Ok(())
            }
            Expr::Ident(name) => self.assign(name, value, scope),
            _ => self.throw_error("SyntaxError", "Invalid assignment target"),
        }
    }

    // --------------------------------------------------------
    //  Expressions
    // --------------------------------------------------------

    /// Evaluates an initializer, naming anonymous functions after their
    /// binding (`const f = () => {}` has `f.name === "f"`).
    fn eval_named(&mut self, expr: &Expr, scope: &Scope, target: &Pattern) -> R<Value> {
        let value = self.eval(expr, scope)?;
        if let (Pattern::Ident(name), Expr::Function(func), Value::Obj(object)) = (target, expr, &value) {
            if func.name.is_none() {
                object.borrow_mut().props.set(JsStr::from("name"), Slot::Data(Value::Str(name.clone()), CONFIGURABLE));
            }
        }
        if let (Pattern::Ident(name), Expr::Class(class), Value::Obj(object)) = (target, expr, &value) {
            if class.name.is_none() {
                object.borrow_mut().props.set(JsStr::from("name"), Slot::Data(Value::Str(name.clone()), CONFIGURABLE));
            }
        }
        Ok(value)
    }

    pub fn eval(&mut self, expr: &Expr, scope: &Scope) -> R<Value> {
        match expr {
            Expr::Num(n) => Ok(Value::Num(*n)),
            Expr::Str(s) => Ok(Value::Str(s.clone())),
            Expr::Ident(name) => self.lookup(name, scope),
            Expr::This => self.this_value(scope),
            Expr::Null => Ok(Value::Null),
            Expr::Undefined => Ok(Value::Undefined),
            Expr::Bool(b) => Ok(Value::Bool(*b)),
            Expr::Member(object, name, optional) => {
                let object = self.eval(object, scope)?;
                if *optional && object.is_nullish() {
                    return Err(Ctrl::Break(Some(JsStr::from("\0chain"))));
                }
                ops::get_key_reading(self, &object, name)
            }
            Expr::Index(object, index, optional) => {
                let object = self.eval(object, scope)?;
                if *optional && object.is_nullish() {
                    return Err(Ctrl::Break(Some(JsStr::from("\0chain"))));
                }
                let index = self.eval(index, scope)?;
                ops::get_value_key(self, &object, &index)
            }
            Expr::Call(callee, args, optional) => self.eval_call(callee, args, *optional, scope),
            _ => self.eval_slow(expr, scope),
        }
    }

    fn eval_slow(&mut self, expr: &Expr, scope: &Scope) -> R<Value> {
        self.depth += 1;
        if self.depth > 64 && self.depth % 8 == 0 {
            if let Err(error) = self.check_stack() {
                self.depth -= 1;
                return Err(error);
            }
        }
        let result = self.eval_other(expr, scope);
        self.depth -= 1;
        result
    }

    fn eval_other(&mut self, expr: &Expr, scope: &Scope) -> R<Value> {
        match expr {
            Expr::Template(parts, substitutions) => {
                let mut out = String::from(parts[0].as_str());
                for (index, substitution) in substitutions.iter().enumerate() {
                    let value = self.eval(substitution, scope)?;
                    if let Value::Sym(_) = value {
                        return self.throw_type("Cannot convert a Symbol value to a string");
                    }
                    let text = ops::to_string(self, &value)?;
                    out.push_str(&text);
                    if let Some(part) = parts.get(index + 1) {
                        out.push_str(part);
                    }
                }
                Ok(Value::Str(JsStr::from(out)))
            }
            Expr::Tagged(tag, strings, substitutions) => {
                let (function, this) = self.callee_and_this(tag, scope)?;
                let cooked: Vec<Value> = strings.0.iter().map(|s| s.clone().map_or(Value::Undefined, Value::Str)).collect();
                let raw: Vec<Value> = strings.1.iter().map(|s| Value::Str(s.clone())).collect();
                let raw_array = self.new_array(raw);
                let strings_array = self.new_array(cooked);
                strings_array.put_hidden("raw", Value::Obj(raw_array));
                let mut args = alloc::vec![Value::Obj(strings_array)];
                for substitution in substitutions {
                    args.push(self.eval(substitution, scope)?);
                }
                self.call(&function, this, &args)
            }
            Expr::Regex(pattern, flags) => super::builtins::regexp::create(self, pattern, flags),
            Expr::Array(elements) => {
                let mut values = Vec::with_capacity(elements.len());
                for element in elements {
                    match element {
                        None => values.push(Value::Undefined),
                        Some(Arg::Plain(expr)) => values.push(self.eval(expr, scope)?),
                        Some(Arg::Spread(expr)) => {
                            let iterable = self.eval(expr, scope)?;
                            let mut iterator = ops::get_iterator(self, &iterable)?;
                            while let Some(item) = ops::iterator_step(self, &mut iterator)? {
                                self.tick()?;
                                values.push(item);
                            }
                        }
                    }
                }
                Ok(Value::Obj(self.new_array(values)))
            }
            Expr::Object(properties) => self.eval_object(properties, scope),
            Expr::Function(func) => self.make_function(func.clone(), scope, None),
            Expr::Class(class) => self.eval_class(class, scope),
            Expr::Unary(op, operand) => self.eval_unary(*op, operand, scope),
            Expr::Update(increment, prefix, target) => {
                let old = self.eval(target, scope)?;
                let old = ops::to_numeric(self, &old)?;
                let new = if *increment { old.add(Num::ONE) } else { old.sub(Num::ONE) };
                let pattern = match target.as_ref() {
                    Expr::Ident(name) => Pattern::Ident(name.clone()),
                    other => Pattern::Target(Box::new(other.clone())),
                };
                self.bind_pattern(&pattern, Value::Num(new), scope, None)?;
                Ok(Value::Num(if *prefix { new } else { old }))
            }
            Expr::Binary(op, left, right) => {
                let left = self.eval(left, scope)?;
                let right = self.eval(right, scope)?;
                ops::binary(self, *op, &left, &right)
            }
            Expr::Logic(op, left, right) => {
                let left = self.eval(left, scope)?;
                let short = match op {
                    LogicOp::And => !left.truthy(),
                    LogicOp::Or => left.truthy(),
                    LogicOp::Nullish => !left.is_nullish(),
                };
                if short {
                    Ok(left)
                } else {
                    self.eval(right, scope)
                }
            }
            Expr::Assign(op, target, value) => self.eval_assign(*op, target, value, scope),
            Expr::Cond(test, then, otherwise) => {
                if self.eval(test, scope)?.truthy() {
                    self.eval(then, scope)
                } else {
                    self.eval(otherwise, scope)
                }
            }
            Expr::New(callee, args) => {
                let constructor = self.eval(callee, scope)?;
                let args = self.eval_args(args, scope)?;
                if !constructor.as_obj().is_some_and(|c| c.is_callable()) {
                    let message = alloc::format!("{} is not a constructor", describe_callee(callee));
                    return self.throw_type(&message);
                }
                self.construct(&constructor, &args, None)
            }
            Expr::PrivateMember(object, name) => {
                let object = self.eval(object, scope)?;
                ops::get_key(self, &object, &private_key(name))
            }
            Expr::PrivateIn(name, object) => {
                let object = self.eval(object, scope)?;
                let Some(object) = object.as_obj() else {
                    return self.throw_type("Cannot use 'in' operator to search for a private field in a non-object");
                };
                let key = private_key(name);
                Ok(Value::Bool(object.borrow().props.get(&key).is_some()))
            }
            Expr::Chain(inner) => match self.eval(inner, scope) {
                Err(Ctrl::Break(Some(label))) if label.as_str() == "\0chain" => Ok(Value::Undefined),
                other => other,
            },
            Expr::Seq(list) => {
                let mut last = Value::Undefined;
                for expr in list {
                    last = self.eval(expr, scope)?;
                }
                Ok(last)
            }
            Expr::Await(operand) => {
                let value = self.eval(operand, scope)?;
                self.await_value(value)
            }
            Expr::Yield(value, delegate) => {
                let value = match value {
                    Some(expr) => self.eval(expr, scope)?,
                    None => Value::Undefined,
                };
                let items = if *delegate {
                    let mut iterator = ops::get_iterator(self, &value)?;
                    let mut items = Vec::new();
                    while let Some(item) = ops::iterator_step(self, &mut iterator)? {
                        self.tick()?;
                        items.push(item);
                    }
                    items
                } else {
                    alloc::vec![value]
                };
                let Some(collector) = self.generators.last_mut() else {
                    return self.throw_error("SyntaxError", "yield outside a generator");
                };
                for item in items {
                    collector.push(item);
                    if collector.len() >= GENERATOR_LIMIT {
                        return Err(Ctrl::GeneratorFull);
                    }
                }
                Ok(Value::Undefined)
            }
            Expr::SuperCall(args) => {
                let args = self.eval_args(args, scope)?;
                let Some(frame) = scope.frame() else {
                    return self.throw_error("SyntaxError", "'super' keyword unexpected here");
                };
                let Some(function) = frame.function.clone() else {
                    return self.throw_error("SyntaxError", "'super' keyword unexpected here");
                };
                let parent = function.proto();
                let Some(parent) = parent.filter(|p| p.is_callable()) else {
                    return self.throw_type("Super constructor is not a constructor");
                };
                if frame.this.borrow().is_some() {
                    return self.throw_error("ReferenceError", "Super constructor may only be called once");
                }
                let this = self.construct(&Value::Obj(parent), &args, Some(frame.new_target.clone()))?;
                *frame.this.borrow_mut() = Some(this.clone());
                self.initialize_fields(&function, &this)?;
                Ok(this)
            }
            Expr::SuperMember(key) => {
                let key = self.eval(key, scope)?;
                let key = ops::to_property_key(self, &key)?;
                let this = self.this_value(scope)?;
                let home = scope.frame().and_then(|f| f.home.clone());
                let Some(proto) = home.and_then(|h| h.proto()) else {
                    return Ok(Value::Undefined);
                };
                ops::get_from(self, &proto, &key, &this)
            }
            Expr::NewTarget => Ok(scope.frame().map_or(Value::Undefined, |f| f.new_target.clone())),
            Expr::Import(_) => {
                let error = self.error_object(self.realm.protos.type_error.clone(), "TypeError", "import() n'est pas pris en charge");
                super::builtins::promise::rejected(self, Value::Obj(error))
            }
            // Handled by `eval`.
            Expr::Num(_) | Expr::Str(_) | Expr::Ident(_) | Expr::This | Expr::Null | Expr::Undefined | Expr::Bool(_) | Expr::Member(..)
            | Expr::Index(..) | Expr::Call(..) => self.eval(expr, scope),
        }
    }

    fn eval_unary(&mut self, op: UnaryOp, operand: &Expr, scope: &Scope) -> R<Value> {
        match op {
            UnaryOp::TypeOf => {
                let value = match operand {
                    Expr::Ident(name) => self.lookup_for_typeof(name, scope)?,
                    other => self.eval(other, scope)?,
                };
                Ok(Value::str(value.type_of()))
            }
            UnaryOp::Delete => match operand {
                Expr::Member(object, name, _) => {
                    let object = self.eval(object, scope)?;
                    Ok(Value::Bool(ops::delete(self, &object, name)?))
                }
                Expr::Index(object, index, _) => {
                    let object = self.eval(object, scope)?;
                    let index = self.eval(index, scope)?;
                    let key = ops::to_property_key(self, &index)?;
                    Ok(Value::Bool(ops::delete(self, &object, &key)?))
                }
                Expr::Chain(inner) => match self.eval_unary(UnaryOp::Delete, inner, scope) {
                    Err(Ctrl::Break(Some(label))) if label.as_str() == "\0chain" => Ok(Value::Bool(true)),
                    other => other,
                },
                other => {
                    self.eval(other, scope)?;
                    Ok(Value::Bool(true))
                }
            },
            UnaryOp::Void => {
                self.eval(operand, scope)?;
                Ok(Value::Undefined)
            }
            UnaryOp::Not => Ok(Value::Bool(!self.eval(operand, scope)?.truthy())),
            UnaryOp::Neg => {
                let value = self.eval(operand, scope)?;
                Ok(Value::Num(ops::to_numeric(self, &value)?.neg()))
            }
            UnaryOp::Plus => {
                let value = self.eval(operand, scope)?;
                Ok(Value::Num(ops::to_number(self, &value)?))
            }
            UnaryOp::BitNot => {
                let value = self.eval(operand, scope)?;
                let n = ops::to_numeric(self, &value)?;
                Ok(Value::Num(Num::from_i32(!n.to_i32())))
            }
        }
    }

    fn eval_assign(&mut self, op: AssignOp, target: &Pattern, value: &Expr, scope: &Scope) -> R<Value> {
        match op {
            AssignOp::Assign => {
                let result = self.eval_named(value, scope, target)?;
                self.bind_pattern(target, result.clone(), scope, None)?;
                Ok(result)
            }
            AssignOp::Op(binary) => {
                let current = self.read_target(target, scope)?;
                let right = self.eval(value, scope)?;
                let result = ops::binary(self, binary, &current, &right)?;
                self.bind_pattern(target, result.clone(), scope, None)?;
                Ok(result)
            }
            AssignOp::Logic(logic) => {
                let current = self.read_target(target, scope)?;
                let keep = match logic {
                    LogicOp::And => !current.truthy(),
                    LogicOp::Or => current.truthy(),
                    LogicOp::Nullish => !current.is_nullish(),
                };
                if keep {
                    return Ok(current);
                }
                let result = self.eval_named(value, scope, target)?;
                self.bind_pattern(target, result.clone(), scope, None)?;
                Ok(result)
            }
        }
    }

    fn read_target(&mut self, target: &Pattern, scope: &Scope) -> R<Value> {
        match target {
            Pattern::Ident(name) => self.lookup(name, scope),
            Pattern::Target(expr) => self.eval(expr, scope),
            _ => self.throw_error("SyntaxError", "Invalid compound assignment target"),
        }
    }

    pub fn eval_args(&mut self, args: &[Arg], scope: &Scope) -> R<Vec<Value>> {
        let mut values = Vec::with_capacity(args.len());
        for arg in args {
            match arg {
                Arg::Plain(expr) => values.push(self.eval(expr, scope)?),
                Arg::Spread(expr) => {
                    let iterable = self.eval(expr, scope)?;
                    let mut iterator = ops::get_iterator(self, &iterable)?;
                    while let Some(item) = ops::iterator_step(self, &mut iterator)? {
                        values.push(item);
                        if values.len() > 1_000_000 {
                            return self.throw_range("Too many arguments");
                        }
                    }
                }
            }
        }
        Ok(values)
    }

    /// The function a call expression calls, and its `this`.
    fn callee_and_this(&mut self, callee: &Expr, scope: &Scope) -> R<(Value, Value)> {
        match callee {
            Expr::Member(object, name, optional) => {
                let object = self.eval(object, scope)?;
                if *optional && object.is_nullish() {
                    return Err(Ctrl::Break(Some(JsStr::from("\0chain"))));
                }
                let function = ops::get_key_reading(self, &object, name)?;
                Ok((function, object))
            }
            Expr::Index(object, index, optional) => {
                let object = self.eval(object, scope)?;
                if *optional && object.is_nullish() {
                    return Err(Ctrl::Break(Some(JsStr::from("\0chain"))));
                }
                let index = self.eval(index, scope)?;
                let function = ops::get_value_key(self, &object, &index)?;
                Ok((function, object))
            }
            Expr::PrivateMember(object, name) => {
                let object = self.eval(object, scope)?;
                let function = ops::get_key(self, &object, &private_key(name))?;
                Ok((function, object))
            }
            Expr::SuperMember(_) => {
                let function = self.eval(callee, scope)?;
                let this = self.this_value(scope)?;
                Ok((function, this))
            }
            other => Ok((self.eval(other, scope)?, Value::Undefined)),
        }
    }

    fn eval_call(&mut self, callee: &Expr, args: &[Arg], optional: bool, scope: &Scope) -> R<Value> {
        self.tick()?;
        let (function, this) = self.callee_and_this(callee, scope)?;
        if optional && function.is_nullish() {
            return Err(Ctrl::Break(Some(JsStr::from("\0chain"))));
        }
        let args = self.eval_args(args, scope)?;
        if !function.as_obj().is_some_and(|f| f.is_callable()) {
            let message = alloc::format!("{} is not a function", describe_callee(callee));
            return self.throw_type(&message);
        }
        self.call(&function, this, &args)
    }

    fn eval_object(&mut self, properties: &[PropDef], scope: &Scope) -> R<Value> {
        let object = self.new_object();
        for property in properties {
            if let PropKind::Spread = property.kind {
                let source = self.eval(&property.value, scope)?;
                ops::copy_data_properties(self, &object, &source)?;
                continue;
            }
            let key = match &property.key {
                PropKey::Name(name) => name.clone(),
                PropKey::Computed(expr) => {
                    let key = self.eval(expr, scope)?;
                    ops::to_property_key(self, &key)?
                }
                PropKey::Private(name) => private_key(name),
            };
            match property.kind {
                PropKind::Value => {
                    if key.as_str() == "__proto__" && matches!(property.key, PropKey::Name(_)) {
                        let proto = self.eval(&property.value, scope)?;
                        match proto {
                            Value::Obj(proto) => object.borrow_mut().proto = Some(proto),
                            Value::Null => object.borrow_mut().proto = None,
                            _ => {}
                        }
                        continue;
                    }
                    let value = self.eval(&property.value, scope)?;
                    if let (Expr::Function(func), Value::Obj(function)) = (&property.value, &value) {
                        if func.name.is_none() {
                            function.borrow_mut().props.set(JsStr::from("name"), Slot::Data(Value::Str(key.clone()), CONFIGURABLE));
                        }
                    }
                    object.put_key(key, value, DEFAULT);
                }
                PropKind::Method | PropKind::Get | PropKind::Set => {
                    let Expr::Function(func) = &property.value else {
                        continue;
                    };
                    let function = self.make_function(func.clone(), scope, Some(object.clone()))?;
                    let Value::Obj(function) = function else {
                        continue;
                    };
                    match property.kind {
                        PropKind::Method => object.put_key(key, Value::Obj(function), DEFAULT),
                        PropKind::Get => define_accessor(&object, key, Some(function), None, DEFAULT),
                        _ => define_accessor(&object, key, None, Some(function), DEFAULT),
                    }
                }
                PropKind::Spread => {}
            }
        }
        Ok(Value::Obj(object))
    }

    // --------------------------------------------------------
    //  Functions
    // --------------------------------------------------------

    pub fn make_function(&mut self, func: Rc<Func>, scope: &Scope, home: Option<Obj>) -> R<Value> {
        let closure = Closure {
            func: func.clone(),
            scope: scope.clone(),
            this: None,
            home,
            class: None,
            class_scope: None,
            derived: false,
            self_name: func.name.clone(),
        };
        let proto = self.realm.protos.function.clone();
        let function = self.alloc(Kind::Function(Rc::new(closure)), Some(proto), "Function");
        let name = func.name.clone().unwrap_or_else(|| JsStr::from(""));
        let length = func.params.iter().take_while(|(_, default)| default.is_none()).count();
        {
            let mut object = function.borrow_mut();
            object.props.set(JsStr::from("length"), Slot::Data(Value::num(length as i64), CONFIGURABLE));
            object.props.set(JsStr::from("name"), Slot::Data(Value::Str(name), CONFIGURABLE));
        }
        // Ordinary functions and generators get a prototype object.
        if !func.arrow && !func.is_async {
            let prototype = if func.generator {
                let proto = self.realm.protos.generator.clone();
                self.alloc(Kind::Ordinary, Some(proto), "Object")
            } else {
                let prototype = self.new_object();
                prototype.put_key(JsStr::from("constructor"), Value::Obj(function.clone()), HIDDEN);
                prototype
            };
            function.put_key(JsStr::from("prototype"), Value::Obj(prototype), WRITABLE);
        }
        Ok(Value::Obj(function))
    }

    /// Calls a function value.
    pub fn call(&mut self, function: &Value, this: Value, args: &[Value]) -> R<Value> {
        let Some(object) = function.as_obj() else {
            return self.throw_type("value is not a function");
        };
        enum Target {
            Closure(Rc<Closure>),
            Native(NativeFn, Value),
            Bound(Obj, Value, Vec<Value>),
            None,
        }
        let target = match &object.borrow().kind {
            Kind::Function(closure) => Target::Closure(closure.clone()),
            Kind::Native(native, data, _) => Target::Native(*native, data.clone()),
            Kind::Bound(target, bound_this, bound_args) => Target::Bound(target.clone(), bound_this.clone(), bound_args.clone()),
            _ => Target::None,
        };
        match target {
            Target::Closure(closure) => {
                if closure.func.is_ctor || closure.class.is_some() {
                    return self.throw_type("Class constructor cannot be invoked without 'new'");
                }
                self.call_closure(object, &closure, this, args, Value::Undefined)
            }
            Target::Native(native, data) => {
                self.depth += 1;
                if let Err(error) = self.check_stack() {
                    self.depth -= 1;
                    return Err(error);
                }
                let saved = self.new_target.take();
                let result = native(self, &this, args, &data);
                self.new_target = saved;
                self.depth -= 1;
                result
            }
            Target::Bound(target, bound_this, mut bound_args) => {
                bound_args.extend_from_slice(args);
                self.call(&Value::Obj(target), bound_this, &bound_args)
            }
            Target::None => self.throw_type("value is not a function"),
        }
    }

    fn call_closure(&mut self, function: &Obj, closure: &Rc<Closure>, this: Value, args: &[Value], new_target: Value) -> R<Value> {
        self.tick()?;
        self.depth += 1;
        if let Err(error) = self.check_stack() {
            self.depth -= 1;
            return Err(error);
        }
        let func = &closure.func;
        let result = if func.is_async {
            let outcome = self.run_body(function, closure, this, args, new_target);
            match outcome {
                Ok(value) => super::builtins::promise::resolved(self, value),
                Err(Ctrl::Throw(error)) => super::builtins::promise::rejected(self, error),
                Err(other) => Err(other),
            }
        } else if func.generator {
            self.generators.push(Vec::new());
            let outcome = self.run_body(function, closure, this, args, new_target);
            let collected = self.generators.pop().unwrap_or_default();
            match outcome {
                Ok(_) | Err(Ctrl::GeneratorFull) => {
                    let proto = ops::get_key(self, &Value::Obj(function.clone()), &JsStr::from("prototype"))?;
                    let proto = proto.as_obj().cloned().unwrap_or_else(|| self.realm.protos.generator.clone());
                    let iterator = self.alloc(Kind::Iterator(IterState::List(collected, 0)), Some(proto), "Generator");
                    Ok(Value::Obj(iterator))
                }
                Err(other) => Err(other),
            }
        } else {
            self.run_body(function, closure, this, args, new_target)
        };
        self.depth -= 1;
        result
    }

    fn run_body(&mut self, function: &Obj, closure: &Rc<Closure>, this: Value, args: &[Value], new_target: Value) -> R<Value> {
        let func = &closure.func;
        let frame = if func.arrow {
            None
        } else {
            // Sloppy functions called without `this` see the global
            // object.
            let this = if this.is_nullish() && !func.strict && new_target.is_undefined() {
                Value::Obj(self.realm.global.clone())
            } else {
                this
            };
            let this = if closure.derived && !new_target.is_undefined() { None } else { Some(this) };
            Some(Rc::new(Frame {
                this: RefCell::new(this),
                function: Some(function.clone()),
                new_target,
                home: closure.home.clone(),
                arguments: if func.uses_arguments { args.to_vec() } else { Vec::new() },
            }))
        };
        let scope = self.new_scope(Some(closure.scope.clone()), frame.clone());
        // A named function expression sees itself by its name.
        if let Some(name) = &closure.self_name {
            if !func.arrow {
                scope.declare(name.clone(), Value::Obj(function.clone()), MUTABLE);
            }
        }
        if func.uses_arguments && !func.arrow {
            let proto = self.realm.protos.object.clone();
            let arguments = self.alloc(Kind::Array(args.to_vec()), Some(proto), "Arguments");
            arguments.borrow_mut().kind = Kind::Arguments;
            for (index, value) in args.iter().enumerate() {
                arguments.put_key(JsStr::from(alloc::format!("{}", index)), value.clone(), DEFAULT);
            }
            arguments.put_hidden("length", Value::num(args.len() as i64));
            arguments.put_hidden("callee", Value::Obj(function.clone()));
            let iterator = self.realm.symbols.iterator.clone();
            let values = ops::get_key(self, &Value::Obj(self.realm.protos.array.clone()), &JsStr::from("values"))?;
            arguments.put_key(iterator, values, HIDDEN);
            scope.declare(JsStr::from("arguments"), Value::Obj(arguments), MUTABLE);
        }
        // Parameters.
        if func.simple_params {
            for (index, (pattern, _)) in func.params.iter().enumerate() {
                if let Pattern::Ident(name) = pattern {
                    scope.declare(name.clone(), args.get(index).cloned().unwrap_or(Value::Undefined), MUTABLE);
                }
            }
        } else {
            for (index, (pattern, default)) in func.params.iter().enumerate() {
                let mut value = args.get(index).cloned().unwrap_or(Value::Undefined);
                if value.is_undefined() {
                    if let Some(default) = default {
                        value = self.eval_named(default, &scope, pattern)?;
                    }
                }
                self.bind_pattern(pattern, value, &scope, Some(VarKind::Let))?;
            }
            if let Some(rest) = &func.rest {
                let remaining = args.get(func.params.len()..).map(|r| r.to_vec()).unwrap_or_default();
                let array = self.new_array(remaining);
                self.bind_pattern(rest, Value::Obj(array), &scope, Some(VarKind::Let))?;
            }
        }
        // `var` names not already parameters.
        for name in &func.vars {
            if !scope.has_own(name) {
                scope.declare(name.clone(), Value::Undefined, MUTABLE);
            }
        }
        self.declare_lexical(&func.body, &scope);
        self.hoist_functions(&func.body, &scope, false)?;
        let mut result = Value::Undefined;
        for statement in &func.body {
            match self.exec(statement, &scope) {
                Ok(()) => {}
                Err(Ctrl::Return(value)) => {
                    result = value;
                    break;
                }
                Err(other) => return Err(other),
            }
        }
        if let Some(frame) = frame {
            if !frame.new_target.is_undefined() {
                // A constructor returns its `this` unless it returned an
                // object.
                if let Value::Obj(_) = result {
                    return Ok(result);
                }
                return match frame.this.borrow().clone() {
                    Some(this) => Ok(this),
                    None => self.throw_error("ReferenceError", "Must call super constructor in derived class"),
                };
            }
        }
        Ok(result)
    }

    /// `new constructor(...args)`.
    pub fn construct(&mut self, constructor: &Value, args: &[Value], new_target: Option<Value>) -> R<Value> {
        let Some(object) = constructor.as_obj() else {
            return self.throw_type("not a constructor");
        };
        let new_target = new_target.unwrap_or_else(|| constructor.clone());
        enum Target {
            Closure(Rc<Closure>),
            Native(NativeFn, Value),
            Bound(Obj, Vec<Value>),
            None,
        }
        let target = match &object.borrow().kind {
            Kind::Function(closure) => Target::Closure(closure.clone()),
            Kind::Native(native, data, _) => Target::Native(*native, data.clone()),
            Kind::Bound(target, _, bound_args) => Target::Bound(target.clone(), bound_args.clone()),
            _ => Target::None,
        };
        match target {
            Target::Closure(closure) => {
                if closure.func.arrow || closure.func.is_async || closure.func.generator {
                    return self.throw_type("not a constructor");
                }
                if closure.derived {
                    if closure.class.as_ref().is_some_and(|c| c.constructor.is_none()) {
                        // Implicit `constructor(...args) { super(...args) }`.
                        let parent = object.proto().filter(|p| p.is_callable());
                        let Some(parent) = parent else {
                            return self.throw_type("Super constructor is not a constructor");
                        };
                        let this = self.construct(&Value::Obj(parent), args, Some(new_target))?;
                        self.initialize_fields(object, &this)?;
                        return Ok(this);
                    }
                    return self.call_closure(object, &closure, Value::Undefined, args, new_target);
                }
                let proto = ops::get_key(self, &new_target, &JsStr::from("prototype"))?;
                let proto = proto.as_obj().cloned().unwrap_or_else(|| self.realm.protos.object.clone());
                let this = self.alloc(Kind::Ordinary, Some(proto), "Object");
                let this = Value::Obj(this);
                self.initialize_fields(object, &this)?;
                if closure.class.as_ref().is_some_and(|c| c.constructor.is_none()) {
                    return Ok(this);
                }
                self.call_closure(object, &closure, this, args, new_target)
            }
            Target::Native(native, data) => {
                self.depth += 1;
                if let Err(error) = self.check_stack() {
                    self.depth -= 1;
                    return Err(error);
                }
                let saved = self.new_target.replace(new_target);
                let result = native(self, &Value::Undefined, args, &data);
                self.new_target = saved;
                self.depth -= 1;
                result
            }
            Target::Bound(target, mut bound_args) => {
                bound_args.extend_from_slice(args);
                let target = Value::Obj(target);
                let new_target = if new_target.strict_eq(constructor) { target.clone() } else { new_target };
                self.construct(&target, &bound_args, Some(new_target))
            }
            Target::None => self.throw_type("not a constructor"),
        }
    }

    /// The prototype a native constructor gives its object: from
    /// `new.target` (subclasses), else the default.
    pub fn proto_for_new(&mut self, default: Obj) -> R<Obj> {
        match self.new_target.clone() {
            Some(target) => {
                let proto = ops::get_key(self, &target, &JsStr::from("prototype"))?;
                Ok(proto.as_obj().cloned().unwrap_or(default))
            }
            None => Ok(default),
        }
    }

    // --------------------------------------------------------
    //  Classes
    // --------------------------------------------------------

    fn eval_class(&mut self, class: &Rc<Class>, scope: &Scope) -> R<Value> {
        let class_scope = self.new_scope(Some(scope.clone()), None);
        if let Some(name) = &class.name {
            class_scope.declare(name.clone(), Value::Undefined, UNINITIALIZED);
        }
        let (parent, proto_parent) = match &class.extends {
            None => (None, Some(self.realm.protos.object.clone())),
            Some(expr) => {
                let parent = self.eval(expr, &class_scope)?;
                match parent {
                    Value::Null => (None, None),
                    Value::Obj(parent) if parent.is_callable() => {
                        let proto = ops::get_key(self, &Value::Obj(parent.clone()), &JsStr::from("prototype"))?;
                        match proto {
                            Value::Obj(proto) => (Some(parent), Some(proto)),
                            Value::Null => (Some(parent), None),
                            _ => return self.throw_type("Class extends value does not have valid prototype property"),
                        }
                    }
                    _ => return self.throw_type("Class extends value is not a constructor or null"),
                }
            }
        };
        let prototype = self.alloc(Kind::Ordinary, proto_parent, "Object");
        let func = class.constructor.clone().unwrap_or_else(|| {
            Rc::new(Func {
                name: class.name.clone(),
                params: Vec::new(),
                rest: None,
                body: Vec::new(),
                arrow: false,
                is_async: false,
                generator: false,
                expression: false,
                vars: Vec::new(),
                uses_arguments: false,
                simple_params: true,
                source: (0, 0),
                strict: true,
                is_ctor: true,
            })
        });
        let closure = Closure {
            func: func.clone(),
            scope: class_scope.clone(),
            this: None,
            home: Some(prototype.clone()),
            class: Some(class.clone()),
            class_scope: Some(class_scope.clone()),
            derived: class.extends.is_some(),
            self_name: None,
        };
        let function_proto = parent.clone().unwrap_or_else(|| self.realm.protos.function.clone());
        let constructor = self.alloc(Kind::Function(Rc::new(closure)), Some(function_proto), "Function");
        let name = class.name.clone().unwrap_or_else(|| JsStr::from(""));
        constructor.borrow_mut().props.set(JsStr::from("name"), Slot::Data(Value::Str(name), CONFIGURABLE));
        let length = func.params.iter().take_while(|(_, d)| d.is_none()).count();
        constructor.borrow_mut().props.set(JsStr::from("length"), Slot::Data(Value::num(length as i64), CONFIGURABLE));
        constructor.put_key(JsStr::from("prototype"), Value::Obj(prototype.clone()), 0);
        prototype.put_key(JsStr::from("constructor"), Value::Obj(constructor.clone()), HIDDEN);

        // Methods and accessors.
        for member in &class.members {
            if member.field {
                continue;
            }
            let target = if member.is_static { constructor.clone() } else { prototype.clone() };
            let key = self.member_key(&member.key, &class_scope)?;
            let Some(Expr::Function(method)) = &member.value else {
                continue;
            };
            let function = self.make_function(method.clone(), &class_scope, Some(target.clone()))?;
            let Value::Obj(function) = function else {
                continue;
            };
            function.borrow_mut().props.remove(&JsStr::from("prototype"));
            match member.kind {
                PropKind::Get => define_accessor(&target, key, Some(function), None, CONFIGURABLE),
                PropKind::Set => define_accessor(&target, key, None, Some(function), CONFIGURABLE),
                _ => target.put_key(key, Value::Obj(function), HIDDEN),
            }
        }
        if let Some(name) = &class.name {
            class_scope.declare(name.clone(), Value::Obj(constructor.clone()), CONSTANT);
        }
        // Static fields and blocks, in order, with `this` the class.
        let this = Value::Obj(constructor.clone());
        for member in &class.members {
            if !member.field || !member.is_static {
                continue;
            }
            let key = self.member_key(&member.key, &class_scope)?;
            let value = match &member.value {
                Some(Expr::Function(init)) => {
                    let init = self.make_function(init.clone(), &class_scope, Some(constructor.clone()))?;
                    self.call_initializer(&init, this.clone())?
                }
                _ => Value::Undefined,
            };
            constructor.put_key(key, value, DEFAULT);
        }
        for block in &class.static_blocks {
            let init = self.make_function(block.clone(), &class_scope, Some(constructor.clone()))?;
            self.call_initializer(&init, this.clone())?;
        }
        Ok(Value::Obj(constructor))
    }

    fn call_initializer(&mut self, init: &Value, this: Value) -> R<Value> {
        let Some(object) = init.as_obj() else {
            return Ok(Value::Undefined);
        };
        let closure = match &object.borrow().kind {
            Kind::Function(closure) => closure.clone(),
            _ => return Ok(Value::Undefined),
        };
        self.call_closure(object, &closure, this, &[], Value::Undefined)
    }

    fn member_key(&mut self, key: &PropKey, scope: &Scope) -> R<JsStr> {
        Ok(match key {
            PropKey::Name(name) => name.clone(),
            PropKey::Computed(expr) => {
                let key = self.eval(expr, scope)?;
                ops::to_property_key(self, &key)?
            }
            PropKey::Private(name) => private_key(name),
        })
    }

    /// Instance fields of the class `constructor`, on a new `this`.
    fn initialize_fields(&mut self, constructor: &Obj, this: &Value) -> R<()> {
        let (class, class_scope, home) = match &constructor.borrow().kind {
            Kind::Function(closure) => (closure.class.clone(), closure.class_scope.clone(), closure.home.clone()),
            _ => return Ok(()),
        };
        let (Some(class), Some(class_scope)) = (class, class_scope) else {
            return Ok(());
        };
        let Some(target) = this.as_obj().cloned() else {
            return Ok(());
        };
        for member in &class.members {
            if !member.field || member.is_static {
                continue;
            }
            let key = self.member_key(&member.key, &class_scope)?;
            let value = match &member.value {
                Some(Expr::Function(init)) => {
                    let init = self.make_function(init.clone(), &class_scope, home.clone())?;
                    self.call_initializer(&init, this.clone())?
                }
                _ => Value::Undefined,
            };
            target.put_key(key, value, DEFAULT);
        }
        Ok(())
    }

    // --------------------------------------------------------
    //  Await
    // --------------------------------------------------------

    /// Waits in place for a value to settle (see the module comment).
    pub fn await_value(&mut self, value: Value) -> R<Value> {
        let promise = match &value {
            Value::Obj(object) if matches!(object.borrow().kind, Kind::Promise(_)) => object.clone(),
            Value::Obj(_) => {
                // A thenable: adopt it through a promise.
                let then = ops::get_key(self, &value, &JsStr::from("then"))?;
                if !then.as_obj().is_some_and(|t| t.is_callable()) {
                    return Ok(value);
                }
                let promise = super::builtins::promise::resolved(self, value)?;
                match promise {
                    Value::Obj(promise) => promise,
                    other => return Ok(other),
                }
            }
            _ => {
                // Awaiting a plain value still lets queued jobs run first.
                self.drain_some_jobs()?;
                return Ok(value);
            }
        };
        self.await_depth += 1;
        if self.await_depth > 32 {
            self.await_depth -= 1;
            return self.throw_range("await trop imbrique");
        }
        let result = self.await_promise(&promise);
        self.await_depth -= 1;
        result
    }

    fn drain_some_jobs(&mut self) -> R<()> {
        for _ in 0..1000 {
            let Some(job) = self.realm.jobs.pop_front() else {
                break;
            };
            match self.run_job(job) {
                Err(Ctrl::Halt(reason)) => return Err(Ctrl::Halt(reason)),
                Err(Ctrl::Throw(value)) => {
                    let text = self.describe_error(&value);
                    self.report(text);
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn await_promise(&mut self, promise: &Obj) -> R<Value> {
        loop {
            self.tick()?;
            let state = match &promise.borrow().kind {
                Kind::Promise(data) => (data.state, data.value.clone()),
                _ => (PromiseState::Fulfilled, Value::Undefined),
            };
            match state {
                (PromiseState::Fulfilled, value) => return Ok(value),
                (PromiseState::Rejected, reason) => {
                    if let Kind::Promise(data) = &mut promise.borrow_mut().kind {
                        data.handled = true;
                    }
                    return Err(Ctrl::Throw(reason));
                }
                (PromiseState::Pending, _) => {}
            }
            if let Some(job) = self.realm.jobs.pop_front() {
                match self.run_job(job) {
                    Err(Ctrl::Halt(reason)) => return Err(Ctrl::Halt(reason)),
                    Err(Ctrl::Throw(value)) => {
                        let text = self.describe_error(&value);
                        self.report(text);
                    }
                    _ => {}
                }
                continue;
            }
            // No job left: fast-forward to the earliest timer.
            let earliest = self.realm.timers.iter().enumerate().min_by_key(|(_, t)| (t.due, t.id)).map(|(i, t)| (i, t.due));
            match earliest {
                Some((position, due)) => {
                    let now = self.now();
                    if due > now {
                        self.realm.clock_skew += due - now;
                    }
                    if let Err(reason) = self.fire_timer(position) {
                        return Err(Ctrl::Halt(reason));
                    }
                }
                None => {
                    return self.throw_type("await: la promesse ne sera jamais reglee (reseau ou evenement attendu)");
                }
            }
        }
    }
}

/// Private names live in the object's properties under a key no script
/// can spell.
pub fn private_key(name: &str) -> JsStr {
    JsStr::from(alloc::format!("\0#{}", name))
}

pub fn define_accessor(object: &Obj, key: JsStr, getter: Option<Obj>, setter: Option<Obj>, flags: u8) {
    let mut borrowed = object.borrow_mut();
    let (old_get, old_set) = match borrowed.props.get(&key) {
        Some(Slot::Accessor(g, s, _)) => (g.clone(), s.clone()),
        _ => (None, None),
    };
    let slot = Slot::Accessor(getter.or(old_get), setter.or(old_set), flags);
    borrowed.props.set(key, slot);
}

/// A short text naming what was called, for error messages.
fn describe_callee(callee: &Expr) -> String {
    match callee {
        Expr::Ident(name) => String::from(name.as_str()),
        Expr::Member(object, name, _) => alloc::format!("{}.{}", describe_callee(object), name.as_str()),
        Expr::Index(object, _, _) => alloc::format!("{}[...]", describe_callee(object)),
        Expr::This => String::from("this"),
        Expr::Call(inner, _, _) => alloc::format!("{}(...)", describe_callee(inner)),
        _ => String::from("expression"),
    }
}
