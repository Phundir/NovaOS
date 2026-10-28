//! `Function`: `call`, `apply`, `bind`, `toString`, and the constructor
//! that compiles source text.

use alloc::string::String;
use alloc::vec::Vec;

use super::arg;
use crate::web::js::interp::{Ctx, R};
use crate::web::js::ops;
use crate::web::js::value::*;

/// `new Function("a", "b", "return a + b")`: the text is compiled as a
/// function expression in the global scope.
fn function_constructor(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let mut params: Vec<String> = Vec::new();
    for value in args.iter().take(args.len().saturating_sub(1)) {
        params.push(String::from(ops::to_string(ctx, value)?.as_str()));
    }
    let body = match args.last() {
        Some(value) => String::from(ops::to_string(ctx, value)?.as_str()),
        None => String::new(),
    };
    let source = alloc::format!("(function anonymous({}\n) {{\n{}\n}})", params.join(","), body);
    ctx.eval_global(&source)
}

fn call(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    ops::require_callable(ctx, this, "Function.prototype.call target")?;
    let rest = if args.is_empty() { &[][..] } else { &args[1..] };
    ctx.call(this, arg(args, 0), rest)
}

fn apply(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    ops::require_callable(ctx, this, "Function.prototype.apply target")?;
    let list = ops::list_from_array_like(ctx, &arg(args, 1))?;
    ctx.call(this, arg(args, 0), &list)
}

fn bind(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let target = ops::require_callable(ctx, this, "Bind target")?;
    let bound_this = arg(args, 0);
    let bound_args: Vec<Value> = args.iter().skip(1).cloned().collect();
    let name = ops::get(ctx, this, "name")?;
    let name = alloc::format!("bound {}", ops::to_display(&name));
    let length = ops::get(ctx, this, "length")?;
    let length = match length {
        Value::Num(n) => (n.to_i64_saturating() - bound_args.len() as i64).max(0),
        _ => 0,
    };
    let proto = target.proto();
    let bound = ctx.alloc(Kind::Bound(target, bound_this, bound_args), proto, "Function");
    bound.borrow_mut().props.set(JsStr::from("name"), Slot::Data(Value::Str(JsStr::from(name)), CONFIGURABLE));
    bound.borrow_mut().props.set(JsStr::from("length"), Slot::Data(Value::num(length), CONFIGURABLE));
    Ok(Value::Obj(bound))
}

fn to_string(ctx: &mut Ctx, this: &Value, _: &[Value], _: &Value) -> R<Value> {
    let Value::Obj(object) = this else {
        return ctx.throw_type("Function.prototype.toString requires that 'this' be a Function");
    };
    let text = match &object.borrow().kind {
        Kind::Function(closure) => {
            let (start, end) = closure.func.source;
            // The source the function came from: the last script whose
            // span covers it is not tracked, so search them all for one
            // long enough.
            let found = ctx
                .realm
                .sources
                .iter()
                .rev()
                .find(|s| end <= s.len() && start < end && s.is_char_boundary(start) && s.is_char_boundary(end))
                .map(|s| String::from(&s[start..end]));
            found.unwrap_or_else(|| alloc::format!("function {}() {{ [code] }}", closure.func.name.as_deref().unwrap_or("")))
        }
        Kind::Native(_, _, name) => alloc::format!("function {}() {{ [native code] }}", name.as_str()),
        Kind::Bound(..) => String::from("function () { [native code] }"),
        _ => return ctx.throw_type("Function.prototype.toString requires that 'this' be a Function"),
    };
    Ok(Value::Str(JsStr::from(text)))
}

fn has_instance(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    // The default: ordinary instanceof, without recursion into this
    // method.
    let Value::Obj(object) = arg(args, 0) else {
        return Ok(Value::Bool(false));
    };
    let prototype = ops::get(ctx, this, "prototype")?;
    let Value::Obj(prototype) = prototype else {
        return Ok(Value::Bool(false));
    };
    let mut current = object.proto();
    while let Some(proto) = current {
        if proto.ptr_eq(&prototype) {
            return Ok(Value::Bool(true));
        }
        current = proto.proto();
    }
    Ok(Value::Bool(false))
}

pub fn install(ctx: &mut Ctx) {
    let proto = ctx.realm.protos.function.clone();
    super::constructor(ctx, "Function", 1, function_constructor, &proto);
    proto.put_key(JsStr::from("name"), Value::str(""), CONFIGURABLE);
    proto.put_key(JsStr::from("length"), Value::num(0), CONFIGURABLE);
    ctx.method(&proto, "call", 1, call);
    ctx.method(&proto, "apply", 2, apply);
    ctx.method(&proto, "bind", 1, bind);
    ctx.method(&proto, "toString", 0, to_string);
    let symbol = ctx.realm.symbols.has_instance.clone();
    let method = ctx.native("[Symbol.hasInstance]", 1, has_instance);
    proto.put_key(symbol, Value::Obj(method), 0);
}
