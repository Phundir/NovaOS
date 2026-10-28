//! The standard library: the global objects every script expects.
//!
//! One file per family. Each has an `install` that fills its prototype
//! and puts its constructor on the global object. The prototypes
//! themselves are made first, all at once, in `new_realm`, because they
//! point at each other (a function's prototype is an object, an object's
//! methods are functions).

pub mod array;
pub mod collections;
pub mod date;
pub mod function;
pub mod global;
pub mod json;
pub mod number;
pub mod object;
pub mod promise;
pub mod regexp;
pub mod string;

use alloc::vec::Vec;

use super::interp::{Ctx, Protos, Realm, Symbols, R};
use super::value::*;

/// A native doing nothing: the body of `Function.prototype`.
fn empty(_: &mut Ctx, _: &Value, _: &[Value], _: &Value) -> R<Value> {
    Ok(Value::Undefined)
}

/// Argument `index`, or undefined.
pub fn arg(args: &[Value], index: usize) -> Value {
    args.get(index).cloned().unwrap_or(Value::Undefined)
}

/// Makes the realm: prototypes, well-known symbols, global object. The
/// library is installed by `install`, once a context exists.
pub fn new_realm() -> Realm {
    let object = Obj::new(Kind::Ordinary, None, "Object");
    let make = |class: &'static str, kind: Kind| Obj::new(kind, Some(object.clone()), class);
    let function = make("Function", Kind::Native(empty, Value::Undefined, JsStr::from("")));
    let error = make("Error", Kind::Ordinary);
    let make_error = |name: &'static str| Obj::new(Kind::Ordinary, Some(error.clone()), name);
    let iterator = make("Iterator", Kind::Ordinary);
    let event_target = make("EventTarget", Kind::Ordinary);
    let node = Obj::new(Kind::Ordinary, Some(event_target.clone()), "Node");
    let element = Obj::new(Kind::Ordinary, Some(node.clone()), "Element");
    let document = Obj::new(Kind::Ordinary, Some(node.clone()), "Document");
    let text = Obj::new(Kind::Ordinary, Some(node.clone()), "Text");
    let protos = Protos {
        function: function.clone(),
        array: make("Array", Kind::Array(Vec::new())),
        string: make("String", Kind::String(JsStr::from(""))),
        number: make("Number", Kind::Number(super::num::Num::ZERO)),
        boolean: make("Boolean", Kind::Boolean(false)),
        symbol: make("Symbol", Kind::Ordinary),
        type_error: make_error("TypeError"),
        range_error: make_error("RangeError"),
        syntax_error: make_error("SyntaxError"),
        reference_error: make_error("ReferenceError"),
        eval_error: make_error("EvalError"),
        uri_error: make_error("URIError"),
        error: error.clone(),
        date: make("Date", Kind::Ordinary),
        regexp: make("RegExp", Kind::Ordinary),
        map: make("Map", Kind::Ordinary),
        set: make("Set", Kind::Ordinary),
        weak_map: make("WeakMap", Kind::Ordinary),
        weak_set: make("WeakSet", Kind::Ordinary),
        promise: make("Promise", Kind::Ordinary),
        generator: Obj::new(Kind::Ordinary, Some(iterator.clone()), "Generator"),
        iterator,
        event_target,
        node,
        element,
        document,
        text,
        event: make("Event", Kind::Ordinary),
        dom_token_list: make("DOMTokenList", Kind::Ordinary),
        style: make("CSSStyleDeclaration", Kind::Ordinary),
        node_list: make("NodeList", Kind::Ordinary),
        object: object.clone(),
    };
    let symbols = Symbols {
        iterator: symbol_key(1, "Symbol.iterator"),
        async_iterator: symbol_key(2, "Symbol.asyncIterator"),
        has_instance: symbol_key(3, "Symbol.hasInstance"),
        to_primitive: symbol_key(4, "Symbol.toPrimitive"),
        to_string_tag: symbol_key(5, "Symbol.toStringTag"),
        species: symbol_key(6, "Symbol.species"),
        unscopables: symbol_key(7, "Symbol.unscopables"),
    };
    let global = Obj::new(Kind::Ordinary, Some(object.clone()), "Window");
    let mut realm = Realm::assemble(global.clone(), protos, symbols);
    // Track the objects made before the realm existed.
    let all = [
        &realm.protos.object,
        &realm.protos.function,
        &realm.protos.array,
        &realm.protos.string,
        &realm.protos.number,
        &realm.protos.boolean,
        &realm.protos.symbol,
        &realm.protos.error,
        &realm.protos.type_error,
        &realm.protos.range_error,
        &realm.protos.syntax_error,
        &realm.protos.reference_error,
        &realm.protos.eval_error,
        &realm.protos.uri_error,
        &realm.protos.date,
        &realm.protos.regexp,
        &realm.protos.map,
        &realm.protos.set,
        &realm.protos.weak_map,
        &realm.protos.weak_set,
        &realm.protos.promise,
        &realm.protos.iterator,
        &realm.protos.generator,
        &realm.protos.event_target,
        &realm.protos.node,
        &realm.protos.element,
        &realm.protos.document,
        &realm.protos.text,
        &realm.protos.event,
        &realm.protos.dom_token_list,
        &realm.protos.style,
        &realm.protos.node_list,
    ]
    .map(|o| o.clone());
    for object in all.iter() {
        realm.track(object);
    }
    realm.track(&global);
    realm
}

/// Makes a constructor function, links it with its prototype, and puts
/// it on the global object.
pub fn constructor(ctx: &mut Ctx, name: &str, length: usize, function: NativeFn, prototype: &Obj) -> Obj {
    let constructor = ctx.native(name, length, function);
    constructor.put_key(JsStr::from("prototype"), Value::Obj(prototype.clone()), 0);
    prototype.put_hidden("constructor", Value::Obj(constructor.clone()));
    ctx.realm.global.put_hidden(name, Value::Obj(constructor.clone()));
    constructor
}

/// Installs the whole library on the global object.
pub fn install(ctx: &mut Ctx) {
    let global = ctx.realm.global.clone();
    global.put_hidden("globalThis", Value::Obj(global.clone()));
    object::install(ctx);
    function::install(ctx);
    array::install(ctx);
    string::install(ctx);
    number::install(ctx);
    install_errors(ctx);
    json::install(ctx);
    date::install(ctx);
    regexp::install(ctx);
    collections::install(ctx);
    promise::install(ctx);
    global::install(ctx);
    // `Symbol.toStringTag` of the global: "[object Window]".
    let tag = ctx.realm.symbols.to_string_tag.clone();
    global.put_key(tag, Value::str("Window"), CONFIGURABLE);
}

fn error_constructor(ctx: &mut Ctx, _this: &Value, args: &[Value], data: &Value) -> R<Value> {
    let Value::Obj(default_proto) = data else {
        return Ok(Value::Undefined);
    };
    let proto = ctx.proto_for_new(default_proto.clone())?;
    let error = ctx.alloc(Kind::Error, Some(proto), "Error");
    let name = super::ops::get(ctx, &Value::Obj(error.clone()), "name")?;
    let name = super::ops::to_display(&name);
    let message = arg(args, 0);
    let text = if message.is_undefined() { alloc::string::String::new() } else { super::ops::to_string(ctx, &message)?.as_str().into() };
    if !message.is_undefined() {
        error.put_hidden("message", Value::str(&text));
    }
    if let Value::Obj(options) = arg(args, 1) {
        if super::ops::has_own(&options, "cause") {
            let cause = super::ops::get(ctx, &Value::Obj(options), "cause")?;
            error.put_hidden("cause", cause);
        }
    }
    error.put_hidden("stack", Value::Str(JsStr::from(alloc::format!("{}: {}\n    at <script>", name, text))));
    Ok(Value::Obj(error))
}

fn error_to_string(ctx: &mut Ctx, this: &Value, _args: &[Value], _data: &Value) -> R<Value> {
    if !matches!(this, Value::Obj(_)) {
        return ctx.throw_type("Error.prototype.toString called on non-object");
    }
    let name = super::ops::get(ctx, this, "name")?;
    let name = if name.is_undefined() { JsStr::from("Error") } else { super::ops::to_string(ctx, &name)? };
    let message = super::ops::get(ctx, this, "message")?;
    let message = if message.is_undefined() { JsStr::from("") } else { super::ops::to_string(ctx, &message)? };
    Ok(Value::Str(if name.is_empty() {
        message
    } else if message.is_empty() {
        name
    } else {
        JsStr::from(alloc::format!("{}: {}", name.as_str(), message.as_str()))
    }))
}

fn capture_stack_trace(_: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    if let Value::Obj(target) = arg(args, 0) {
        target.put_hidden("stack", Value::str("Error\n    at <script>"));
    }
    Ok(Value::Undefined)
}

fn install_errors(ctx: &mut Ctx) {
    let protos = [
        ("Error", ctx.realm.protos.error.clone()),
        ("TypeError", ctx.realm.protos.type_error.clone()),
        ("RangeError", ctx.realm.protos.range_error.clone()),
        ("SyntaxError", ctx.realm.protos.syntax_error.clone()),
        ("ReferenceError", ctx.realm.protos.reference_error.clone()),
        ("EvalError", ctx.realm.protos.eval_error.clone()),
        ("URIError", ctx.realm.protos.uri_error.clone()),
    ];
    let mut base: Option<Obj> = None;
    for (name, proto) in protos {
        let constructor = ctx.native_with(name, 1, error_constructor, Value::Obj(proto.clone()));
        constructor.put_key(JsStr::from("prototype"), Value::Obj(proto.clone()), 0);
        proto.put_hidden("constructor", Value::Obj(constructor.clone()));
        proto.put_hidden("name", Value::str(name));
        proto.put_hidden("message", Value::str(""));
        ctx.realm.global.put_hidden(name, Value::Obj(constructor.clone()));
        match &base {
            None => {
                ctx.method(&proto, "toString", 0, error_to_string);
                ctx.method(&constructor, "captureStackTrace", 1, capture_stack_trace);
                constructor.put("stackTraceLimit", Value::num(10));
                base = Some(constructor);
            }
            Some(base) => constructor.borrow_mut().proto = Some(base.clone()),
        }
    }
    // AggregateError, for Promise.any.
    let proto = ctx.alloc(Kind::Ordinary, Some(ctx.realm.protos.error.clone()), "AggregateError");
    proto.put_hidden("name", Value::str("AggregateError"));
    let constructor = ctx.native_with("AggregateError", 2, error_constructor, Value::Obj(proto.clone()));
    constructor.put_key(JsStr::from("prototype"), Value::Obj(proto.clone()), 0);
    proto.put_hidden("constructor", Value::Obj(constructor.clone()));
    ctx.realm.global.put_hidden("AggregateError", Value::Obj(constructor));
}
