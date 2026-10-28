//! Sequences: `list`, `tuple`, `range`, `bytes`, and what indexes them.

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

use super::super::args;
use super::super::ast::CmpOp;
use super::super::int::Int;
use super::super::value::*;
use super::super::vm::{PyResult, Vm};
use super::{index_of, method, slice_indices};

// ------------------------------------------------------------
//  Constructors
// ------------------------------------------------------------

pub fn new_list(vm: &mut Vm, _class: &Rc<Class>, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    args::no_keywords(vm, "list", kw)?;
    args::count(vm, "list", a, 0, 1)?;
    let items = match a.first() {
        Some(v) => vm.collect(v)?,
        None => Vec::new(),
    };
    Ok(Value::list(items))
}

pub fn new_tuple(vm: &mut Vm, _class: &Rc<Class>, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    args::no_keywords(vm, "tuple", kw)?;
    args::count(vm, "tuple", a, 0, 1)?;
    match a.first() {
        Some(Value::Tuple(t)) => Ok(Value::Tuple(t.clone())),
        Some(v) => Ok(Value::tuple(vm.collect(v)?)),
        None => Ok(Value::tuple(Vec::new())),
    }
}

pub fn new_bytes(vm: &mut Vm, _class: &Rc<Class>, a: &[Value], _kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    match a.first() {
        None => Ok(Value::Bytes(Rc::from(Vec::new()))),
        Some(Value::Str(s)) => {
            if a.get(1).is_none() {
                return vm.type_error("string argument without an encoding");
            }
            Ok(Value::Bytes(Rc::from(s.as_bytes())))
        }
        Some(Value::Int(Int::Small(n))) => {
            if *n < 0 {
                return vm.value_error("negative count");
            }
            Ok(Value::Bytes(Rc::from(alloc::vec![0u8; *n as usize])))
        }
        Some(Value::Bytes(b)) => Ok(Value::Bytes(b.clone())),
        Some(other) => {
            let items = vm.collect(other)?;
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                let n = args::int(vm, &item)?;
                if !(0..=255).contains(&n) {
                    return vm.value_error("bytes must be in range(0, 256)");
                }
                out.push(n as u8);
            }
            Ok(Value::Bytes(Rc::from(out)))
        }
    }
}

pub fn new_range(vm: &mut Vm, _class: &Rc<Class>, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    args::no_keywords(vm, "range", kw)?;
    let ints: Vec<i64> = {
        let mut out = Vec::new();
        for v in a {
            out.push(args::int(vm, v)?);
        }
        out
    };
    let (start, stop, step) = match ints.as_slice() {
        [stop] => (0, *stop, 1),
        [start, stop] => (*start, *stop, 1),
        [start, stop, step] => (*start, *stop, *step),
        [] => return vm.type_error("range expected at least 1 argument, got 0"),
        _ => {
            let message = alloc::format!("range expected at most 3 arguments, got {}", ints.len());
            return vm.type_error(&message);
        }
    };
    if step == 0 {
        return vm.value_error("range() arg 3 must not be zero");
    }
    Ok(Value::Range(Rc::new(Range { start, stop, step })))
}

pub fn new_slice(vm: &mut Vm, _class: &Rc<Class>, a: &[Value], _kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let (start, stop, step) = match a {
        [stop] => (Value::None, stop.clone(), Value::None),
        [start, stop] => (start.clone(), stop.clone(), Value::None),
        [start, stop, step] => (start.clone(), stop.clone(), step.clone()),
        _ => return vm.type_error("slice expected at least 1 argument, got 0"),
    };
    Ok(Value::Slice(Rc::new([start, stop, step])))
}

// ------------------------------------------------------------
//  Subscripts
// ------------------------------------------------------------

pub fn get_item(vm: &mut Vm, object: &Value, index: &Value) -> PyResult<Value> {
    let what = match object {
        Value::List(_) => "list",
        Value::Tuple(_) => "tuple",
        Value::Str(_) => "string",
        Value::Bytes(_) => "index",
        _ => "range object",
    };
    if let Value::Slice(slice) = index {
        return slice_of(vm, object, slice);
    }
    match object {
        Value::List(l) => {
            let len = l.borrow().len();
            let i = index_of(vm, index, len, what)?;
            Ok(l.borrow()[i].clone())
        }
        Value::Tuple(t) => {
            let i = index_of(vm, index, t.len(), what)?;
            Ok(t[i].clone())
        }
        Value::Str(s) => {
            let len = super::str::char_len(s);
            let i = index_of(vm, index, len, what)?;
            if s.is_ascii() {
                return Ok(Value::str(&s[i..i + 1]));
            }
            let c = s.chars().nth(i).unwrap_or(' ');
            let mut buffer = [0u8; 4];
            Ok(Value::str(c.encode_utf8(&mut buffer)))
        }
        Value::Bytes(b) => {
            let i = index_of(vm, index, b.len(), what)?;
            Ok(Value::int(b[i] as i64))
        }
        Value::Range(r) => {
            let len = r.len().max(0) as usize;
            let i = index_of(vm, index, len, what)?;
            Ok(Value::int(r.get(i as i64)))
        }
        _ => vm.type_error("objet non indexable"),
    }
}

fn slice_of(vm: &mut Vm, object: &Value, slice: &[Value; 3]) -> PyResult<Value> {
    match object {
        Value::List(l) => {
            let items = l.borrow().clone();
            let (start, _, step, count) = slice_indices(vm, slice, items.len())?;
            Ok(Value::list((0..count).map(|k| items[(start + k as i64 * step) as usize].clone()).collect()))
        }
        Value::Tuple(t) => {
            let (start, _, step, count) = slice_indices(vm, slice, t.len())?;
            Ok(Value::tuple((0..count).map(|k| t[(start + k as i64 * step) as usize].clone()).collect()))
        }
        Value::Str(s) => {
            if s.is_ascii() {
                let (start, stop, step, count) = slice_indices(vm, slice, s.len())?;
                if step == 1 {
                    return Ok(Value::str(&s[start as usize..stop.max(start) as usize]));
                }
                let bytes = s.as_bytes();
                let out: String = (0..count).map(|k| bytes[(start + k as i64 * step) as usize] as char).collect();
                return Ok(Value::str(&out));
            }
            let chars: Vec<char> = s.chars().collect();
            let (start, _, step, count) = slice_indices(vm, slice, chars.len())?;
            let out: String = (0..count).map(|k| chars[(start + k as i64 * step) as usize]).collect();
            Ok(Value::str(&out))
        }
        Value::Bytes(b) => {
            let (start, _, step, count) = slice_indices(vm, slice, b.len())?;
            let out: Vec<u8> = (0..count).map(|k| b[(start + k as i64 * step) as usize]).collect();
            Ok(Value::Bytes(Rc::from(out)))
        }
        Value::Range(r) => {
            let len = r.len().max(0) as usize;
            let (start, _, step, count) = slice_indices(vm, slice, len)?;
            let first = r.get(start);
            let new_step = r.step * step;
            Ok(Value::Range(Rc::new(Range { start: first, stop: first + new_step * count as i64, step: new_step })))
        }
        _ => vm.type_error("objet non indexable"),
    }
}

pub fn set_list_item(vm: &mut Vm, object: &Value, index: &Value, value: Value) -> PyResult<()> {
    let Value::List(l) = object else {
        return Ok(());
    };
    if let Value::Slice(slice) = index {
        let items = vm.collect(&value)?;
        let len = l.borrow().len();
        let (start, stop, step, count) = slice_indices(vm, slice, len)?;
        if step == 1 {
            let stop = stop.max(start) as usize;
            l.borrow_mut().splice(start as usize..stop, items);
            return Ok(());
        }
        if items.len() != count {
            let message = alloc::format!("attempt to assign sequence of size {} to extended slice of size {}", items.len(), count);
            return vm.value_error(&message);
        }
        let mut list = l.borrow_mut();
        for (k, item) in items.into_iter().enumerate() {
            list[(start + k as i64 * step) as usize] = item;
        }
        return Ok(());
    }
    let len = l.borrow().len();
    let i = index_of(vm, index, len, "list assignment")?;
    l.borrow_mut()[i] = value;
    Ok(())
}

pub fn del_list_item(vm: &mut Vm, object: &Value, index: &Value) -> PyResult<()> {
    let Value::List(l) = object else {
        return Ok(());
    };
    let len = l.borrow().len();
    if let Value::Slice(slice) = index {
        let (start, _, step, count) = slice_indices(vm, slice, len)?;
        let mut doomed: Vec<usize> = (0..count).map(|k| (start + k as i64 * step) as usize).collect();
        doomed.sort_unstable();
        let mut list = l.borrow_mut();
        for i in doomed.into_iter().rev() {
            list.remove(i);
        }
        return Ok(());
    }
    let i = index_of(vm, index, len, "list assignment")?;
    l.borrow_mut().remove(i);
    Ok(())
}

// ------------------------------------------------------------
//  Sorting
// ------------------------------------------------------------

/// A stable merge sort whose comparison may raise.
pub fn sort_values(vm: &mut Vm, values: Vec<Value>, key: &Value, reverse: bool) -> PyResult<Vec<Value>> {
    let mut items: Vec<(Value, Value)> = Vec::with_capacity(values.len());
    for v in values {
        let k = if key.is_none() { v.clone() } else { vm.call(key, core::slice::from_ref(&v), &[])? };
        items.push((k, v));
    }
    if reverse {
        items.reverse();
    }
    let mut buffer = items.clone();
    let n = items.len();
    let mut width = 1;
    while width < n {
        let mut i = 0;
        while i < n {
            let mid = (i + width).min(n);
            let end = (i + 2 * width).min(n);
            let (mut a, mut b, mut k) = (i, mid, i);
            while a < mid && b < end {
                // Take from the right only when strictly smaller: stable.
                let less = vm.compare(CmpOp::Lt, &items[b].0, &items[a].0)?;
                if vm.truthy(&less)? {
                    buffer[k] = items[b].clone();
                    b += 1;
                } else {
                    buffer[k] = items[a].clone();
                    a += 1;
                }
                k += 1;
            }
            while a < mid {
                buffer[k] = items[a].clone();
                a += 1;
                k += 1;
            }
            while b < end {
                buffer[k] = items[b].clone();
                b += 1;
                k += 1;
            }
            i += 2 * width;
        }
        core::mem::swap(&mut items, &mut buffer);
        width *= 2;
    }
    if reverse {
        items.reverse();
    }
    Ok(items.into_iter().map(|(_, v)| v).collect())
}

// ------------------------------------------------------------
//  list
// ------------------------------------------------------------

fn this_list(vm: &mut Vm, a: &[Value]) -> PyResult<Rc<RefCell<Vec<Value>>>> {
    match a.first().map(|v| vm.unwrap_builtin(v)) {
        Some(Value::List(l)) => Ok(l),
        _ => vm.type_error("descriptor requires a 'list' object"),
    }
}

fn append(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let l = this_list(vm, a)?;
    let Some(v) = a.get(1) else {
        return vm.type_error("list.append() takes exactly one argument (0 given)");
    };
    l.borrow_mut().push(v.clone());
    Ok(Value::None)
}

fn extend(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let l = this_list(vm, a)?;
    let Some(v) = a.get(1) else {
        return vm.type_error("list.extend() takes exactly one argument (0 given)");
    };
    let items = vm.collect(v)?;
    l.borrow_mut().extend(items);
    Ok(Value::None)
}

fn insert(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let l = this_list(vm, a)?;
    let (Some(i), Some(v)) = (a.get(1), a.get(2)) else {
        return vm.type_error("insert expected 2 arguments");
    };
    let i = args::int(vm, i)?;
    let len = l.borrow().len() as i64;
    let at = if i < 0 { (i + len).max(0) } else { i.min(len) };
    l.borrow_mut().insert(at as usize, v.clone());
    Ok(Value::None)
}

fn remove(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let l = this_list(vm, a)?;
    let Some(target) = a.get(1) else {
        return vm.type_error("list.remove() takes exactly one argument (0 given)");
    };
    let items = l.borrow().clone();
    for (i, v) in items.iter().enumerate() {
        if vm.eq(v, target)? {
            l.borrow_mut().remove(i);
            return Ok(Value::None);
        }
    }
    vm.value_error("list.remove(x): x not in list")
}

fn pop(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let l = this_list(vm, a)?;
    let len = l.borrow().len();
    if len == 0 {
        return vm.index_error("pop from empty list");
    }
    let i = match a.get(1) {
        Some(i) => index_of(vm, i, len, "pop")?,
        None => len - 1,
    };
    Ok(l.borrow_mut().remove(i))
}

fn clear(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let l = this_list(vm, a)?;
    l.borrow_mut().clear();
    Ok(Value::None)
}

fn copy(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let l = this_list(vm, a)?;
    let items = l.borrow().clone();
    Ok(Value::list(items))
}

fn reverse(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let l = this_list(vm, a)?;
    l.borrow_mut().reverse();
    Ok(Value::None)
}

fn sort(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let l = this_list(vm, a)?;
    if a.len() > 1 {
        return vm.type_error("sort() takes no positional arguments");
    }
    let key = args::keyword(kw, "key").cloned().unwrap_or(Value::None);
    let reverse = match args::keyword(kw, "reverse") {
        Some(r) => vm.truthy(r)?,
        None => false,
    };
    // The list looks empty while it is sorted, as in CPython.
    let items = core::mem::take(&mut *l.borrow_mut());
    let sorted = sort_values(vm, items.clone(), &key, reverse);
    match sorted {
        Ok(sorted) => {
            *l.borrow_mut() = sorted;
            Ok(Value::None)
        }
        Err(e) => {
            *l.borrow_mut() = items;
            Err(e)
        }
    }
}

/// `index` and `count` of lists and tuples.
fn seq_items(vm: &mut Vm, a: &[Value]) -> PyResult<Vec<Value>> {
    match a.first().map(|v| vm.unwrap_builtin(v)) {
        Some(Value::List(l)) => Ok(l.borrow().clone()),
        Some(Value::Tuple(t)) => Ok(t.to_vec()),
        Some(Value::Range(r)) => Ok((0..r.len().max(0)).map(|i| Value::int(r.get(i))).collect()),
        _ => vm.type_error("sequence expected"),
    }
}

fn index(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let items = seq_items(vm, a)?;
    let Some(target) = a.get(1) else {
        return vm.type_error("index expected at least 1 argument, got 0");
    };
    let len = items.len() as i64;
    let norm = |v: i64| if v < 0 { (v + len).max(0) } else { v.min(len) } as usize;
    let start = norm(args::opt_int(vm, a.get(2), 0)?);
    let stop = norm(args::opt_int(vm, a.get(3), len)?);
    for i in start..stop.max(start) {
        if vm.eq(&items[i], target)? {
            return Ok(Value::int(i as i64));
        }
    }
    let kind = vm.type_name(&a[0]);
    let message = if &*kind == "list" { String::from("list.index(x): x not in list") } else { alloc::format!("{}.index(x): x not in {}", kind, kind) };
    vm.value_error(&message)
}

fn count(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let items = seq_items(vm, a)?;
    let Some(target) = a.get(1) else {
        return vm.type_error("count() takes exactly one argument (0 given)");
    };
    let mut n = 0;
    for v in &items {
        if vm.eq(v, target)? {
            n += 1;
        }
    }
    Ok(Value::int(n))
}

// ------------------------------------------------------------
//  bytes
// ------------------------------------------------------------

fn this_bytes(vm: &mut Vm, a: &[Value]) -> PyResult<Rc<[u8]>> {
    match a.first() {
        Some(Value::Bytes(b)) => Ok(b.clone()),
        _ => vm.type_error("descriptor requires a 'bytes' object"),
    }
}

fn decode(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let b = this_bytes(vm, a)?;
    match core::str::from_utf8(&b) {
        Ok(s) => Ok(Value::str(s)),
        Err(e) => {
            let class = vm.types.unicode_error.clone();
            let message = alloc::format!("'utf-8' codec can't decode byte 0x{:02x} in position {}: invalid start byte", b[e.valid_up_to()], e.valid_up_to());
            Err(vm.error(&class, &message))
        }
    }
}

fn hex(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let b = this_bytes(vm, a)?;
    let text: String = b.iter().map(|x| alloc::format!("{:02x}", x)).collect();
    Ok(Value::str(&text))
}

pub fn install(vm: &mut Vm) {
    let list = vm.types.list.clone();
    let entries: &[(&'static str, NativeFn)] = &[
        ("append", append), ("extend", extend), ("insert", insert), ("remove", remove), ("pop", pop), ("clear", clear),
        ("copy", copy), ("reverse", reverse), ("sort", sort), ("index", index), ("count", count),
    ];
    for (name, f) in entries {
        method(&list, name, *f);
    }
    let tuple = vm.types.tuple.clone();
    method(&tuple, "index", index);
    method(&tuple, "count", count);
    let range = vm.types.range.clone();
    method(&range, "index", index);
    method(&range, "count", count);
    let bytes = vm.types.bytes.clone();
    method(&bytes, "decode", decode);
    method(&bytes, "hex", hex);
}
