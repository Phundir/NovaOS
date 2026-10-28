//! `Number`, `Boolean`, and the numeric functions of the global object
//! (`parseInt`, `parseFloat`, `isNaN`, `isFinite`).
//!
//! Every conversion goes through `num::Num`, whose decimal routines are
//! exact: `(0.1).toFixed(20)` prints the digits of the double, not of a
//! float approximation made on the way.

use alloc::string::String;

use super::arg;
use crate::web::js::interp::{Ctx, R};
use crate::web::js::num::Num;
use crate::web::js::ops;
use crate::web::js::value::*;

/// `this` as a number: a primitive or a `Number` wrapper.
fn this_number(ctx: &mut Ctx, this: &Value, method: &str) -> R<Num> {
    match this {
        Value::Num(n) => Ok(*n),
        Value::Obj(object) => {
            if let Kind::Number(n) = object.borrow().kind {
                return Ok(n);
            }
            let message = alloc::format!("Number.prototype.{} requires that 'this' be a Number", method);
            ctx.throw_type(&message)
        }
        _ => {
            let message = alloc::format!("Number.prototype.{} requires that 'this' be a Number", method);
            ctx.throw_type(&message)
        }
    }
}

fn number_constructor(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let n = match args.first() {
        None => Num::ZERO,
        Some(value) => ops::to_numeric(ctx, value)?,
    };
    if ctx.new_target.is_some() {
        let proto = ctx.proto_for_new(ctx.realm.protos.number.clone())?;
        return Ok(Value::Obj(ctx.alloc(Kind::Number(n), Some(proto), "Number")));
    }
    Ok(Value::Num(n))
}

fn number_of(value: &Value) -> Option<Num> {
    match value {
        Value::Num(n) => Some(*n),
        _ => None,
    }
}

fn is_finite_static(_: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    Ok(Value::Bool(number_of(&arg(args, 0)).is_some_and(Num::is_finite)))
}

fn is_nan_static(_: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    Ok(Value::Bool(number_of(&arg(args, 0)).is_some_and(Num::is_nan)))
}

fn is_integer_static(_: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    Ok(Value::Bool(number_of(&arg(args, 0)).is_some_and(|n| n.is_finite() && n.is_integer())))
}

fn is_safe_integer_static(_: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    Ok(Value::Bool(
        number_of(&arg(args, 0)).is_some_and(|n| n.is_finite() && n.is_integer() && n.abs().partial_cmp(Num::MAX_SAFE) != Some(core::cmp::Ordering::Greater)),
    ))
}

/// `parseFloat`: the longest decimal prefix after the white space.
fn parse_float(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let text = ops::to_string(ctx, &arg(args, 0))?;
    let trimmed = crate::web::js::num::trim_js(&text);
    // Numeric separators belong to source text, not to parseFloat.
    let end = trimmed.find('_').unwrap_or(trimmed.len());
    Ok(Value::Num(match Num::parse_prefix(&trimmed[..end]) {
        Some((value, used)) if used > 0 => value,
        _ => Num::NAN,
    }))
}

/// `parseInt(text, radix)`.
fn parse_int(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let text = ops::to_string(ctx, &arg(args, 0))?;
    let radix = ops::to_number(ctx, &arg(args, 1))?.to_i32();
    if radix != 0 && !(2..=36).contains(&radix) {
        return Ok(Value::Num(Num::NAN));
    }
    Ok(Value::Num(Num::parse_int(&text, radix as u32)))
}

fn global_is_nan(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    Ok(Value::Bool(ops::to_number(ctx, &arg(args, 0))?.is_nan()))
}

fn global_is_finite(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    Ok(Value::Bool(ops::to_number(ctx, &arg(args, 0))?.is_finite()))
}

fn to_string(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let n = this_number(ctx, this, "toString")?;
    let radix = match arg(args, 0) {
        Value::Undefined => 10,
        other => ops::to_integer(ctx, &other)?,
    };
    if !(2..=36).contains(&radix) {
        return ctx.throw_range("toString() radix must be between 2 and 36");
    }
    if radix == 10 {
        return Ok(Value::Str(ops::number_string(n)));
    }
    Ok(Value::Str(JsStr::from(n.to_radix(radix as u32))))
}

/// A digit-count argument, checked against `[low, 100]`.
fn digits_argument(ctx: &mut Ctx, value: &Value, low: i64, method: &str) -> R<usize> {
    let count = ops::to_integer(ctx, value)?;
    if !(low..=100).contains(&count) {
        let message = alloc::format!("{}() digits argument must be between {} and 100", method, low);
        return ctx.throw_range(&message);
    }
    Ok(count as usize)
}

fn to_fixed(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let n = this_number(ctx, this, "toFixed")?;
    let fraction = digits_argument(ctx, &arg(args, 0), 0, "toFixed")?;
    Ok(Value::Str(JsStr::from(n.to_fixed(fraction))))
}

fn to_exponential(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let n = this_number(ctx, this, "toExponential")?;
    let fraction = match arg(args, 0) {
        Value::Undefined => None,
        other => {
            let count = ops::to_integer(ctx, &other)?;
            if !n.is_finite() {
                return Ok(Value::Str(ops::number_string(n)));
            }
            if !(0..=100).contains(&count) {
                return ctx.throw_range("toExponential() argument must be between 0 and 100");
            }
            Some(count as usize)
        }
    };
    Ok(Value::Str(JsStr::from(n.to_exponential(fraction))))
}

fn to_precision(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let n = this_number(ctx, this, "toPrecision")?;
    let precision = arg(args, 0);
    if precision.is_undefined() || !n.is_finite() {
        return Ok(Value::Str(ops::number_string(n)));
    }
    let precision = digits_argument(ctx, &precision, 1, "toPrecision")?;
    Ok(Value::Str(JsStr::from(n.to_precision(precision))))
}

fn value_of(ctx: &mut Ctx, this: &Value, _: &[Value], _: &Value) -> R<Value> {
    Ok(Value::Num(this_number(ctx, this, "valueOf")?))
}

/// Groups the integer digits of a plain decimal text by three.
pub fn group_thousands(text: &str, separator: char) -> String {
    let (sign, rest) = match text.strip_prefix('-') {
        Some(rest) => ("-", rest),
        None => ("", text),
    };
    let (integer, fraction) = match rest.find('.') {
        Some(point) => (&rest[..point], &rest[point..]),
        None => (rest, ""),
    };
    let mut out = String::from(sign);
    for (index, digit) in integer.chars().enumerate() {
        if index > 0 && (integer.len() - index) % 3 == 0 {
            out.push(separator);
        }
        out.push(digit);
    }
    out.push_str(fraction);
    out
}

/// `toLocaleString`: the default of `Intl.NumberFormat` in English --
/// thousands grouped, at most three fraction digits.
fn to_locale_string(ctx: &mut Ctx, this: &Value, _: &[Value], _: &Value) -> R<Value> {
    let n = this_number(ctx, this, "toLocaleString")?;
    if !n.is_finite() {
        return Ok(Value::Str(JsStr::from(if n.is_nan() { "NaN" } else if n.is_negative() { "-\u{221e}" } else { "\u{221e}" })));
    }
    let fixed = n.to_fixed(3);
    let trimmed = if fixed.contains('.') { fixed.trim_end_matches('0').trim_end_matches('.') } else { &fixed };
    let trimmed = if trimmed == "-0" { "0" } else { trimmed };
    Ok(Value::Str(JsStr::from(group_thousands(trimmed, ','))))
}

// ------------------------------------------------------------
//  Boolean
// ------------------------------------------------------------

fn this_boolean(ctx: &mut Ctx, this: &Value) -> R<bool> {
    match this {
        Value::Bool(b) => Ok(*b),
        Value::Obj(object) => {
            if let Kind::Boolean(b) = object.borrow().kind {
                return Ok(b);
            }
            ctx.throw_type("Boolean.prototype method requires that 'this' be a Boolean")
        }
        _ => ctx.throw_type("Boolean.prototype method requires that 'this' be a Boolean"),
    }
}

fn boolean_constructor(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let b = arg(args, 0).truthy();
    if ctx.new_target.is_some() {
        let proto = ctx.proto_for_new(ctx.realm.protos.boolean.clone())?;
        return Ok(Value::Obj(ctx.alloc(Kind::Boolean(b), Some(proto), "Boolean")));
    }
    Ok(Value::Bool(b))
}

fn boolean_to_string(ctx: &mut Ctx, this: &Value, _: &[Value], _: &Value) -> R<Value> {
    Ok(Value::str(if this_boolean(ctx, this)? { "true" } else { "false" }))
}

fn boolean_value_of(ctx: &mut Ctx, this: &Value, _: &[Value], _: &Value) -> R<Value> {
    Ok(Value::Bool(this_boolean(ctx, this)?))
}

pub fn install(ctx: &mut Ctx) {
    let proto = ctx.realm.protos.number.clone();
    let constructor = super::constructor(ctx, "Number", 1, number_constructor, &proto);
    let constants = [
        ("EPSILON", Num::EPSILON),
        ("MAX_SAFE_INTEGER", Num::MAX_SAFE),
        ("MIN_SAFE_INTEGER", Num::MAX_SAFE.neg()),
        ("MAX_VALUE", Num::MAX),
        ("MIN_VALUE", Num::MIN_POSITIVE),
        ("NaN", Num::NAN),
        ("NEGATIVE_INFINITY", Num::NEG_INFINITY),
        ("POSITIVE_INFINITY", Num::INFINITY),
    ];
    for (name, value) in constants {
        constructor.put_key(JsStr::from(name), Value::Num(value), 0);
    }
    ctx.method(&constructor, "isFinite", 1, is_finite_static);
    ctx.method(&constructor, "isNaN", 1, is_nan_static);
    ctx.method(&constructor, "isInteger", 1, is_integer_static);
    ctx.method(&constructor, "isSafeInteger", 1, is_safe_integer_static);
    // `Number.parseInt === parseInt`: one function object, two names.
    let parse_int_function = ctx.native("parseInt", 2, parse_int);
    let parse_float_function = ctx.native("parseFloat", 1, parse_float);
    constructor.put_hidden("parseInt", Value::Obj(parse_int_function.clone()));
    constructor.put_hidden("parseFloat", Value::Obj(parse_float_function.clone()));
    let global = ctx.realm.global.clone();
    global.put_hidden("parseInt", Value::Obj(parse_int_function));
    global.put_hidden("parseFloat", Value::Obj(parse_float_function));
    ctx.method(&global, "isNaN", 1, global_is_nan);
    ctx.method(&global, "isFinite", 1, global_is_finite);
    global.put_key(JsStr::from("NaN"), Value::Num(Num::NAN), 0);
    global.put_key(JsStr::from("Infinity"), Value::Num(Num::INFINITY), 0);
    global.put_key(JsStr::from("undefined"), Value::Undefined, 0);

    let methods: &[(&str, usize, NativeFn)] = &[
        ("toString", 1, to_string),
        ("toFixed", 1, to_fixed),
        ("toExponential", 1, to_exponential),
        ("toPrecision", 1, to_precision),
        ("valueOf", 0, value_of),
        ("toLocaleString", 0, to_locale_string),
    ];
    for (name, length, function) in methods {
        ctx.method(&proto, name, *length, *function);
    }

    let proto = ctx.realm.protos.boolean.clone();
    super::constructor(ctx, "Boolean", 1, boolean_constructor, &proto);
    ctx.method(&proto, "toString", 0, boolean_to_string);
    ctx.method(&proto, "valueOf", 0, boolean_value_of);
}
