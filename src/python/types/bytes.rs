//! The methods of `bytes` and `bytearray`.
//!
//! Most of them are those of `str` on the Latin-1 reading of the bytes
//! (byte n is the character U+00nn): `split`, `replace`, `find`,
//! `partition`... give the same positions and pieces, which are read back
//! as bytes. Where `bytes` differs from `str` -- the case of letters and
//! what counts as blank space are ASCII only -- the method is written
//! here. A `bytearray` answers with `bytearray`s, and has the methods of
//! a mutable sequence besides.

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

use super::super::args;
use super::super::value::*;
use super::super::vm::{PyResult, Vm};

/// Bytes as the Latin-1 text of the same code points.
pub fn latin1(b: &[u8]) -> String {
    b.iter().map(|&c| c as char).collect()
}

fn from_latin1(s: &str) -> Vec<u8> {
    s.chars().map(|c| c as u32 as u8).collect()
}

/// The bytes of a bytes-like value.
pub fn bytes_of(value: &Value) -> Option<Vec<u8>> {
    match value {
        Value::Bytes(b) => Some(b.to_vec()),
        Value::ByteArray(b) => Some(b.borrow().clone()),
        _ => None,
    }
}

pub fn new_bytearray_value(bytes: Vec<u8>) -> Value {
    Value::ByteArray(Rc::new(RefCell::new(bytes)))
}

fn wrap(bytes: Vec<u8>, array: bool) -> Value {
    if array { new_bytearray_value(bytes) } else { Value::Bytes(Rc::from(bytes)) }
}

/// A result of a `str` method read back as bytes.
fn back(value: Value, array: bool) -> Value {
    match value {
        Value::Str(s) => wrap(from_latin1(&s), array),
        Value::List(items) => {
            let converted: Vec<Value> = items.borrow().iter().cloned().map(|v| back(v, array)).collect();
            Value::list(converted)
        }
        Value::Tuple(items) => Value::tuple(items.iter().cloned().map(|v| back(v, array)).collect()),
        other => other,
    }
}

const ASCII_SPACE: &[u8] = b" \t\n\r\x0b\x0c";

fn this(vm: &mut Vm, value: Option<&Value>) -> PyResult<(Vec<u8>, bool)> {
    match value {
        Some(Value::Bytes(b)) => Ok((b.to_vec(), false)),
        Some(Value::ByteArray(b)) => Ok((b.borrow().clone(), true)),
        Some(v @ Value::Instance(_)) => {
            let inner = vm.unwrap_builtin(v);
            match &inner {
                Value::Bytes(b) => Ok((b.to_vec(), false)),
                _ => vm.type_error("descriptor requires a 'bytes' object"),
            }
        }
        _ => vm.type_error("descriptor requires a 'bytes' object"),
    }
}

/// An argument of a bytes method, as Latin-1 text.
fn argument(vm: &mut Vm, value: &Value, allow_int: bool) -> PyResult<Value> {
    if let Some(b) = bytes_of(value) {
        return Ok(Value::str(&latin1(&b)));
    }
    match value {
        Value::Int(_) | Value::Bool(_) if allow_int => {
            let n = args::int(vm, value)?;
            if !(0..=255).contains(&n) {
                return vm.value_error("byte must be in range(0, 256)");
            }
            Ok(Value::str(&latin1(&[n as u8])))
        }
        Value::Tuple(items) => {
            let mut converted = Vec::new();
            for item in items.iter() {
                converted.push(argument(vm, item, false)?);
            }
            Ok(Value::tuple(converted))
        }
        Value::None | Value::Int(_) | Value::Bool(_) => Ok(value.clone()),
        other => {
            let name = vm.type_name(other);
            vm.type_error(&alloc::format!("a bytes-like object is required, not '{}'", name))
        }
    }
}

/// A method of `str` applied to the Latin-1 reading.
fn via_str(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let Some(Value::Str(name)) = a.first() else {
        return vm.type_error("methode d'octets sans nom");
    };
    let name = name.clone();
    let (bytes, array) = this(vm, a.get(1))?;
    let allow_int = matches!(&*name, "find" | "rfind" | "index" | "rindex" | "count");
    let mut converted = Vec::with_capacity(a.len().saturating_sub(2));
    if &*name == "join" {
        let Some(iterable) = a.get(2) else {
            return vm.type_error("join() takes exactly one argument (0 given)");
        };
        let items = vm.collect(iterable)?;
        let mut parts = Vec::with_capacity(items.len());
        for (index, item) in items.iter().enumerate() {
            match bytes_of(item) {
                Some(b) => parts.push(Value::str(&latin1(&b))),
                None => {
                    let kind = vm.type_name(item);
                    return vm.type_error(&alloc::format!(
                        "sequence item {}: expected a bytes-like object, {} found",
                        index, kind
                    ));
                }
            }
        }
        converted.push(Value::list(parts));
    } else {
        for value in a[2..].iter() {
            converted.push(argument(vm, value, allow_int)?);
        }
    }
    let mut keywords = Vec::new();
    for (key, value) in kw {
        keywords.push((key.clone(), argument(vm, value, false)?));
    }
    let text = Value::str(&latin1(&bytes));
    let method = vm.get_attr(&text, &name)?;
    let result = vm.call(&method, &converted, &keywords)?;
    Ok(back(result, array))
}

fn ascii_map(vm: &mut Vm, a: &[Value], f: fn(&[u8]) -> Vec<u8>) -> PyResult<Value> {
    let (bytes, array) = this(vm, a.first())?;
    Ok(wrap(f(&bytes), array))
}

fn upper(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    ascii_map(vm, a, |b| b.to_ascii_uppercase())
}

fn lower(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    ascii_map(vm, a, |b| b.to_ascii_lowercase())
}

fn swapcase(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    ascii_map(vm, a, |b| {
        b.iter()
            .map(|c| if c.is_ascii_uppercase() { c.to_ascii_lowercase() } else { c.to_ascii_uppercase() })
            .collect()
    })
}

fn capitalize(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    ascii_map(vm, a, |b| {
        b.iter().enumerate().map(|(i, c)| if i == 0 { c.to_ascii_uppercase() } else { c.to_ascii_lowercase() }).collect()
    })
}

fn title(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    ascii_map(vm, a, |b| {
        let mut previous_cased = false;
        b.iter()
            .map(|&c| {
                let out = if previous_cased { c.to_ascii_lowercase() } else { c.to_ascii_uppercase() };
                previous_cased = c.is_ascii_alphabetic();
                out
            })
            .collect()
    })
}

fn predicate(vm: &mut Vm, a: &[Value], test: fn(&[u8]) -> bool) -> PyResult<Value> {
    let (bytes, _) = this(vm, a.first())?;
    Ok(Value::Bool(test(&bytes)))
}

fn isalpha(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    predicate(vm, a, |b| !b.is_empty() && b.iter().all(u8::is_ascii_alphabetic))
}

fn isdigit(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    predicate(vm, a, |b| !b.is_empty() && b.iter().all(u8::is_ascii_digit))
}

fn isalnum(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    predicate(vm, a, |b| !b.is_empty() && b.iter().all(u8::is_ascii_alphanumeric))
}

fn isspace(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    predicate(vm, a, |b| !b.is_empty() && b.iter().all(|c| ASCII_SPACE.contains(c)))
}

fn isupper(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    predicate(vm, a, |b| b.iter().any(u8::is_ascii_uppercase) && !b.iter().any(u8::is_ascii_lowercase))
}

fn islower(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    predicate(vm, a, |b| b.iter().any(u8::is_ascii_lowercase) && !b.iter().any(u8::is_ascii_uppercase))
}

fn isascii(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    predicate(vm, a, |b| b.is_ascii())
}

/// `split()` and `strip()` without an argument: ASCII blank space only.
fn split_or_strip(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let Some(Value::Str(name)) = a.first() else {
        return vm.type_error("methode d'octets sans nom");
    };
    let name = name.clone();
    let explicit = a.get(2).is_some_and(|v| !v.is_none()) || kw.iter().any(|(k, v)| (&**k == "sep" || &**k == "chars") && !v.is_none());
    if explicit {
        return via_str(vm, a, kw);
    }
    let (bytes, array) = this(vm, a.get(1))?;
    let space = |c: &u8| ASCII_SPACE.contains(c);
    match &*name {
        "strip" | "lstrip" | "rstrip" => {
            let mut start = 0;
            let mut end = bytes.len();
            if &*name != "rstrip" {
                while start < end && space(&bytes[start]) {
                    start += 1;
                }
            }
            if &*name != "lstrip" {
                while end > start && space(&bytes[end - 1]) {
                    end -= 1;
                }
            }
            Ok(wrap(bytes[start..end].to_vec(), array))
        }
        _ => {
            // split / rsplit with maxsplit.
            let mut maxsplit = match a.get(3) {
                Some(v) => args::int(vm, v)?,
                None => -1,
            };
            for (key, value) in kw {
                if &**key == "maxsplit" {
                    maxsplit = args::int(vm, value)?;
                }
            }
            let mut words: Vec<Vec<u8>> = Vec::new();
            if &*name == "split" {
                let mut i = 0;
                while i < bytes.len() {
                    while i < bytes.len() && space(&bytes[i]) {
                        i += 1;
                    }
                    if i >= bytes.len() {
                        break;
                    }
                    if maxsplit >= 0 && words.len() as i64 == maxsplit {
                        let mut end = bytes.len();
                        while end > i && space(&bytes[end - 1]) {
                            end -= 1;
                        }
                        words.push(bytes[i..end].to_vec());
                        break;
                    }
                    let start = i;
                    while i < bytes.len() && !space(&bytes[i]) {
                        i += 1;
                    }
                    words.push(bytes[start..i].to_vec());
                }
            } else {
                let mut i = bytes.len();
                while i > 0 {
                    while i > 0 && space(&bytes[i - 1]) {
                        i -= 1;
                    }
                    if i == 0 {
                        break;
                    }
                    if maxsplit >= 0 && words.len() as i64 == maxsplit {
                        let mut start = 0;
                        while start < i && space(&bytes[start]) {
                            start += 1;
                        }
                        words.push(bytes[start..i].to_vec());
                        break;
                    }
                    let end = i;
                    while i > 0 && !space(&bytes[i - 1]) {
                        i -= 1;
                    }
                    words.push(bytes[i..end].to_vec());
                }
                words.reverse();
            }
            Ok(Value::list(words.into_iter().map(|w| wrap(w, array)).collect()))
        }
    }
}

fn decode(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let (bytes, _) = this(vm, a.first())?;
    let mut encoding = String::from("utf-8");
    let mut errors = String::from("strict");
    if let Some(Value::Str(e)) = a.get(1) {
        encoding = String::from(&**e);
    }
    if let Some(Value::Str(e)) = a.get(2) {
        errors = String::from(&**e);
    }
    for (key, value) in kw {
        if let Value::Str(v) = value {
            match &**key {
                "encoding" => encoding = String::from(&**v),
                "errors" => errors = String::from(&**v),
                _ => {}
            }
        }
    }
    let normalized = encoding.to_ascii_lowercase().replace('_', "-");
    match normalized.as_str() {
        "latin-1" | "latin1" | "iso-8859-1" | "iso8859-1" | "l1" => Ok(Value::str(&latin1(&bytes))),
        "ascii" | "us-ascii" => {
            let mut out = String::new();
            for (i, &b) in bytes.iter().enumerate() {
                if b < 128 {
                    out.push(b as char);
                } else {
                    match errors.as_str() {
                        "ignore" => {}
                        "replace" => out.push('\u{FFFD}'),
                        _ => {
                            let class = vm.types.unicode_decode_error.clone();
                            let message = alloc::format!(
                                "'ascii' codec can't decode byte 0x{:02x} in position {}: ordinal not in range(128)",
                                b, i
                            );
                            return Err(vm.error(&class, &message));
                        }
                    }
                }
            }
            Ok(Value::str(&out))
        }
        "utf-8" | "utf8" | "u8" => {
            let mut out = String::new();
            let mut rest: &[u8] = &bytes;
            let mut offset = 0;
            loop {
                match core::str::from_utf8(rest) {
                    Ok(s) => {
                        out.push_str(s);
                        break;
                    }
                    Err(e) => {
                        let valid = e.valid_up_to();
                        out.push_str(core::str::from_utf8(&rest[..valid]).unwrap_or(""));
                        let bad = e.error_len().unwrap_or(rest.len() - valid);
                        match errors.as_str() {
                            "ignore" => {}
                            "replace" => out.push('\u{FFFD}'),
                            _ => {
                                let class = vm.types.unicode_decode_error.clone();
                                let reason = if e.error_len().is_none() { "unexpected end of data" } else { "invalid start byte" };
                                let message = alloc::format!(
                                    "'utf-8' codec can't decode byte 0x{:02x} in position {}: {}",
                                    rest[valid],
                                    offset + valid,
                                    reason
                                );
                                return Err(vm.error(&class, &message));
                            }
                        }
                        offset += valid + bad;
                        rest = &rest[valid + bad..];
                    }
                }
            }
            Ok(Value::str(&out))
        }
        _ => {
            let class = vm.types.lookup_error.clone();
            Err(vm.error(&class, &alloc::format!("unknown encoding: {}", encoding)))
        }
    }
}

fn hex(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let (bytes, _) = this(vm, a.first())?;
    let separator = match a.get(1) {
        Some(Value::Str(s)) => Some(String::from(&**s)),
        Some(v) => bytes_of(v).map(|b| latin1(&b)),
        None => None,
    };
    let parts: Vec<String> = bytes.iter().map(|x| alloc::format!("{:02x}", x)).collect();
    Ok(Value::str(&match separator {
        Some(sep) => parts.join(&sep),
        None => parts.concat(),
    }))
}

fn fromhex(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let array = matches!(a.first(), Some(Value::Class(c)) if &*c.name == "bytearray");
    let Some(Value::Str(text)) = a.get(1) else {
        return vm.type_error("fromhex() argument must be str");
    };
    let mut out = Vec::new();
    let digits: Vec<(usize, char)> = text.char_indices().collect();
    let mut i = 0;
    while i < digits.len() {
        let (position, c) = digits[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        let high = c.to_digit(16);
        let low = digits.get(i + 1).and_then(|(_, d)| d.to_digit(16));
        match (high, low) {
            (Some(h), Some(l)) => out.push((h * 16 + l) as u8),
            _ => {
                let at = if high.is_none() { position } else { digits.get(i + 1).map(|(p, _)| *p).unwrap_or(text.len()) };
                return vm.value_error(&alloc::format!("non-hexadecimal number found in fromhex() arg at position {}", at));
            }
        }
        i += 2;
    }
    Ok(wrap(out, array))
}

fn maketrans(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let (Some(from), Some(to)) = (a.first().and_then(bytes_of), a.get(1).and_then(bytes_of)) else {
        return vm.type_error("maketrans() arguments must be bytes-like");
    };
    if from.len() != to.len() {
        return vm.value_error("maketrans arguments must have same length");
    }
    let mut table: Vec<u8> = (0..=255u8).collect();
    for (f, t) in from.iter().zip(to.iter()) {
        table[*f as usize] = *t;
    }
    Ok(Value::Bytes(Rc::from(table)))
}

fn translate(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let (bytes, array) = this(vm, a.first())?;
    let table = match a.get(1) {
        Some(Value::None) | None => None,
        Some(v) => match bytes_of(v) {
            Some(t) if t.len() == 256 => Some(t),
            _ => return vm.value_error("translation table must be 256 characters long"),
        },
    };
    let mut delete: Vec<u8> = a.get(2).and_then(bytes_of).unwrap_or_default();
    for (key, value) in kw {
        if &**key == "delete" {
            delete = bytes_of(value).unwrap_or_default();
        }
    }
    let out: Vec<u8> = bytes
        .iter()
        .filter(|b| !delete.contains(b))
        .map(|&b| table.as_ref().map_or(b, |t| t[b as usize]))
        .collect();
    Ok(wrap(out, array))
}

// ------------------------------------------------------------
//  bytearray: construction and mutation
// ------------------------------------------------------------

/// The bytes `bytes(x)` / `bytearray(x)` builds from x.
pub fn build(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Vec<u8>> {
    let source = a.first().cloned().or_else(|| kw.iter().find(|(k, _)| &**k == "source").map(|(_, v)| v.clone()));
    let encoding = a.get(1).cloned().or_else(|| kw.iter().find(|(k, _)| &**k == "encoding").map(|(_, v)| v.clone()));
    match source {
        None => Ok(Vec::new()),
        Some(Value::Str(s)) => {
            let Some(Value::Str(encoding)) = encoding else {
                return vm.type_error("string argument without an encoding");
            };
            let normalized = encoding.to_ascii_lowercase().replace('_', "-");
            match normalized.as_str() {
                "latin-1" | "latin1" | "iso-8859-1" => {
                    if let Some(c) = s.chars().find(|c| *c as u32 > 255) {
                        let class = vm.types.unicode_encode_error.clone();
                        let position = s.chars().position(|x| x == c).unwrap_or(0);
                        let message = alloc::format!(
                            "'latin-1' codec can't encode character '\\u{:04x}' in position {}: ordinal not in range(256)",
                            c as u32, position
                        );
                        return Err(vm.error(&class, &message));
                    }
                    Ok(from_latin1(&s))
                }
                "ascii" => {
                    if let Some((position, c)) = s.chars().enumerate().find(|(_, c)| *c as u32 > 127) {
                        let class = vm.types.unicode_encode_error.clone();
                        let message = alloc::format!(
                            "'ascii' codec can't encode character '\\x{:02x}' in position {}: ordinal not in range(128)",
                            c as u32, position
                        );
                        return Err(vm.error(&class, &message));
                    }
                    Ok(s.as_bytes().to_vec())
                }
                _ => Ok(s.as_bytes().to_vec()),
            }
        }
        Some(Value::Int(_) | Value::Bool(_)) => {
            let n = args::int(vm, a.first().unwrap_or(&Value::None))?;
            if n < 0 {
                return vm.value_error("negative count");
            }
            if n > 1 << 28 {
                let class = vm.types.memory_error.clone();
                return Err(vm.error(&class, ""));
            }
            Ok(alloc::vec![0u8; n as usize])
        }
        Some(other) => {
            if let Some(b) = bytes_of(&other) {
                return Ok(b);
            }
            if let Some(method) = vm.special(&other, "__bytes__")? {
                let result = vm.call(&method, &[], &[])?;
                if let Some(b) = bytes_of(&result) {
                    return Ok(b);
                }
            }
            let items = vm.collect(&other)?;
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                let n = args::int(vm, &item)?;
                if !(0..=255).contains(&n) {
                    return vm.value_error("byte must be in range(0, 256)");
                }
                out.push(n as u8);
            }
            Ok(out)
        }
    }
}

pub fn new_bytearray(vm: &mut Vm, _class: &Rc<Class>, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    Ok(new_bytearray_value(build(vm, a, kw)?))
}

fn this_array(vm: &mut Vm, a: &[Value]) -> PyResult<Rc<RefCell<Vec<u8>>>> {
    match a.first() {
        Some(Value::ByteArray(b)) => Ok(b.clone()),
        _ => vm.type_error("descriptor requires a 'bytearray' object"),
    }
}

fn byte_value(vm: &mut Vm, value: Option<&Value>) -> PyResult<u8> {
    let Some(value) = value else {
        return vm.type_error("an integer is required");
    };
    if !matches!(value, Value::Int(_) | Value::Bool(_)) {
        let name = vm.type_name(value);
        return vm.type_error(&alloc::format!("'{}' object cannot be interpreted as an integer", name));
    }
    let n = args::int(vm, value)?;
    if !(0..=255).contains(&n) {
        return vm.value_error("byte must be in range(0, 256)");
    }
    Ok(n as u8)
}

fn ba_append(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let array = this_array(vm, a)?;
    let byte = byte_value(vm, a.get(1))?;
    array.borrow_mut().push(byte);
    Ok(Value::None)
}

fn ba_extend(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let array = this_array(vm, a)?;
    let Some(source) = a.get(1) else {
        return vm.type_error("extend() takes exactly one argument (0 given)");
    };
    if matches!(source, Value::Str(_)) {
        return vm.type_error("expected iterable of integers; got: 'str'");
    }
    let bytes = build(vm, core::slice::from_ref(source), &[])?;
    array.borrow_mut().extend(bytes);
    Ok(Value::None)
}

fn ba_insert(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let array = this_array(vm, a)?;
    let index = args::int(vm, a.get(1).unwrap_or(&Value::None))?;
    let byte = byte_value(vm, a.get(2))?;
    let len = array.borrow().len() as i64;
    let at = if index < 0 { (index + len).max(0) } else { index.min(len) } as usize;
    array.borrow_mut().insert(at, byte);
    Ok(Value::None)
}

fn ba_pop(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let array = this_array(vm, a)?;
    let len = array.borrow().len() as i64;
    if len == 0 {
        return vm.index_error("pop from empty bytearray");
    }
    let index = match a.get(1) {
        Some(v) => args::int(vm, v)?,
        None => -1,
    };
    let at = if index < 0 { index + len } else { index };
    if !(0..len).contains(&at) {
        return vm.index_error("pop index out of range");
    }
    let byte = array.borrow_mut().remove(at as usize);
    Ok(Value::int(byte as i64))
}

fn ba_remove(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let array = this_array(vm, a)?;
    let byte = byte_value(vm, a.get(1))?;
    let position = array.borrow().iter().position(|b| *b == byte);
    match position {
        Some(i) => {
            array.borrow_mut().remove(i);
            Ok(Value::None)
        }
        None => vm.value_error("value not found in bytearray"),
    }
}

fn ba_reverse(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let array = this_array(vm, a)?;
    array.borrow_mut().reverse();
    Ok(Value::None)
}

fn ba_clear(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let array = this_array(vm, a)?;
    array.borrow_mut().clear();
    Ok(Value::None)
}

fn ba_copy(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let array = this_array(vm, a)?;
    let copy = array.borrow().clone();
    Ok(new_bytearray_value(copy))
}

/// Methods taken from `str`.
const VIA_STR: &[&str] = &[
    "center", "count", "endswith", "expandtabs", "find", "index", "join", "ljust", "partition", "removeprefix",
    "removesuffix", "replace", "rfind", "rindex", "rjust", "rpartition", "splitlines", "startswith", "zfill",
];

pub fn install(vm: &mut Vm) {
    let bytes = vm.types.bytes.clone();
    let bytearray = vm.types.bytearray.clone();
    for class in [&bytes, &bytearray] {
        for name in VIA_STR {
            class.dict.borrow_mut().set_str(
                name,
                Value::Builtin(Rc::new(Builtin { name, call: via_str, method: true, data: Value::str(name) })),
            );
        }
        for name in ["split", "rsplit", "strip", "lstrip", "rstrip"] {
            class.dict.borrow_mut().set_str(
                name,
                Value::Builtin(Rc::new(Builtin { name, call: split_or_strip, method: true, data: Value::str(name) })),
            );
        }
        let entries: &[(&'static str, NativeFn)] = &[
            ("upper", upper), ("lower", lower), ("swapcase", swapcase), ("capitalize", capitalize), ("title", title),
            ("isalpha", isalpha), ("isdigit", isdigit), ("isalnum", isalnum), ("isspace", isspace),
            ("isupper", isupper), ("islower", islower), ("isascii", isascii), ("decode", decode), ("hex", hex),
            ("translate", translate),
        ];
        for (name, f) in entries {
            super::method(class, name, *f);
        }
        class.dict.borrow_mut().set_str("fromhex", Value::ClassMethod(Rc::new(super::native("fromhex", fromhex))));
        class.dict.borrow_mut().set_str("maketrans", Value::StaticMethod(Rc::new(super::native("maketrans", maketrans))));
    }
    let entries: &[(&'static str, NativeFn)] = &[
        ("append", ba_append), ("extend", ba_extend), ("insert", ba_insert), ("pop", ba_pop), ("remove", ba_remove),
        ("reverse", ba_reverse), ("clear", ba_clear), ("copy", ba_copy),
    ];
    for (name, f) in entries {
        super::method(&bytearray, name, *f);
    }
}
