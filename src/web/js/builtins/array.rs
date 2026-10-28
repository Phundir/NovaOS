//! `Array`, and the iterator objects every built-in collection hands out.
//!
//! The methods work on real arrays directly, and on array-likes
//! (`arguments`, node lists, `{length: 2, 0: a, 1: b}`) through their
//! `length` and indices, as `Array.prototype.slice.call(arguments)` needs.

use alloc::string::String;
use alloc::vec::Vec;
use core::cmp::Ordering;

use super::arg;
use crate::web::js::interp::{Ctrl, Ctx, R};
use crate::web::js::num::Num;
use crate::web::js::ops;
use crate::web::js::value::*;

/// The elements of `this`: a copy.
fn elements(ctx: &mut Ctx, this: &Value) -> R<Vec<Value>> {
    if let Value::Obj(object) = this {
        if let Kind::Array(items) = &object.borrow().kind {
            return Ok(items.clone());
        }
    }
    if let Value::Str(s) = this {
        let mut out = Vec::new();
        for index in 0..s.units() {
            out.push(ops::string_index(s, index).unwrap_or(Value::Undefined));
        }
        return Ok(out);
    }
    if this.is_nullish() {
        return ctx.throw_type("Array.prototype method called on null or undefined");
    }
    ops::list_from_array_like(ctx, this)
}

/// Element `index` of `this`, read live.
fn element(ctx: &mut Ctx, this: &Value, snapshot: &[Value], index: usize) -> R<Value> {
    if let Value::Obj(object) = this {
        if let Kind::Array(items) = &object.borrow().kind {
            return Ok(items.get(index).cloned().unwrap_or(Value::Undefined));
        }
    }
    let _ = ctx;
    Ok(snapshot.get(index).cloned().unwrap_or(Value::Undefined))
}

/// Writes a whole list back into `this`.
fn store(ctx: &mut Ctx, this: &Value, items: Vec<Value>) -> R<()> {
    if let Value::Obj(object) = this {
        let frozen = object.borrow().frozen;
        if frozen {
            return ctx.throw_type("Cannot modify a frozen array");
        }
        if let Kind::Array(current) = &mut object.borrow_mut().kind {
            *current = items;
            return Ok(());
        }
    }
    let old_length = ops::get(ctx, this, "length")?;
    let old_length = ops::to_integer(ctx, &old_length)?.max(0) as usize;
    let new_length = items.len();
    for (index, value) in items.into_iter().enumerate() {
        ops::set(ctx, this, ops::number_string(Num::from_i64(index as i64)), value)?;
    }
    for index in new_length..old_length {
        ops::delete(ctx, this, &ops::number_string(Num::from_i64(index as i64)))?;
    }
    ops::set(ctx, this, JsStr::from("length"), Value::num(new_length as i64))
}

fn callback(ctx: &mut Ctx, args: &[Value], name: &str) -> R<Value> {
    let function = arg(args, 0);
    if !function.as_obj().is_some_and(|f| f.is_callable()) {
        let message = alloc::format!("{} is not a function", ops::to_display(&function));
        let _ = name;
        return ctx.throw_type(&message);
    }
    Ok(function)
}

// ------------------------------------------------------------
//  Constructor and statics
// ------------------------------------------------------------

fn array_constructor(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let proto = ctx.proto_for_new(ctx.realm.protos.array.clone())?;
    let items = if args.len() == 1 {
        if let Value::Num(n) = &args[0] {
            match n.to_index() {
                Some(length) if length <= 10_000_000 => alloc::vec![Value::Undefined; length],
                _ => return ctx.throw_range("Invalid array length"),
            }
        } else {
            args.to_vec()
        }
    } else {
        args.to_vec()
    };
    Ok(Value::Obj(ctx.alloc(Kind::Array(items), Some(proto), "Array")))
}

fn is_array(_: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    Ok(Value::Bool(arg(args, 0).as_obj().is_some_and(|o| o.is_array())))
}

fn from(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let source = arg(args, 0);
    let map = arg(args, 1);
    let iterator_key = ctx.realm.symbols.iterator.clone();
    let iterable = match &source {
        Value::Str(_) => true,
        Value::Obj(_) => !ops::get_key(ctx, &source, &iterator_key)?.is_nullish(),
        _ => false,
    };
    let items = if iterable { ops::collect(ctx, &source)? } else { ops::list_from_array_like(ctx, &source)? };
    let items = if map.as_obj().is_some_and(|m| m.is_callable()) {
        let mut out = Vec::with_capacity(items.len());
        for (index, item) in items.into_iter().enumerate() {
            ctx.tick()?;
            out.push(ctx.call(&map, arg(args, 2), &[item, Value::num(index as i64)])?);
        }
        out
    } else {
        items
    };
    Ok(Value::Obj(ctx.new_array(items)))
}

fn of(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    Ok(Value::Obj(ctx.new_array(args.to_vec())))
}

// ------------------------------------------------------------
//  Mutators
// ------------------------------------------------------------

fn push(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    if let Value::Obj(object) = this {
        let mut borrowed = object.borrow_mut();
        if !borrowed.frozen {
            if let Kind::Array(items) = &mut borrowed.kind {
                if items.len() + args.len() > 50_000_000 {
                    drop(borrowed);
                    return ctx.throw_range("Invalid array length");
                }
                items.extend_from_slice(args);
                return Ok(Value::num(items.len() as i64));
            }
        }
    }
    let mut items = elements(ctx, this)?;
    items.extend_from_slice(args);
    let length = items.len();
    store(ctx, this, items)?;
    Ok(Value::num(length as i64))
}

fn pop(ctx: &mut Ctx, this: &Value, _: &[Value], _: &Value) -> R<Value> {
    if let Value::Obj(object) = this {
        if let Kind::Array(items) = &mut object.borrow_mut().kind {
            return Ok(items.pop().unwrap_or(Value::Undefined));
        }
    }
    let mut items = elements(ctx, this)?;
    let value = items.pop().unwrap_or(Value::Undefined);
    store(ctx, this, items)?;
    Ok(value)
}

fn shift(ctx: &mut Ctx, this: &Value, _: &[Value], _: &Value) -> R<Value> {
    if let Value::Obj(object) = this {
        if let Kind::Array(items) = &mut object.borrow_mut().kind {
            if items.is_empty() {
                return Ok(Value::Undefined);
            }
            return Ok(items.remove(0));
        }
    }
    let mut items = elements(ctx, this)?;
    if items.is_empty() {
        return Ok(Value::Undefined);
    }
    let value = items.remove(0);
    store(ctx, this, items)?;
    Ok(value)
}

fn unshift(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let mut items = args.to_vec();
    items.extend(elements(ctx, this)?);
    let length = items.len();
    store(ctx, this, items)?;
    Ok(Value::num(length as i64))
}

fn splice(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let mut items = elements(ctx, this)?;
    let length = items.len();
    let start = ops::relative_index(ctx, args.first(), length, 0)?;
    let count = match args.len() {
        0 => 0,
        1 => length - start,
        _ => (ops::to_integer(ctx, &args[1])?.max(0) as usize).min(length - start),
    };
    let inserted: Vec<Value> = args.iter().skip(2).cloned().collect();
    let removed: Vec<Value> = items.splice(start..start + count, inserted).collect();
    store(ctx, this, items)?;
    Ok(Value::Obj(ctx.new_array(removed)))
}

fn reverse(ctx: &mut Ctx, this: &Value, _: &[Value], _: &Value) -> R<Value> {
    let mut items = elements(ctx, this)?;
    items.reverse();
    store(ctx, this, items)?;
    Ok(this.clone())
}

fn fill(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let mut items = elements(ctx, this)?;
    let length = items.len();
    let start = ops::relative_index(ctx, args.get(1), length, 0)?;
    let end = ops::relative_index(ctx, args.get(2), length, length)?;
    for item in items.iter_mut().take(end).skip(start) {
        *item = arg(args, 0);
    }
    store(ctx, this, items)?;
    Ok(this.clone())
}

fn copy_within(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let mut items = elements(ctx, this)?;
    let length = items.len();
    let target = ops::relative_index(ctx, args.first(), length, 0)?;
    let start = ops::relative_index(ctx, args.get(1), length, 0)?;
    let end = ops::relative_index(ctx, args.get(2), length, length)?;
    let chunk: Vec<Value> = items[start..end.max(start)].to_vec();
    for (offset, value) in chunk.into_iter().enumerate() {
        if target + offset < length {
            items[target + offset] = value;
        }
    }
    store(ctx, this, items)?;
    Ok(this.clone())
}

/// Stable merge sort with a comparator that may throw.
fn sort_values(ctx: &mut Ctx, items: &mut Vec<Value>, compare: &Value) -> R<()> {
    // undefined sorts last, without calling the comparator.
    let mut defined: Vec<Value> = Vec::with_capacity(items.len());
    let mut undefined = 0;
    for item in items.drain(..) {
        if item.is_undefined() {
            undefined += 1;
        } else {
            defined.push(item);
        }
    }
    let keys: Option<Vec<JsStr>> = if compare.is_undefined() {
        let mut keys = Vec::with_capacity(defined.len());
        for item in &defined {
            keys.push(ops::to_string(ctx, item)?);
        }
        Some(keys)
    } else {
        None
    };
    let mut order: Vec<usize> = (0..defined.len()).collect();
    let mut buffer = order.clone();
    let mut width = 1;
    let n = order.len();
    while width < n {
        let mut start = 0;
        while start < n {
            let middle = (start + width).min(n);
            let end = (start + 2 * width).min(n);
            let (mut i, mut j, mut k) = (start, middle, start);
            while i < middle && j < end {
                ctx.tick()?;
                let greater = match &keys {
                    Some(keys) => ops::compare_strings(&keys[order[i]], &keys[order[j]]) == Ordering::Greater,
                    None => {
                        let result = ctx.call(compare, Value::Undefined, &[defined[order[i]].clone(), defined[order[j]].clone()])?;
                        let result = ops::to_number(ctx, &result)?;
                        result.partial_cmp(Num::ZERO) == Some(Ordering::Greater)
                    }
                };
                if greater {
                    buffer[k] = order[j];
                    j += 1;
                } else {
                    buffer[k] = order[i];
                    i += 1;
                }
                k += 1;
            }
            while i < middle {
                buffer[k] = order[i];
                i += 1;
                k += 1;
            }
            while j < end {
                buffer[k] = order[j];
                j += 1;
                k += 1;
            }
            start = end;
        }
        core::mem::swap(&mut order, &mut buffer);
        width *= 2;
    }
    items.extend(order.into_iter().map(|i| defined[i].clone()));
    items.extend(core::iter::repeat_n(Value::Undefined, undefined));
    Ok(())
}

fn sort(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let compare = arg(args, 0);
    if !compare.is_undefined() && !compare.as_obj().is_some_and(|c| c.is_callable()) {
        return ctx.throw_type("The comparison function must be either a function or undefined");
    }
    let mut items = elements(ctx, this)?;
    sort_values(ctx, &mut items, &compare)?;
    store(ctx, this, items)?;
    Ok(this.clone())
}

fn to_sorted(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let mut items = elements(ctx, this)?;
    sort_values(ctx, &mut items, &arg(args, 0))?;
    Ok(Value::Obj(ctx.new_array(items)))
}

fn to_reversed(ctx: &mut Ctx, this: &Value, _: &[Value], _: &Value) -> R<Value> {
    let mut items = elements(ctx, this)?;
    items.reverse();
    Ok(Value::Obj(ctx.new_array(items)))
}

fn with(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let mut items = elements(ctx, this)?;
    let length = items.len() as i64;
    let index = ops::to_integer(ctx, &arg(args, 0))?;
    let index = if index < 0 { length + index } else { index };
    if index < 0 || index >= length {
        return ctx.throw_range("Invalid index");
    }
    items[index as usize] = arg(args, 1);
    Ok(Value::Obj(ctx.new_array(items)))
}

fn to_spliced(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let copy = Value::Obj(ctx.new_array(elements(ctx, this)?));
    splice(ctx, &copy, args, &Value::Undefined)?;
    Ok(copy)
}

// ------------------------------------------------------------
//  Accessors
// ------------------------------------------------------------

fn concat(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let mut items = elements(ctx, this)?;
    for value in args {
        match value {
            Value::Obj(object) if object.is_array() => {
                if let Kind::Array(more) = &object.borrow().kind {
                    items.extend_from_slice(more);
                }
            }
            other => items.push(other.clone()),
        }
    }
    Ok(Value::Obj(ctx.new_array(items)))
}

fn slice(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let items = elements(ctx, this)?;
    let length = items.len();
    let start = ops::relative_index(ctx, args.first(), length, 0)?;
    let end = ops::relative_index(ctx, args.get(1), length, length)?;
    let part = if start < end { items[start..end].to_vec() } else { Vec::new() };
    Ok(Value::Obj(ctx.new_array(part)))
}

pub fn join_values(ctx: &mut Ctx, items: &[Value], separator: &str) -> R<JsStr> {
    let mut out = String::new();
    for (index, item) in items.iter().enumerate() {
        if index > 0 {
            out.push_str(separator);
        }
        if !item.is_nullish() {
            let text = ops::to_string(ctx, item)?;
            out.push_str(&text);
            if out.len() > ops::MAX_STRING {
                return ctx.throw_range("Invalid string length");
            }
        }
    }
    Ok(JsStr::from(out))
}

fn join(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let items = elements(ctx, this)?;
    let separator = match arg(args, 0) {
        Value::Undefined => JsStr::from(","),
        other => ops::to_string(ctx, &other)?,
    };
    ctx.depth += 1;
    if ctx.depth > 200 {
        ctx.depth -= 1;
        return Ok(Value::str(""));
    }
    let result = join_values(ctx, &items, &separator);
    ctx.depth -= 1;
    Ok(Value::Str(result?))
}

fn to_string(ctx: &mut Ctx, this: &Value, _: &[Value], _: &Value) -> R<Value> {
    join(ctx, this, &[], &Value::Undefined)
}

fn index_of(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let items = elements(ctx, this)?;
    let start = ops::relative_index(ctx, args.get(1), items.len(), 0)?;
    let target = arg(args, 0);
    for (index, item) in items.iter().enumerate().skip(start) {
        if item.strict_eq(&target) {
            return Ok(Value::num(index as i64));
        }
    }
    Ok(Value::num(-1))
}

fn last_index_of(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let items = elements(ctx, this)?;
    if items.is_empty() {
        return Ok(Value::num(-1));
    }
    let from = if args.len() > 1 {
        let n = ops::to_integer(ctx, &args[1])?;
        if n < 0 {
            items.len() as i64 + n
        } else {
            n.min(items.len() as i64 - 1)
        }
    } else {
        items.len() as i64 - 1
    };
    let target = arg(args, 0);
    let mut index = from;
    while index >= 0 {
        if items[index as usize].strict_eq(&target) {
            return Ok(Value::num(index));
        }
        index -= 1;
    }
    Ok(Value::num(-1))
}

fn includes(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let items = elements(ctx, this)?;
    let start = ops::relative_index(ctx, args.get(1), items.len(), 0)?;
    let target = arg(args, 0);
    Ok(Value::Bool(items.iter().skip(start).any(|item| item.same_value_zero(&target))))
}

fn at(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let items = elements(ctx, this)?;
    let index = ops::to_integer(ctx, &arg(args, 0))?;
    let index = if index < 0 { items.len() as i64 + index } else { index };
    Ok(if index < 0 { Value::Undefined } else { items.get(index as usize).cloned().unwrap_or(Value::Undefined) })
}

fn flatten(ctx: &mut Ctx, items: &[Value], depth: i64, out: &mut Vec<Value>) -> R<()> {
    for item in items {
        ctx.tick()?;
        match item {
            Value::Obj(object) if depth > 0 && object.is_array() => {
                let inner = match &object.borrow().kind {
                    Kind::Array(inner) => inner.clone(),
                    _ => Vec::new(),
                };
                flatten(ctx, &inner, depth - 1, out)?;
            }
            other => out.push(other.clone()),
        }
    }
    Ok(())
}

fn flat(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let items = elements(ctx, this)?;
    let depth = match arg(args, 0) {
        Value::Undefined => 1,
        other => ops::to_integer(ctx, &other)?.min(100),
    };
    let mut out = Vec::new();
    flatten(ctx, &items, depth, &mut out)?;
    Ok(Value::Obj(ctx.new_array(out)))
}

// ------------------------------------------------------------
//  Iteration with callbacks
// ------------------------------------------------------------

fn for_each(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let function = callback(ctx, args, "forEach")?;
    let snapshot = elements(ctx, this)?;
    for index in 0..snapshot.len() {
        ctx.tick()?;
        let item = element(ctx, this, &snapshot, index)?;
        ctx.call(&function, arg(args, 1), &[item, Value::num(index as i64), this.clone()])?;
    }
    Ok(Value::Undefined)
}

fn map(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let function = callback(ctx, args, "map")?;
    let snapshot = elements(ctx, this)?;
    let mut out = Vec::with_capacity(snapshot.len());
    for index in 0..snapshot.len() {
        ctx.tick()?;
        let item = element(ctx, this, &snapshot, index)?;
        out.push(ctx.call(&function, arg(args, 1), &[item, Value::num(index as i64), this.clone()])?);
    }
    Ok(Value::Obj(ctx.new_array(out)))
}

fn filter(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let function = callback(ctx, args, "filter")?;
    let snapshot = elements(ctx, this)?;
    let mut out = Vec::new();
    for index in 0..snapshot.len() {
        ctx.tick()?;
        let item = element(ctx, this, &snapshot, index)?;
        if ctx.call(&function, arg(args, 1), &[item.clone(), Value::num(index as i64), this.clone()])?.truthy() {
            out.push(item);
        }
    }
    Ok(Value::Obj(ctx.new_array(out)))
}

/// find, findIndex, findLast, findLastIndex, some, every.
fn search(ctx: &mut Ctx, this: &Value, args: &[Value], backwards: bool) -> R<Option<(usize, Value)>> {
    let function = callback(ctx, args, "find")?;
    let snapshot = elements(ctx, this)?;
    let order: Vec<usize> = if backwards { (0..snapshot.len()).rev().collect() } else { (0..snapshot.len()).collect() };
    for index in order {
        ctx.tick()?;
        let item = element(ctx, this, &snapshot, index)?;
        if ctx.call(&function, arg(args, 1), &[item.clone(), Value::num(index as i64), this.clone()])?.truthy() {
            return Ok(Some((index, item)));
        }
    }
    Ok(None)
}

fn find(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    Ok(search(ctx, this, args, false)?.map_or(Value::Undefined, |(_, v)| v))
}

fn find_index(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    Ok(Value::num(search(ctx, this, args, false)?.map_or(-1, |(i, _)| i as i64)))
}

fn find_last(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    Ok(search(ctx, this, args, true)?.map_or(Value::Undefined, |(_, v)| v))
}

fn find_last_index(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    Ok(Value::num(search(ctx, this, args, true)?.map_or(-1, |(i, _)| i as i64)))
}

fn some(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    Ok(Value::Bool(search(ctx, this, args, false)?.is_some()))
}

fn every(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let function = callback(ctx, args, "every")?;
    let snapshot = elements(ctx, this)?;
    for index in 0..snapshot.len() {
        ctx.tick()?;
        let item = element(ctx, this, &snapshot, index)?;
        if !ctx.call(&function, arg(args, 1), &[item, Value::num(index as i64), this.clone()])?.truthy() {
            return Ok(Value::Bool(false));
        }
    }
    Ok(Value::Bool(true))
}

fn reduce_with(ctx: &mut Ctx, this: &Value, args: &[Value], backwards: bool) -> R<Value> {
    let function = callback(ctx, args, "reduce")?;
    let snapshot = elements(ctx, this)?;
    let mut order: Vec<usize> = (0..snapshot.len()).collect();
    if backwards {
        order.reverse();
    }
    let mut order = order.into_iter();
    let mut accumulator = if args.len() > 1 {
        args[1].clone()
    } else {
        match order.next() {
            Some(index) => snapshot[index].clone(),
            None => return ctx.throw_type("Reduce of empty array with no initial value"),
        }
    };
    for index in order {
        ctx.tick()?;
        let item = element(ctx, this, &snapshot, index)?;
        accumulator = ctx.call(&function, Value::Undefined, &[accumulator, item, Value::num(index as i64), this.clone()])?;
    }
    Ok(accumulator)
}

fn reduce(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    reduce_with(ctx, this, args, false)
}

fn reduce_right(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    reduce_with(ctx, this, args, true)
}

fn flat_map(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let mapped = map(ctx, this, args, &Value::Undefined)?;
    let items = elements(ctx, &mapped)?;
    let mut out = Vec::new();
    flatten(ctx, &items, 1, &mut out)?;
    Ok(Value::Obj(ctx.new_array(out)))
}

// ------------------------------------------------------------
//  Iterators
// ------------------------------------------------------------

fn make_iterator(ctx: &mut Ctx, this: &Value, mode: u8) -> R<Value> {
    let object = ops::to_object(ctx, this)?;
    let proto = ctx.realm.protos.iterator.clone();
    let state = if let Kind::String(s) = &object.borrow().kind {
        IterState::Str(s.clone(), 0)
    } else {
        IterState::Array(object.clone(), 0, mode)
    };
    Ok(Value::Obj(ctx.alloc(Kind::Iterator(state), Some(proto), "Array Iterator")))
}

fn keys(ctx: &mut Ctx, this: &Value, _: &[Value], _: &Value) -> R<Value> {
    make_iterator(ctx, this, 0)
}

pub fn values(ctx: &mut Ctx, this: &Value, _: &[Value], _: &Value) -> R<Value> {
    if let Value::Str(s) = this {
        let proto = ctx.realm.protos.iterator.clone();
        return Ok(Value::Obj(ctx.alloc(Kind::Iterator(IterState::Str(s.clone(), 0)), Some(proto), "String Iterator")));
    }
    make_iterator(ctx, this, 1)
}

fn entries(ctx: &mut Ctx, this: &Value, _: &[Value], _: &Value) -> R<Value> {
    make_iterator(ctx, this, 2)
}

/// `next()` of every built-in iterator.
pub fn iterator_next(ctx: &mut Ctx, this: &Value, _: &[Value], _: &Value) -> R<Value> {
    let Value::Obj(iterator) = this else {
        return ctx.throw_type("next method called on incompatible receiver");
    };
    enum Step {
        Value(Value),
        ArrayAt(Obj, usize, u8),
        MapAt(Obj, usize, u8),
        Done,
    }
    let step = {
        let mut borrowed = iterator.borrow_mut();
        match &mut borrowed.kind {
            Kind::Iterator(IterState::List(items, position)) => {
                if *position < items.len() {
                    *position += 1;
                    Step::Value(items[*position - 1].clone())
                } else {
                    Step::Done
                }
            }
            Kind::Iterator(IterState::Str(s, position)) => match s[*position..].chars().next() {
                Some(c) => {
                    *position += c.len_utf8();
                    let mut text = String::new();
                    text.push(c);
                    Step::Value(Value::Str(JsStr::from(text)))
                }
                None => Step::Done,
            },
            Kind::Iterator(IterState::Array(array, position, mode)) => {
                let at = *position;
                *position += 1;
                Step::ArrayAt(array.clone(), at, *mode)
            }
            Kind::Iterator(IterState::Map(map, position, mode)) => {
                let at = *position;
                Step::MapAt(map.clone(), at, *mode)
            }
            _ => return ctx.throw_type("next method called on incompatible receiver"),
        }
    };
    let value = match step {
        Step::Value(value) => Some(value),
        Step::Done => None,
        Step::ArrayAt(array, index, mode) => {
            let length = match &array.borrow().kind {
                Kind::Array(items) => Some(items.len()),
                _ => None,
            };
            let length = match length {
                Some(length) => length,
                None => {
                    let length = ops::get(ctx, &Value::Obj(array.clone()), "length")?;
                    ops::to_integer(ctx, &length)?.max(0) as usize
                }
            };
            if index >= length {
                None
            } else {
                let item = ops::get_value_key(ctx, &Value::Obj(array), &Value::num(index as i64))?;
                Some(match mode {
                    0 => Value::num(index as i64),
                    1 => item,
                    _ => Value::Obj(ctx.new_array(alloc::vec![Value::num(index as i64), item])),
                })
            }
        }
        Step::MapAt(map, mut index, mode) => {
            let found = {
                let borrowed = map.borrow();
                let entries = match &borrowed.kind {
                    Kind::Map(data) | Kind::Set(data) => &data.entries,
                    _ => return ctx.throw_type("incompatible map iterator"),
                };
                let mut found = None;
                while index < entries.len() {
                    if let Some((key, value)) = &entries[index] {
                        found = Some((key.clone(), value.clone()));
                        index += 1;
                        break;
                    }
                    index += 1;
                }
                found
            };
            if let Kind::Iterator(IterState::Map(_, position, _)) = &mut iterator.borrow_mut().kind {
                *position = index;
            }
            match found {
                None => None,
                Some((key, value)) => Some(match mode {
                    0 => key,
                    1 => value,
                    _ => Value::Obj(ctx.new_array(alloc::vec![key, value])),
                }),
            }
        }
    };
    match value {
        Some(value) => Ok(ops::iter_result(ctx, value, false)),
        None => {
            // Exhausted for good.
            iterator.borrow_mut().kind = Kind::Iterator(IterState::List(Vec::new(), 0));
            Ok(ops::iter_result(ctx, Value::Undefined, true))
        }
    }
}

fn iterator_return(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    if let Value::Obj(iterator) = this {
        if matches!(iterator.borrow().kind, Kind::Iterator(_)) {
            iterator.borrow_mut().kind = Kind::Iterator(IterState::List(Vec::new(), 0));
        }
    }
    Ok(ops::iter_result(ctx, arg(args, 0), true))
}

fn iterator_throw(_: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    if let Value::Obj(iterator) = this {
        iterator.borrow_mut().kind = Kind::Iterator(IterState::List(Vec::new(), 0));
    }
    Err(Ctrl::Throw(arg(args, 0)))
}

fn return_this(_: &mut Ctx, this: &Value, _: &[Value], _: &Value) -> R<Value> {
    Ok(this.clone())
}

/// `iterator.toArray()` and friends of the iterator helpers.
fn iterator_to_array(ctx: &mut Ctx, this: &Value, _: &[Value], _: &Value) -> R<Value> {
    let items = ops::collect(ctx, this)?;
    Ok(Value::Obj(ctx.new_array(items)))
}

pub fn install(ctx: &mut Ctx) {
    let proto = ctx.realm.protos.array.clone();
    let constructor = super::constructor(ctx, "Array", 1, array_constructor, &proto);
    ctx.method(&constructor, "isArray", 1, is_array);
    ctx.method(&constructor, "from", 1, from);
    ctx.method(&constructor, "of", 0, of);
    let methods: &[(&str, usize, NativeFn)] = &[
        ("push", 1, push),
        ("pop", 0, pop),
        ("shift", 0, shift),
        ("unshift", 1, unshift),
        ("splice", 2, splice),
        ("reverse", 0, reverse),
        ("fill", 1, fill),
        ("copyWithin", 2, copy_within),
        ("sort", 1, sort),
        ("toSorted", 1, to_sorted),
        ("toReversed", 0, to_reversed),
        ("toSpliced", 2, to_spliced),
        ("with", 2, with),
        ("concat", 1, concat),
        ("slice", 2, slice),
        ("join", 1, join),
        ("toString", 0, to_string),
        ("toLocaleString", 0, to_string),
        ("indexOf", 1, index_of),
        ("lastIndexOf", 1, last_index_of),
        ("includes", 1, includes),
        ("at", 1, at),
        ("flat", 0, flat),
        ("flatMap", 1, flat_map),
        ("forEach", 1, for_each),
        ("map", 1, map),
        ("filter", 1, filter),
        ("find", 1, find),
        ("findIndex", 1, find_index),
        ("findLast", 1, find_last),
        ("findLastIndex", 1, find_last_index),
        ("some", 1, some),
        ("every", 1, every),
        ("reduce", 1, reduce),
        ("reduceRight", 1, reduce_right),
        ("keys", 0, keys),
        ("values", 0, values),
        ("entries", 0, entries),
    ];
    for (name, length, function) in methods {
        ctx.method(&proto, name, *length, *function);
    }
    let symbol = ctx.realm.symbols.iterator.clone();
    let values_function = ops::get(ctx, &Value::Obj(proto.clone()), "values").unwrap_or(Value::Undefined);
    proto.put_key(symbol.clone(), values_function, HIDDEN);

    let iterator = ctx.realm.protos.iterator.clone();
    ctx.method(&iterator, "next", 0, iterator_next);
    ctx.method(&iterator, "return", 1, iterator_return);
    ctx.method(&iterator, "throw", 1, iterator_throw);
    ctx.method(&iterator, "toArray", 0, iterator_to_array);
    let self_iterator = ctx.native("[Symbol.iterator]", 0, return_this);
    iterator.put_key(symbol, Value::Obj(self_iterator), HIDDEN);
}
