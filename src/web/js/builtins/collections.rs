//! `Map`, `Set`, `WeakMap`, `WeakSet`, `WeakRef`.
//!
//! The storage is `value::MapData`: entries in insertion order, deleted
//! ones left as holes so that a live iterator keeps its place (deleting
//! while iterating is common and legal). The holes are squeezed out on
//! insertion, once they far outnumber the entries -- an insertion during
//! an iteration over a map that sparse is the one case where an iterator
//! could skip an entry.
//!
//! **The weak collections are not weak.** A key is held strongly, as in
//! `Map`: the realm frees everything at once when the page goes away
//! (`value.rs`), and a page that keeps adding keys meets the realm's
//! memory budget like any other. They refuse primitive keys, as the
//! language requires, so code written for them behaves the same.

use alloc::vec::Vec;

use super::arg;
use crate::web::js::interp::{Ctx, R};
use crate::web::js::ops;
use crate::web::js::value::*;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Family {
    Map,
    Set,
    WeakMap,
    WeakSet,
}

impl Family {
    fn name(self) -> &'static str {
        match self {
            Family::Map => "Map",
            Family::Set => "Set",
            Family::WeakMap => "WeakMap",
            Family::WeakSet => "WeakSet",
        }
    }

    fn is_set(self) -> bool {
        matches!(self, Family::Set | Family::WeakSet)
    }

    fn is_weak(self) -> bool {
        matches!(self, Family::WeakMap | Family::WeakSet)
    }
}

/// The collection behind `this`, checked against its family.
fn this_collection(ctx: &mut Ctx, this: &Value, family: Family, method: &str) -> R<Obj> {
    if let Value::Obj(object) = this {
        let borrowed = object.borrow();
        let fits = match (&borrowed.kind, family.is_set()) {
            (Kind::Map(_), false) | (Kind::Set(_), true) => borrowed.class == family.name(),
            _ => false,
        };
        drop(borrowed);
        if fits {
            return Ok(object.clone());
        }
    }
    let message = alloc::format!("Method {}.prototype.{} called on incompatible receiver", family.name(), method);
    ctx.throw_type(&message)
}

fn with_data<T>(object: &Obj, f: impl FnOnce(&mut MapData) -> T) -> Option<T> {
    match &mut object.borrow_mut().kind {
        Kind::Map(data) | Kind::Set(data) => Some(f(data)),
        _ => None,
    }
}

fn check_weak_key(ctx: &mut Ctx, family: Family, key: &Value) -> R<()> {
    if family.is_weak() && !matches!(key, Value::Obj(_) | Value::Sym(_)) {
        let message = alloc::format!("Invalid value used in {}", family.name());
        return ctx.throw_type(&message);
    }
    Ok(())
}

fn construct(ctx: &mut Ctx, args: &[Value], family: Family) -> R<Value> {
    if ctx.new_target.is_none() {
        let message = alloc::format!("Constructor {} requires 'new'", family.name());
        return ctx.throw_type(&message);
    }
    let default = match family {
        Family::Map => ctx.realm.protos.map.clone(),
        Family::Set => ctx.realm.protos.set.clone(),
        Family::WeakMap => ctx.realm.protos.weak_map.clone(),
        Family::WeakSet => ctx.realm.protos.weak_set.clone(),
    };
    let proto = ctx.proto_for_new(default)?;
    let kind = if family.is_set() { Kind::Set(MapData::default()) } else { Kind::Map(MapData::default()) };
    let object = ctx.alloc(kind, Some(proto), family.name());
    let source = arg(args, 0);
    if source.is_nullish() {
        return Ok(Value::Obj(object));
    }
    let mut iterator = ops::get_iterator(ctx, &source)?;
    loop {
        let Some(item) = ops::iterator_step(ctx, &mut iterator)? else {
            break;
        };
        ctx.tick()?;
        let step = (|| -> R<()> {
            let (key, value) = if family.is_set() {
                (item.clone(), item.clone())
            } else {
                if !matches!(item, Value::Obj(_)) {
                    return ctx.throw_type("Iterator value is not an entry object");
                }
                let key = ops::get_value_key(ctx, &item, &Value::num(0))?;
                let value = ops::get_value_key(ctx, &item, &Value::num(1))?;
                (key, value)
            };
            check_weak_key(ctx, family, &key)?;
            with_data(&object, |data| {
                data.compact();
                data.set(key, value)
            });
            Ok(())
        })();
        if let Err(error) = step {
            ops::iterator_close(ctx, &iterator);
            return Err(error);
        }
    }
    Ok(Value::Obj(object))
}

fn map_constructor(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    construct(ctx, args, Family::Map)
}

fn set_constructor(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    construct(ctx, args, Family::Set)
}

fn weak_map_constructor(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    construct(ctx, args, Family::WeakMap)
}

fn weak_set_constructor(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    construct(ctx, args, Family::WeakSet)
}

/// The family a native was installed for travels in its data.
fn family_of(data: &Value) -> Family {
    match data {
        Value::Num(n) => match n.to_i64_saturating() {
            1 => Family::Set,
            2 => Family::WeakMap,
            3 => Family::WeakSet,
            _ => Family::Map,
        },
        _ => Family::Map,
    }
}

fn get(ctx: &mut Ctx, this: &Value, args: &[Value], data: &Value) -> R<Value> {
    let object = this_collection(ctx, this, family_of(data), "get")?;
    let key = arg(args, 0);
    Ok(with_data(&object, |d| d.get(&key).cloned()).flatten().unwrap_or(Value::Undefined))
}

fn has(ctx: &mut Ctx, this: &Value, args: &[Value], data: &Value) -> R<Value> {
    let object = this_collection(ctx, this, family_of(data), "has")?;
    let key = arg(args, 0);
    Ok(Value::Bool(with_data(&object, |d| d.has(&key)).unwrap_or(false)))
}

fn set(ctx: &mut Ctx, this: &Value, args: &[Value], data: &Value) -> R<Value> {
    let family = family_of(data);
    let object = this_collection(ctx, this, family, "set")?;
    let key = arg(args, 0);
    check_weak_key(ctx, family, &key)?;
    let value = arg(args, 1);
    with_data(&object, |d| {
        d.compact();
        d.set(key, value)
    });
    Ok(this.clone())
}

fn add(ctx: &mut Ctx, this: &Value, args: &[Value], data: &Value) -> R<Value> {
    let family = family_of(data);
    let object = this_collection(ctx, this, family, "add")?;
    let key = arg(args, 0);
    check_weak_key(ctx, family, &key)?;
    with_data(&object, |d| {
        d.compact();
        d.set(key.clone(), key)
    });
    Ok(this.clone())
}

fn delete(ctx: &mut Ctx, this: &Value, args: &[Value], data: &Value) -> R<Value> {
    let object = this_collection(ctx, this, family_of(data), "delete")?;
    let key = arg(args, 0);
    Ok(Value::Bool(with_data(&object, |d| d.delete(&key)).unwrap_or(false)))
}

fn clear(ctx: &mut Ctx, this: &Value, _: &[Value], data: &Value) -> R<Value> {
    let object = this_collection(ctx, this, family_of(data), "clear")?;
    with_data(&object, MapData::clear);
    Ok(Value::Undefined)
}

fn size(ctx: &mut Ctx, this: &Value, _: &[Value], data: &Value) -> R<Value> {
    let object = this_collection(ctx, this, family_of(data), "size")?;
    Ok(Value::num(with_data(&object, |d| d.len()).unwrap_or(0) as i64))
}

/// `forEach`: live -- entries added during the walk are visited, deleted
/// ones are not.
fn for_each(ctx: &mut Ctx, this: &Value, args: &[Value], data: &Value) -> R<Value> {
    let object = this_collection(ctx, this, family_of(data), "forEach")?;
    let callback = arg(args, 0);
    ops::require_callable(ctx, &callback, "forEach callback")?;
    let this_arg = arg(args, 1);
    let mut position = 0;
    loop {
        let entry = with_data(&object, |d| {
            while position < d.entries.len() {
                position += 1;
                if let Some(entry) = &d.entries[position - 1] {
                    return Some(entry.clone());
                }
            }
            None
        })
        .flatten();
        let Some((key, value)) = entry else {
            break;
        };
        ctx.tick()?;
        ctx.call(&callback, this_arg.clone(), &[value, key, this.clone()])?;
    }
    Ok(Value::Undefined)
}

fn iterator(ctx: &mut Ctx, this: &Value, family: Family, mode: u8, method: &str) -> R<Value> {
    let object = this_collection(ctx, this, family, method)?;
    let proto = ctx.realm.protos.iterator.clone();
    let class = if family.is_set() { "Set Iterator" } else { "Map Iterator" };
    Ok(Value::Obj(ctx.alloc(Kind::Iterator(IterState::Map(object, 0, mode)), Some(proto), class)))
}

fn keys(ctx: &mut Ctx, this: &Value, _: &[Value], data: &Value) -> R<Value> {
    iterator(ctx, this, family_of(data), 0, "keys")
}

fn values(ctx: &mut Ctx, this: &Value, _: &[Value], data: &Value) -> R<Value> {
    iterator(ctx, this, family_of(data), 1, "values")
}

fn entries(ctx: &mut Ctx, this: &Value, _: &[Value], data: &Value) -> R<Value> {
    iterator(ctx, this, family_of(data), 2, "entries")
}

/// `Map.groupBy(items, callback)`.
fn group_by(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let callback = arg(args, 1);
    ops::require_callable(ctx, &callback, "Map.groupBy callback")?;
    let items = ops::collect(ctx, &arg(args, 0))?;
    let proto = ctx.realm.protos.map.clone();
    let map = ctx.alloc(Kind::Map(MapData::default()), Some(proto), "Map");
    for (index, item) in items.into_iter().enumerate() {
        let key = ctx.call(&callback, Value::Undefined, &[item.clone(), Value::num(index as i64)])?;
        let existing = with_data(&map, |d| d.get(&key).cloned()).flatten();
        match existing {
            Some(Value::Obj(group)) => {
                if let Kind::Array(list) = &mut group.borrow_mut().kind {
                    list.push(item);
                }
            }
            _ => {
                let group = ctx.new_array(alloc::vec![item]);
                with_data(&map, |d| d.set(key, Value::Obj(group)));
            }
        }
    }
    Ok(Value::Obj(map))
}

// ------------------------------------------------------------
//  Set algebra (ES2025)
// ------------------------------------------------------------

/// The values of the other operand: a Set directly, or anything with
/// `keys()` (the "set-like" protocol), or an iterable.
fn other_values(ctx: &mut Ctx, other: &Value) -> R<Vec<Value>> {
    if let Value::Obj(object) = other {
        if let Some(values) = with_data(object, |d| d.entries.iter().flatten().map(|(k, _)| k.clone()).collect::<Vec<_>>()) {
            return Ok(values);
        }
        let keys_method = ops::get(ctx, other, "keys")?;
        if keys_method.as_obj().is_some_and(|k| k.is_callable()) {
            let iterator = ctx.call(&keys_method, other.clone(), &[])?;
            let mut values = Vec::new();
            let next = ops::get(ctx, &iterator, "next")?;
            loop {
                ctx.tick()?;
                let result = ctx.call(&next, iterator.clone(), &[])?;
                if ops::get(ctx, &result, "done")?.truthy() {
                    break;
                }
                values.push(ops::get(ctx, &result, "value")?);
            }
            return Ok(values);
        }
    }
    ops::collect(ctx, other)
}

fn own_values(object: &Obj) -> Vec<Value> {
    with_data(object, |d| d.entries.iter().flatten().map(|(k, _)| k.clone()).collect()).unwrap_or_default()
}

fn new_set(ctx: &mut Ctx, values: Vec<Value>) -> Value {
    let proto = ctx.realm.protos.set.clone();
    let mut data = MapData::default();
    for value in values {
        data.set(value.clone(), value);
    }
    Value::Obj(ctx.alloc(Kind::Set(data), Some(proto), "Set"))
}

fn contains(values: &[Value], value: &Value) -> bool {
    values.iter().any(|v| v.same_value_zero(value))
}

fn algebra(ctx: &mut Ctx, this: &Value, args: &[Value], data: &Value) -> R<Value> {
    let operation = match data {
        Value::Num(n) => n.to_i64_saturating(),
        _ => 0,
    };
    let object = this_collection(ctx, this, Family::Set, "union")?;
    let mine = own_values(&object);
    let theirs = other_values(ctx, &arg(args, 0))?;
    let result = match operation {
        0 => {
            let mut all = mine;
            all.extend(theirs);
            return Ok(new_set(ctx, all));
        }
        1 => return Ok(new_set(ctx, mine.into_iter().filter(|v| contains(&theirs, v)).collect())),
        2 => return Ok(new_set(ctx, mine.into_iter().filter(|v| !contains(&theirs, v)).collect())),
        3 => {
            let mut out: Vec<Value> = mine.iter().filter(|v| !contains(&theirs, v)).cloned().collect();
            out.extend(theirs.iter().filter(|v| !contains(&mine, v)).cloned());
            return Ok(new_set(ctx, out));
        }
        4 => mine.iter().all(|v| contains(&theirs, v)),
        5 => theirs.iter().all(|v| contains(&mine, v)),
        _ => !mine.iter().any(|v| contains(&theirs, v)),
    };
    Ok(Value::Bool(result))
}

// ------------------------------------------------------------
//  WeakRef
// ------------------------------------------------------------

fn weak_ref_constructor(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    if ctx.new_target.is_none() {
        return ctx.throw_type("Constructor WeakRef requires 'new'");
    }
    let target = arg(args, 0);
    if !matches!(target, Value::Obj(_)) {
        return ctx.throw_type("WeakRef: target must be an object");
    }
    let proto = ctx.proto_for_new(ctx.realm.protos.object.clone())?;
    let object = ctx.alloc(Kind::Ordinary, Some(proto), "WeakRef");
    object.put_key(crate::web::js::interp::private_key("target"), target, 0);
    Ok(Value::Obj(object))
}

fn weak_ref_deref(_: &mut Ctx, this: &Value, _: &[Value], _: &Value) -> R<Value> {
    let key = crate::web::js::interp::private_key("target");
    Ok(match this {
        Value::Obj(object) => match object.borrow().props.get(&key) {
            Some(Slot::Data(value, _)) => value.clone(),
            _ => Value::Undefined,
        },
        _ => Value::Undefined,
    })
}

fn finalization_constructor(ctx: &mut Ctx, _: &Value, _: &[Value], _: &Value) -> R<Value> {
    if ctx.new_target.is_none() {
        return ctx.throw_type("Constructor FinalizationRegistry requires 'new'");
    }
    let proto = ctx.proto_for_new(ctx.realm.protos.object.clone())?;
    Ok(Value::Obj(ctx.alloc(Kind::Ordinary, Some(proto), "FinalizationRegistry")))
}

/// `register` and `unregister`: nothing is ever collected early, so no
/// callback is ever due.
fn finalization_noop(_: &mut Ctx, _: &Value, _: &[Value], _: &Value) -> R<Value> {
    Ok(Value::Undefined)
}

pub fn install(ctx: &mut Ctx) {
    let families = [
        (Family::Map, ctx.realm.protos.map.clone(), map_constructor as NativeFn),
        (Family::Set, ctx.realm.protos.set.clone(), set_constructor as NativeFn),
        (Family::WeakMap, ctx.realm.protos.weak_map.clone(), weak_map_constructor as NativeFn),
        (Family::WeakSet, ctx.realm.protos.weak_set.clone(), weak_set_constructor as NativeFn),
    ];
    let iterator_symbol = ctx.realm.symbols.iterator.clone();
    let tag = ctx.realm.symbols.to_string_tag.clone();
    for (index, (family, proto, function)) in families.into_iter().enumerate() {
        let constructor = super::constructor(ctx, family.name(), 0, function, &proto);
        let data = Value::num(index as i64);
        let mut methods: Vec<(&str, usize, NativeFn)> = alloc::vec![("has", 1, has), ("delete", 1, delete)];
        if family.is_set() {
            methods.push(("add", 1, add));
        } else {
            methods.push(("get", 1, get));
            methods.push(("set", 2, set));
        }
        if !family.is_weak() {
            methods.extend_from_slice(&[
                ("clear", 0, clear as NativeFn),
                ("forEach", 1, for_each),
                ("keys", 0, keys),
                ("values", 0, values),
                ("entries", 0, entries),
            ]);
        }
        for (name, length, function) in methods {
            let native = ctx.native_with(name, length, function, data.clone());
            proto.put_hidden(name, Value::Obj(native));
        }
        if !family.is_weak() {
            let getter = ctx.native_with("size", 0, size, data.clone());
            proto.borrow_mut().props.set(JsStr::from("size"), Slot::Accessor(Some(getter), None, CONFIGURABLE));
            // Map: entries; Set: values (and keys is values too).
            let default_iterator = if family.is_set() { "values" } else { "entries" };
            let function = proto.own_value(default_iterator).unwrap_or(Value::Undefined);
            proto.put_key(iterator_symbol.clone(), function.clone(), HIDDEN);
            if family.is_set() {
                proto.put_hidden("keys", function);
            }
        }
        proto.put_key(tag.clone(), Value::str(family.name()), CONFIGURABLE);
        if family == Family::Map {
            ctx.method(&constructor, "groupBy", 2, group_by);
        }
        if family == Family::Set {
            let operations = [
                "union",
                "intersection",
                "difference",
                "symmetricDifference",
                "isSubsetOf",
                "isSupersetOf",
                "isDisjointFrom",
            ];
            for (operation, name) in operations.into_iter().enumerate() {
                let native = ctx.native_with(name, 1, algebra, Value::num(operation as i64));
                proto.put_hidden(name, Value::Obj(native));
            }
        }
    }

    let weak_ref_proto = ctx.new_object();
    super::constructor(ctx, "WeakRef", 1, weak_ref_constructor, &weak_ref_proto);
    ctx.method(&weak_ref_proto, "deref", 0, weak_ref_deref);
    weak_ref_proto.put_key(tag.clone(), Value::str("WeakRef"), CONFIGURABLE);
    let registry_proto = ctx.new_object();
    super::constructor(ctx, "FinalizationRegistry", 1, finalization_constructor, &registry_proto);
    ctx.method(&registry_proto, "register", 2, finalization_noop);
    ctx.method(&registry_proto, "unregister", 1, finalization_noop);
    registry_proto.put_key(tag, Value::str("FinalizationRegistry"), CONFIGURABLE);
}
