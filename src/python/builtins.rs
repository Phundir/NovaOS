//! The `builtins` module: `print`, `len`, `range`, `sorted`...
//!
//! Each function follows its CPython counterpart down to the error
//! messages, and to the arithmetic where it shows: `sum` of floats uses
//! Neumaier's compensated summation as CPython 3.12+ does (`sum([0.1] *
//! 10)` is exactly 1.0), `round` rounds the exact decimal value ties to
//! even.

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

use super::args;
use super::ast::{BinOp, CmpOp};
use super::dict::Dict;
use super::int::Int;
use super::types::{native, num};
use super::value::*;
use super::vm::{PyResult, Vm};
use crate::web::js::num::Num;

fn first(vm: &mut Vm, a: &[Value], function: &str) -> PyResult<Value> {
    match a.first() {
        Some(v) => Ok(v.clone()),
        None => {
            let message = alloc::format!("{}() takes exactly one argument (0 given)", function);
            vm.type_error(&message)
        }
    }
}

// ------------------------------------------------------------
//  Output and input
// ------------------------------------------------------------

fn print(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let text_arg = |vm: &mut Vm, name: &str, default: &str| -> PyResult<String> {
        match args::keyword(kw, name) {
            None | Some(Value::None) => Ok(String::from(default)),
            Some(Value::Str(s)) => Ok(String::from(&**s)),
            Some(other) => {
                let t = vm.type_name(other);
                let message = alloc::format!("{} must be None or a string, not {}", name, t);
                vm.type_error(&message)
            }
        }
    };
    for (k, _) in kw {
        if !matches!(&**k, "sep" | "end" | "file" | "flush") {
            let message = alloc::format!("'{}' is an invalid keyword argument for print()", k);
            return vm.type_error(&message);
        }
    }
    let sep = text_arg(vm, "sep", " ")?;
    let end = text_arg(vm, "end", "\n")?;
    let mut out = String::new();
    for (i, value) in a.iter().enumerate() {
        if i > 0 {
            out.push_str(&sep);
        }
        out.push_str(&vm.str_of(value)?);
    }
    out.push_str(&end);
    let mut file = args::keyword(kw, "file").cloned();
    if file.as_ref().is_none_or(|f| f.is_none()) {
        // `sys.stdout` replaced by the program (redirect_stdout, StringIO).
        let sys = vm.sys_modules.as_ref().and_then(|d| d.borrow().get_str("sys"));
        if let Some(Value::Module(sys)) = sys {
            let stdout = sys.dict.borrow().get_str("stdout");
            match stdout {
                Some(Value::File(f)) if &*f.borrow().path == "<stdout>" => {}
                Some(Value::None) | None => {}
                Some(other) => file = Some(other),
            }
        }
    }
    match file.as_ref() {
        None | Some(Value::None) => vm.host.write(&out, false),
        Some(Value::File(f)) => {
            let f = f.clone();
            super::types::object::write_to(vm, &f, &Value::str(&out))?;
        }
        Some(other) => {
            let write = vm.get_attr(other, "write")?;
            vm.call(&write, &[Value::str(&out)], &[])?;
        }
    }
    Ok(Value::None)
}

fn input(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    if let Some(prompt) = a.first() {
        let text = vm.str_of(prompt)?;
        vm.host.write(&text, false);
    }
    match vm.host.input() {
        Some(line) => Ok(Value::str(line.strip_suffix('\n').unwrap_or(&line))),
        None => {
            let class = vm.types.eof_error.clone();
            Err(vm.error(&class, "EOF when reading a line"))
        }
    }
}

fn open(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let path = match args::get(a, kw, 0, "file").cloned() {
        Some(v) => super::modules::system::fspath(vm, &v)?,
        None => return vm.type_error("open() missing required argument 'file' (pos 1)"),
    };
    let mode = match args::get(a, kw, 1, "mode") {
        Some(Value::Str(s)) => s.clone(),
        Some(_) => return vm.type_error("open() argument 'mode' must be str"),
        None => Rc::from("r"),
    };
    let valid = mode.chars().all(|c| "rwaxbt+".contains(c))
        && mode.chars().filter(|c| "rwax".contains(*c)).count() == 1
        && !(mode.contains('b') && mode.contains('t'));
    if !valid {
        let message = alloc::format!("invalid mode: '{}'", mode);
        return vm.value_error(&message);
    }
    let binary = mode.contains('b');
    let plus = mode.contains('+');
    let kind = mode.chars().find(|c| "rwax".contains(*c)).unwrap_or('r');
    let existing = vm.host.read_file(&path);
    let data = match (kind, existing) {
        ('r', Ok(data)) | ('a', Ok(data)) => data,
        ('r', Err(e)) => return Err(super::modules::os_error(vm, e, &path)),
        ('x', Ok(_)) => {
            return Err(super::modules::os_error(vm, super::FileError::Exists, &path));
        }
        ('a', Err(super::FileError::NotFound)) | ('x', Err(super::FileError::NotFound)) | ('w', _) => {
            // Created now, as CPython does, so a bad directory fails here.
            if let Err(e) = vm.host.write_file(&path, &[]) {
                return Err(super::modules::os_error(vm, e, &path));
            }
            Vec::new()
        }
        (_, Err(e)) => return Err(super::modules::os_error(vm, e, &path)),
        (_, Ok(_)) => Vec::new(),
    };
    let position = if kind == 'a' { data.len() } else { 0 };
    let file = File {
        path,
        data,
        position,
        readable: kind == 'r' || plus,
        writable: kind != 'r' || plus,
        append: kind == 'a',
        binary,
        closed: false,
        dirty: false,
    };
    Ok(Value::File(shared(file)))
}

// ------------------------------------------------------------
//  Numbers
// ------------------------------------------------------------

fn abs(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let x = first(vm, a, "abs")?;
    match &x {
        Value::Int(i) => Ok(Value::Int(i.abs())),
        Value::Bool(b) => Ok(Value::int(*b as i64)),
        Value::Float(f) => Ok(Value::Float(f.abs())),
        Value::Complex(r, i) => Ok(Value::Float(super::types::complex::hypot(*r, *i))),
        Value::Instance(_) => {
            if let Some(r) = vm.call_special(&x, "__abs__", &[])? {
                return Ok(r);
            }
            let inner = vm.unwrap_builtin(&x);
            if !inner.is(&x) {
                return abs(vm, &[inner], &[]);
            }
            let name = vm.type_name(&x);
            let message = alloc::format!("bad operand type for abs(): '{}'", name);
            vm.type_error(&message)
        }
        other => {
            let name = vm.type_name(other);
            let message = alloc::format!("bad operand type for abs(): '{}'", name);
            vm.type_error(&message)
        }
    }
}

fn index_value(vm: &mut Vm, value: &Value) -> PyResult<Int> {
    match value {
        Value::Int(i) => Ok(i.clone()),
        Value::Bool(b) => Ok(Int::Small(*b as i64)),
        Value::Instance(_) => match vm.call_special(value, "__index__", &[])? {
            Some(Value::Int(i)) => Ok(i),
            _ => {
                let inner = vm.unwrap_builtin(value);
                if let Value::Int(i) = inner {
                    return Ok(i);
                }
                let name = vm.type_name(value);
                let message = alloc::format!("'{}' object cannot be interpreted as an integer", name);
                vm.type_error(&message)
            }
        },
        other => {
            let name = vm.type_name(other);
            let message = alloc::format!("'{}' object cannot be interpreted as an integer", name);
            vm.type_error(&message)
        }
    }
}

fn radix_text(vm: &mut Vm, a: &[Value], name: &str, radix: u32, prefix: &str) -> PyResult<Value> {
    let x = first(vm, a, name)?;
    let i = index_value(vm, &x)?;
    let digits = i.abs().to_radix(radix);
    let sign = if i.is_negative() { "-" } else { "" };
    Ok(Value::str(&alloc::format!("{}{}{}", sign, prefix, digits)))
}

fn bin(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    radix_text(vm, a, "bin", 2, "0b")
}

fn oct(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    radix_text(vm, a, "oct", 8, "0o")
}

fn hex(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    radix_text(vm, a, "hex", 16, "0x")
}

fn divmod(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let (Some(x), Some(y)) = (a.first(), a.get(1)) else {
        let message = alloc::format!("divmod expected 2 arguments, got {}", a.len());
        return vm.type_error(&message);
    };
    let numeric = |v: &Value| matches!(v, Value::Int(_) | Value::Bool(_) | Value::Float(_));
    if numeric(x) && numeric(y) {
        let q = vm.binary(BinOp::FloorDiv, x, y, false)?;
        let r = vm.binary(BinOp::Mod, x, y, false)?;
        return Ok(Value::tuple(alloc::vec![q, r]));
    }
    if let Some(r) = vm.call_special(x, "__divmod__", core::slice::from_ref(y))? {
        if !matches!(r, Value::NotImplemented) {
            return Ok(r);
        }
    }
    if let Some(r) = vm.call_special(y, "__rdivmod__", core::slice::from_ref(x))? {
        if !matches!(r, Value::NotImplemented) {
            return Ok(r);
        }
    }
    let (ux, uy) = (vm.unwrap_builtin(x), vm.unwrap_builtin(y));
    if numeric(&ux) && numeric(&uy) {
        return divmod(vm, &[ux, uy], &[]);
    }
    let (tx, ty) = (vm.type_name(x), vm.type_name(y));
    let message = alloc::format!("unsupported operand type(s) for divmod(): '{}' and '{}'", tx, ty);
    vm.type_error(&message)
}

fn pow(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let (Some(base), Some(exp)) = (args::get(a, kw, 0, "base"), args::get(a, kw, 1, "exp")) else {
        return vm.type_error("pow() missing required argument 'exp' (pos 2)");
    };
    let modulus = args::get(a, kw, 2, "mod").cloned().unwrap_or(Value::None);
    if modulus.is_none() {
        return vm.binary(BinOp::Pow, base, exp, false);
    }
    let ints = |v: &Value| matches!(v, Value::Int(_) | Value::Bool(_));
    if !(ints(base) && ints(exp) && ints(&modulus)) {
        // Three-argument pow of an object: its own `__pow__(exp, mod)`.
        if let Value::Instance(_) = base {
            let class = vm.class_of(base);
            if let Some(method @ Value::Function(_)) = class.lookup("__pow__") {
                let r = vm.call(&method, &[base.clone(), exp.clone(), modulus.clone()], &[])?;
                if !matches!(r, Value::NotImplemented) {
                    return Ok(r);
                }
            }
            let (x, y, z) = (vm.type_name(base), vm.type_name(exp), vm.type_name(&modulus));
            let message = alloc::format!("unsupported operand type(s) for ** or pow(): '{}', '{}', '{}'", x, y, z);
            return vm.type_error(&message);
        }
        return vm.type_error("pow() 3rd argument not allowed unless all arguments are integers");
    }
    let b = index_value(vm, base)?;
    let e = index_value(vm, exp)?;
    let m = index_value(vm, &modulus)?;
    if m.is_zero() {
        return vm.value_error("pow() 3rd argument cannot be 0");
    }
    let (b, e) = if e.is_negative() {
        // A negative exponent: the inverse of base modulo m.
        match inverse(&b, &m) {
            Some(inv) => (inv, e.neg()),
            None => return vm.value_error("base is not invertible for the given modulus"),
        }
    } else {
        (b, e)
    };
    match b.pow_mod(&e, &m) {
        Some(r) => Ok(Value::Int(r)),
        None => vm.value_error("pow() 3rd argument cannot be 0"),
    }
}

/// The inverse of `a` modulo `m` (extended Euclid), if it exists.
pub fn inverse(a: &Int, m: &Int) -> Option<Int> {
    let modulus = m.abs();
    let (mut old_r, mut r) = (a.divmod(&modulus)?.1, modulus.clone());
    let (mut old_s, mut s) = (Int::Small(1), Int::zero());
    while !r.is_zero() {
        let (q, rem) = old_r.divmod(&r)?;
        old_r = core::mem::replace(&mut r, rem);
        let next = old_s.sub(&q.mul(&s).ok()?);
        old_s = core::mem::replace(&mut s, next);
    }
    if old_r != Int::Small(1) {
        return None;
    }
    let inv = old_s.divmod(&modulus)?.1;
    if m.is_negative() && !inv.is_zero() { Some(inv.add(m)) } else { Some(inv) }
}

fn round(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let Some(number) = args::get(a, kw, 0, "number") else {
        return vm.type_error("round() missing required argument 'number' (pos 1)");
    };
    let ndigits = args::get(a, kw, 1, "ndigits").cloned().unwrap_or(Value::None);
    match number {
        Value::Float(f) => {
            if ndigits.is_none() {
                return num::float_to_int(vm, num::round_half_even(*f));
            }
            let n = args::int(vm, &ndigits)?;
            Ok(Value::Float(num::round_float(*f, n)))
        }
        Value::Int(_) | Value::Bool(_) => {
            let i = index_value(vm, number)?;
            if ndigits.is_none() {
                return Ok(Value::Int(i));
            }
            let n = args::int(vm, &ndigits)?;
            Ok(Value::Int(num::round_int(&i, n)))
        }
        Value::Instance(_) => {
            let extra: Vec<Value> = if ndigits.is_none() { Vec::new() } else { alloc::vec![ndigits.clone()] };
            if let Some(r) = vm.call_special(number, "__round__", &extra)? {
                return Ok(r);
            }
            let inner = vm.unwrap_builtin(number);
            if !inner.is(number) {
                return round(vm, &[inner, ndigits], &[]);
            }
            let name = vm.type_name(number);
            let message = alloc::format!("type {} doesn't define __round__ method", name);
            vm.type_error(&message)
        }
        other => {
            let name = vm.type_name(other);
            let message = alloc::format!("type {} doesn't define __round__ method", name);
            vm.type_error(&message)
        }
    }
}

/// `sum(iterable, start=0)`, with CPython 3.12's paths: exact machine
/// ints, then compensated floats, then generic `+`.
fn sum(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let Some(iterable) = args::get(a, kw, 0, "iterable") else {
        return vm.type_error("sum() takes at least 1 positional argument (0 given)");
    };
    let start = args::get(a, kw, 1, "start").cloned().unwrap_or(Value::int(0));
    match &start {
        Value::Str(_) => return vm.type_error("sum() can't sum strings [use ''.join(seq) instead]"),
        Value::Bytes(_) => return vm.type_error("sum() can't sum bytes [use b''.join(seq) instead]"),
        _ => {}
    }
    let iterator = vm.get_iter(iterable)?;
    let mut result = start;
    // Ints that fit a machine word.
    if let Value::Int(Int::Small(mut total)) = result {
        loop {
            let Some(item) = vm.next(&iterator)? else {
                return Ok(Value::int(total));
            };
            let small = match &item {
                Value::Int(Int::Small(b)) => Some(*b),
                Value::Bool(b) => Some(*b as i64),
                _ => None,
            };
            if let Some(b) = small {
                if let Some(t) = total.checked_add(b) {
                    total = t;
                    continue;
                }
            }
            result = vm.binary(BinOp::Add, &Value::int(total), &item, false)?;
            break;
        }
        if let Value::Int(Int::Small(t)) = result {
            // Only reached on overflow: the generic path below.
            let _ = t;
        }
    }
    // Floats, compensated (Neumaier).
    if let Value::Float(f) = result {
        let mut total = f;
        let mut c = Num::ZERO;
        loop {
            let Some(item) = vm.next(&iterator)? else {
                if !c.is_zero() && c.is_finite() {
                    total = total.add(c);
                }
                return Ok(Value::Float(total));
            };
            match &item {
                Value::Float(x) => {
                    let x = *x;
                    let t = total.add(x);
                    if total.abs().partial_cmp(x.abs()) != Some(core::cmp::Ordering::Less) {
                        c = c.add(total.sub(t).add(x));
                    } else {
                        c = c.add(x.sub(t).add(total));
                    }
                    total = t;
                    continue;
                }
                Value::Int(Int::Small(v)) => {
                    total = total.add(Num::from_i64(*v));
                    continue;
                }
                Value::Bool(b) => {
                    total = total.add(if *b { Num::ONE } else { Num::ZERO });
                    continue;
                }
                _ => {}
            }
            if !c.is_zero() && c.is_finite() {
                total = total.add(c);
            }
            result = vm.binary(BinOp::Add, &Value::Float(total), &item, false)?;
            break;
        }
    }
    while let Some(item) = vm.next(&iterator)? {
        result = vm.binary(BinOp::Add, &result, &item, false)?;
    }
    Ok(result)
}

// ------------------------------------------------------------
//  Characters
// ------------------------------------------------------------

fn chr(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let x = first(vm, a, "chr")?;
    let i = index_value(vm, &x)?;
    match i.to_i64().and_then(|v| u32::try_from(v).ok()).and_then(char::from_u32) {
        Some(c) => {
            let mut buffer = [0u8; 4];
            Ok(Value::str(c.encode_utf8(&mut buffer)))
        }
        None => {
            if i.to_i64().is_some_and(|v| (0xd800..0xe000).contains(&v)) {
                // Lone surrogates have no UTF-8 form.
                return vm.value_error("chr() arg is a surrogate, which this interpreter cannot represent");
            }
            vm.value_error("chr() arg not in range(0x110000)")
        }
    }
}

fn ord(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let x = first(vm, a, "ord")?;
    match &x {
        Value::Str(s) => {
            let mut chars = s.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => Ok(Value::int(c as i64)),
                _ => {
                    let message = alloc::format!("ord() expected a character, but string of length {} found", super::types::str::char_len(s));
                    vm.type_error(&message)
                }
            }
        }
        Value::Bytes(b) if b.len() == 1 => Ok(Value::int(b[0] as i64)),
        Value::Bytes(b) => {
            let message = alloc::format!("ord() expected a character, but string of length {} found", b.len());
            vm.type_error(&message)
        }
        other => {
            let name = vm.type_name(other);
            let message = alloc::format!("ord() expected string of length 1, but {} found", name);
            vm.type_error(&message)
        }
    }
}

fn repr(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let x = first(vm, a, "repr")?;
    Ok(Value::str(&vm.repr(&x)?))
}

fn ascii(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let x = first(vm, a, "ascii")?;
    let r = vm.repr(&x)?;
    Ok(Value::str(&super::format::ascii(&r)))
}

fn format(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let x = first(vm, a, "format")?;
    let spec = match a.get(1) {
        Some(Value::Str(s)) => s.clone(),
        Some(other) => {
            let name = vm.type_name(other);
            let message = alloc::format!("format() argument 2 must be str, not {}", name);
            return vm.type_error(&message);
        }
        None => Rc::from(""),
    };
    Ok(Value::str(&vm.format(&x, &spec)?))
}

// ------------------------------------------------------------
//  Iteration
// ------------------------------------------------------------

fn len(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    if a.len() != 1 {
        let message = alloc::format!("len() takes exactly one argument ({} given)", a.len());
        return vm.type_error(&message);
    }
    Ok(Value::int(vm.len(&a[0])? as i64))
}

fn iter(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    match a {
        [x] => vm.get_iter(x),
        [function, sentinel] => {
            if !callable_value(vm, function) {
                return vm.type_error("iter(v, w): v must be callable");
            }
            Ok(Value::Iter(shared(Iter::Callable { function: function.clone(), sentinel: sentinel.clone() })))
        }
        _ => {
            let message = alloc::format!("iter expected at most 2 arguments, got {}", a.len());
            vm.type_error(&message)
        }
    }
}

fn next(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let x = first(vm, a, "next")?;
    let is_iterator = match &x {
        Value::Iter(_) | Value::Generator(_) | Value::File(_) => true,
        Value::Instance(i) => i.class().lookup("__next__").is_some(),
        _ => false,
    };
    if !is_iterator {
        let name = vm.type_name(&x);
        let message = alloc::format!("'{}' object is not an iterator", name);
        return vm.type_error(&message);
    }
    let item = match &x {
        Value::Instance(_) | Value::File(_) => vm.call_special(&x, "__next__", &[]).map(|r| r),
        Value::Generator(g) => {
            let g = g.clone();
            vm.resume(&g, Value::None, None).map(Some)
        }
        _ => vm.next(&x),
    };
    match item {
        Ok(Some(v)) => Ok(v),
        Ok(None) => match a.get(1) {
            Some(default) => Ok(default.clone()),
            None => Err(vm.stop_iteration(Value::None)),
        },
        Err(e) => {
            let stop = vm.types.stop_iteration.clone();
            if vm.is_instance_of(&e, &stop) {
                if let Some(default) = a.get(1) {
                    return Ok(default.clone());
                }
            }
            Err(e)
        }
    }
}

fn enumerate(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let Some(iterable) = args::get(a, kw, 0, "iterable") else {
        return vm.type_error("enumerate() missing required argument 'iterable' (pos 1)");
    };
    let start = match args::get(a, kw, 1, "start") {
        Some(v) => index_value(vm, v)?,
        None => Int::zero(),
    };
    let inner = vm.get_iter(iterable)?;
    Ok(Value::Iter(shared(Iter::Enumerate { inner, count: start })))
}

fn zip(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let strict = match args::keyword(kw, "strict") {
        Some(v) => vm.truthy(v)?,
        None => false,
    };
    let mut inners = Vec::with_capacity(a.len());
    for (i, x) in a.iter().enumerate() {
        match vm.get_iter(x) {
            Ok(it) => inners.push(it),
            Err(e) => {
                let class = vm.types.type_error.clone();
                if vm.is_instance_of(&e, &class) {
                    let name = vm.type_name(x);
                    let message = alloc::format!("zip argument #{} must support iteration", i + 1);
                    let _ = name;
                    return vm.type_error(&message);
                }
                return Err(e);
            }
        }
    }
    Ok(Value::Iter(shared(Iter::Zip { inners, strict })))
}

fn map(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    if a.len() < 2 {
        return vm.type_error("map() must have at least two arguments.");
    }
    let mut inners = Vec::new();
    for x in &a[1..] {
        inners.push(vm.get_iter(x)?);
    }
    Ok(Value::Iter(shared(Iter::Map { function: a[0].clone(), inners })))
}

fn filter(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    if a.len() != 2 {
        let message = alloc::format!("filter expected 2 arguments, got {}", a.len());
        return vm.type_error(&message);
    }
    let inner = vm.get_iter(&a[1])?;
    Ok(Value::Iter(shared(Iter::Filter { function: a[0].clone(), inner })))
}

fn reversed(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let x = first(vm, a, "reversed")?;
    let state = match &x {
        Value::List(l) => Iter::Reversed { index: l.borrow().len(), seq: x.clone() },
        Value::Tuple(t) => Iter::Reversed { index: t.len(), seq: x.clone() },
        Value::Str(s) => {
            let chars: Vec<Value> = s.chars().map(|c| {
                let mut buffer = [0u8; 4];
                Value::str(c.encode_utf8(&mut buffer))
            }).collect();
            Iter::Reversed { index: chars.len(), seq: Value::tuple(chars) }
        }
        Value::Bytes(b) => {
            let items: Vec<Value> = b.iter().map(|x| Value::int(*x as i64)).collect();
            Iter::Reversed { index: items.len(), seq: Value::tuple(items) }
        }
        Value::Range(r) => {
            let n = r.len().max(0);
            if n == 0 {
                Iter::Done
            } else {
                let last = r.get(n - 1);
                Iter::Range { next: last, stop: r.start - r.step, step: -r.step }
            }
        }
        Value::Dict(d) => {
            let keys = d.borrow().keys();
            Iter::Reversed { index: keys.len(), seq: Value::tuple(keys) }
        }
        Value::View(v) => {
            let items = vm.collect(&x)?;
            let _ = v;
            Iter::Reversed { index: items.len(), seq: Value::tuple(items) }
        }
        Value::Instance(i) => {
            if let Some(r) = vm.call_special(&x, "__reversed__", &[])? {
                return Ok(r);
            }
            let class = i.class();
            if class.lookup("__len__").is_some() && class.lookup("__getitem__").is_some() {
                let n = vm.len(&x)?;
                Iter::Reversed { index: n, seq: x.clone() }
            } else {
                let inner = vm.unwrap_builtin(&x);
                if !inner.is(&x) {
                    return reversed(vm, &[inner], &[]);
                }
                let message = alloc::format!("'{}' object is not reversible", class.name);
                return vm.type_error(&message);
            }
        }
        other => {
            let name = vm.type_name(other);
            let message = alloc::format!("'{}' object is not reversible", name);
            return vm.type_error(&message);
        }
    };
    Ok(Value::Iter(shared(state)))
}

fn sorted(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    if a.len() != 1 {
        let message = alloc::format!("sorted expected 1 argument, got {}", a.len());
        return vm.type_error(&message);
    }
    for (k, _) in kw {
        if !matches!(&**k, "key" | "reverse") {
            let message = alloc::format!("sort() got an unexpected keyword argument '{}'", k);
            return vm.type_error(&message);
        }
    }
    let items = vm.collect(&a[0])?;
    let key = args::keyword(kw, "key").cloned().unwrap_or(Value::None);
    let reverse = match args::keyword(kw, "reverse") {
        Some(r) => vm.truthy(r)?,
        None => false,
    };
    let sorted = super::types::seq::sort_values(vm, items, &key, reverse)?;
    Ok(Value::list(sorted))
}

/// `min` and `max`.
fn extreme(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)], name: &str, op: CmpOp) -> PyResult<Value> {
    for (k, _) in kw {
        if !matches!(&**k, "key" | "default") {
            let message = alloc::format!("{}() got an unexpected keyword argument '{}'", name, k);
            return vm.type_error(&message);
        }
    }
    let key = args::keyword(kw, "key").cloned().unwrap_or(Value::None);
    let default = args::keyword(kw, "default").cloned();
    let items = match a.len() {
        0 => {
            let message = alloc::format!("{} expected at least 1 argument, got 0", name);
            return vm.type_error(&message);
        }
        1 => vm.collect(&a[0])?,
        _ => {
            if default.is_some() {
                let message = alloc::format!("Cannot specify a default for {}() with multiple positional arguments", name);
                return vm.type_error(&message);
            }
            a.to_vec()
        }
    };
    let mut best: Option<(Value, Value)> = None;
    for item in items {
        let k = if key.is_none() { item.clone() } else { vm.call(&key, core::slice::from_ref(&item), &[])? };
        match &best {
            None => best = Some((k, item)),
            Some((best_key, _)) => {
                let better = vm.compare(op, &k, best_key)?;
                if vm.truthy(&better)? {
                    best = Some((k, item));
                }
            }
        }
    }
    match best {
        Some((_, v)) => Ok(v),
        None => match default {
            Some(d) => Ok(d),
            None => {
                let message = alloc::format!("{}() iterable argument is empty", name);
                vm.value_error(&message)
            }
        },
    }
}

fn min(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    extreme(vm, a, kw, "min", CmpOp::Lt)
}

fn max(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    extreme(vm, a, kw, "max", CmpOp::Gt)
}

fn all(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let x = first(vm, a, "all")?;
    let iterator = vm.get_iter(&x)?;
    while let Some(v) = vm.next(&iterator)? {
        if !vm.truthy(&v)? {
            return Ok(Value::Bool(false));
        }
    }
    Ok(Value::Bool(true))
}

fn any(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let x = first(vm, a, "any")?;
    let iterator = vm.get_iter(&x)?;
    while let Some(v) = vm.next(&iterator)? {
        if vm.truthy(&v)? {
            return Ok(Value::Bool(true));
        }
    }
    Ok(Value::Bool(false))
}

// ------------------------------------------------------------
//  Objects and types
// ------------------------------------------------------------

pub fn callable_value(vm: &mut Vm, value: &Value) -> bool {
    match value {
        Value::Function(_) | Value::Builtin(_) | Value::Method(_) | Value::Class(_) => true,
        Value::Instance(i) => i.class().lookup("__call__").is_some(),
        _ => {
            let _ = vm;
            false
        }
    }
}

fn callable(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let x = first(vm, a, "callable")?;
    Ok(Value::Bool(callable_value(vm, &x)))
}

fn attr_name(vm: &mut Vm, value: Option<&Value>) -> PyResult<Rc<str>> {
    match value {
        Some(Value::Str(s)) => Ok(s.clone()),
        Some(other) => {
            let name = vm.type_name(other);
            let message = alloc::format!("attribute name must be string, not '{}'", name);
            vm.type_error(&message)
        }
        None => vm.type_error("expected at least 2 arguments"),
    }
}

fn getattr(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let object = first(vm, a, "getattr")?;
    let name = attr_name(vm, a.get(1))?;
    match vm.get_attr(&object, &name) {
        Ok(v) => Ok(v),
        Err(e) => {
            let class = vm.types.attribute_error.clone();
            match a.get(2) {
                Some(default) if vm.is_instance_of(&e, &class) => Ok(default.clone()),
                _ => Err(e),
            }
        }
    }
}

fn hasattr(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let object = first(vm, a, "hasattr")?;
    let name = attr_name(vm, a.get(1))?;
    match vm.get_attr(&object, &name) {
        Ok(_) => Ok(Value::Bool(true)),
        Err(e) => {
            let class = vm.types.attribute_error.clone();
            if vm.is_instance_of(&e, &class) { Ok(Value::Bool(false)) } else { Err(e) }
        }
    }
}

fn setattr(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let object = first(vm, a, "setattr")?;
    let name = attr_name(vm, a.get(1))?;
    let Some(value) = a.get(2) else {
        return vm.type_error("setattr expected 3 arguments, got 2");
    };
    vm.set_attr(&object, &name, value.clone())?;
    Ok(Value::None)
}

fn delattr(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let object = first(vm, a, "delattr")?;
    let name = attr_name(vm, a.get(1))?;
    vm.del_attr(&object, &name)?;
    Ok(Value::None)
}

/// Is `class` (a class or a tuple of them) matched by `of`?
fn class_matches(vm: &mut Vm, class: &Rc<Class>, of: &Value, function: &str) -> PyResult<bool> {
    if vm.is_union(of) {
        for member in vm.union_members(of) {
            if class_matches(vm, class, &member, function)? {
                return Ok(true);
            }
        }
        return Ok(false);
    }
    match of {
        Value::Class(c) => Ok(class.is_subclass(c)),
        Value::Tuple(items) => {
            for item in items.iter() {
                if class_matches(vm, class, item, function)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        _ => {
            let message = alloc::format!("{}() arg 2 must be a type, a tuple of types, or a union", function);
            vm.type_error(&message)
        }
    }
}

/// `isinstance(object, of)`: tuples member by member, then the
/// metaclass's `__instancecheck__` (abc.ABCMeta), then the MRO.
fn instance_check(vm: &mut Vm, object: &Value, of: &Value) -> PyResult<bool> {
    let class = vm.class_of(object);
    match of {
        Value::Class(c) if class.id == c.id => return Ok(true),
        Value::Tuple(items) => {
            for item in items.iter() {
                if instance_check(vm, object, item)? {
                    return Ok(true);
                }
            }
            return Ok(false);
        }
        Value::Class(c) => {
            let meta = c.metaclass.borrow().clone();
            if let Some(hook) = meta.and_then(|m| m.lookup("__instancecheck__")) {
                if matches!(hook, Value::Function(_)) {
                    let r = vm.call(&hook, &[of.clone(), object.clone()], &[])?;
                    return vm.truthy(&r);
                }
            }
        }
        _ => {}
    }
    class_matches(vm, &class, of, "isinstance")
}

/// `issubclass(class, of)`, through the metaclass's `__subclasscheck__`.
fn subclass_check(vm: &mut Vm, class: &Rc<Class>, of: &Value) -> PyResult<bool> {
    match of {
        Value::Tuple(items) => {
            for item in items.iter() {
                if subclass_check(vm, class, item)? {
                    return Ok(true);
                }
            }
            return Ok(false);
        }
        Value::Class(c) => {
            let meta = c.metaclass.borrow().clone();
            if let Some(hook) = meta.and_then(|m| m.lookup("__subclasscheck__")) {
                if matches!(hook, Value::Function(_)) {
                    let r = vm.call(&hook, &[of.clone(), Value::Class(class.clone())], &[])?;
                    return vm.truthy(&r);
                }
            }
        }
        _ => {}
    }
    class_matches(vm, class, of, "issubclass")
}

fn isinstance(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let (Some(object), Some(of)) = (a.first(), a.get(1)) else {
        let message = alloc::format!("isinstance expected 2 arguments, got {}", a.len());
        return vm.type_error(&message);
    };
    Ok(Value::Bool(instance_check(vm, object, of)?))
}

fn issubclass(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let (Some(class), Some(of)) = (a.first(), a.get(1)) else {
        let message = alloc::format!("issubclass expected 2 arguments, got {}", a.len());
        return vm.type_error(&message);
    };
    let Value::Class(class) = class else {
        return vm.type_error("issubclass() arg 1 must be a class");
    };
    let class = class.clone();
    Ok(Value::Bool(subclass_check(vm, &class, of)?))
}

fn id(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let x = first(vm, a, "id")?;
    let id = match &x {
        Value::None => 0x1000,
        Value::Bool(b) => 0x1010 + *b as usize * 16,
        Value::Int(Int::Small(i)) => 0x10_0000 + (*i as usize).wrapping_mul(32),
        Value::Float(f) => (f.0 >> 4) as usize,
        other => other.pointer().unwrap_or(0),
    };
    Ok(Value::int(id as i64))
}

fn hash(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let x = first(vm, a, "hash")?;
    Ok(Value::int(vm.hash(&x)?))
}

/// The current frame's local names, as a dict (a snapshot in a
/// function, the namespace itself in a module or class body).
pub fn locals_of(vm: &mut Vm) -> Rc<RefCell<Dict>> {
    let Some(frame) = vm.frames.last() else {
        return shared(Dict::new());
    };
    if let Some(ns) = &frame.namespace {
        return ns.clone();
    }
    let mut dict = Dict::new();
    for (name, value) in frame.code.varnames.iter().zip(frame.fast.iter()) {
        if let Some(v) = value {
            dict.set_str(name, v.clone());
        }
    }
    let cell_names: Vec<Rc<str>> = frame.code.cellvars.iter().chain(frame.code.freevars.iter()).cloned().collect();
    for (name, cell) in cell_names.iter().zip(frame.cells.iter()) {
        if let Some(v) = cell.borrow().clone() {
            dict.set_str(name, v);
        }
    }
    shared(dict)
}

fn globals(vm: &mut Vm, _: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    match vm.frames.last() {
        Some(frame) => Ok(Value::Dict(frame.globals.dict.clone())),
        None => Ok(Value::Dict(shared(Dict::new()))),
    }
}

fn locals(vm: &mut Vm, _: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    Ok(Value::Dict(locals_of(vm)))
}

fn vars(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    match a.first() {
        None => Ok(Value::Dict(locals_of(vm))),
        Some(Value::Instance(i)) => Ok(Value::Dict(i.dict.clone())),
        Some(Value::Module(m)) => Ok(Value::Dict(m.dict.clone())),
        Some(Value::Class(c)) => Ok(Value::Dict(shared(c.dict.borrow().clone()))),
        Some(other) => {
            let _ = other;
            vm.type_error("vars() argument must have __dict__ attribute")
        }
    }
}

fn dir(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    match a.first() {
        None => {
            let ns = locals_of(vm);
            let mut names: Vec<Rc<str>> = ns.borrow().keys().into_iter().filter_map(|k| if let Value::Str(s) = k { Some(s) } else { None }).collect();
            names.sort();
            Ok(Value::list(names.into_iter().map(Value::Str).collect()))
        }
        Some(x) => {
            if let Value::Instance(i) = x {
                if let Some(Value::Function(_)) = i.class().lookup("__dir__") {
                    let r = vm.call_special(x, "__dir__", &[])?.unwrap_or(Value::None);
                    let items = vm.collect(&r)?;
                    let sorted = super::types::seq::sort_values(vm, items, &Value::None, false)?;
                    return Ok(Value::list(sorted));
                }
            }
            Ok(Value::list(super::types::object::attribute_names(vm, x)))
        }
    }
}

// ------------------------------------------------------------
//  Code
// ------------------------------------------------------------

/// `eval` and `exec`: the namespaces they run in.
fn namespaces(vm: &mut Vm, globals: Option<&Value>, locals: Option<&Value>) -> PyResult<(Rc<Module>, Option<Rc<RefCell<Dict>>>)> {
    match globals {
        None | Some(Value::None) => {
            let module = match vm.frames.last() {
                Some(f) => f.globals.clone(),
                None => vm.builtins.clone(),
            };
            let ns = match locals {
                Some(Value::Dict(d)) => d.clone(),
                _ => locals_of(vm),
            };
            Ok((module, Some(ns)))
        }
        Some(Value::Dict(d)) => {
            if d.borrow().get_str("__builtins__").is_none() {
                d.borrow_mut().set_str("__builtins__", Value::Module(vm.builtins.clone()));
            }
            let module = Rc::new(Module { name: Rc::from("__main__"), dict: d.clone(), file: Rc::from("") });
            let ns = match locals {
                Some(Value::Dict(l)) => l.clone(),
                _ => d.clone(),
            };
            Ok((module, Some(ns)))
        }
        Some(other) => {
            let name = vm.type_name(other);
            let message = alloc::format!("globals must be a dict, not {}", name);
            vm.type_error(&message)
        }
    }
}

fn source_text(vm: &mut Vm, value: Option<&Value>, function: &str) -> PyResult<String> {
    match value {
        Some(Value::Str(s)) => Ok(String::from(&**s)),
        Some(Value::Bytes(b)) => Ok(String::from_utf8_lossy(b).into_owned()),
        _ => {
            let message = alloc::format!("{}() arg 1 must be a string, bytes or code object", function);
            vm.type_error(&message)
        }
    }
}

fn eval(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let (module, ns) = namespaces(vm, a.get(1), a.get(2))?;
    if let Some(Value::Code(code)) = a.first() {
        return super::run_code(vm, code.clone(), module, ns);
    }
    let source = source_text(vm, a.first(), "eval")?;
    let trimmed = source.trim_start_matches([' ', '\t']);
    let code = super::compile(vm, trimmed, "<string>", super::Mode::Eval)?;
    super::run_code(vm, code, module, ns)
}

fn exec(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let (module, ns) = namespaces(vm, a.get(1), a.get(2))?;
    let code = match a.first() {
        Some(Value::Code(code)) => code.clone(),
        other => {
            let source = source_text(vm, other, "exec")?;
            super::compile(vm, &source, "<string>", super::Mode::Module)?
        }
    };
    super::run_code(vm, code, module, ns)?;
    Ok(Value::None)
}

/// `compile(source, filename, mode)`: a code object for exec or eval.
fn compile(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let source = args::get(a, kw, 0, "source").cloned();
    if let Some(Value::Code(_)) = source {
        return Ok(source.unwrap_or(Value::None));
    }
    let text = source_text(vm, source.as_ref(), "compile")?;
    let filename = match args::get(a, kw, 1, "filename") {
        Some(v) => super::modules::system::fspath(vm, v)?,
        None => return vm.type_error("compile() missing required argument 'filename' (pos 2)"),
    };
    let mode = match args::get(a, kw, 2, "mode") {
        Some(Value::Str(m)) => m.clone(),
        _ => return vm.type_error("compile() missing required argument 'mode' (pos 3)"),
    };
    let code = match &*mode {
        "exec" | "single" => super::compile(vm, &text, &filename, super::Mode::Module)?,
        "eval" => super::compile(vm, text.trim_start_matches([' ', '\t']), &filename, super::Mode::Eval)?,
        _ => return vm.value_error("compile() mode must be 'exec', 'eval' or 'single'"),
    };
    Ok(Value::Code(code))
}

fn import(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let name = match args::get(a, kw, 0, "name") {
        Some(Value::Str(s)) => s.clone(),
        _ => return vm.type_error("__import__() argument 1 must be str"),
    };
    let fromlist = args::get(a, kw, 3, "fromlist").cloned().unwrap_or(Value::None);
    let level = args::opt_int(vm, args::get(a, kw, 4, "level"), 0)?;
    super::modules::import(vm, &name, level.max(0) as usize, &fromlist)
}

fn exit(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let class = vm.types.system_exit.clone();
    let made = vm.call(&Value::Class(class), a, &[])?;
    Err(super::vm::PyErr::Exc(made))
}

/// Installs the builtins in a new module.
pub fn module(vm: &mut Vm) -> Rc<Module> {
    let dict = shared(Dict::new());
    let functions: &[(&'static str, NativeFn)] = &[
        ("print", print), ("input", input), ("open", open), ("abs", abs), ("bin", bin), ("oct", oct), ("hex", hex),
        ("divmod", divmod), ("pow", pow), ("round", round), ("sum", sum), ("chr", chr), ("ord", ord), ("repr", repr),
        ("ascii", ascii), ("format", format), ("len", len), ("iter", iter), ("next", next), ("enumerate", enumerate),
        ("zip", zip), ("map", map), ("filter", filter), ("reversed", reversed), ("sorted", sorted), ("min", min),
        ("max", max), ("all", all), ("any", any), ("callable", callable), ("getattr", getattr), ("hasattr", hasattr),
        ("setattr", setattr), ("delattr", delattr), ("isinstance", isinstance), ("issubclass", issubclass), ("id", id),
        ("hash", hash), ("globals", globals), ("locals", locals), ("vars", vars), ("dir", dir), ("eval", eval),
        ("exec", exec), ("compile", compile), ("__import__", import), ("exit", exit), ("quit", exit),
    ];
    {
        let mut d = dict.borrow_mut();
        for (name, f) in functions {
            d.set_str(name, native(name, *f));
        }
        for class in vm.types.named() {
            let name = class.name.clone();
            d.set_str(&name, Value::Class(class));
        }
        d.set_str("NotImplemented", Value::NotImplemented);
        d.set_str("Ellipsis", Value::Ellipsis);
        d.set_str("__name__", Value::str("builtins"));
        d.set_str("__debug__", Value::Bool(true));
        // Aliases CPython keeps.
        let os_error = Value::Class(vm.types.os_error.clone());
        d.set_str("IOError", os_error.clone());
        d.set_str("EnvironmentError", os_error);
    }
    Rc::new(Module { name: Rc::from("builtins"), dict, file: Rc::from("") })
}
