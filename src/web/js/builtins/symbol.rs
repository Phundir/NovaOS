//! `Symbol`.
//!
//! A symbol is a property key no string can spell (`value::symbol_key`):
//! `\0@{id}:{description}`, the id unique in the realm. `Symbol.for`
//! keeps its registry in an object hung on the constructor under a
//! private key, so that it goes away with the realm like everything else.

use alloc::string::String;

use super::arg;
use crate::web::js::interp::{private_key, Ctx, R};
use crate::web::js::ops;
use crate::web::js::value::*;

fn this_symbol(ctx: &mut Ctx, this: &Value) -> R<JsStr> {
    match this {
        Value::Sym(key) => Ok(key.clone()),
        Value::Obj(object) => {
            if let Kind::Symbol(key) = &object.borrow().kind {
                return Ok(key.clone());
            }
            ctx.throw_type("Symbol.prototype method requires that 'this' be a Symbol")
        }
        _ => ctx.throw_type("Symbol.prototype method requires that 'this' be a Symbol"),
    }
}

/// A new symbol; `None` for one made without a description.
pub fn new_symbol(ctx: &mut Ctx, description: Option<&str>) -> Value {
    let id = ctx.realm.next_symbol;
    ctx.realm.next_symbol += 1;
    let key = match description {
        Some(text) => symbol_key(id, text),
        None => JsStr::from(alloc::format!("\0@{}", id)),
    };
    Value::Sym(key)
}

fn symbol_constructor(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    if ctx.new_target.is_some() {
        return ctx.throw_type("Symbol is not a constructor");
    }
    let description = match arg(args, 0) {
        Value::Undefined => None,
        other => Some(String::from(ops::to_string(ctx, &other)?.as_str())),
    };
    Ok(new_symbol(ctx, description.as_deref()))
}

fn registry(ctx: &mut Ctx) -> R<Obj> {
    let constructor = ops::get(ctx, &Value::Obj(ctx.realm.global.clone()), "Symbol")?;
    let Value::Obj(constructor) = constructor else {
        return ctx.throw_type("Symbol registry unavailable");
    };
    let key = private_key("registry");
    if let Some(Slot::Data(Value::Obj(table), _)) = constructor.borrow().props.get(&key) {
        return Ok(table.clone());
    }
    let table = ctx.alloc(Kind::Ordinary, None, "SymbolRegistry");
    constructor.put_key(key, Value::Obj(table.clone()), 0);
    Ok(table)
}

fn symbol_for(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let key = ops::to_string(ctx, &arg(args, 0))?;
    let table = registry(ctx)?;
    if let Some(existing) = table.own_value(&key) {
        return Ok(existing);
    }
    let symbol = new_symbol(ctx, Some(&key));
    table.put_key(key, symbol.clone(), DEFAULT);
    Ok(symbol)
}

fn key_for(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let Value::Sym(symbol) = arg(args, 0) else {
        return ctx.throw_type("Symbol.keyFor: argument is not a symbol");
    };
    let table = registry(ctx)?;
    let description = JsStr::from(symbol_description(&symbol));
    match table.own_value(&description) {
        Some(Value::Sym(found)) if found == symbol => Ok(Value::Str(description)),
        _ => Ok(Value::Undefined),
    }
}

fn to_string(ctx: &mut Ctx, this: &Value, _: &[Value], _: &Value) -> R<Value> {
    let key = this_symbol(ctx, this)?;
    Ok(Value::Str(JsStr::from(alloc::format!("Symbol({})", symbol_description(&key)))))
}

fn value_of(ctx: &mut Ctx, this: &Value, _: &[Value], _: &Value) -> R<Value> {
    Ok(Value::Sym(this_symbol(ctx, this)?))
}

fn description(ctx: &mut Ctx, this: &Value, _: &[Value], _: &Value) -> R<Value> {
    let key = this_symbol(ctx, this)?;
    Ok(if key.contains(':') { Value::str(symbol_description(&key)) } else { Value::Undefined })
}

pub fn install(ctx: &mut Ctx) {
    let proto = ctx.realm.protos.symbol.clone();
    let constructor = super::constructor(ctx, "Symbol", 0, symbol_constructor, &proto);
    ctx.method(&constructor, "for", 1, symbol_for);
    ctx.method(&constructor, "keyFor", 1, key_for);
    let symbols = &ctx.realm.symbols;
    let well_known = [
        ("iterator", symbols.iterator.clone()),
        ("asyncIterator", symbols.async_iterator.clone()),
        ("hasInstance", symbols.has_instance.clone()),
        ("toPrimitive", symbols.to_primitive.clone()),
        ("toStringTag", symbols.to_string_tag.clone()),
        ("species", symbols.species.clone()),
        ("unscopables", symbols.unscopables.clone()),
    ];
    for (name, key) in well_known {
        constructor.put_key(JsStr::from(name), Value::Sym(key), 0);
    }
    // Recognized nowhere in the engine, but scripts test for them.
    for (id, name) in [(8, "match"), (9, "matchAll"), (10, "replace"), (11, "search"), (12, "split"), (13, "isConcatSpreadable")] {
        let key = symbol_key(id, &alloc::format!("Symbol.{}", name));
        constructor.put_key(JsStr::from(name), Value::Sym(key), 0);
    }
    ctx.method(&proto, "toString", 0, to_string);
    ctx.method(&proto, "valueOf", 0, value_of);
    ctx.accessor(&proto, "description", Some(description), None);
    let to_primitive = ctx.realm.symbols.to_primitive.clone();
    let primitive = ctx.native("[Symbol.toPrimitive]", 1, value_of);
    proto.put_key(to_primitive, Value::Obj(primitive), CONFIGURABLE);
    let tag = ctx.realm.symbols.to_string_tag.clone();
    proto.put_key(tag, Value::str("Symbol"), CONFIGURABLE);
}
