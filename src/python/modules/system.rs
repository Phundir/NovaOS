//! `sys`, `time`, `os`, `os.path`, `traceback`: the program's view of the
//! machine, through `PyHost`.
//!
//! Paths are the host's: absolute from the file system's root, or
//! relative to the directory `dev interpret` was typed in. `os.path`
//! follows `posixpath` (the separator is `/`, `normpath` collapses `.`
//! and `..`). The clock has no time zone: `localtime` is UTC.

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use super::super::args;
use super::super::dict::Dict;
use super::super::int::Int;
use super::super::types::{class, method, num::float_of};
use super::super::value::*;
use super::super::vm::{PyErr, PyResult, Vm};
use crate::web::js::num::Num;

// ------------------------------------------------------------
//  Struct sequences: tuples with named fields
// ------------------------------------------------------------

/// The field `data` of a struct sequence (a property's getter).
fn field(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let (Some(Value::Int(i)), Some(this)) = (a.first(), a.get(1)) else {
        return vm.type_error("field expected");
    };
    let index = i.to_i64().unwrap_or(0);
    let inner = vm.unwrap_builtin(this);
    vm.get_item(&inner, &Value::int(index))
}

fn struct_repr(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let Some(this) = a.first() else {
        return vm.type_error("expected 1 argument");
    };
    let class = vm.class_of(this);
    let names = match class.dict.borrow().get_str("_fields") {
        Some(Value::Tuple(t)) => t.to_vec(),
        _ => Vec::new(),
    };
    let module = match class.dict.borrow().get_str("__module__") {
        Some(Value::Str(m)) => alloc::format!("{}.", m),
        _ => String::new(),
    };
    let values = vm.collect(&vm.unwrap_builtin(this))?;
    let mut parts = Vec::new();
    for (name, value) in names.iter().zip(values.iter()) {
        let shown = vm.repr(value)?;
        parts.push(alloc::format!("{}={}", vm.str_of(name)?, shown));
    }
    Ok(Value::str(&alloc::format!("{}{}({})", module, class.name, parts.join(", "))))
}

/// A tuple class whose fields are also attributes.
pub fn struct_class(vm: &mut Vm, module: &str, name: &str, fields: &[&'static str]) -> Rc<Class> {
    let tuple = vm.types.tuple.clone();
    let c = class(name, alloc::vec![tuple], Base::Tuple);
    {
        let mut d = c.dict.borrow_mut();
        d.set_str("__module__", Value::str(module));
        d.set_str("_fields", Value::tuple(fields.iter().map(|f| Value::str(f)).collect()));
        for (i, f) in fields.iter().enumerate() {
            let getter = Value::Builtin(Rc::new(Builtin { name: f, call: field, method: false, data: Value::int(i as i64) }));
            d.set_str(f, Value::Property(Rc::new([getter, Value::None, Value::None])));
        }
    }
    method(&c, "__repr__", struct_repr);
    c
}

pub fn make_struct(class: &Rc<Class>, values: Vec<Value>) -> Value {
    let instance = Instance::new(class.clone());
    *instance.inner.borrow_mut() = Value::tuple(values);
    Value::Instance(instance)
}

fn std_file(path: &str, readable: bool) -> Value {
    Value::File(shared(File {
        path: Rc::from(path),
        data: Vec::new(),
        position: 0,
        readable,
        writable: !readable,
        append: false,
        binary: false,
        closed: false,
        dirty: false,
    }))
}

// ------------------------------------------------------------
//  sys
// ------------------------------------------------------------

fn sys_exit(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let class = vm.types.system_exit.clone();
    let made = vm.call(&Value::Class(class), a, &[])?;
    Err(PyErr::Exc(made))
}

fn getrecursionlimit(vm: &mut Vm, _: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    Ok(Value::int(vm.recursion_limit as i64))
}

fn setrecursionlimit(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let n = args::int(vm, a.first().unwrap_or(&Value::None))?;
    if n < 1 {
        return vm.value_error("recursion limit must be greater or equal than 1");
    }
    // Frames are on the heap; the limit only bounds memory.
    vm.recursion_limit = (n as usize).min(100_000);
    Ok(Value::None)
}

fn exc_info(vm: &mut Vm, _: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    match vm.handling.last().cloned() {
        Some(exc) => {
            let class = Value::Class(vm.class_of(&exc));
            Ok(Value::tuple(alloc::vec![class, exc, Value::None]))
        }
        None => Ok(Value::tuple(alloc::vec![Value::None, Value::None, Value::None])),
    }
}

fn exception(vm: &mut Vm, _: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    Ok(vm.handling.last().cloned().unwrap_or(Value::None))
}

fn intern(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    match a.first() {
        Some(s @ Value::Str(_)) => Ok(s.clone()),
        _ => vm.type_error("intern() argument must be str"),
    }
}

fn int_max_str_digits(_: &mut Vm, _: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    Ok(Value::int(super::super::int::MAX_STR_DIGITS as i64))
}

fn set_int_max_str_digits(vm: &mut Vm, _: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let class = vm.types.not_implemented_error.clone();
    Err(vm.error(&class, "la limite de 4300 chiffres est fixe dans cet interprete"))
}

fn getsizeof(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let x = a.first().cloned().unwrap_or(Value::None);
    // An estimate in CPython's spirit: a header and the items.
    let size = match &x {
        Value::List(l) => 56 + 8 * l.borrow().len(),
        Value::Tuple(t) => 40 + 8 * t.len(),
        Value::Str(s) => 41 + s.len(),
        Value::Dict(d) => 64 + 24 * d.borrow().len(),
        Value::Int(i) => 28 + 4 * (i.bit_length() as usize / 30),
        Value::Float(_) => 24,
        _ => 48,
    };
    let _ = vm;
    Ok(Value::int(size as i64))
}

pub fn sys(vm: &mut Vm) -> Rc<Module> {
    let functions: &[(&'static str, NativeFn)] = &[
        ("exit", sys_exit), ("getrecursionlimit", getrecursionlimit), ("setrecursionlimit", setrecursionlimit),
        ("exc_info", exc_info), ("exception", exception), ("intern", intern),
        ("get_int_max_str_digits", int_max_str_digits), ("set_int_max_str_digits", set_int_max_str_digits),
        ("getsizeof", getsizeof),
    ];
    let module = super::native_module(vm, "sys", functions);
    let argv: Vec<Value> = vm.argv.iter().map(|a| Value::str(a)).collect();
    super::set(&module, "argv", Value::list(argv));
    let path: Vec<Value> = vm.path.iter().map(|p| Value::str(p)).collect();
    super::set(&module, "path", Value::list(path));
    let mut modules = Dict::new();
    for (name, m) in &vm.modules {
        modules.set_str(name, m.clone());
    }
    super::set(&module, "modules", Value::dict(modules));
    super::set(&module, "version", Value::str("3.13.0 (Nova OS, interprete integre)"));
    let info = struct_class(vm, "sys", "version_info", &["major", "minor", "micro", "releaselevel", "serial"]);
    let version = make_struct(&info, alloc::vec![Value::int(3), Value::int(13), Value::int(0), Value::str("final"), Value::int(0)]);
    super::set(&module, "version_info", version);
    super::set(&module, "platform", Value::str("nova"));
    super::set(&module, "byteorder", Value::str("little"));
    super::set(&module, "maxsize", Value::int(i64::MAX));
    super::set(&module, "executable", Value::str(""));
    super::set(&module, "stdout", std_file("<stdout>", false));
    super::set(&module, "stderr", std_file("<stderr>", false));
    super::set(&module, "stdin", std_file("<stdin>", true));
    super::set(&module, "builtin_module_names", Value::tuple(super::library().iter().map(|n| Value::str(n)).collect()));
    module
}

// ------------------------------------------------------------
//  time
// ------------------------------------------------------------

fn seconds(ns: u64) -> Value {
    Value::Float(Num::from_u64(ns).div(Num::from_u64(1_000_000_000)))
}

fn time_time(vm: &mut Vm, _: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    Ok(seconds(vm.host.time_ns()))
}

fn time_time_ns(vm: &mut Vm, _: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    Ok(Value::Int(Int::from_u128(vm.host.time_ns() as u128)))
}

fn time_monotonic(vm: &mut Vm, _: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    Ok(seconds(vm.host.monotonic_ns()))
}

fn time_monotonic_ns(vm: &mut Vm, _: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    Ok(Value::Int(Int::from_u128(vm.host.monotonic_ns() as u128)))
}

fn time_sleep(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let s = float_of(vm, a.first().unwrap_or(&Value::None))?;
    if s.is_negative() {
        return vm.value_error("sleep length must be non-negative");
    }
    let ns = s.mul(Num::from_u64(1_000_000_000)).to_i64_saturating().max(0) as u64;
    vm.host.sleep(ns);
    Ok(Value::None)
}

/// Days since 1970-01-01 to (year, month, day): Howard Hinnant's
/// algorithm.
fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// The nine fields of a struct_time, UTC.
fn broken_down(secs: i64) -> [i64; 9] {
    let days = secs.div_euclid(86_400);
    let rest = secs.rem_euclid(86_400);
    let (y, m, d) = civil(days);
    let wday = (days + 3).rem_euclid(7); // 1970-01-01 was a Thursday (3, Monday = 0)
    const BEFORE: [i64; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
    let yday = BEFORE[(m - 1) as usize] + d + if m > 2 && leap(y) { 1 } else { 0 };
    [y, m, d, rest / 3600, rest % 3600 / 60, rest % 60, wday, yday, 0]
}

fn struct_time_class(vm: &mut Vm) -> Rc<Class> {
    if let Some(Value::Module(m)) = vm.modules.iter().find(|(n, _)| &**n == "time").map(|(_, m)| m.clone()) {
        if let Some(Value::Class(c)) = m.dict.borrow().get_str("struct_time") {
            return c;
        }
    }
    struct_class(vm, "time", "struct_time", &["tm_year", "tm_mon", "tm_mday", "tm_hour", "tm_min", "tm_sec", "tm_wday", "tm_yday", "tm_isdst"])
}

fn gmtime(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let secs = match a.first() {
        None | Some(Value::None) => (vm.host.time_ns() / 1_000_000_000) as i64,
        Some(v) => float_of(vm, v)?.floor().to_i64_saturating(),
    };
    let fields = broken_down(secs);
    let class = struct_time_class(vm);
    Ok(make_struct(&class, fields.iter().map(|f| Value::int(*f)).collect()))
}

fn time_fields(vm: &mut Vm, value: Option<&Value>) -> PyResult<[i64; 9]> {
    match value {
        None | Some(Value::None) => Ok(broken_down((vm.host.time_ns() / 1_000_000_000) as i64)),
        Some(v) => {
            let items = vm.collect(v)?;
            if items.len() != 9 {
                return vm.type_error("time tuple must have 9 elements");
            }
            let mut out = [0i64; 9];
            for (i, item) in items.iter().enumerate() {
                out[i] = args::int(vm, item)?;
            }
            Ok(out)
        }
    }
}

const DAYS: [&str; 7] = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];
const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

fn strftime_text(format: &str, t: &[i64; 9]) -> String {
    let mut out = String::new();
    let mut chars = format.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        let Some(code) = chars.next() else {
            out.push('%');
            break;
        };
        let month = MONTHS[((t[1] - 1).clamp(0, 11)) as usize];
        let day = DAYS[(t[6].clamp(0, 6)) as usize];
        let piece = match code {
            'Y' => alloc::format!("{}", t[0]),
            'y' => alloc::format!("{:02}", t[0].rem_euclid(100)),
            'm' => alloc::format!("{:02}", t[1]),
            'd' => alloc::format!("{:02}", t[2]),
            'H' => alloc::format!("{:02}", t[3]),
            'I' => alloc::format!("{:02}", if t[3] % 12 == 0 { 12 } else { t[3] % 12 }),
            'M' => alloc::format!("{:02}", t[4]),
            'S' => alloc::format!("{:02}", t[5]),
            'p' => String::from(if t[3] < 12 { "AM" } else { "PM" }),
            'j' => alloc::format!("{:03}", t[7]),
            'a' => String::from(&day[..3]),
            'A' => String::from(day),
            'b' | 'h' => String::from(&month[..3]),
            'B' => String::from(month),
            'w' => alloc::format!("{}", (t[6] + 1) % 7),
            'u' => alloc::format!("{}", t[6] + 1),
            'Z' => String::from("UTC"),
            'z' => String::from("+0000"),
            'c' => alloc::format!("{} {} {:2} {:02}:{:02}:{:02} {}", &day[..3], &month[..3], t[2], t[3], t[4], t[5], t[0]),
            'x' => alloc::format!("{:02}/{:02}/{:02}", t[1], t[2], t[0].rem_euclid(100)),
            'X' => alloc::format!("{:02}:{:02}:{:02}", t[3], t[4], t[5]),
            'F' => alloc::format!("{}-{:02}-{:02}", t[0], t[1], t[2]),
            'T' => alloc::format!("{:02}:{:02}:{:02}", t[3], t[4], t[5]),
            '%' => String::from("%"),
            other => alloc::format!("%{}", other),
        };
        out.push_str(&piece);
    }
    out
}

fn strftime(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let format = args::string(vm, a.first().unwrap_or(&Value::None), "strftime() argument 1")?;
    let t = time_fields(vm, a.get(1))?;
    Ok(Value::str(&strftime_text(&format, &t)))
}

fn asctime(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let t = time_fields(vm, a.first())?;
    Ok(Value::str(&strftime_text("%c", &t)))
}

fn ctime(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let secs = match a.first() {
        None | Some(Value::None) => (vm.host.time_ns() / 1_000_000_000) as i64,
        Some(v) => float_of(vm, v)?.floor().to_i64_saturating(),
    };
    Ok(Value::str(&strftime_text("%c", &broken_down(secs))))
}

fn mktime(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let t = time_fields(vm, a.first())?;
    // Days from the civil date (inverse of `civil`).
    let (y, m, d) = (t[0], t[1], t[2]);
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + t[3] * 3600 + t[4] * 60 + t[5];
    Ok(Value::Float(Num::from_i64(secs)))
}

pub fn time(vm: &mut Vm) -> Rc<Module> {
    let functions: &[(&'static str, NativeFn)] = &[
        ("time", time_time), ("time_ns", time_time_ns), ("monotonic", time_monotonic), ("monotonic_ns", time_monotonic_ns),
        ("perf_counter", time_monotonic), ("perf_counter_ns", time_monotonic_ns), ("process_time", time_monotonic),
        ("process_time_ns", time_monotonic_ns), ("sleep", time_sleep), ("gmtime", gmtime), ("localtime", gmtime),
        ("strftime", strftime), ("asctime", asctime), ("ctime", ctime), ("mktime", mktime),
    ];
    let module = super::native_module(vm, "time", functions);
    let st = struct_time_class(vm);
    super::set(&module, "struct_time", Value::Class(st));
    super::set(&module, "timezone", Value::int(0));
    super::set(&module, "tzname", Value::tuple(alloc::vec![Value::str("UTC"), Value::str("UTC")]));
    module
}

// ------------------------------------------------------------
//  os and os.path
// ------------------------------------------------------------

fn path_arg(vm: &mut Vm, value: Option<&Value>) -> PyResult<String> {
    match value {
        Some(Value::Str(s)) => Ok(String::from(&**s)),
        Some(other) => {
            let name = vm.type_name(other);
            let message = alloc::format!("expected str, bytes or os.PathLike object, not {}", name);
            vm.type_error(&message)
        }
        None => vm.type_error("missing required argument 'path'"),
    }
}

fn getcwd(vm: &mut Vm, _: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    Ok(Value::str(&vm.host.cwd()))
}

fn listdir(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let path = match a.first() {
        None | Some(Value::None) => String::from("."),
        some => path_arg(vm, some)?,
    };
    match vm.host.list_dir(&path) {
        Ok(names) => Ok(Value::list(names.iter().map(|n| Value::str(n)).collect())),
        Err(e) => Err(super::os_error(vm, e, &path)),
    }
}

fn mkdir(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let path = path_arg(vm, a.first())?;
    if vm.host.kind(&path).is_some() {
        return Err(super::os_error(vm, super::super::FileError::Exists, &path));
    }
    match vm.host.make_dir(&path) {
        Ok(()) => Ok(Value::None),
        Err(e) => Err(super::os_error(vm, e, &path)),
    }
}

fn makedirs(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let path = path_arg(vm, a.first())?;
    let exist_ok = match args::get(&a[a.len().min(1)..], kw, 1, "exist_ok") {
        Some(v) => vm.truthy(v)?,
        None => false,
    };
    if vm.host.kind(&path) == Some(true) {
        if exist_ok {
            return Ok(Value::None);
        }
        return Err(super::os_error(vm, super::super::FileError::Exists, &path));
    }
    let mut current = String::new();
    for (i, part) in path.split('/').enumerate() {
        if i > 0 || path.starts_with('/') {
            current.push('/');
        }
        current.push_str(part);
        if part.is_empty() || part == "." {
            continue;
        }
        match vm.host.kind(&current) {
            Some(true) => {}
            Some(false) => return Err(super::os_error(vm, super::super::FileError::Exists, &current)),
            None => {
                if let Err(e) = vm.host.make_dir(&current) {
                    return Err(super::os_error(vm, e, &current));
                }
            }
        }
    }
    Ok(Value::None)
}

fn remove(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let path = path_arg(vm, a.first())?;
    if vm.host.kind(&path) == Some(true) {
        return Err(super::os_error(vm, super::super::FileError::IsDir, &path));
    }
    match vm.host.remove(&path) {
        Ok(()) => Ok(Value::None),
        Err(e) => Err(super::os_error(vm, e, &path)),
    }
}

fn rmdir(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let path = path_arg(vm, a.first())?;
    if vm.host.kind(&path) == Some(false) {
        return Err(super::os_error(vm, super::super::FileError::NotDir, &path));
    }
    match vm.host.remove(&path) {
        Ok(()) => Ok(Value::None),
        Err(e) => Err(super::os_error(vm, e, &path)),
    }
}

fn rename(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let from = path_arg(vm, a.first())?;
    let to = path_arg(vm, a.get(1))?;
    let data = match vm.host.read_file(&from) {
        Ok(d) => d,
        Err(e) => return Err(super::os_error(vm, e, &from)),
    };
    if let Err(e) = vm.host.write_file(&to, &data) {
        return Err(super::os_error(vm, e, &to));
    }
    if let Err(e) = vm.host.remove(&from) {
        return Err(super::os_error(vm, e, &from));
    }
    Ok(Value::None)
}

fn getenv(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let key = path_arg(vm, a.first())?;
    let found = match key.as_str() {
        "HOME" => Some(String::from("/")),
        "PWD" => Some(vm.host.cwd()),
        _ => None,
    };
    Ok(found.map(|v| Value::str(&v)).unwrap_or_else(|| a.get(1).cloned().unwrap_or(Value::None)))
}

fn walk(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let top = path_arg(vm, a.first())?;
    let mut out = Vec::new();
    let mut pending = alloc::vec![top];
    while let Some(dir) = pending.pop() {
        let Ok(names) = vm.host.list_dir(&dir) else {
            continue;
        };
        let mut dirs = Vec::new();
        let mut files = Vec::new();
        for name in names {
            let full = super::join(&dir, &name);
            if vm.host.kind(&full) == Some(true) {
                dirs.push(name);
            } else {
                files.push(name);
            }
        }
        for d in dirs.iter().rev() {
            pending.push(super::join(&dir, d));
        }
        out.push(Value::tuple(alloc::vec![
            Value::str(&dir),
            Value::list(dirs.iter().map(|d| Value::str(d)).collect()),
            Value::list(files.iter().map(|f| Value::str(f)).collect()),
        ]));
    }
    vm.get_iter(&Value::list(out))
}

/// `posixpath.normpath`.
pub fn normpath(path: &str) -> String {
    if path.is_empty() {
        return String::from(".");
    }
    let initial = if path.starts_with("//") && !path.starts_with("///") {
        2
    } else if path.starts_with('/') {
        1
    } else {
        0
    };
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if !parts.is_empty() && parts.last() != Some(&"..") {
                    parts.pop();
                } else if initial == 0 {
                    parts.push("..");
                }
            }
            other => parts.push(other),
        }
    }
    let body = parts.join("/");
    let out = alloc::format!("{}{}", "/".repeat(initial), body);
    if out.is_empty() { String::from(".") } else { out }
}

fn path_join(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let mut out = path_arg(vm, a.first())?;
    for part in &a[1..] {
        let p = path_arg(vm, Some(part))?;
        if p.starts_with('/') {
            out = p;
        } else if out.is_empty() || out.ends_with('/') {
            out.push_str(&p);
        } else {
            out.push('/');
            out.push_str(&p);
        }
    }
    Ok(Value::str(&out))
}

fn split_path(path: &str) -> (String, String) {
    match path.rfind('/') {
        Some(i) => {
            let head = &path[..i + 1];
            let trimmed = head.trim_end_matches('/');
            let head = if trimmed.is_empty() { head } else { trimmed };
            (String::from(head), String::from(&path[i + 1..]))
        }
        None => (String::new(), String::from(path)),
    }
}

fn path_split(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let p = path_arg(vm, a.first())?;
    let (head, tail) = split_path(&p);
    Ok(Value::tuple(alloc::vec![Value::str(&head), Value::str(&tail)]))
}

fn basename(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let p = path_arg(vm, a.first())?;
    Ok(Value::str(&split_path(&p).1))
}

fn dirname(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let p = path_arg(vm, a.first())?;
    Ok(Value::str(&split_path(&p).0))
}

fn splitext(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let p = path_arg(vm, a.first())?;
    let base_start = p.rfind('/').map_or(0, |i| i + 1);
    let base = &p[base_start..];
    let leading = base.len() - base.trim_start_matches('.').len();
    match base[leading..].rfind('.') {
        Some(i) => {
            let at = base_start + leading + i;
            Ok(Value::tuple(alloc::vec![Value::str(&p[..at]), Value::str(&p[at..])]))
        }
        None => Ok(Value::tuple(alloc::vec![Value::str(&p), Value::str("")])),
    }
}

fn exists(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let p = path_arg(vm, a.first())?;
    Ok(Value::Bool(vm.host.kind(&p).is_some()))
}

fn isfile(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let p = path_arg(vm, a.first())?;
    Ok(Value::Bool(vm.host.kind(&p) == Some(false)))
}

fn isdir(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let p = path_arg(vm, a.first())?;
    Ok(Value::Bool(vm.host.kind(&p) == Some(true)))
}

fn isabs(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let p = path_arg(vm, a.first())?;
    Ok(Value::Bool(p.starts_with('/')))
}

fn abspath(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let p = path_arg(vm, a.first())?;
    let full = if p.starts_with('/') { p } else { super::join(&vm.host.cwd(), &p) };
    Ok(Value::str(&normpath(&full)))
}

fn normpath_fn(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let p = path_arg(vm, a.first())?;
    Ok(Value::str(&normpath(&p)))
}

fn getsize(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let p = path_arg(vm, a.first())?;
    match vm.host.read_file(&p) {
        Ok(data) => Ok(Value::int(data.len() as i64)),
        Err(e) => Err(super::os_error(vm, e, &p)),
    }
}

fn identity(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let p = path_arg(vm, a.first())?;
    Ok(Value::str(&p))
}

pub fn os(vm: &mut Vm) -> Rc<Module> {
    let path_functions: &[(&'static str, NativeFn)] = &[
        ("join", path_join), ("split", path_split), ("basename", basename), ("dirname", dirname), ("splitext", splitext),
        ("exists", exists), ("isfile", isfile), ("isdir", isdir), ("isabs", isabs), ("abspath", abspath),
        ("normpath", normpath_fn), ("realpath", abspath), ("getsize", getsize), ("expanduser", identity),
    ];
    let path = super::native_module(vm, "os.path", path_functions);
    super::set(&path, "sep", Value::str("/"));
    let functions: &[(&'static str, NativeFn)] = &[
        ("getcwd", getcwd), ("listdir", listdir), ("mkdir", mkdir), ("makedirs", makedirs), ("remove", remove),
        ("unlink", remove), ("rmdir", rmdir), ("rename", rename), ("replace", rename), ("getenv", getenv), ("walk", walk),
    ];
    let module = super::native_module(vm, "os", functions);
    super::set(&module, "path", Value::Module(path));
    super::set(&module, "sep", Value::str("/"));
    super::set(&module, "linesep", Value::str("\n"));
    super::set(&module, "curdir", Value::str("."));
    super::set(&module, "pardir", Value::str(".."));
    super::set(&module, "name", Value::str("posix"));
    let mut environ = Dict::new();
    environ.set_str("HOME", Value::str("/"));
    environ.set_str("PWD", Value::str(&vm.host.cwd()));
    super::set(&module, "environ", Value::dict(environ));
    module
}

// ------------------------------------------------------------
//  traceback
// ------------------------------------------------------------

fn current_or(vm: &mut Vm, a: &[Value]) -> Option<Value> {
    match a.first() {
        Some(v @ Value::Instance(_)) => Some(v.clone()),
        Some(Value::Class(_)) => a.get(1).cloned(),
        _ => vm.handling.last().cloned(),
    }
}

fn format_exc(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let _ = a;
    match vm.handling.last().cloned() {
        Some(exc) => Ok(Value::str(&super::super::format_exception(vm, &exc))),
        None => Ok(Value::str("NoneType: None\n")),
    }
}

fn print_exc(vm: &mut Vm, _: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let text = match vm.handling.last().cloned() {
        Some(exc) => super::super::format_exception(vm, &exc),
        None => String::from("NoneType: None\n"),
    };
    vm.host.write(&text, true);
    Ok(Value::None)
}

fn format_exception(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    match current_or(vm, a) {
        Some(exc) => Ok(Value::list(alloc::vec![Value::str(&super::super::format_exception(vm, &exc))])),
        None => Ok(Value::list(Vec::new())),
    }
}

fn print_exception(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    if let Some(exc) = current_or(vm, a) {
        let text = super::super::format_exception(vm, &exc);
        vm.host.write(&text, true);
    }
    Ok(Value::None)
}

fn format_exception_only(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    match current_or(vm, a) {
        Some(exc) => {
            let line = super::super::exception_line(vm, &exc);
            Ok(Value::list(alloc::vec![Value::str(&alloc::format!("{}\n", line))]))
        }
        None => Ok(Value::list(Vec::new())),
    }
}

pub fn traceback(vm: &mut Vm) -> Rc<Module> {
    let functions: &[(&'static str, NativeFn)] = &[
        ("format_exc", format_exc), ("print_exc", print_exc), ("format_exception", format_exception),
        ("print_exception", print_exception), ("format_exception_only", format_exception_only),
    ];
    super::native_module(vm, "traceback", functions)
}
