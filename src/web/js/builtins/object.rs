//! `Object`, its prototype, property descriptors, and `Reflect`.

use alloc::vec::Vec;

use super::arg;
use crate::web::js::interp::{define_accessor, Ctx, R};
use crate::web::js::ops;
use crate::web::js::value::*;

fn object_constructor(ctx: &mut Ctx, _this: &Value, args: &[Value], _: &Value) -> R<Value> {
    match arg(args, 0) {
        Value::Undefined | Value::Null => {
            let proto = ctx.proto_for_new(ctx.realm.protos.object.clone())?;
            Ok(Value::Obj(ctx.alloc(Kind::Ordinary, Some(proto), "Object")))
        }
        other => Ok(Value::Obj(ops::to_object(ctx, &other)?)),
    }
}

fn require_object(ctx: &mut Ctx, value: &Value) -> R<Obj> {
    match value {
        Value::Obj(object) => Ok(object.clone()),
        _ => ctx.throw_type("Object.defineProperty called on non-object"),
    }
}

fn keys(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let keys = ops::own_enumerable_keys(ctx, &arg(args, 0))?;
    let values = keys.into_iter().map(Value::Str).collect();
    Ok(Value::Obj(ctx.new_array(values)))
}

fn values(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let target = arg(args, 0);
    let keys = ops::own_enumerable_keys(ctx, &target)?;
    let mut out = Vec::with_capacity(keys.len());
    for key in keys {
        out.push(ops::get_key(ctx, &target, &key)?);
    }
    Ok(Value::Obj(ctx.new_array(out)))
}

fn entries(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let target = arg(args, 0);
    let keys = ops::own_enumerable_keys(ctx, &target)?;
    let mut out = Vec::with_capacity(keys.len());
    for key in keys {
        let value = ops::get_key(ctx, &target, &key)?;
        let pair = ctx.new_array(alloc::vec![Value::Str(key), value]);
        out.push(Value::Obj(pair));
    }
    Ok(Value::Obj(ctx.new_array(out)))
}

fn assign(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let target = ops::to_object(ctx, &arg(args, 0))?;
    let target_value = Value::Obj(target.clone());
    for source in args.iter().skip(1) {
        if source.is_nullish() {
            continue;
        }
        let keys = match source {
            Value::Obj(object) => ops::own_keys(object, true, true),
            other => ops::own_enumerable_keys(ctx, other)?,
        };
        for key in keys {
            let value = ops::get_key(ctx, source, &key)?;
            ops::set(ctx, &target_value, key, value)?;
        }
    }
    Ok(target_value)
}

fn create(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let proto = match arg(args, 0) {
        Value::Obj(proto) => Some(proto),
        Value::Null => None,
        _ => return ctx.throw_type("Object prototype may only be an Object or null"),
    };
    let object = ctx.alloc(Kind::Ordinary, proto, "Object");
    if let properties @ Value::Obj(_) = arg(args, 1) {
        define_all(ctx, &object, &properties)?;
    }
    Ok(Value::Obj(object))
}

/// Applies a property descriptor object to `target[key]`.
pub fn define_from_descriptor(ctx: &mut Ctx, target: &Obj, key: JsStr, descriptor: &Value) -> R<()> {
    if !matches!(descriptor, Value::Obj(_)) {
        return ctx.throw_type("Property description must be an object");
    }
    let has = |ctx: &mut Ctx, name: &str| ops::has_property(ctx, descriptor, &JsStr::from(name));
    let existing = target.borrow().props.get(&key).cloned();
    let old_flags = existing.as_ref().map_or(0, |s| s.flags());
    let mut flags = old_flags;
    for (name, bit) in [("enumerable", ENUMERABLE), ("configurable", CONFIGURABLE), ("writable", WRITABLE)] {
        if has(ctx, name)? {
            if ops::get(ctx, descriptor, name)?.truthy() {
                flags |= bit;
            } else {
                flags &= !bit;
            }
        }
    }
    let getter = if has(ctx, "get")? { Some(ops::get(ctx, descriptor, "get")?) } else { None };
    let setter = if has(ctx, "set")? { Some(ops::get(ctx, descriptor, "set")?) } else { None };
    if getter.is_some() || setter.is_some() {
        let as_function = |v: Option<Value>| v.and_then(|v| v.as_obj().filter(|f| f.is_callable()).cloned());
        let (old_get, old_set) = match &existing {
            Some(Slot::Accessor(g, s, _)) => (g.clone(), s.clone()),
            _ => (None, None),
        };
        let get = if getter.is_some() { as_function(getter) } else { old_get };
        let set = if setter.is_some() { as_function(setter) } else { old_set };
        target.borrow_mut().props.set(key, Slot::Accessor(get, set, flags & !WRITABLE));
        return Ok(());
    }
    let value = if has(ctx, "value")? {
        ops::get(ctx, descriptor, "value")?
    } else {
        match &existing {
            Some(Slot::Data(value, _)) => value.clone(),
            _ => Value::Undefined,
        }
    };
    // Array indices and length stay in the array.
    if target.is_array() && (key.as_str() == "length" || array_index(&key).is_some()) {
        return ops::set(ctx, &Value::Obj(target.clone()), key, value);
    }
    target.borrow_mut().props.set(key, Slot::Data(value, flags));
    Ok(())
}

fn define_property(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let target = require_object(ctx, &arg(args, 0))?;
    let key = ops::to_property_key(ctx, &arg(args, 1))?;
    define_from_descriptor(ctx, &target, key, &arg(args, 2))?;
    Ok(Value::Obj(target))
}

fn define_all(ctx: &mut Ctx, target: &Obj, properties: &Value) -> R<()> {
    for key in ops::own_enumerable_keys(ctx, properties)? {
        let descriptor = ops::get_key(ctx, properties, &key)?;
        define_from_descriptor(ctx, target, key, &descriptor)?;
    }
    Ok(())
}

fn define_properties(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let target = require_object(ctx, &arg(args, 0))?;
    define_all(ctx, &target, &arg(args, 1))?;
    Ok(Value::Obj(target))
}

pub fn descriptor_of(ctx: &mut Ctx, target: &Value, key: &JsStr) -> R<Value> {
    let Value::Obj(object) = target else {
        return Ok(Value::Undefined);
    };
    if let Some(value) = {
        let is_exotic = ops::has_own(object, key) && object.borrow().props.get(key).is_none();
        if is_exotic { Some(ops::get_key(ctx, target, key)?) } else { None }
    } {
        let descriptor = ctx.new_object();
        descriptor.put("value", value);
        descriptor.put("writable", Value::Bool(true));
        descriptor.put("enumerable", Value::Bool(key.as_str() != "length"));
        descriptor.put("configurable", Value::Bool(key.as_str() != "length"));
        return Ok(Value::Obj(descriptor));
    }
    let slot = object.borrow().props.get(key).cloned();
    let Some(slot) = slot else {
        return Ok(Value::Undefined);
    };
    let descriptor = ctx.new_object();
    let flags = slot.flags();
    match slot {
        Slot::Data(value, _) => {
            descriptor.put("value", value);
            descriptor.put("writable", Value::Bool(flags & WRITABLE != 0));
        }
        Slot::Accessor(get, set, _) => {
            descriptor.put("get", get.map_or(Value::Undefined, Value::Obj));
            descriptor.put("set", set.map_or(Value::Undefined, Value::Obj));
        }
    }
    descriptor.put("enumerable", Value::Bool(flags & ENUMERABLE != 0));
    descriptor.put("configurable", Value::Bool(flags & CONFIGURABLE != 0));
    Ok(Value::Obj(descriptor))
}

fn get_own_property_descriptor(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let key = ops::to_property_key(ctx, &arg(args, 1))?;
    descriptor_of(ctx, &arg(args, 0), &key)
}

fn get_own_property_descriptors(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let target = arg(args, 0);
    let out = ctx.new_object();
    if let Value::Obj(object) = &target {
        for key in ops::own_keys(object, true, false) {
            let descriptor = descriptor_of(ctx, &target, &key)?;
            out.put_key(key, descriptor, DEFAULT);
        }
    }
    Ok(Value::Obj(out))
}

fn get_own_property_names(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let target = arg(args, 0);
    let keys = match &target {
        Value::Obj(object) => ops::own_keys(object, false, false),
        other => ops::own_enumerable_keys(ctx, other)?,
    };
    let values = keys.into_iter().map(Value::Str).collect();
    Ok(Value::Obj(ctx.new_array(values)))
}

fn get_own_property_symbols(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let mut out = Vec::new();
    if let Value::Obj(object) = arg(args, 0) {
        for key in ops::own_keys(&object, true, false) {
            if is_symbol_key(&key) {
                out.push(Value::Sym(key));
            }
        }
    }
    Ok(Value::Obj(ctx.new_array(out)))
}

fn get_prototype_of(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let object = ops::to_object(ctx, &arg(args, 0))?;
    Ok(object.proto().map_or(Value::Null, Value::Obj))
}

pub fn set_proto(ctx: &mut Ctx, target: &Obj, proto: &Value) -> R<bool> {
    let proto = match proto {
        Value::Obj(proto) => Some(proto.clone()),
        Value::Null => None,
        _ => return ctx.throw_type("Object prototype may only be an Object or null"),
    };
    // No cycles.
    let mut current = proto.clone();
    while let Some(p) = current {
        if p.ptr_eq(target) {
            return ctx.throw_type("Cyclic __proto__ value");
        }
        current = p.proto();
    }
    target.borrow_mut().proto = proto;
    Ok(true)
}

fn set_prototype_of(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let target = arg(args, 0);
    if let Value::Obj(object) = &target {
        set_proto(ctx, object, &arg(args, 1))?;
    }
    Ok(target)
}

fn freeze(_: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let target = arg(args, 0);
    if let Value::Obj(object) = &target {
        let mut borrowed = object.borrow_mut();
        borrowed.frozen = true;
        borrowed.extensible = false;
    }
    Ok(target)
}

fn is_frozen(_: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    Ok(Value::Bool(match arg(args, 0) {
        Value::Obj(object) => object.borrow().frozen,
        _ => true,
    }))
}

fn prevent_extensions(_: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let target = arg(args, 0);
    if let Value::Obj(object) = &target {
        object.borrow_mut().extensible = false;
    }
    Ok(target)
}

fn is_extensible(_: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    Ok(Value::Bool(match arg(args, 0) {
        Value::Obj(object) => object.borrow().extensible,
        _ => false,
    }))
}

fn is(_: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    Ok(Value::Bool(arg(args, 0).same_value(&arg(args, 1))))
}

fn from_entries(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let object = ctx.new_object();
    for entry in ops::collect(ctx, &arg(args, 0))? {
        let key = ops::get_value_key(ctx, &entry, &Value::num(0))?;
        let value = ops::get_value_key(ctx, &entry, &Value::num(1))?;
        let key = ops::to_property_key(ctx, &key)?;
        object.put_key(key, value, DEFAULT);
    }
    Ok(Value::Obj(object))
}

fn has_own_static(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let object = ops::to_object(ctx, &arg(args, 0))?;
    let key = ops::to_property_key(ctx, &arg(args, 1))?;
    Ok(Value::Bool(ops::has_own(&object, &key)))
}

fn group_by(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let callback = arg(args, 1);
    let out = ctx.alloc(Kind::Ordinary, None, "Object");
    for (index, item) in ops::collect(ctx, &arg(args, 0))?.into_iter().enumerate() {
        let key = ctx.call(&callback, Value::Undefined, &[item.clone(), Value::num(index as i64)])?;
        let key = ops::to_property_key(ctx, &key)?;
        let existing = out.own_value(&key);
        match existing {
            Some(Value::Obj(group)) => {
                if let Kind::Array(items) = &mut group.borrow_mut().kind {
                    items.push(item);
                }
            }
            _ => {
                let group = ctx.new_array(alloc::vec![item]);
                out.put_key(key, Value::Obj(group), DEFAULT);
            }
        }
    }
    Ok(Value::Obj(out))
}

// ------------------------------------------------------------
//  Object.prototype
// ------------------------------------------------------------

fn has_own_property(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let key = ops::to_property_key(ctx, &arg(args, 0))?;
    let object = ops::to_object(ctx, this)?;
    Ok(Value::Bool(ops::has_own(&object, &key)))
}

fn is_prototype_of(_: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let (Value::Obj(this), Value::Obj(object)) = (this, arg(args, 0)) else {
        return Ok(Value::Bool(false));
    };
    let mut current = object.proto();
    while let Some(proto) = current {
        if proto.ptr_eq(this) {
            return Ok(Value::Bool(true));
        }
        current = proto.proto();
    }
    Ok(Value::Bool(false))
}

fn property_is_enumerable(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let key = ops::to_property_key(ctx, &arg(args, 0))?;
    let Value::Obj(object) = this else {
        return Ok(Value::Bool(false));
    };
    if object.borrow().props.get(&key).is_none() {
        return Ok(Value::Bool(ops::has_own(object, &key) && key.as_str() != "length"));
    }
    let enumerable = object.borrow().props.get(&key).is_some_and(|s| s.flags() & ENUMERABLE != 0);
    Ok(Value::Bool(enumerable))
}

pub fn object_to_string(ctx: &mut Ctx, this: &Value, _: &[Value], _: &Value) -> R<Value> {
    let tag = match this {
        Value::Undefined => return Ok(Value::str("[object Undefined]")),
        Value::Null => return Ok(Value::str("[object Null]")),
        Value::Obj(object) => {
            let symbol = ctx.realm.symbols.to_string_tag.clone();
            let custom = ops::get_key(ctx, this, &symbol)?;
            if let Value::Str(tag) = custom {
                return Ok(Value::Str(JsStr::from(alloc::format!("[object {}]", tag.as_str()))));
            }
            match &object.borrow().kind {
                Kind::Array(_) => "Array",
                Kind::Function(_) | Kind::Native(..) | Kind::Bound(..) => "Function",
                Kind::Error => "Error",
                Kind::Boolean(_) => "Boolean",
                Kind::Number(_) => "Number",
                Kind::String(_) => "String",
                Kind::Date(_) => "Date",
                Kind::RegExp(_) => "RegExp",
                Kind::Arguments => "Arguments",
                _ => "Object",
            }
        }
        Value::Num(_) => "Number",
        Value::Str(_) => "String",
        Value::Bool(_) => "Boolean",
        Value::Sym(_) => "Symbol",
    };
    Ok(Value::Str(JsStr::from(alloc::format!("[object {}]", tag))))
}

fn to_locale_string(ctx: &mut Ctx, this: &Value, _: &[Value], _: &Value) -> R<Value> {
    let method = ops::get(ctx, this, "toString")?;
    ctx.call(&method, this.clone(), &[])
}

fn value_of(ctx: &mut Ctx, this: &Value, _: &[Value], _: &Value) -> R<Value> {
    Ok(Value::Obj(ops::to_object(ctx, this)?))
}

fn proto_getter(ctx: &mut Ctx, this: &Value, _: &[Value], _: &Value) -> R<Value> {
    let object = ops::to_object(ctx, this)?;
    Ok(object.proto().map_or(Value::Null, Value::Obj))
}

fn proto_setter(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    if let Value::Obj(object) = this {
        let proto = arg(args, 0);
        if matches!(proto, Value::Obj(_) | Value::Null) {
            set_proto(ctx, object, &proto)?;
        }
    }
    Ok(Value::Undefined)
}

fn define_getter(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    if let (Value::Obj(object), Value::Obj(getter)) = (this, arg(args, 1)) {
        let key = ops::to_property_key(ctx, &arg(args, 0))?;
        define_accessor(object, key, Some(getter), None, DEFAULT);
    }
    Ok(Value::Undefined)
}

fn define_setter(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    if let (Value::Obj(object), Value::Obj(setter)) = (this, arg(args, 1)) {
        let key = ops::to_property_key(ctx, &arg(args, 0))?;
        define_accessor(object, key, None, Some(setter), DEFAULT);
    }
    Ok(Value::Undefined)
}

// ------------------------------------------------------------
//  Reflect
// ------------------------------------------------------------

fn reflect_own_keys(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let Value::Obj(object) = arg(args, 0) else {
        return ctx.throw_type("Reflect.ownKeys called on non-object");
    };
    let keys = ops::own_keys(&object, true, false)
        .into_iter()
        .map(|k| if is_symbol_key(&k) { Value::Sym(k) } else { Value::Str(k) })
        .collect();
    Ok(Value::Obj(ctx.new_array(keys)))
}

fn reflect_has(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let key = ops::to_property_key(ctx, &arg(args, 1))?;
    Ok(Value::Bool(ops::has_property(ctx, &arg(args, 0), &key)?))
}

fn reflect_get(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let Value::Obj(target) = arg(args, 0) else {
        return ctx.throw_type("Reflect.get called on non-object");
    };
    let key = ops::to_property_key(ctx, &arg(args, 1))?;
    let receiver = if args.len() > 2 { arg(args, 2) } else { Value::Obj(target.clone()) };
    ops::get_from(ctx, &target, &key, &receiver)
}

fn reflect_set(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let key = ops::to_property_key(ctx, &arg(args, 1))?;
    ops::set(ctx, &arg(args, 0), key, arg(args, 2))?;
    Ok(Value::Bool(true))
}

fn reflect_apply(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let list = ops::list_from_array_like(ctx, &arg(args, 2))?;
    ctx.call(&arg(args, 0), arg(args, 1), &list)
}

fn reflect_construct(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let list = ops::list_from_array_like(ctx, &arg(args, 1))?;
    let new_target = if args.len() > 2 { Some(arg(args, 2)) } else { None };
    ctx.construct(&arg(args, 0), &list, new_target)
}

fn reflect_define_property(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let target = require_object(ctx, &arg(args, 0))?;
    let key = ops::to_property_key(ctx, &arg(args, 1))?;
    define_from_descriptor(ctx, &target, key, &arg(args, 2))?;
    Ok(Value::Bool(true))
}

fn reflect_delete(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let key = ops::to_property_key(ctx, &arg(args, 1))?;
    Ok(Value::Bool(ops::delete(ctx, &arg(args, 0), &key)?))
}

fn reflect_set_prototype_of(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let target = require_object(ctx, &arg(args, 0))?;
    Ok(Value::Bool(set_proto(ctx, &target, &arg(args, 1))?))
}

pub fn install(ctx: &mut Ctx) {
    let proto = ctx.realm.protos.object.clone();
    let constructor = super::constructor(ctx, "Object", 1, object_constructor, &proto);
    let statics: &[(&str, usize, NativeFn)] = &[
        ("keys", 1, keys),
        ("values", 1, values),
        ("entries", 1, entries),
        ("assign", 2, assign),
        ("create", 2, create),
        ("defineProperty", 3, define_property),
        ("defineProperties", 2, define_properties),
        ("getOwnPropertyDescriptor", 2, get_own_property_descriptor),
        ("getOwnPropertyDescriptors", 1, get_own_property_descriptors),
        ("getOwnPropertyNames", 1, get_own_property_names),
        ("getOwnPropertySymbols", 1, get_own_property_symbols),
        ("getPrototypeOf", 1, get_prototype_of),
        ("setPrototypeOf", 2, set_prototype_of),
        ("freeze", 1, freeze),
        ("seal", 1, prevent_extensions),
        ("preventExtensions", 1, prevent_extensions),
        ("isFrozen", 1, is_frozen),
        ("isSealed", 1, is_frozen),
        ("isExtensible", 1, is_extensible),
        ("is", 2, is),
        ("fromEntries", 1, from_entries),
        ("hasOwn", 2, has_own_static),
        ("groupBy", 2, group_by),
    ];
    for (name, length, function) in statics {
        ctx.method(&constructor, name, *length, *function);
    }
    let methods: &[(&str, usize, NativeFn)] = &[
        ("hasOwnProperty", 1, has_own_property),
        ("isPrototypeOf", 1, is_prototype_of),
        ("propertyIsEnumerable", 1, property_is_enumerable),
        ("toString", 0, object_to_string),
        ("toLocaleString", 0, to_locale_string),
        ("valueOf", 0, value_of),
        ("__defineGetter__", 2, define_getter),
        ("__defineSetter__", 2, define_setter),
    ];
    for (name, length, function) in methods {
        ctx.method(&proto, name, *length, *function);
    }
    ctx.accessor(&proto, "__proto__", Some(proto_getter), Some(proto_setter));

    let reflect = ctx.new_object();
    let functions: &[(&str, usize, NativeFn)] = &[
        ("ownKeys", 1, reflect_own_keys),
        ("has", 2, reflect_has),
        ("get", 2, reflect_get),
        ("set", 3, reflect_set),
        ("apply", 3, reflect_apply),
        ("construct", 2, reflect_construct),
        ("defineProperty", 3, reflect_define_property),
        ("deleteProperty", 2, reflect_delete),
        ("getPrototypeOf", 1, get_prototype_of),
        ("setPrototypeOf", 2, reflect_set_prototype_of),
        ("getOwnPropertyDescriptor", 2, get_own_property_descriptor),
        ("isExtensible", 1, is_extensible),
        ("preventExtensions", 1, prevent_extensions),
    ];
    for (name, length, function) in functions {
        ctx.method(&reflect, name, *length, *function);
    }
    ctx.realm.global.put_hidden("Reflect", Value::Obj(reflect));
}
