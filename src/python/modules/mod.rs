//! `import`: the program's own modules, and the standard library.
//!
//! `import x` looks in `sys.modules`, then among the native modules
//! (`math`, `random`, `sys`, `time`, `os`, `json`, `re`, `traceback`),
//! then among the modules shipped as Python source (`lib/*.py`: written in
//! Python because that is the shortest correct way to write
//! `collections` or `itertools`), then in the program's directory: `x.py`,
//! or `x/__init__.py` for a package. A module is in `sys.modules` before
//! its body runs, so a cycle of imports sees the partial module, as in
//! CPython; a module whose body fails is removed again.

pub mod codec;
pub mod json;
pub mod libm;
pub mod math;
pub mod random;
pub mod re;
pub mod system;

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use super::dict::Dict;
use super::types::native;
use super::value::*;
use super::vm::{PyErr, PyResult, Vm};
use super::{FileError, Mode};

/// The modules written in Python, compiled on first import.
const SOURCES: &[(&str, &str)] = &[
    ("_asyncgen", include_str!("lib/_asyncgen.py")),
    ("abc", include_str!("lib/abc.py")),
    ("asyncio", include_str!("lib/asyncio.py")),
    ("cmath", include_str!("lib/cmath.py")),
    ("bisect", include_str!("lib/bisect.py")),
    ("collections", include_str!("lib/collections.py")),
    ("collections.abc", include_str!("lib/collections_abc.py")),
    ("fractions", include_str!("lib/fractions.py")),
    ("numbers", include_str!("lib/numbers.py")),
    ("statistics", include_str!("lib/statistics.py")),
    ("contextlib", include_str!("lib/contextlib.py")),
    ("copy", include_str!("lib/copy.py")),
    ("dataclasses", include_str!("lib/dataclasses.py")),
    ("datetime", include_str!("lib/datetime.py")),
    ("enum", include_str!("lib/enum.py")),
    ("functools", include_str!("lib/functools.py")),
    ("heapq", include_str!("lib/heapq.py")),
    ("itertools", include_str!("lib/itertools.py")),
    ("operator", include_str!("lib/operator.py")),
    ("string", include_str!("lib/string.py")),
    ("typing", include_str!("lib/typing.py")),
    ("fnmatch", include_str!("lib/fnmatch.py")),
    ("glob", include_str!("lib/glob.py")),
    ("pathlib", include_str!("lib/pathlib.py")),
    ("base64", include_str!("lib/base64.py")),
    ("binascii", include_str!("lib/binascii.py")),
    ("hashlib", include_str!("lib/hashlib.py")),
    ("hmac", include_str!("lib/hmac.py")),
    ("io", include_str!("lib/io.py")),
    ("struct", include_str!("lib/struct.py")),
    ("textwrap", include_str!("lib/textwrap.py")),
    ("zlib", include_str!("lib/zlib.py")),
];

/// The names `help('modules')` would list: native and shipped.
pub fn library() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = alloc::vec!["math", "random", "sys", "time", "os", "os.path", "json", "re", "traceback"];
    names.extend(SOURCES.iter().map(|(n, _)| *n));
    names
}

/// A new module registered in `sys.modules`.
pub fn new_module(vm: &mut Vm, name: &str, file: &str) -> Rc<Module> {
    let dict = shared_dict(Dict::new());
    {
        let mut d = dict.borrow_mut();
        d.set_str("__name__", Value::str(name));
        d.set_str("__doc__", Value::None);
        if !file.is_empty() {
            d.set_str("__file__", Value::str(file));
        }
        d.set_str("__builtins__", Value::Module(vm.builtins.clone()));
    }
    let module = Rc::new(Module { name: Rc::from(name), dict, file: Rc::from(file) });
    vm.modules.retain(|(n, _)| &**n != name);
    vm.modules.push((Rc::from(name), Value::Module(module.clone())));
    module
}

/// A native module made of functions (and later constants).
pub fn native_module(vm: &mut Vm, name: &str, functions: &[(&'static str, NativeFn)]) -> Rc<Module> {
    let module = new_module(vm, name, "");
    {
        let mut d = module.dict.borrow_mut();
        for (n, f) in functions {
            d.set_str(n, native(n, *f));
        }
    }
    module
}

pub fn set(module: &Rc<Module>, name: &str, value: Value) {
    module.dict.borrow_mut().set_str(name, value);
}

fn registered(vm: &Vm, name: &str) -> Option<Value> {
    vm.modules.iter().find(|(n, _)| &**n == name).map(|(_, m)| m.clone())
}

fn not_found(vm: &mut Vm, name: &str) -> PyErr {
    let class = vm.types.module_not_found_error.clone();
    let message = alloc::format!("No module named '{}'", name);
    let err = vm.error(&class, &message);
    if let PyErr::Exc(Value::Instance(i)) = &err {
        i.dict.borrow_mut().set_str("name", Value::str(name));
    }
    err
}

/// An `OSError` subclass for a failed file operation on `path`.
pub fn os_error(vm: &mut Vm, error: FileError, path: &str) -> PyErr {
    let (class, errno, text) = match &error {
        FileError::NotFound => (vm.types.file_not_found_error.clone(), 2, String::from("No such file or directory")),
        FileError::Exists => (vm.types.file_exists_error.clone(), 17, String::from("File exists")),
        FileError::Permission => (vm.types.permission_error.clone(), 13, String::from("Permission denied")),
        FileError::IsDir => (vm.types.is_a_directory_error.clone(), 21, String::from("Is a directory")),
        FileError::NotDir => (vm.types.not_a_directory_error.clone(), 20, String::from("Not a directory")),
        FileError::NotEmpty => (vm.types.os_error.clone(), 39, String::from("Directory not empty")),
        FileError::Other(message) => (vm.types.os_error.clone(), 5, message.clone()),
    };
    match vm.call(&Value::Class(class), &[Value::int(errno), Value::str(&text), Value::str(path)], &[]) {
        Ok(exc) => PyErr::Exc(exc),
        Err(e) => e,
    }
}

/// Joins a directory and a name.
pub fn join(directory: &str, name: &str) -> String {
    if name.starts_with('/') || directory.is_empty() || directory == "." {
        return String::from(name);
    }
    if directory.ends_with('/') {
        alloc::format!("{}{}", directory, name)
    } else {
        alloc::format!("{}/{}", directory, name)
    }
}

/// Runs a module's source in a new module.
fn execute(vm: &mut Vm, name: &str, file: &str, source: &str, package: Option<&str>) -> PyResult<Value> {
    let module = new_module(vm, name, file);
    match package {
        Some(directory) => {
            module.dict.borrow_mut().set_str("__path__", Value::list(alloc::vec![Value::str(directory)]));
            module.dict.borrow_mut().set_str("__package__", Value::str(name));
        }
        None => {
            let parent = name.rfind('.').map_or("", |i| &name[..i]);
            module.dict.borrow_mut().set_str("__package__", Value::str(parent));
        }
    }
    let result = super::compile(vm, source, file, Mode::Module).and_then(|code| super::run_code(vm, code, module.clone(), None));
    match result {
        Ok(_) => Ok(Value::Module(module)),
        Err(e) => {
            vm.modules.retain(|(n, _)| &**n != name);
            Err(e)
        }
    }
}

/// Loads the module `name` (absolute, dotted), its parents loaded.
fn load(vm: &mut Vm, name: &str) -> PyResult<Value> {
    if let Some(m) = registered(vm, name) {
        return Ok(m);
    }
    let native = match name {
        "math" => Some(math::module(vm)),
        "random" => Some(random::module(vm)),
        "sys" => Some(system::sys(vm)),
        "time" => Some(system::time(vm)),
        "os" => {
            let module = system::os(vm);
            // The parts of os written in Python, added onto the module.
            execute(vm, "_osextra", "<_osextra>", include_str!("lib/_osextra.py"), None)?;
            return Ok(Value::Module(module));
        }
        "os.path" => {
            load(vm, "os")?;
            return registered(vm, "os.path").ok_or_else(|| not_found(vm, name));
        }
        "json" => Some(json::module(vm)),
        "_nova" => Some(codec::module(vm)),
        "re" => Some(re::module(vm)),
        "traceback" => Some(system::traceback(vm)),
        _ => None,
    };
    if let Some(module) = native {
        return Ok(Value::Module(module));
    }
    if let Some((_, source)) = SOURCES.iter().find(|(n, _)| *n == name) {
        if let Some(i) = name.rfind('.') {
            load(vm, &name[..i])?;
        }
        let file = alloc::format!("<{}>", name);
        let module = execute(vm, name, &file, source, None)?;
        bind_in_parent(vm, name, &module);
        return Ok(module);
    }
    // The program's own modules: the directories to look in.
    let (directories, leaf) = match name.rfind('.') {
        Some(i) => {
            let parent = load(vm, &name[..i])?;
            let path = match &parent {
                Value::Module(m) => m.dict.borrow().get_str("__path__"),
                _ => None,
            };
            let directories: Vec<String> = match path {
                Some(Value::List(l)) => l.borrow().iter().filter_map(|d| if let Value::Str(s) = d { Some(String::from(&**s)) } else { None }).collect(),
                _ => {
                    let class = vm.types.module_not_found_error.clone();
                    let message = alloc::format!("No module named '{}'; '{}' is not a package", name, &name[..i]);
                    return Err(vm.error(&class, &message));
                }
            };
            (directories, &name[i + 1..])
        }
        None => (vm.path.clone(), name),
    };
    for directory in directories {
        let file = join(&directory, &alloc::format!("{}.py", leaf));
        if vm.host.kind(&file) == Some(false) {
            let source = read_source(vm, &file)?;
            let module = execute(vm, name, &file, &source, None)?;
            bind_in_parent(vm, name, &module);
            return Ok(module);
        }
        let package = join(&directory, leaf);
        let init = join(&package, "__init__.py");
        if vm.host.kind(&init) == Some(false) {
            let source = read_source(vm, &init)?;
            let module = execute(vm, name, &init, &source, Some(&package))?;
            bind_in_parent(vm, name, &module);
            return Ok(module);
        }
    }
    Err(not_found(vm, name))
}

/// `import a.b` makes `b` an attribute of `a`.
fn bind_in_parent(vm: &mut Vm, name: &str, module: &Value) {
    if let Some(i) = name.rfind('.') {
        if let Some(Value::Module(parent)) = registered(vm, &name[..i]) {
            parent.dict.borrow_mut().set_str(&name[i + 1..], module.clone());
        }
    }
}

fn read_source(vm: &mut Vm, path: &str) -> PyResult<String> {
    match vm.host.read_file(path) {
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(text) => Ok(text),
            Err(_) => {
                let message = alloc::format!("(unicode error) 'utf-8' codec can't decode the file {}", path);
                Err(super::syntax_error(vm, &message, path, 1, None))
            }
        },
        Err(e) => Err(os_error(vm, e, path)),
    }
}

/// The `__package__` of the module running now.
fn current_package(vm: &Vm) -> Option<String> {
    let frame = vm.frames.last()?;
    match frame.globals.dict.borrow().get_str("__package__") {
        Some(Value::Str(p)) if !p.is_empty() => Some(String::from(&*p)),
        _ => None,
    }
}

/// `import name` (`level` dots before it); `fromlist` is what `from ...
/// import` names, None for a plain import.
pub fn import(vm: &mut Vm, name: &str, level: usize, fromlist: &Value) -> PyResult<Value> {
    let absolute = if level > 0 {
        let Some(package) = current_package(vm) else {
            let class = vm.types.import_error.clone();
            return Err(vm.error(&class, "attempted relative import with no known parent package"));
        };
        let parts: Vec<&str> = package.split('.').collect();
        if level - 1 >= parts.len() {
            let class = vm.types.import_error.clone();
            return Err(vm.error(&class, "attempted relative import beyond top-level package"));
        }
        let base = parts[..parts.len() - (level - 1)].join(".");
        if name.is_empty() { base } else { alloc::format!("{}.{}", base, name) }
    } else {
        String::from(name)
    };
    // Each prefix, so that `import a.b.c` binds a, a.b, a.b.c.
    let mut leaf = Value::None;
    let mut first = Value::None;
    let mut prefix = String::new();
    for (i, part) in absolute.split('.').enumerate() {
        if i > 0 {
            prefix.push('.');
        }
        prefix.push_str(part);
        leaf = load(vm, &prefix)?;
        if i == 0 {
            first = leaf.clone();
        }
    }
    let names: Vec<Value> = match fromlist {
        Value::None => Vec::new(),
        other => vm.collect(other)?,
    };
    if names.is_empty() {
        return Ok(if level > 0 { leaf } else { first });
    }
    // `from package import submodule`.
    if let Value::Module(m) = &leaf {
        let is_package = m.dict.borrow().get_str("__path__").is_some();
        if is_package {
            for item in names {
                if let Value::Str(item) = item {
                    if &*item != "*" && m.dict.borrow().get_str(&item).is_none() {
                        let full = alloc::format!("{}.{}", absolute, item);
                        match load(vm, &full) {
                            Ok(_) => {}
                            Err(e) => {
                                let class = vm.types.module_not_found_error.clone();
                                if !vm.is_instance_of(&e, &class) {
                                    return Err(e);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(leaf)
}

/// Writes back every file left open: the end of a run.
pub fn flush_all(vm: &mut Vm) -> PyResult<()> {
    let files = core::mem::take(&mut vm.open_files);
    let mut result = Ok(());
    for file in &files {
        let closed = file.borrow().closed;
        if !closed {
            if let Err(e) = super::types::object::flush_file(vm, file) {
                if result.is_ok() {
                    result = Err(e);
                }
            }
        }
    }
    result
}
