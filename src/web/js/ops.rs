//! The abstract operations of the specification: conversions, property
//! access, operators and iteration.
//!
//! Arrays and strings have "exotic" properties -- `length` and the
//! indices -- that live in their `Kind` rather than in their property
//! list; every access below looks there first.

use alloc::string::String;
use alloc::vec::Vec;
use core::cmp::Ordering;

use super::ast::BinOp;
use super::interp::{Ctrl, Ctx, R};
use super::num::Num;
use super::value::*;

/// Arrays grow densely up to this far past their end; an index further
/// out becomes an ordinary property (`a[1e9] = 1` must not allocate
/// sixteen gigabytes).
const DENSE_GAP: usize = 100_000;
/// Longest string a script may build.
pub const MAX_STRING: usize = 32 * 1024 * 1024;
/// Longest array a script may build, in elements.
pub const MAX_ARRAY: usize = 16 * 1024 * 1024;
/// Below this, an allocation is not worth asking the host about.
const RESERVE_FROM: usize = 256 * 1024;

/// Before a large allocation: the host must have room for it. The
/// kernel's allocator halts the machine when it fails; a script gets a
/// `RangeError` instead, which it can catch.
pub fn reserve(ctx: &mut Ctx, bytes: usize, what: &str) -> R<()> {
    if bytes < RESERVE_FROM || bytes < ctx.reserved_up_to {
        return Ok(());
    }
    if !ctx.host.can_allocate(bytes) {
        return ctx.throw_range(what);
    }
    ctx.reserved_up_to = bytes.saturating_mul(2);
    Ok(())
}

/// A string about to reach `length` bytes: within the bound, and room.
pub fn check_string(ctx: &mut Ctx, length: usize) -> R<()> {
    if length > MAX_STRING {
        return ctx.throw_range("Invalid string length");
    }
    reserve(ctx, length, "Invalid string length")
}

/// An array about to reach `length` elements.
pub fn check_array(ctx: &mut Ctx, length: usize) -> R<()> {
    if length > MAX_ARRAY {
        return ctx.throw_range("Invalid array length");
    }
    reserve(ctx, length.saturating_mul(core::mem::size_of::<Value>()), "Invalid array length")
}

/// Before `extra` elements are added to an array: past its capacity the
/// vector moves to a block twice its size, and that block must exist.
pub fn check_growth(ctx: &mut Ctx, object: &Obj, extra: usize) -> R<()> {
    let wanted = match &object.borrow().kind {
        Kind::Array(items) => {
            let length = items.len().saturating_add(extra);
            (length > items.capacity()).then(|| length.max(items.capacity().saturating_mul(2)))
        }
        _ => None,
    };
    match wanted {
        Some(length) => check_array(ctx, length),
        None => Ok(()),
    }
}

// ============================================================
//  Conversions
// ============================================================

/// Text for the console and error messages: no user code runs.
pub fn to_display(value: &Value) -> String {
    display_depth(value, 0)
}

fn display_depth(value: &Value, depth: usize) -> String {
    match value {
        Value::Str(s) => {
            if depth == 0 {
                String::from(s.as_str())
            } else {
                alloc::format!("\"{}\"", s.as_str())
            }
        }
        Value::Obj(object) => {
            let borrowed = match object.0.try_borrow() {
                Ok(b) => b,
                Err(_) => return String::from("[object]"),
            };
            match &borrowed.kind {
                Kind::Array(items) => {
                    if depth > 1 {
                        return alloc::format!("Array({})", items.len());
                    }
                    let parts: Vec<String> = items.iter().take(20).map(|v| display_depth(v, depth + 1)).collect();
                    let more = if items.len() > 20 { ", ..." } else { "" };
                    alloc::format!("[{}{}]", parts.join(", "), more)
                }
                Kind::Error => {
                    let message = match borrowed.props.get_str("message") {
                        Some(Slot::Data(v, _)) => display_depth(v, 1).trim_matches('"').into(),
                        _ => String::new(),
                    };
                    let mut name = String::from("Error");
                    let mut proto = borrowed.proto.clone();
                    while let Some(p) = proto {
                        if let Some(Slot::Data(Value::Str(n), _)) = p.borrow().props.get_str("name") {
                            name = String::from(n.as_str());
                            break;
                        }
                        proto = p.proto();
                    }
                    alloc::format!("{}: {}", name, message)
                }
                Kind::Function(_) | Kind::Native(..) | Kind::Bound(..) => {
                    let name = match borrowed.props.get_str("name") {
                        Some(Slot::Data(Value::Str(n), _)) => String::from(n.as_str()),
                        _ => String::new(),
                    };
                    alloc::format!("[Function: {}]", if name.is_empty() { "(anonyme)" } else { &name })
                }
                Kind::Node(_) => alloc::format!("[{}]", borrowed.class),
                Kind::Date(n) => super::builtins::date::iso_string(*n).unwrap_or_else(|| String::from("Invalid Date")),
                Kind::Promise(_) => String::from("Promise {}"),
                _ => {
                    if depth > 1 {
                        return String::from("{...}");
                    }
                    let parts: Vec<String> = borrowed
                        .props
                        .iter()
                        .filter(|(k, s)| !is_symbol_key(k) && s.flags() & ENUMERABLE != 0)
                        .take(12)
                        .map(|(k, s)| match s {
                            Slot::Data(v, _) => alloc::format!("{}: {}", k.as_str(), display_depth(v, depth + 1)),
                            Slot::Accessor(..) => alloc::format!("{}: [Getter/Setter]", k.as_str()),
                        })
                        .collect();
                    alloc::format!("{{{}}}", parts.join(", "))
                }
            }
        }
        Value::Sym(key) => alloc::format!("Symbol({})", symbol_description(key)),
        Value::Num(n) => n.to_js_string(),
        other => alloc::format!("{:?}", other),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Hint {
    Default,
    Number,
    String,
}

pub fn to_primitive(ctx: &mut Ctx, value: &Value, hint: Hint) -> R<Value> {
    let Value::Obj(object) = value else {
        return Ok(value.clone());
    };
    let exotic = get_key(ctx, value, &ctx.realm.symbols.to_primitive.clone())?;
    if exotic.as_obj().is_some_and(|f| f.is_callable()) {
        let hint_text = match hint {
            Hint::Default => "default",
            Hint::Number => "number",
            Hint::String => "string",
        };
        let result = ctx.call(&exotic, value.clone(), &[Value::str(hint_text)])?;
        if let Value::Obj(_) = result {
            return ctx.throw_type("Cannot convert object to primitive value");
        }
        return Ok(result);
    }
    let is_date = matches!(object.borrow().kind, Kind::Date(_));
    let order: [&str; 2] = if hint == Hint::String || (hint == Hint::Default && is_date) {
        ["toString", "valueOf"]
    } else {
        ["valueOf", "toString"]
    };
    for name in order {
        let method = get_key(ctx, value, &JsStr::from(name))?;
        if method.as_obj().is_some_and(|f| f.is_callable()) {
            let result = ctx.call(&method, value.clone(), &[])?;
            if !matches!(result, Value::Obj(_)) {
                return Ok(result);
            }
        }
    }
    ctx.throw_type("Cannot convert object to primitive value")
}

pub fn to_number(ctx: &mut Ctx, value: &Value) -> R<Num> {
    Ok(match value {
        Value::Num(n) => *n,
        Value::Undefined => Num::NAN,
        Value::Null => Num::ZERO,
        Value::Bool(b) => {
            if *b {
                Num::ONE
            } else {
                Num::ZERO
            }
        }
        Value::Str(s) => Num::parse(s),
        Value::Sym(_) => return ctx.throw_type("Cannot convert a Symbol value to a number"),
        Value::Obj(_) => {
            let primitive = to_primitive(ctx, value, Hint::Number)?;
            return to_number(ctx, &primitive);
        }
    })
}

/// Numbers only: BigInt is read as a number by the lexer.
pub fn to_numeric(ctx: &mut Ctx, value: &Value) -> R<Num> {
    to_number(ctx, value)
}

pub fn to_string(ctx: &mut Ctx, value: &Value) -> R<JsStr> {
    Ok(match value {
        Value::Str(s) => s.clone(),
        Value::Num(n) => number_string(*n),
        Value::Undefined => JsStr::from("undefined"),
        Value::Null => JsStr::from("null"),
        Value::Bool(b) => JsStr::from(if *b { "true" } else { "false" }),
        Value::Sym(_) => return ctx.throw_type("Cannot convert a Symbol value to a string"),
        Value::Obj(_) => {
            let primitive = to_primitive(ctx, value, Hint::String)?;
            return to_string(ctx, &primitive);
        }
    })
}

pub fn number_string(n: Num) -> JsStr {
    if let Some(i) = n.to_i64_exact() {
        if (0..10).contains(&i) {
            const DIGITS: [&str; 10] = ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"];
            return JsStr::from(DIGITS[i as usize]);
        }
    }
    JsStr::from(n.to_js_string())
}

pub fn to_property_key(ctx: &mut Ctx, value: &Value) -> R<JsStr> {
    match value {
        Value::Str(s) => Ok(s.clone()),
        Value::Sym(key) => Ok(key.clone()),
        Value::Num(n) => Ok(number_string(*n)),
        Value::Obj(_) => {
            let primitive = to_primitive(ctx, value, Hint::String)?;
            to_property_key(ctx, &primitive)
        }
        other => to_string(ctx, other),
    }
}

pub fn to_object(ctx: &mut Ctx, value: &Value) -> R<Obj> {
    let protos = &ctx.realm.protos;
    let (kind, proto, class) = match value {
        Value::Obj(object) => return Ok(object.clone()),
        Value::Undefined | Value::Null => return ctx.throw_type("Cannot convert undefined or null to object"),
        Value::Bool(b) => (Kind::Boolean(*b), protos.boolean.clone(), "Boolean"),
        Value::Num(n) => (Kind::Number(*n), protos.number.clone(), "Number"),
        Value::Str(s) => (Kind::String(s.clone()), protos.string.clone(), "String"),
        Value::Sym(s) => (Kind::Symbol(s.clone()), protos.symbol.clone(), "Symbol"),
    };
    Ok(ctx.alloc(kind, Some(proto), class))
}

/// ToIntegerOrInfinity, as a clamped i64.
pub fn to_integer(ctx: &mut Ctx, value: &Value) -> R<i64> {
    let n = to_number(ctx, value)?;
    if n.is_nan() {
        return Ok(0);
    }
    Ok(n.to_i64_saturating())
}

/// A relative index argument (`slice(-2)`), resolved against `length`.
pub fn relative_index(ctx: &mut Ctx, value: Option<&Value>, length: usize, default: usize) -> R<usize> {
    match value {
        None | Some(Value::Undefined) => Ok(default),
        Some(value) => {
            let n = to_integer(ctx, value)?;
            Ok(if n < 0 { (length as i64 + n).max(0) as usize } else { (n as usize).min(length) })
        }
    }
}

// ============================================================
//  Property access
// ============================================================

/// `length` and indices of arrays and strings.
fn exotic_get(object: &Obj, key: &str) -> Option<Value> {
    let borrowed = object.borrow();
    match &borrowed.kind {
        Kind::Array(items) => {
            if key == "length" {
                return Some(Value::num(items.len() as i64));
            }
            let index = array_index(key)?;
            items.get(index).cloned()
        }
        Kind::String(s) => {
            if key == "length" {
                return Some(Value::num(s.units() as i64));
            }
            let index = array_index(key)?;
            string_index(s, index)
        }
        _ => None,
    }
}

pub fn string_index(s: &JsStr, index: usize) -> Option<Value> {
    if s.is_ascii() {
        return s.as_bytes().get(index).map(|b| Value::Str(JsStr::from(core::str::from_utf8(&[*b]).unwrap_or(""))));
    }
    let unit = s.unit_at(index)?;
    Some(Value::Str(unit_string(unit, s, index)))
}

/// A one-unit string; a lone surrogate half becomes U+FFFD.
fn unit_string(unit: u16, _s: &JsStr, _index: usize) -> JsStr {
    let c = char::from_u32(unit as u32).unwrap_or('\u{fffd}');
    let mut text = String::new();
    text.push(c);
    JsStr::from(text)
}

/// Reads `key` from `object` and its prototypes, with `receiver` as the
/// `this` of getters.
pub fn get_from(ctx: &mut Ctx, object: &Obj, key: &JsStr, receiver: &Value) -> R<Value> {
    let mut current = object.clone();
    for _ in 0..10_000 {
        if let Some(value) = exotic_get(&current, key) {
            return Ok(value);
        }
        let slot = current.borrow().props.get(key).cloned();
        match slot {
            Some(Slot::Data(value, _)) => return Ok(value),
            Some(Slot::Accessor(getter, _, _)) => {
                return match getter {
                    Some(getter) => ctx.call(&Value::Obj(getter), receiver.clone(), &[]),
                    None => Ok(Value::Undefined),
                };
            }
            None => {}
        }
        let next = current.proto();
        match next {
            Some(next) => current = next,
            None => return Ok(Value::Undefined),
        }
    }
    Ok(Value::Undefined)
}

/// Like `get_from`, but tells a missing property from an undefined one.
pub fn get_if_present(ctx: &mut Ctx, object: &Obj, key: &JsStr) -> R<Option<Value>> {
    let mut current = object.clone();
    for _ in 0..10_000 {
        if let Some(value) = exotic_get(&current, key) {
            return Ok(Some(value));
        }
        let slot = current.borrow().props.get(key).cloned();
        match slot {
            Some(Slot::Data(value, _)) => return Ok(Some(value)),
            Some(Slot::Accessor(getter, _, _)) => {
                return match getter {
                    Some(getter) => Ok(Some(ctx.call(&Value::Obj(getter), Value::Obj(object.clone()), &[])?)),
                    None => Ok(Some(Value::Undefined)),
                };
            }
            None => {}
        }
        let next = current.proto();
        match next {
            Some(next) => current = next,
            None => return Ok(None),
        }
    }
    Ok(None)
}

fn primitive_proto(ctx: &Ctx, value: &Value) -> Option<Obj> {
    let protos = &ctx.realm.protos;
    match value {
        Value::Str(_) => Some(protos.string.clone()),
        Value::Num(_) => Some(protos.number.clone()),
        Value::Bool(_) => Some(protos.boolean.clone()),
        Value::Sym(_) => Some(protos.symbol.clone()),
        _ => None,
    }
}

pub fn get_key(ctx: &mut Ctx, value: &Value, key: &JsStr) -> R<Value> {
    match value {
        Value::Obj(object) => get_from(ctx, object, key, value),
        Value::Str(s) => {
            if key.as_str() == "length" {
                return Ok(Value::num(s.units() as i64));
            }
            if let Some(index) = array_index(key) {
                return Ok(string_index(s, index).unwrap_or(Value::Undefined));
            }
            let proto = ctx.realm.protos.string.clone();
            get_from(ctx, &proto, key, value)
        }
        Value::Undefined | Value::Null => {
            let message = alloc::format!(
                "Cannot read properties of {} (reading '{}')",
                if value.is_undefined() { "undefined" } else { "null" },
                key.as_str().trim_start_matches('\0')
            );
            ctx.throw_type(&message)
        }
        other => {
            let proto = primitive_proto(ctx, other).expect("primitive");
            get_from(ctx, &proto, key, value)
        }
    }
}

/// `object.name` in source: same as `get_key`.
pub fn get_key_reading(ctx: &mut Ctx, value: &Value, key: &JsStr) -> R<Value> {
    get_key(ctx, value, key)
}

pub fn get(ctx: &mut Ctx, value: &Value, key: &str) -> R<Value> {
    get_key(ctx, value, &JsStr::from(key))
}

/// `object[index]`, with a fast path for arrays and numbers.
pub fn get_value_key(ctx: &mut Ctx, value: &Value, index: &Value) -> R<Value> {
    if let (Value::Obj(object), Value::Num(n)) = (value, index) {
        if let Some(i) = n.to_index() {
            if let Kind::Array(items) = &object.borrow().kind {
                if let Some(item) = items.get(i) {
                    return Ok(item.clone());
                }
            }
        }
    }
    if let (Value::Str(s), Value::Num(n)) = (value, index) {
        if let Some(i) = n.to_index() {
            return Ok(string_index(s, i).unwrap_or(Value::Undefined));
        }
    }
    let key = to_property_key(ctx, index)?;
    get_key(ctx, value, &key)
}

pub fn set_value_key(ctx: &mut Ctx, target: &Value, index: &Value, value: Value) -> R<()> {
    if let (Value::Obj(object), Value::Num(n)) = (target, index) {
        if let Some(i) = n.to_index() {
            check_growth(ctx, object, 1)?;
            let mut borrowed = object.borrow_mut();
            if !borrowed.frozen {
                if let Kind::Array(items) = &mut borrowed.kind {
                    if i < items.len() {
                        items[i] = value;
                        return Ok(());
                    }
                    if i == items.len() {
                        items.push(value);
                        return Ok(());
                    }
                }
            }
        }
    }
    let key = to_property_key(ctx, index)?;
    set(ctx, target, key, value)
}

/// `target[key] = value`.
pub fn set(ctx: &mut Ctx, target: &Value, key: JsStr, value: Value) -> R<()> {
    let object = match target {
        Value::Obj(object) => object.clone(),
        Value::Undefined | Value::Null => {
            let message = alloc::format!(
                "Cannot set properties of {} (setting '{}')",
                if target.is_undefined() { "undefined" } else { "null" },
                key.as_str().trim_start_matches('\0')
            );
            return ctx.throw_type(&message);
        }
        // Writes to primitives vanish.
        _ => return Ok(()),
    };
    // Arrays: length and indices.
    if let Some(index) = array_index(&key) {
        let extra = match &object.borrow().kind {
            Kind::Array(items) if index >= items.len() => index + 1 - items.len(),
            _ => 0,
        };
        if extra > 0 && extra <= DENSE_GAP + 1 {
            check_growth(ctx, &object, extra)?;
        }
    }
    {
        let mut borrowed = object.borrow_mut();
        if borrowed.frozen {
            return Ok(());
        }
        if let Kind::Array(items) = &mut borrowed.kind {
            if key.as_str() == "length" {
                drop(borrowed);
                let length = to_number(ctx, &value)?;
                let Some(length) = length.to_index().filter(|l| *l <= u32::MAX as usize) else {
                    return ctx.throw_range("Invalid array length");
                };
                let current = match &object.borrow().kind {
                    Kind::Array(items) => items.len(),
                    _ => return Ok(()),
                };
                if length > current + DENSE_GAP {
                    return ctx.throw_range("Invalid array length");
                }
                if length > current {
                    check_growth(ctx, &object, length - current)?;
                }
                if let Kind::Array(items) = &mut object.borrow_mut().kind {
                    items.resize(length, Value::Undefined);
                }
                return Ok(());
            }
            if let Some(index) = array_index(&key) {
                if index < items.len() {
                    items[index] = value;
                    return Ok(());
                }
                if index <= items.len() + DENSE_GAP {
                    items.resize(index, Value::Undefined);
                    items.push(value);
                    return Ok(());
                }
            }
        }
    }
    // Own property?
    let own = object.borrow().props.get(&key).cloned();
    match own {
        Some(Slot::Data(_, flags)) => {
            if flags & WRITABLE != 0 {
                if let Some(Slot::Data(slot, _)) = object.borrow_mut().props.get_mut(&key) {
                    *slot = value;
                }
            }
            return Ok(());
        }
        Some(Slot::Accessor(_, setter, _)) => {
            if let Some(setter) = setter {
                ctx.call(&Value::Obj(setter), target.clone(), &[value])?;
            }
            return Ok(());
        }
        None => {}
    }
    // A setter or a read-only property up the chain?
    let mut current = object.proto();
    let mut depth = 0;
    while let Some(proto) = current {
        depth += 1;
        if depth > 10_000 {
            break;
        }
        let slot = proto.borrow().props.get(&key).cloned();
        match slot {
            Some(Slot::Accessor(_, setter, _)) => {
                if let Some(setter) = setter {
                    ctx.call(&Value::Obj(setter), target.clone(), &[value])?;
                }
                return Ok(());
            }
            Some(Slot::Data(_, flags)) => {
                if flags & WRITABLE == 0 {
                    return Ok(());
                }
                break;
            }
            None => {}
        }
        current = proto.proto();
    }
    let mut borrowed = object.borrow_mut();
    if !borrowed.extensible {
        return Ok(());
    }
    borrowed.props.set(key, Slot::Data(value, DEFAULT));
    Ok(())
}

pub fn delete(ctx: &mut Ctx, target: &Value, key: &JsStr) -> R<bool> {
    let Value::Obj(object) = target else {
        if target.is_nullish() {
            return ctx.throw_type("Cannot convert undefined or null to object");
        }
        return Ok(true);
    };
    let mut borrowed = object.borrow_mut();
    if borrowed.frozen {
        return Ok(false);
    }
    if let Kind::Array(items) = &mut borrowed.kind {
        if let Some(index) = array_index(key) {
            if index < items.len() {
                items[index] = Value::Undefined;
            }
            return Ok(true);
        }
    }
    let configurable = borrowed.props.get(key).map(|s| s.flags() & CONFIGURABLE != 0);
    match configurable {
        Some(false) => Ok(false),
        Some(true) => {
            borrowed.props.remove(key);
            Ok(true)
        }
        None => Ok(true),
    }
}

pub fn has_own(object: &Obj, key: &str) -> bool {
    let borrowed = object.borrow();
    match &borrowed.kind {
        Kind::Array(items) => {
            if key == "length" {
                return true;
            }
            if let Some(index) = array_index(key) {
                if index < items.len() {
                    return true;
                }
            }
        }
        Kind::String(s) => {
            if key == "length" {
                return true;
            }
            if let Some(index) = array_index(key) {
                return index < s.units();
            }
        }
        _ => {}
    }
    borrowed.props.get_str(key).is_some()
}

pub fn has_property(ctx: &mut Ctx, target: &Value, key: &JsStr) -> R<bool> {
    let Value::Obj(object) = target else {
        return ctx.throw_type("Cannot use 'in' operator on a primitive");
    };
    let mut current = Some(object.clone());
    let mut depth = 0;
    while let Some(object) = current {
        if has_own(&object, key) {
            return Ok(true);
        }
        depth += 1;
        if depth > 10_000 {
            break;
        }
        current = object.proto();
    }
    Ok(false)
}

/// Own keys in the specification's order: indices ascending, then
/// strings in insertion order, then symbols.
pub fn own_keys(object: &Obj, include_symbols: bool, only_enumerable: bool) -> Vec<JsStr> {
    let borrowed = object.borrow();
    let mut indices: Vec<(usize, JsStr)> = Vec::new();
    let mut strings: Vec<JsStr> = Vec::new();
    let mut symbols: Vec<JsStr> = Vec::new();
    match &borrowed.kind {
        Kind::Array(items) => {
            for index in 0..items.len() {
                indices.push((index, number_string(Num::from_i64(index as i64))));
            }
        }
        Kind::String(s) => {
            for index in 0..s.units() {
                indices.push((index, number_string(Num::from_i64(index as i64))));
            }
        }
        _ => {}
    }
    let builtin_indices = indices.len();
    for (key, slot) in borrowed.props.iter() {
        if only_enumerable && slot.flags() & ENUMERABLE == 0 {
            continue;
        }
        if is_symbol_key(key) {
            if include_symbols && !key.starts_with("\0#") {
                symbols.push(key.clone());
            }
        } else if let Some(index) = array_index(key) {
            indices.push((index, key.clone()));
        } else {
            strings.push(key.clone());
        }
    }
    if indices.len() > builtin_indices {
        indices.sort_by_key(|(i, _)| *i);
        indices.dedup_by_key(|(i, _)| *i);
    }
    let mut out: Vec<JsStr> = indices.into_iter().map(|(_, k)| k).collect();
    if !only_enumerable {
        if let Kind::Array(_) = borrowed.kind {
            strings.insert(0, JsStr::from("length"));
        }
    }
    out.extend(strings);
    out.extend(symbols);
    out
}

pub fn own_enumerable_keys(ctx: &mut Ctx, value: &Value) -> R<Vec<JsStr>> {
    match value {
        Value::Obj(object) => Ok(own_keys(object, false, true)),
        Value::Str(s) => Ok((0..s.units()).map(|i| number_string(Num::from_i64(i as i64))).collect()),
        Value::Undefined | Value::Null => ctx.throw_type("Cannot convert undefined or null to object"),
        _ => Ok(Vec::new()),
    }
}

/// The keys `for...in` visits: own and inherited enumerable strings,
/// each once.
pub fn enumerable_keys_with_protos(ctx: &mut Ctx, value: &Value) -> R<Vec<JsStr>> {
    let object = match value {
        Value::Obj(object) => object.clone(),
        other => to_object(ctx, other)?,
    };
    let mut seen: alloc::collections::BTreeSet<JsStr> = alloc::collections::BTreeSet::new();
    let mut out = Vec::new();
    let mut current = Some(object);
    let mut depth = 0;
    while let Some(object) = current {
        for key in own_keys(&object, false, false) {
            let enumerable = {
                let borrowed = object.borrow();
                match borrowed.props.get(&key) {
                    Some(slot) => slot.flags() & ENUMERABLE != 0,
                    None => key.as_str() != "length",
                }
            };
            if seen.insert(key.clone()) && enumerable {
                out.push(key);
            }
        }
        depth += 1;
        if depth > 1000 {
            break;
        }
        current = object.proto();
    }
    Ok(out)
}

/// `{...source}` and `Object.assign`: own enumerable properties copied.
pub fn copy_data_properties(ctx: &mut Ctx, target: &Obj, source: &Value) -> R<()> {
    if source.is_nullish() {
        return Ok(());
    }
    let keys = match source {
        Value::Obj(object) => own_keys(object, true, true),
        other => own_enumerable_keys(ctx, other)?,
    };
    for key in keys {
        let value = get_key(ctx, source, &key)?;
        target.put_key(key, value, DEFAULT);
    }
    Ok(())
}

/// The elements of an array-like, for `apply`, `Array.from` and friends.
pub fn list_from_array_like(ctx: &mut Ctx, value: &Value) -> R<Vec<Value>> {
    match value {
        Value::Undefined | Value::Null => Ok(Vec::new()),
        Value::Obj(object) => {
            if let Kind::Array(items) = &object.borrow().kind {
                return Ok(items.clone());
            }
            let length = get(ctx, value, "length")?;
            let length = to_integer(ctx, &length)?.clamp(0, 10_000_000) as usize;
            let mut out = Vec::with_capacity(length.min(4096));
            for index in 0..length {
                ctx.tick()?;
                out.push(get_key(ctx, value, &number_string(Num::from_i64(index as i64)))?);
            }
            Ok(out)
        }
        _ => ctx.throw_type("CreateListFromArrayLike called on non-object"),
    }
}

// ============================================================
//  Iteration
// ============================================================

pub enum Iter {
    Array(Obj, usize),
    Str(JsStr, usize),
    Generic(Value, Value),
    Done,
}

pub fn get_iterator(ctx: &mut Ctx, value: &Value) -> R<Iter> {
    match value {
        Value::Str(s) => return Ok(Iter::Str(s.clone(), 0)),
        Value::Obj(object) => {
            let plain_array = matches!(object.borrow().kind, Kind::Array(_))
                && object.borrow().props.get(&ctx.realm.symbols.iterator).is_none();
            if plain_array {
                return Ok(Iter::Array(object.clone(), 0));
            }
        }
        _ => {}
    }
    let symbol = ctx.realm.symbols.iterator.clone();
    let method = if value.is_nullish() { Value::Undefined } else { get_key(ctx, value, &symbol)? };
    if !method.as_obj().is_some_and(|m| m.is_callable()) {
        let message = alloc::format!("{} is not iterable", to_display(value));
        return ctx.throw_type(&message);
    }
    let iterator = ctx.call(&method, value.clone(), &[])?;
    if !matches!(iterator, Value::Obj(_)) {
        return ctx.throw_type("Result of the Symbol.iterator method is not an object");
    }
    let next = get(ctx, &iterator, "next")?;
    Ok(Iter::Generic(iterator, next))
}

pub fn iterator_step(ctx: &mut Ctx, iterator: &mut Iter) -> R<Option<Value>> {
    match iterator {
        Iter::Array(array, position) => {
            let item = match &array.borrow().kind {
                Kind::Array(items) => items.get(*position).cloned(),
                _ => None,
            };
            match item {
                Some(item) => {
                    *position += 1;
                    Ok(Some(item))
                }
                None => {
                    *iterator = Iter::Done;
                    Ok(None)
                }
            }
        }
        Iter::Str(s, position) => match s[*position..].chars().next() {
            Some(c) => {
                *position += c.len_utf8();
                let mut text = String::new();
                text.push(c);
                Ok(Some(Value::Str(JsStr::from(text))))
            }
            None => {
                *iterator = Iter::Done;
                Ok(None)
            }
        },
        Iter::Generic(object, next) => {
            let result = ctx.call(next, object.clone(), &[])?;
            if !matches!(result, Value::Obj(_)) {
                return ctx.throw_type("Iterator result is not an object");
            }
            let done = get(ctx, &result, "done")?.truthy();
            if done {
                *iterator = Iter::Done;
                return Ok(None);
            }
            Ok(Some(get(ctx, &result, "value")?))
        }
        Iter::Done => Ok(None),
    }
}

/// Stops an iterator early: calls its `return` if it has one.
pub fn iterator_close(ctx: &mut Ctx, iterator: &Iter) {
    if let Iter::Generic(object, _) = iterator {
        if let Ok(method) = get(ctx, object, "return") {
            if method.as_obj().is_some_and(|m| m.is_callable()) {
                let _ = ctx.call(&method, object.clone(), &[]);
            }
        }
    }
}

/// All the values of an iterable.
pub fn collect(ctx: &mut Ctx, value: &Value) -> R<Vec<Value>> {
    let mut iterator = get_iterator(ctx, value)?;
    let mut out = Vec::new();
    while let Some(item) = iterator_step(ctx, &mut iterator)? {
        ctx.tick()?;
        out.push(item);
    }
    Ok(out)
}

// ============================================================
//  Operators
// ============================================================

/// `==`.
pub fn loose_eq(ctx: &mut Ctx, a: &Value, b: &Value) -> R<bool> {
    Ok(match (a, b) {
        (Value::Undefined | Value::Null, Value::Undefined | Value::Null) => true,
        (Value::Undefined | Value::Null, _) | (_, Value::Undefined | Value::Null) => false,
        (Value::Num(x), Value::Str(s)) | (Value::Str(s), Value::Num(x)) => x.equals(Num::parse(s)),
        (Value::Bool(x), other) | (other, Value::Bool(x)) => {
            let n = Value::Num(if *x { Num::ONE } else { Num::ZERO });
            return loose_eq(ctx, &n, other);
        }
        (Value::Obj(_), Value::Obj(_)) => a.strict_eq(b),
        (Value::Obj(_), _) => {
            let primitive = to_primitive(ctx, a, Hint::Default)?;
            return loose_eq(ctx, &primitive, b);
        }
        (_, Value::Obj(_)) => {
            let primitive = to_primitive(ctx, b, Hint::Default)?;
            return loose_eq(ctx, a, &primitive);
        }
        _ => a.strict_eq(b),
    })
}

/// UTF-16 order of two strings.
pub fn compare_strings(a: &str, b: &str) -> Ordering {
    if a.is_ascii() && b.is_ascii() {
        return a.cmp(b);
    }
    a.encode_utf16().cmp(b.encode_utf16())
}

/// `<` family: None when either is NaN.
fn less_than(ctx: &mut Ctx, a: &Value, b: &Value) -> R<Option<Ordering>> {
    let a = to_primitive(ctx, a, Hint::Number)?;
    let b = to_primitive(ctx, b, Hint::Number)?;
    if let (Value::Str(x), Value::Str(y)) = (&a, &b) {
        return Ok(Some(compare_strings(x, y)));
    }
    let x = to_numeric(ctx, &a)?;
    let y = to_numeric(ctx, &b)?;
    Ok(x.partial_cmp(y))
}

pub fn instance_of(ctx: &mut Ctx, value: &Value, constructor: &Value) -> R<bool> {
    let Value::Obj(target) = constructor else {
        return ctx.throw_type("Right-hand side of 'instanceof' is not callable");
    };
    let has_instance = ctx.realm.symbols.has_instance.clone();
    let custom = target.borrow().props.get(&has_instance).cloned();
    if let Some(Slot::Data(method, _)) = custom {
        if method.as_obj().is_some_and(|m| m.is_callable()) {
            return Ok(ctx.call(&method, constructor.clone(), &[value.clone()])?.truthy());
        }
    }
    if !target.is_callable() {
        return ctx.throw_type("Right-hand side of 'instanceof' is not callable");
    }
    if let Kind::Bound(inner, _, _) = &target.borrow().kind {
        let inner = Value::Obj(inner.clone());
        return instance_of(ctx, value, &inner);
    }
    let Value::Obj(object) = value else {
        return Ok(false);
    };
    let prototype = get(ctx, constructor, "prototype")?;
    let Value::Obj(prototype) = prototype else {
        return ctx.throw_type("Function has non-object prototype in instanceof check");
    };
    let mut current = object.proto();
    let mut depth = 0;
    while let Some(proto) = current {
        if proto.ptr_eq(&prototype) {
            return Ok(true);
        }
        depth += 1;
        if depth > 10_000 {
            break;
        }
        current = proto.proto();
    }
    Ok(false)
}

pub fn concat(ctx: &mut Ctx, a: &str, b: &str) -> R<Value> {
    check_string(ctx, a.len() + b.len())?;
    let mut text = String::with_capacity(a.len() + b.len());
    text.push_str(a);
    text.push_str(b);
    Ok(Value::Str(JsStr::from(text)))
}

pub fn binary(ctx: &mut Ctx, op: BinOp, left: &Value, right: &Value) -> R<Value> {
    // Fast path: two numbers.
    if let (Value::Num(a), Value::Num(b)) = (left, right) {
        let (a, b) = (*a, *b);
        match op {
            BinOp::Add => return Ok(Value::Num(a.add(b))),
            BinOp::Sub => return Ok(Value::Num(a.sub(b))),
            BinOp::Mul => return Ok(Value::Num(a.mul(b))),
            BinOp::Lt => return Ok(Value::Bool(a.lt(b))),
            BinOp::Gt => return Ok(Value::Bool(b.lt(a))),
            BinOp::Le => return Ok(Value::Bool(matches!(a.partial_cmp(b), Some(Ordering::Less | Ordering::Equal)))),
            BinOp::Ge => return Ok(Value::Bool(matches!(a.partial_cmp(b), Some(Ordering::Greater | Ordering::Equal)))),
            BinOp::StrictEq | BinOp::Eq => return Ok(Value::Bool(a.equals(b))),
            BinOp::StrictNe | BinOp::Ne => return Ok(Value::Bool(!a.equals(b))),
            _ => {}
        }
    }
    match op {
        BinOp::Add => {
            if let (Value::Str(a), Value::Str(b)) = (left, right) {
                return concat(ctx, a, b);
            }
            let a = to_primitive(ctx, left, Hint::Default)?;
            let b = to_primitive(ctx, right, Hint::Default)?;
            if matches!(a, Value::Str(_)) || matches!(b, Value::Str(_)) {
                let a = to_string(ctx, &a)?;
                let b = to_string(ctx, &b)?;
                return concat(ctx, &a, &b);
            }
            let a = to_numeric(ctx, &a)?;
            let b = to_numeric(ctx, &b)?;
            Ok(Value::Num(a.add(b)))
        }
        BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem | BinOp::Exp => {
            let a = to_numeric(ctx, left)?;
            let b = to_numeric(ctx, right)?;
            Ok(Value::Num(match op {
                BinOp::Sub => a.sub(b),
                BinOp::Mul => a.mul(b),
                BinOp::Div => a.div(b),
                BinOp::Rem => a.rem(b),
                _ => a.pow(b),
            }))
        }
        BinOp::Shl | BinOp::Shr | BinOp::UShr | BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor => {
            let a = to_numeric(ctx, left)?;
            let b = to_numeric(ctx, right)?;
            let (x, y) = (a.to_i32(), b.to_u32());
            Ok(Value::Num(match op {
                BinOp::Shl => Num::from_i32(x.wrapping_shl(y & 31)),
                BinOp::Shr => Num::from_i32(x >> (y & 31)),
                BinOp::UShr => Num::from_i64((a.to_u32() >> (y & 31)) as i64),
                BinOp::BitAnd => Num::from_i32(x & b.to_i32()),
                BinOp::BitOr => Num::from_i32(x | b.to_i32()),
                _ => Num::from_i32(x ^ b.to_i32()),
            }))
        }
        BinOp::StrictEq => Ok(Value::Bool(left.strict_eq(right))),
        BinOp::StrictNe => Ok(Value::Bool(!left.strict_eq(right))),
        BinOp::Eq => Ok(Value::Bool(loose_eq(ctx, left, right)?)),
        BinOp::Ne => Ok(Value::Bool(!loose_eq(ctx, left, right)?)),
        BinOp::Lt => Ok(Value::Bool(less_than(ctx, left, right)? == Some(Ordering::Less))),
        BinOp::Gt => Ok(Value::Bool(less_than(ctx, left, right)? == Some(Ordering::Greater))),
        BinOp::Le => Ok(Value::Bool(matches!(less_than(ctx, left, right)?, Some(Ordering::Less | Ordering::Equal)))),
        BinOp::Ge => Ok(Value::Bool(matches!(less_than(ctx, left, right)?, Some(Ordering::Greater | Ordering::Equal)))),
        BinOp::In => {
            if !matches!(right, Value::Obj(_)) {
                return ctx.throw_type("Cannot use 'in' operator to search for a key in a non-object");
            }
            let key = to_property_key(ctx, left)?;
            Ok(Value::Bool(has_property(ctx, right, &key)?))
        }
        BinOp::InstanceOf => Ok(Value::Bool(instance_of(ctx, left, right)?)),
    }
}

/// Throws unless `value` is callable; returns it as an object.
pub fn require_callable(ctx: &mut Ctx, value: &Value, what: &str) -> R<Obj> {
    match value {
        Value::Obj(object) if object.is_callable() => Ok(object.clone()),
        _ => {
            let message = alloc::format!("{} is not a function", what);
            ctx.throw_type(&message)
        }
    }
}

/// An iterator result object `{ value, done }`.
pub fn iter_result(ctx: &mut Ctx, value: Value, done: bool) -> Value {
    let object = ctx.new_object();
    object.put("value", value);
    object.put("done", Value::Bool(done));
    Value::Obj(object)
}

pub fn throw_value<T>(value: Value) -> R<T> {
    Err(Ctrl::Throw(value))
}
