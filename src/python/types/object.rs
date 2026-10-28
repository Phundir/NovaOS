//! `object`, `type`, exceptions, generators, iterators, files, and the
//! descriptors (`property`, `staticmethod`, `classmethod`, `super`).
//!
//! The methods of `object` are what `super()` reaches at the end of every
//! MRO: `super().__init__()` in a class deriving from nothing calls
//! `object.__init__`, which refuses extra arguments exactly as CPython's
//! does. The built-in types also carry their operators as methods
//! (`int.__add__`), so a subclass can call `super().__add__(x)`; those are
//! one native function each, told which operator by the data it carries.

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

use super::super::args;
use super::super::ast::{BinOp, CmpOp};
use super::super::dict::Dict;
use super::super::value::*;
use super::super::vm::{PyErr, PyResult, Vm};
use super::{constructor, method};

// ------------------------------------------------------------
//  Making instances
// ------------------------------------------------------------

/// A new instance of `class` (what `object.__new__` and the built-in
/// `__new__` make): the built-in value inside for a subclass of a
/// built-in type -- built from the arguments for an immutable one,
/// empty for a list, dict or set, which `__init__` fills.
pub fn allocate(vm: &mut Vm, class: &Rc<Class>, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let instance = Instance::new(class.clone());
    if !matches!(class.base, Base::Object | Base::Exception | Base::Other) {
        if let Some(make) = constructor(vm, class) {
            let mutable = matches!(class.base, Base::List | Base::Dict | Base::Set);
            let inner = if mutable { make(vm, class, &[], &[])? } else { make(vm, class, a, kw)? };
            *instance.inner.borrow_mut() = inner;
        }
    }
    if class.base == Base::Exception {
        instance.dict.borrow_mut().set_str("args", Value::tuple(a.to_vec()));
    }
    Ok(Value::Instance(instance))
}

/// `object.__new__(cls, ...)` and the `__new__` of built-in types.
fn object_new(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let Some(Value::Class(class)) = a.first() else {
        return vm.type_error("object.__new__(X): X is not a type object");
    };
    let class = class.clone();
    if super::is_builtin(vm, &class) {
        if let Some(make) = constructor(vm, &class) {
            return make(vm, &class, &a[1..], kw);
        }
    }
    if class.base == Base::Object && (a.len() > 1 || !kw.is_empty()) {
        let user_new = class.lookup("__new__").is_some_and(|n| matches!(n, Value::Function(_) | Value::StaticMethod(_)));
        if user_new {
            return vm.type_error("object.__new__() takes exactly one argument (the type to instantiate)");
        }
    }
    allocate(vm, &class, &a[1..], kw)
}

fn object_init(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    if a.len() <= 1 && kw.is_empty() {
        return Ok(Value::None);
    }
    let class = match a.first() {
        Some(v) => vm.class_of(v),
        None => return Ok(Value::None),
    };
    if !matches!(class.base, Base::Object | Base::Other) {
        // The arguments went to the built-in `__new__`.
        return Ok(Value::None);
    }
    let own_init = class.lookup("__init__").is_some_and(|i| !matches!(i, Value::Builtin(_)));
    let user_new = class.lookup("__new__").is_some_and(|n| matches!(n, Value::Function(_) | Value::StaticMethod(_)));
    if own_init {
        return vm.type_error("object.__init__() takes exactly one argument (the instance to initialize)");
    }
    if user_new {
        return Ok(Value::None);
    }
    let message = alloc::format!("{}() takes no arguments", class.name);
    vm.type_error(&message)
}

fn object_init_subclass(_: &mut Vm, _: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    Ok(Value::None)
}

/// `<__main__.Point object at 0x...>`
pub fn default_repr(vm: &mut Vm, value: &Value) -> String {
    let class = vm.class_of(value);
    let module = match class.dict.borrow().get_str("__module__") {
        Some(Value::Str(m)) if &*m != "builtins" => alloc::format!("{}.", m),
        _ => String::new(),
    };
    let qualname = class.qualname.borrow().clone();
    alloc::format!("<{}{} object at {:#x}>", module, qualname, value.pointer().unwrap_or(0))
}

fn object_repr(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let value = a.first().cloned().unwrap_or(Value::None);
    Ok(Value::str(&default_repr(vm, &value)))
}

fn object_str(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let value = a.first().cloned().unwrap_or(Value::None);
    Ok(Value::str(&vm.repr(&value)?))
}

fn object_format(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let value = a.first().cloned().unwrap_or(Value::None);
    let spec = match a.get(1) {
        Some(Value::Str(s)) => s.clone(),
        _ => Rc::from(""),
    };
    if !spec.is_empty() {
        let name = vm.type_name(&value);
        let message = alloc::format!("unsupported format string passed to {}.__format__", name);
        return vm.type_error(&message);
    }
    Ok(Value::str(&vm.str_of(&value)?))
}

fn object_eq(_: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    match (a.first(), a.get(1)) {
        (Some(x), Some(y)) if x.is(y) => Ok(Value::Bool(true)),
        _ => Ok(Value::NotImplemented),
    }
}

fn object_ne(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let (Some(x), Some(y)) = (a.first(), a.get(1)) else {
        return Ok(Value::NotImplemented);
    };
    // The inverse of __eq__, unless that is NotImplemented.
    let eq = match vm.special(x, "__eq__")? {
        Some(m) => vm.call(&m, core::slice::from_ref(y), &[])?,
        None => Value::NotImplemented,
    };
    if matches!(eq, Value::NotImplemented) {
        return Ok(eq);
    }
    Ok(Value::Bool(!vm.truthy(&eq)?))
}

fn object_hash(_: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let p = a.first().and_then(|v| v.pointer()).unwrap_or(0);
    Ok(Value::int((p >> 4) as i64))
}

/// `object.__setattr__`: past a class's own `__setattr__`.
fn object_setattr(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let (Some(object), Some(name), Some(value)) = (a.first(), a.get(1), a.get(2)) else {
        return vm.type_error("expected 3 arguments");
    };
    let name = args::string(vm, name, "attribute name")?;
    match object {
        Value::Instance(i) => {
            let class = i.class();
            match class.lookup(&name) {
                Some(Value::Property(p)) => {
                    if p[1].is_none() {
                        let message = alloc::format!("property '{}' of '{}' object has no setter", name, class.name);
                        return vm.attribute_error(&message);
                    }
                    vm.call(&p[1], &[object.clone(), value.clone()], &[])?;
                    return Ok(Value::None);
                }
                Some(descriptor @ Value::Instance(_)) => {
                    if let Some(set) = vm.special(&descriptor, "__set__")? {
                        vm.call(&set, &[object.clone(), value.clone()], &[])?;
                        return Ok(Value::None);
                    }
                }
                _ => {}
            }
            match &*name {
                "__class__" => {
                    if let Value::Class(c) = value {
                        *i.class.borrow_mut() = c.clone();
                    }
                }
                _ => i.dict.borrow_mut().set_str(&name, value.clone()),
            }
            Ok(Value::None)
        }
        other => {
            vm.set_attr(other, &name, value.clone())?;
            Ok(Value::None)
        }
    }
}

fn object_getattribute(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let (Some(object), Some(name)) = (a.first(), a.get(1)) else {
        return vm.type_error("expected 2 arguments");
    };
    let name = args::string(vm, name, "attribute name")?;
    vm.get_attr(object, &name)
}

fn object_delattr(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let (Some(object), Some(name)) = (a.first(), a.get(1)) else {
        return vm.type_error("expected 2 arguments");
    };
    let name = args::string(vm, name, "attribute name")?;
    if let Value::Instance(i) = object {
        if i.dict.borrow_mut().remove_str(&name).is_none() {
            let message = alloc::format!("'{}' object has no attribute '{}'", i.class().name, name);
            return vm.attribute_error(&message);
        }
        return Ok(Value::None);
    }
    vm.del_attr(object, &name)?;
    Ok(Value::None)
}

fn object_dir(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let value = a.first().cloned().unwrap_or(Value::None);
    Ok(Value::list(attribute_names(vm, &value)))
}

/// The names `dir()` lists: the instance's, then its classes', sorted.
pub fn attribute_names(vm: &mut Vm, value: &Value) -> Vec<Value> {
    let mut names: Vec<Rc<str>> = Vec::new();
    let mut add = |d: &Dict| {
        for key in d.keys() {
            if let Value::Str(s) = key {
                if !names.contains(&s) {
                    names.push(s);
                }
            }
        }
    };
    match value {
        Value::Instance(i) => {
            add(&i.dict.borrow());
            for class in i.class().mro.borrow().iter() {
                add(&class.dict.borrow());
            }
        }
        Value::Class(c) => {
            for class in c.mro.borrow().iter() {
                add(&class.dict.borrow());
            }
        }
        Value::Module(m) => add(&m.dict.borrow()),
        other => {
            let class = vm.class_of(other);
            for class in class.mro.borrow().iter() {
                add(&class.dict.borrow());
            }
        }
    }
    names.sort();
    names.into_iter().map(Value::Str).collect()
}

pub fn new_object(vm: &mut Vm, _class: &Rc<Class>, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    if !a.is_empty() || !kw.is_empty() {
        return vm.type_error("object() takes no arguments");
    }
    let class = vm.types.object.clone();
    Ok(Value::Instance(Instance::new(class)))
}

// ------------------------------------------------------------
//  type
// ------------------------------------------------------------

/// `type(x)` or `type(name, bases, namespace)`.
pub fn new_type(vm: &mut Vm, _class: &Rc<Class>, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    match a {
        [value] if kw.is_empty() => Ok(Value::Class(vm.class_of(value))),
        [name, bases, namespace] => {
            let name = args::string(vm, name, "type.__new__() argument 1")?;
            let bases = match bases {
                Value::Tuple(t) => t.to_vec(),
                _ => return vm.type_error("type.__new__() argument 2 must be tuple"),
            };
            let mut classes = Vec::new();
            for b in bases {
                match b {
                    Value::Class(c) => classes.push(c),
                    other => {
                        let n = vm.type_name(&other);
                        let message = alloc::format!("bases must be types, not {}", n);
                        return vm.type_error(&message);
                    }
                }
            }
            let dict = match namespace {
                Value::Dict(d) => shared(d.borrow().clone()),
                _ => return vm.type_error("type.__new__() argument 3 must be dict"),
            };
            if dict.borrow().get_str("__qualname__").is_none() {
                dict.borrow_mut().set_str("__qualname__", Value::Str(name.clone()));
            }
            if dict.borrow().get_str("__module__").is_none() {
                let module = vm.frames.last().and_then(|f| f.globals.dict.borrow().get_str("__name__")).unwrap_or(Value::str("__main__"));
                dict.borrow_mut().set_str("__module__", module);
            }
            super::make_class(vm, name, classes, dict, Value::None, kw.to_vec())
        }
        _ => vm.type_error("type() takes 1 or 3 arguments"),
    }
}

fn type_mro(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    match a.first() {
        Some(Value::Class(c)) => Ok(Value::list(c.mro.borrow().iter().map(|c| Value::Class(c.clone())).collect())),
        _ => vm.type_error("descriptor 'mro' requires a 'type' object"),
    }
}

/// `int | None` prints as CPython's `types.UnionType`.
fn union_repr(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let value = a.first().cloned().unwrap_or(Value::None);
    let mut parts = Vec::new();
    for member in vm.union_members(&value) {
        parts.push(match &member {
            Value::Class(c) if c.id == vm.types.none_type.id => String::from("None"),
            Value::Class(c) => match c.dict.borrow().get_str("__module__") {
                Some(Value::Str(m)) if &*m != "builtins" => alloc::format!("{}.{}", m, c.qualname.borrow()),
                _ => String::from(&**c.qualname.borrow()),
            },
            other => vm.repr(other)?,
        });
    }
    Ok(Value::str(&parts.join(" | ")))
}

// ------------------------------------------------------------
//  Exceptions
// ------------------------------------------------------------

/// The `args` tuple of an exception.
pub fn exception_args(value: &Value) -> Vec<Value> {
    match value {
        Value::Instance(i) => match i.dict.borrow().get_str("args") {
            Some(Value::Tuple(t)) => t.to_vec(),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

fn exc_init(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let Some(Value::Instance(i)) = a.first() else {
        return Ok(Value::None);
    };
    if let Some((k, _)) = kw.first() {
        let message = alloc::format!("{}() got an unexpected keyword argument '{}'", i.class().name, k);
        return vm.type_error(&message);
    }
    let rest = a[1..].to_vec();
    let class = i.class();
    if class.is_subclass(&vm.types.stop_iteration) {
        i.dict.borrow_mut().set_str("value", rest.first().cloned().unwrap_or(Value::None));
    }
    if class.is_subclass(&vm.types.system_exit) {
        let code = match rest.len() {
            0 => Value::None,
            1 => rest[0].clone(),
            _ => Value::tuple(rest.clone()),
        };
        i.dict.borrow_mut().set_str("code", code);
    }
    if class.is_subclass(&vm.types.os_error) && (2..=3).contains(&rest.len()) {
        i.dict.borrow_mut().set_str("errno", rest[0].clone());
        i.dict.borrow_mut().set_str("strerror", rest[1].clone());
        i.dict.borrow_mut().set_str("filename", rest.get(2).cloned().unwrap_or(Value::None));
    }
    i.dict.borrow_mut().set_str("args", Value::tuple(rest));
    Ok(Value::None)
}

fn exc_str(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let value = a.first().cloned().unwrap_or(Value::None);
    let args = exception_args(&value);
    let class = vm.class_of(&value);
    if class.is_subclass(&vm.types.os_error) {
        if let Value::Instance(i) = &value {
            let errno = i.dict.borrow().get_str("errno");
            let strerror = i.dict.borrow().get_str("strerror");
            let filename = i.dict.borrow().get_str("filename");
            if let (Some(errno), Some(strerror)) = (errno, strerror) {
                if !errno.is_none() {
                    let mut text = alloc::format!("[Errno {}] {}", vm.str_of(&errno)?, vm.str_of(&strerror)?);
                    if let Some(f) = filename.filter(|f| !f.is_none()) {
                        text.push_str(": ");
                        text.push_str(&vm.repr(&f)?);
                    }
                    return Ok(Value::str(&text));
                }
            }
        }
    }
    let text = match args.len() {
        0 => String::new(),
        1 if class.is_subclass(&vm.types.key_error) => vm.repr(&args[0])?,
        1 => vm.str_of(&args[0])?,
        _ => vm.repr(&Value::tuple(args))?,
    };
    Ok(Value::str(&text))
}

fn exc_repr(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let value = a.first().cloned().unwrap_or(Value::None);
    let args = exception_args(&value);
    let name = vm.type_name(&value);
    let mut parts = Vec::new();
    for arg in &args {
        parts.push(vm.repr(arg)?);
    }
    Ok(Value::str(&alloc::format!("{}({})", name, parts.join(", "))))
}

fn exc_with_traceback(_: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    Ok(a.first().cloned().unwrap_or(Value::None))
}

fn exc_add_note(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let (Some(Value::Instance(i)), Some(note)) = (a.first(), a.get(1)) else {
        return vm.type_error("add_note() takes exactly one argument");
    };
    if !matches!(note, Value::Str(_)) {
        let name = vm.type_name(note);
        let message = alloc::format!("note must be a str, not '{}'", name);
        return vm.type_error(&message);
    }
    let notes = i.dict.borrow().get_str("__notes__");
    match notes {
        Some(Value::List(l)) => l.borrow_mut().push(note.clone()),
        _ => i.dict.borrow_mut().set_str("__notes__", Value::list(alloc::vec![note.clone()])),
    }
    Ok(Value::None)
}

// ------------------------------------------------------------
//  Generators and iterators
// ------------------------------------------------------------

fn this_generator(vm: &mut Vm, a: &[Value]) -> PyResult<Rc<RefCell<Generator>>> {
    match a.first() {
        Some(Value::Generator(g)) => Ok(g.clone()),
        _ => vm.type_error("descriptor requires a 'generator' object"),
    }
}

fn gen_send(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let g = this_generator(vm, a)?;
    let value = a.get(1).cloned().unwrap_or(Value::None);
    vm.resume(&g, value, None)
}

fn gen_next(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let g = this_generator(vm, a)?;
    vm.resume(&g, Value::None, None)
}

/// An exception from `throw(type)`, `throw(type, value)`, `throw(e)`.
fn thrown(vm: &mut Vm, kind: &Value, value: Option<&Value>) -> PyResult<PyErr> {
    match kind {
        Value::Class(c) if c.is_subclass(&vm.types.base_exception) => {
            let args: Vec<Value> = match value {
                None | Some(Value::None) => Vec::new(),
                Some(v @ Value::Instance(i)) if i.class().is_subclass(c) => return Ok(PyErr::Exc(v.clone())),
                Some(v) => alloc::vec![v.clone()],
            };
            Ok(PyErr::Exc(vm.call(kind, &args, &[])?))
        }
        Value::Instance(i) if i.class().is_subclass(&vm.types.base_exception) => Ok(PyErr::Exc(kind.clone())),
        _ => Err(vm.type_error::<()>("exceptions must be classes or instances deriving from BaseException, not str").err().unwrap_or(PyErr::Halt(String::new()))),
    }
}

fn gen_throw(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let g = this_generator(vm, a)?;
    let Some(kind) = a.get(1) else {
        return vm.type_error("throw expected at least 1 argument, got 0");
    };
    let err = thrown(vm, kind, a.get(2))?;
    vm.resume(&g, Value::None, Some(err))
}

fn gen_close(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let g = this_generator(vm, a)?;
    let started = g.borrow().frame.as_ref().is_some_and(|f| f.ip > 0);
    if !started {
        // Never started: just finished.
        g.borrow_mut().frame = None;
        return Ok(Value::None);
    }
    let class = vm.types.generator_exit.clone();
    let err = vm.error(&class, "");
    if let PyErr::Exc(Value::Instance(i)) = &err {
        i.dict.borrow_mut().set_str("args", Value::tuple(Vec::new()));
    }
    match vm.resume(&g, Value::None, Some(err)) {
        Ok(_) => {
            let class = vm.types.runtime_error.clone();
            Err(vm.error(&class, "generator ignored GeneratorExit"))
        }
        Err(e) => {
            let exit = vm.types.generator_exit.clone();
            let stop = vm.types.stop_iteration.clone();
            if vm.is_instance_of(&e, &exit) || vm.is_instance_of(&e, &stop) {
                Ok(Value::None)
            } else {
                Err(e)
            }
        }
    }
}

fn self_iter(_: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    Ok(a.first().cloned().unwrap_or(Value::None))
}

fn iter_next_method(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let iterator = a.first().cloned().unwrap_or(Value::None);
    match vm.next(&iterator)? {
        Some(v) => Ok(v),
        None => Err(vm.stop_iteration(Value::None)),
    }
}

// ------------------------------------------------------------
//  Files
// ------------------------------------------------------------

fn this_file(vm: &mut Vm, a: &[Value]) -> PyResult<Rc<RefCell<File>>> {
    let f = match a.first() {
        Some(Value::File(f)) => f.clone(),
        _ => return vm.type_error("descriptor requires a file object"),
    };
    if f.borrow().closed {
        return vm.value_error("I/O operation on closed file.");
    }
    Ok(f)
}

fn unsupported(vm: &mut Vm, what: &str) -> PyErr {
    let class = vm.types.os_error.clone();
    vm.error(&class, what)
}

/// The text of `bytes` in a text-mode file.
fn decode(vm: &mut Vm, bytes: &[u8]) -> PyResult<String> {
    match core::str::from_utf8(bytes) {
        Ok(s) => Ok(s.replace("\r\n", "\n")),
        Err(e) => {
            let class = vm.types.unicode_error.clone();
            let message = alloc::format!("'utf-8' codec can't decode byte 0x{:02x} in position {}: invalid start byte", bytes[e.valid_up_to()], e.valid_up_to());
            Err(vm.error(&class, &message))
        }
    }
}

fn read_value(vm: &mut Vm, file: &Rc<RefCell<File>>, bytes: Vec<u8>) -> PyResult<Value> {
    if file.borrow().binary {
        return Ok(Value::Bytes(Rc::from(bytes)));
    }
    let text = decode(vm, &bytes)?;
    Ok(Value::str(&text))
}

fn file_read(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let f = this_file(vm, a)?;
    if !f.borrow().readable {
        return Err(unsupported(vm, "not readable"));
    }
    let n = args::opt_int(vm, a.get(1), -1)?;
    let bytes = {
        let mut file = f.borrow_mut();
        let start = file.position.min(file.data.len());
        let end = if n < 0 {
            file.data.len()
        } else if file.binary {
            (start + n as usize).min(file.data.len())
        } else {
            // n characters.
            let rest = &file.data[start..];
            let mut count = 0;
            let mut end = start;
            for (i, b) in rest.iter().enumerate() {
                if b & 0xc0 != 0x80 {
                    if count == n as usize {
                        end = start + i;
                        break;
                    }
                    count += 1;
                }
                end = start + i + 1;
            }
            end
        };
        file.position = end;
        file.data[start..end].to_vec()
    };
    read_value(vm, &f, bytes)
}

fn file_readline(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let f = this_file(vm, a)?;
    if !f.borrow().readable {
        return Err(unsupported(vm, "not readable"));
    }
    let bytes = {
        let mut file = f.borrow_mut();
        let start = file.position.min(file.data.len());
        let end = file.data[start..].iter().position(|b| *b == b'\n').map_or(file.data.len(), |p| start + p + 1);
        file.position = end;
        file.data[start..end].to_vec()
    };
    read_value(vm, &f, bytes)
}

fn file_readlines(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let mut lines = Vec::new();
    loop {
        let line = file_readline(vm, &a[..1], &[])?;
        if !vm.truthy(&line)? {
            break;
        }
        lines.push(line);
    }
    Ok(Value::list(lines))
}

fn file_next(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let line = file_readline(vm, &a[..a.len().min(1)], &[])?;
    if !vm.truthy(&line)? {
        return Err(vm.stop_iteration(Value::None));
    }
    Ok(line)
}

/// Writes to a file (or to the console for stdout and stderr).
pub fn write_to(vm: &mut Vm, file: &Rc<RefCell<File>>, value: &Value) -> PyResult<usize> {
    let (binary, writable, closed) = {
        let f = file.borrow();
        (f.binary, f.writable, f.closed)
    };
    if closed {
        return vm.value_error("I/O operation on closed file.");
    }
    if !writable {
        return Err(unsupported(vm, "not writable"));
    }
    let (bytes, count) = match (binary, value) {
        (false, Value::Str(s)) => (s.as_bytes().to_vec(), super::str::char_len(s)),
        (true, Value::Bytes(b)) => (b.to_vec(), b.len()),
        (false, other) => {
            let name = vm.type_name(other);
            let message = alloc::format!("write() argument must be str, not {}", name);
            return vm.type_error(&message);
        }
        (true, other) => {
            let name = vm.type_name(other);
            let message = alloc::format!("a bytes-like object is required, not '{}'", name);
            return vm.type_error(&message);
        }
    };
    let path = file.borrow().path.clone();
    match &*path {
        "<stdout>" | "<stderr>" => {
            let text = String::from_utf8_lossy(&bytes).into_owned();
            vm.host.write(&text, &*path == "<stderr>");
        }
        _ => {
            let mut f = file.borrow_mut();
            if f.append {
                f.position = f.data.len();
            }
            let position = f.position.min(f.data.len());
            let end = (position + bytes.len()).min(f.data.len());
            f.data.splice(position..end, bytes.iter().copied());
            f.position = position + bytes.len();
            f.dirty = true;
        }
    }
    Ok(count)
}

fn file_write(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let f = this_file(vm, a)?;
    let Some(value) = a.get(1) else {
        return vm.type_error("write() takes exactly one argument (0 given)");
    };
    let count = write_to(vm, &f, value)?;
    Ok(Value::int(count as i64))
}

fn file_writelines(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let f = this_file(vm, a)?;
    let lines = vm.collect(a.get(1).unwrap_or(&Value::None))?;
    for line in lines {
        write_to(vm, &f, &line)?;
    }
    Ok(Value::None)
}

/// Writes a file's content back to the disk if it changed.
pub fn flush_file(vm: &mut Vm, file: &Rc<RefCell<File>>) -> PyResult<()> {
    let (path, data, dirty) = {
        let f = file.borrow();
        (f.path.clone(), f.data.clone(), f.dirty)
    };
    if !dirty || path.starts_with('<') {
        return Ok(());
    }
    if let Err(e) = vm.host.write_file(&path, &data) {
        return Err(super::super::modules::os_error(vm, e, &path));
    }
    file.borrow_mut().dirty = false;
    Ok(())
}

fn file_flush(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let f = this_file(vm, a)?;
    flush_file(vm, &f)?;
    Ok(Value::None)
}

fn file_close(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let Some(Value::File(f)) = a.first() else {
        return Ok(Value::None);
    };
    if f.borrow().closed || f.borrow().path.starts_with('<') {
        return Ok(Value::None);
    }
    let f = f.clone();
    let result = flush_file(vm, &f);
    f.borrow_mut().closed = true;
    result?;
    Ok(Value::None)
}

fn file_exit(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    file_close(vm, &a[..a.len().min(1)], &[])?;
    Ok(Value::Bool(false))
}

fn file_seek(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let f = this_file(vm, a)?;
    let offset = args::opt_int(vm, a.get(1), 0)?;
    let whence = args::opt_int(vm, a.get(2), 0)?;
    let mut file = f.borrow_mut();
    let base = match whence {
        0 => 0,
        1 => file.position as i64,
        _ => file.data.len() as i64,
    };
    file.position = (base + offset).max(0) as usize;
    Ok(Value::int(file.position as i64))
}

fn file_tell(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let f = this_file(vm, a)?;
    let position = f.borrow().position;
    Ok(Value::int(position as i64))
}

fn file_true(_: &mut Vm, _: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    Ok(Value::Bool(true))
}

// ------------------------------------------------------------
//  Descriptors
// ------------------------------------------------------------

pub fn new_property(vm: &mut Vm, _class: &Rc<Class>, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let get = |i: usize, name: &str| args::get(a, kw, i, name).cloned().unwrap_or(Value::None);
    let _ = vm;
    Ok(Value::Property(Rc::new([get(0, "fget"), get(1, "fset"), get(2, "fdel")])))
}

fn property_with(vm: &mut Vm, a: &[Value], slot: usize) -> PyResult<Value> {
    let (Some(Value::Property(p)), Some(f)) = (a.first(), a.get(1)) else {
        return vm.type_error("expected a function");
    };
    let mut parts = (**p).clone();
    parts[slot] = f.clone();
    Ok(Value::Property(Rc::new(parts)))
}

fn property_getter(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    property_with(vm, a, 0)
}

fn property_setter(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    property_with(vm, a, 1)
}

fn property_deleter(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    property_with(vm, a, 2)
}

pub fn new_staticmethod(vm: &mut Vm, _class: &Rc<Class>, a: &[Value], _kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    match a.first() {
        Some(f) => Ok(Value::StaticMethod(Rc::new(f.clone()))),
        None => vm.type_error("staticmethod expected 1 argument, got 0"),
    }
}

pub fn new_classmethod(vm: &mut Vm, _class: &Rc<Class>, a: &[Value], _kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    match a.first() {
        Some(f) => Ok(Value::ClassMethod(Rc::new(f.clone()))),
        None => vm.type_error("classmethod expected 1 argument, got 0"),
    }
}

pub fn new_super(vm: &mut Vm, _class: &Rc<Class>, a: &[Value], _kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    match a {
        [Value::Class(start), object] => {
            let ok = match object {
                Value::Class(c) => c.is_subclass(start),
                other => vm.class_of(other).is_subclass(start),
            };
            if !ok {
                return vm.type_error("super(type, obj): obj must be an instance or subtype of type");
            }
            Ok(Value::Super(Rc::new((start.clone(), object.clone()))))
        }
        [] => {
            let class = vm.types.runtime_error.clone();
            Err(vm.error(&class, "super(): no arguments"))
        }
        _ => vm.type_error("super() argument 1 must be a type"),
    }
}

// ------------------------------------------------------------
//  Operators of built-in types, as methods
// ------------------------------------------------------------

enum Dunder {
    Binary(BinOp, bool),
    Compare(CmpOp),
}

const DUNDERS: &[(&str, Dunder)] = &[
    ("__add__", Dunder::Binary(BinOp::Add, false)),
    ("__radd__", Dunder::Binary(BinOp::Add, true)),
    ("__sub__", Dunder::Binary(BinOp::Sub, false)),
    ("__rsub__", Dunder::Binary(BinOp::Sub, true)),
    ("__mul__", Dunder::Binary(BinOp::Mul, false)),
    ("__rmul__", Dunder::Binary(BinOp::Mul, true)),
    ("__truediv__", Dunder::Binary(BinOp::Div, false)),
    ("__rtruediv__", Dunder::Binary(BinOp::Div, true)),
    ("__floordiv__", Dunder::Binary(BinOp::FloorDiv, false)),
    ("__rfloordiv__", Dunder::Binary(BinOp::FloorDiv, true)),
    ("__mod__", Dunder::Binary(BinOp::Mod, false)),
    ("__rmod__", Dunder::Binary(BinOp::Mod, true)),
    ("__pow__", Dunder::Binary(BinOp::Pow, false)),
    ("__rpow__", Dunder::Binary(BinOp::Pow, true)),
    ("__lshift__", Dunder::Binary(BinOp::LShift, false)),
    ("__rshift__", Dunder::Binary(BinOp::RShift, false)),
    ("__and__", Dunder::Binary(BinOp::BitAnd, false)),
    ("__rand__", Dunder::Binary(BinOp::BitAnd, true)),
    ("__or__", Dunder::Binary(BinOp::BitOr, false)),
    ("__ror__", Dunder::Binary(BinOp::BitOr, true)),
    ("__xor__", Dunder::Binary(BinOp::BitXor, false)),
    ("__rxor__", Dunder::Binary(BinOp::BitXor, true)),
    ("__eq__", Dunder::Compare(CmpOp::Eq)),
    ("__ne__", Dunder::Compare(CmpOp::NotEq)),
    ("__lt__", Dunder::Compare(CmpOp::Lt)),
    ("__le__", Dunder::Compare(CmpOp::LtE)),
    ("__gt__", Dunder::Compare(CmpOp::Gt)),
    ("__ge__", Dunder::Compare(CmpOp::GtE)),
];

/// One operator of a built-in type, the index into `DUNDERS` first.
fn dunder(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let (Some(Value::Int(code)), Some(this), Some(other)) = (a.first(), a.get(1), a.get(2)) else {
        return vm.type_error("expected 1 argument");
    };
    let index = code.to_i64().unwrap_or(0) as usize;
    let x = vm.unwrap_builtin(this);
    let y = vm.unwrap_builtin(other);
    if let Value::Instance(_) = y {
        return Ok(Value::NotImplemented);
    }
    match DUNDERS[index].1 {
        Dunder::Binary(op, reflected) => {
            let (p, q) = if reflected { (&y, &x) } else { (&x, &y) };
            Ok(vm.binary_builtin(op, p, q, false)?.unwrap_or(Value::NotImplemented))
        }
        Dunder::Compare(op) => {
            // Orderings only within a family of types.
            let family = |v: &Value| match v {
                Value::Int(_) | Value::Bool(_) | Value::Float(_) => 0,
                Value::Set(_) | Value::FrozenSet(_) => 1,
                Value::Str(_) => 2,
                Value::List(_) => 3,
                Value::Tuple(_) => 4,
                Value::Bytes(_) => 5,
                _ => 6,
            };
            if family(&x) != family(&y) && matches!(op, CmpOp::Lt | CmpOp::LtE | CmpOp::Gt | CmpOp::GtE) {
                return Ok(Value::NotImplemented);
            }
            vm.compare(op, &x, &y)
        }
    }
}

/// `repr`, `str`, `format`, `hash` of the value inside, for subclasses
/// of built-in types.
fn builtin_repr(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let inner = a.first().map(|v| vm.unwrap_builtin(v)).unwrap_or(Value::None);
    Ok(Value::str(&vm.repr(&inner)?))
}

fn builtin_str(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let inner = a.first().map(|v| vm.unwrap_builtin(v)).unwrap_or(Value::None);
    Ok(Value::str(&vm.str_of(&inner)?))
}

fn builtin_format(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let inner = a.first().map(|v| vm.unwrap_builtin(v)).unwrap_or(Value::None);
    let spec = match a.get(1) {
        Some(Value::Str(s)) => s.clone(),
        _ => Rc::from(""),
    };
    Ok(Value::str(&vm.format(&inner, &spec)?))
}

fn builtin_hash(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let inner = a.first().map(|v| vm.unwrap_builtin(v)).unwrap_or(Value::None);
    Ok(Value::int(vm.hash(&inner)?))
}

fn builtin_getitem(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let inner = a.first().map(|v| vm.unwrap_builtin(v)).unwrap_or(Value::None);
    let index = a.get(1).cloned().unwrap_or(Value::None);
    vm.get_item(&inner, &index)
}

fn builtin_setitem(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let inner = a.first().map(|v| vm.unwrap_builtin(v)).unwrap_or(Value::None);
    let index = a.get(1).cloned().unwrap_or(Value::None);
    let value = a.get(2).cloned().unwrap_or(Value::None);
    vm.set_item(&inner, &index, value)?;
    Ok(Value::None)
}

fn builtin_delitem(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let inner = a.first().map(|v| vm.unwrap_builtin(v)).unwrap_or(Value::None);
    let index = a.get(1).cloned().unwrap_or(Value::None);
    vm.del_item(&inner, &index)?;
    Ok(Value::None)
}

fn builtin_contains(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let inner = a.first().map(|v| vm.unwrap_builtin(v)).unwrap_or(Value::None);
    let item = a.get(1).cloned().unwrap_or(Value::None);
    Ok(Value::Bool(vm.contains(&inner, &item)?))
}

fn list_init(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let Some(Value::List(l)) = a.first().map(|v| vm.unwrap_builtin(v)) else {
        return Ok(Value::None);
    };
    let items = match a.get(1) {
        Some(v) => vm.collect(v)?,
        None => Vec::new(),
    };
    *l.borrow_mut() = items;
    Ok(Value::None)
}

pub fn install(vm: &mut Vm) {
    let object = vm.types.object.clone();
    let entries: &[(&'static str, NativeFn)] = &[
        ("__init__", object_init), ("__repr__", object_repr), ("__str__", object_str), ("__format__", object_format),
        ("__eq__", object_eq), ("__ne__", object_ne), ("__hash__", object_hash), ("__setattr__", object_setattr),
        ("__getattribute__", object_getattribute), ("__delattr__", object_delattr), ("__dir__", object_dir),
    ];
    for (name, f) in entries {
        method(&object, name, *f);
    }
    // `__new__` is called with the class: not bound.
    object.dict.borrow_mut().set_str("__new__", super::native("__new__", object_new));
    object.dict.borrow_mut().set_str("__init_subclass__", Value::ClassMethod(Rc::new(super::native("__init_subclass__", object_init_subclass))));

    let type_ = vm.types.type_.clone();
    method(&type_, "mro", type_mro);
    let union = vm.types.union_type.clone();
    union.dict.borrow_mut().set_str("__module__", Value::str("types"));
    method(&union, "__repr__", union_repr);

    let exception = vm.types.base_exception.clone();
    let entries: &[(&'static str, NativeFn)] = &[
        ("__init__", exc_init), ("__str__", exc_str), ("__repr__", exc_repr), ("with_traceback", exc_with_traceback),
        ("add_note", exc_add_note),
    ];
    for (name, f) in entries {
        method(&exception, name, *f);
    }

    let generator = vm.types.generator.clone();
    let entries: &[(&'static str, NativeFn)] = &[
        ("send", gen_send), ("throw", gen_throw), ("close", gen_close), ("__next__", gen_next), ("__iter__", self_iter),
    ];
    for (name, f) in entries {
        method(&generator, name, *f);
    }
    let iterator = vm.types.iterator.clone();
    method(&iterator, "__next__", iter_next_method);
    method(&iterator, "__iter__", self_iter);

    let file = vm.types.file.clone();
    let entries: &[(&'static str, NativeFn)] = &[
        ("read", file_read), ("readline", file_readline), ("readlines", file_readlines), ("write", file_write),
        ("writelines", file_writelines), ("flush", file_flush), ("close", file_close), ("seek", file_seek),
        ("tell", file_tell), ("__enter__", self_iter), ("__exit__", file_exit), ("__iter__", self_iter),
        ("__next__", file_next), ("readable", file_true), ("writable", file_true),
    ];
    for (name, f) in entries {
        method(&file, name, *f);
    }

    let property = vm.types.property.clone();
    method(&property, "getter", property_getter);
    method(&property, "setter", property_setter);
    method(&property, "deleter", property_deleter);

    // Operators and protocols of the built-in types, for subclasses.
    let numeric = [vm.types.int.clone(), vm.types.float.clone()];
    let sequences = [vm.types.str.clone(), vm.types.list.clone(), vm.types.tuple.clone(), vm.types.bytes.clone()];
    let sets = [vm.types.set.clone(), vm.types.frozenset.clone()];
    for (index, (name, kind)) in DUNDERS.iter().enumerate() {
        let dict = [vm.types.dict.clone()];
        let wanted: Vec<&Rc<Class>> = match kind {
            Dunder::Compare(CmpOp::Eq | CmpOp::NotEq) => numeric.iter().chain(sequences.iter()).chain(sets.iter()).chain(dict.iter()).collect(),
            Dunder::Compare(_) => numeric.iter().chain(sequences.iter()).chain(sets.iter()).collect(),
            Dunder::Binary(BinOp::Add | BinOp::Mul, _) => numeric.iter().chain(sequences.iter()).collect(),
            Dunder::Binary(BinOp::Mod, false) => numeric.iter().chain(sequences[..1].iter()).collect(),
            Dunder::Binary(BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor | BinOp::Sub, _) => {
                let mut v: Vec<&Rc<Class>> = numeric.iter().collect();
                if !matches!(kind, Dunder::Binary(_, true)) {
                    v.extend(sets.iter());
                }
                v
            }
            Dunder::Binary(..) => numeric.iter().collect(),
        };
        for class in wanted {
            if class.id == vm.types.float.id && matches!(kind, Dunder::Binary(BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor | BinOp::LShift | BinOp::RShift, _)) {
                continue;
            }
            let builtin = Builtin { name, call: dunder, method: true, data: Value::int(index as i64) };
            class.dict.borrow_mut().set_str(name, Value::Builtin(Rc::new(builtin)));
        }
    }
    let all = [
        vm.types.int.clone(), vm.types.float.clone(), vm.types.str.clone(), vm.types.list.clone(), vm.types.tuple.clone(),
        vm.types.bytes.clone(), vm.types.dict.clone(), vm.types.set.clone(), vm.types.frozenset.clone(),
    ];
    for class in &all {
        method(class, "__repr__", builtin_repr);
        method(class, "__format__", builtin_format);
        class.dict.borrow_mut().set_str("__new__", super::native("__new__", object_new));
        if !matches!(class.base, Base::List | Base::Dict | Base::Set) {
            method(class, "__hash__", builtin_hash);
        }
    }
    for class in [&vm.types.list, &vm.types.dict, &vm.types.set] {
        class.dict.borrow_mut().set_str("__hash__", Value::None);
    }
    method(&vm.types.str, "__str__", builtin_str);
    for class in [vm.types.str.clone(), vm.types.list.clone(), vm.types.tuple.clone(), vm.types.bytes.clone(), vm.types.range.clone()] {
        method(&class, "__getitem__", builtin_getitem);
        method(&class, "__contains__", builtin_contains);
    }
    method(&vm.types.list, "__setitem__", builtin_setitem);
    method(&vm.types.list, "__delitem__", builtin_delitem);
    method(&vm.types.list, "__init__", list_init);
}
