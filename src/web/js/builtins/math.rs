//! `Math`.
//!
//! The elementary functions are those of `num::Num`, computed with
//! integers (the kernel has no FPU state for scripts). Where a result is
//! known exactly -- `Math.log10(1000)`, `Math.log2(8)` -- it is returned
//! exactly, not as the quotient of two rounded logarithms.

use super::arg;
use crate::web::js::interp::{Ctx, R};
use crate::web::js::num::Num;
use crate::web::js::ops;
use crate::web::js::value::*;

fn number(ctx: &mut Ctx, args: &[Value], index: usize) -> R<Num> {
    ops::to_number(ctx, &arg(args, index))
}

macro_rules! unary {
    ($name:ident, $body:expr) => {
        fn $name(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
            let x = number(ctx, args, 0)?;
            let f: fn(Num) -> Num = $body;
            Ok(Value::Num(f(x)))
        }
    };
}

unary!(abs, |x| x.abs());
unary!(acos, |x| x.acos());
unary!(asin, |x| x.asin());
unary!(atan, |x| x.atan());
unary!(cbrt, |x| x.cbrt());
unary!(ceil, |x| x.ceil());
unary!(cos, |x| x.cos());
unary!(exp, |x| x.exp());
unary!(floor, |x| x.floor());
unary!(log, |x| x.ln());
unary!(round, |x| x.round());
unary!(sin, |x| x.sin());
unary!(sqrt, |x| x.sqrt());
unary!(tan, |x| x.tan());
unary!(trunc, |x| x.trunc());
unary!(sign, sign_of);
unary!(sinh, sinh_of);
unary!(cosh, cosh_of);
unary!(tanh, tanh_of);
unary!(asinh, asinh_of);
unary!(acosh, acosh_of);
unary!(atanh, atanh_of);
unary!(expm1, expm1_of);
unary!(log1p, log1p_of);
unary!(log10, log10_of);
unary!(log2, log2_of);
unary!(fround, fround_of);

fn sign_of(x: Num) -> Num {
    if x.is_nan() || x.is_zero() {
        x
    } else if x.is_sign_negative() {
        Num::ONE.neg()
    } else {
        Num::ONE
    }
}

fn sinh_of(x: Num) -> Num {
    if !x.is_finite() || x.is_zero() {
        return x;
    }
    if x.abs().lt(Num::from_i64(1)) {
        // e^x - e^-x loses its digits near zero: the series instead.
        let x2 = x.mul(x);
        let mut term = x;
        let mut sum = x;
        for k in 1..20 {
            term = term.mul(x2).div(Num::from_i64((2 * k) * (2 * k + 1)));
            let next = sum.add(term);
            if next.0 == sum.0 {
                break;
            }
            sum = next;
        }
        return sum;
    }
    x.exp().sub(x.neg().exp()).div(Num::TWO)
}

fn cosh_of(x: Num) -> Num {
    if x.is_nan() {
        return x;
    }
    if x.is_infinite() {
        return Num::INFINITY;
    }
    x.exp().add(x.neg().exp()).div(Num::TWO)
}

fn tanh_of(x: Num) -> Num {
    if x.is_nan() || x.is_zero() {
        return x;
    }
    if x.abs().partial_cmp(Num::from_i64(20)) == Some(core::cmp::Ordering::Greater) {
        return if x.is_sign_negative() { Num::ONE.neg() } else { Num::ONE };
    }
    let e2 = expm1_of(x.mul(Num::TWO));
    e2.div(e2.add(Num::TWO))
}

fn asinh_of(x: Num) -> Num {
    if !x.is_finite() || x.is_zero() {
        return x;
    }
    let a = x.abs();
    let result = log1p_of(a.add(a.mul(a).div(Num::ONE.add(a.mul(a).add(Num::ONE).sqrt()))));
    if x.is_sign_negative() {
        result.neg()
    } else {
        result
    }
}

fn acosh_of(x: Num) -> Num {
    if x.is_nan() || x.lt(Num::ONE) {
        return Num::NAN;
    }
    if x.is_infinite() {
        return x;
    }
    x.add(x.mul(x).sub(Num::ONE).sqrt()).ln()
}

fn atanh_of(x: Num) -> Num {
    if x.is_nan() || x.is_zero() {
        return x;
    }
    let a = x.abs();
    match a.partial_cmp(Num::ONE) {
        Some(core::cmp::Ordering::Greater) => return Num::NAN,
        Some(core::cmp::Ordering::Equal) => return if x.is_sign_negative() { Num::NEG_INFINITY } else { Num::INFINITY },
        _ => {}
    }
    let result = log1p_of(Num::TWO.mul(a).div(Num::ONE.sub(a))).div(Num::TWO);
    if x.is_sign_negative() {
        result.neg()
    } else {
        result
    }
}

/// e^x - 1, accurate near zero.
fn expm1_of(x: Num) -> Num {
    if x.is_nan() || x.is_zero() {
        return x;
    }
    if x.abs().lt(Num::HALF) {
        let mut term = x;
        let mut sum = x;
        for k in 2..30 {
            term = term.mul(x).div(Num::from_i64(k));
            let next = sum.add(term);
            if next.0 == sum.0 {
                break;
            }
            sum = next;
        }
        return sum;
    }
    x.exp().sub(Num::ONE)
}

/// ln(1 + x), accurate near zero.
fn log1p_of(x: Num) -> Num {
    if x.is_nan() || x.is_zero() {
        return x;
    }
    if x.abs().lt(Num::from_decimal(b"1", -4)) {
        // x - x^2/2 + x^3/3 - ...
        let mut power = x;
        let mut sum = x;
        for k in 2..12 {
            power = power.mul(x).neg();
            let next = sum.add(power.div(Num::from_i64(k)));
            if next.0 == sum.0 {
                break;
            }
            sum = next;
        }
        return sum;
    }
    Num::ONE.add(x).ln()
}

fn log10_of(x: Num) -> Num {
    exact_log(x, 10).unwrap_or_else(|| x.ln().div(Num::LN10))
}

fn log2_of(x: Num) -> Num {
    exact_log(x, 2).unwrap_or_else(|| x.ln().div(Num::LN2))
}

/// The logarithm when `x` is an exact power of `base`.
fn exact_log(x: Num, base: i64) -> Option<Num> {
    if !x.is_finite() || x.is_zero() || x.is_sign_negative() {
        return None;
    }
    let base_num = Num::from_i64(base);
    let mut power = Num::ONE;
    if x.partial_cmp(Num::ONE) != Some(core::cmp::Ordering::Less) {
        for k in 0..1100 {
            if power.0 == x.0 {
                return Some(Num::from_i64(k));
            }
            if power.partial_cmp(x) == Some(core::cmp::Ordering::Greater) || power.is_infinite() {
                return None;
            }
            power = power.mul(base_num);
        }
    } else {
        // 1 / base^k, the denominator exact while it is representable:
        // one correctly rounded division gives what the literal reads.
        let mut denominator = Num::ONE;
        for k in 0..1100 {
            if power.0 == x.0 {
                return Some(Num::from_i64(-k));
            }
            if power.lt(x) || power.is_zero() {
                return None;
            }
            denominator = denominator.mul(base_num);
            power = Num::ONE.div(denominator);
        }
    }
    None
}

/// Rounds a double to the nearest single-precision value, ties to even,
/// subnormals and overflow included -- on the bits.
fn fround_of(x: Num) -> Num {
    if !x.is_finite() || x.is_zero() {
        return x;
    }
    let bits = x.0;
    let sign = bits & (1 << 63);
    let exponent = ((bits >> 52) & 0x7ff) as i64 - 1023;
    // Doubles below half the smallest float subnormal round to zero.
    if exponent < -150 {
        return Num(sign);
    }
    let mantissa = (bits & ((1 << 52) - 1)) | (1 << 52);
    // A float keeps 24 significant bits, fewer once subnormal.
    let keep = if exponent < -126 { (24 - (-126 - exponent)).max(0) } else { 24 };
    let drop = 53 - keep;
    let half = 1u64 << (drop - 1);
    let rest = mantissa & ((1u64 << drop) - 1);
    let mut kept = mantissa >> drop;
    if rest > half || (rest == half && kept & 1 == 1) {
        kept += 1;
    }
    let mut exponent = exponent;
    let mut shifted = kept << drop;
    if shifted >> 53 != 0 {
        shifted >>= 1;
        exponent += 1;
    }
    if exponent > 127 {
        return Num(sign | Num::INFINITY.0);
    }
    if shifted == 0 {
        return Num(sign);
    }
    // Renormalize (a subnormal float may have lost its leading bit).
    let mut exponent = exponent;
    while shifted & (1 << 52) == 0 {
        shifted <<= 1;
        exponent -= 1;
    }
    Num(sign | (((exponent + 1023) as u64) << 52) | (shifted & ((1 << 52) - 1)))
}

fn atan2(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let y = number(ctx, args, 0)?;
    let x = number(ctx, args, 1)?;
    Ok(Value::Num(y.atan2(x)))
}

fn pow(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let base = number(ctx, args, 0)?;
    let exponent = number(ctx, args, 1)?;
    Ok(Value::Num(base.pow(exponent)))
}

fn max(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let mut result = Num::NEG_INFINITY;
    let mut nan = false;
    for value in args {
        let n = ops::to_number(ctx, value)?;
        if n.is_nan() {
            nan = true;
        } else if n.partial_cmp(result) == Some(core::cmp::Ordering::Greater)
            || (n.is_zero() && result.is_zero() && !n.is_sign_negative())
        {
            result = n;
        }
    }
    Ok(Value::Num(if nan { Num::NAN } else { result }))
}

fn min(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let mut result = Num::INFINITY;
    let mut nan = false;
    for value in args {
        let n = ops::to_number(ctx, value)?;
        if n.is_nan() {
            nan = true;
        } else if n.lt(result) || (n.is_zero() && result.is_zero() && n.is_sign_negative()) {
            result = n;
        }
    }
    Ok(Value::Num(if nan { Num::NAN } else { result }))
}

fn hypot(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let mut values = alloc::vec::Vec::new();
    for value in args {
        values.push(ops::to_number(ctx, value)?);
    }
    if values.iter().any(|n| n.is_infinite()) {
        return Ok(Value::Num(Num::INFINITY));
    }
    if values.iter().any(|n| n.is_nan()) {
        return Ok(Value::Num(Num::NAN));
    }
    // Scaled by the largest, so that squares neither overflow nor vanish.
    let largest = values.iter().fold(Num::ZERO, |m, n| m.max(n.abs()));
    if largest.is_zero() {
        return Ok(Value::Num(Num::ZERO));
    }
    let sum = values.iter().fold(Num::ZERO, |s, n| {
        let r = n.abs().div(largest);
        s.add(r.mul(r))
    });
    Ok(Value::Num(sum.sqrt().mul(largest)))
}

fn imul(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let a = number(ctx, args, 0)?.to_u32();
    let b = number(ctx, args, 1)?.to_u32();
    Ok(Value::Num(Num::from_i32(a.wrapping_mul(b) as i32)))
}

fn clz32(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let a = number(ctx, args, 0)?.to_u32();
    Ok(Value::num(a.leading_zeros() as i64))
}

/// A double in [0, 1) from 53 random bits.
fn random(ctx: &mut Ctx, _: &Value, _: &[Value], _: &Value) -> R<Value> {
    let bits = ctx.host.random() >> 11;
    Ok(Value::Num(Num::from_u64(bits).scale2(-53)))
}

pub fn install(ctx: &mut Ctx) {
    let math = ctx.new_object();
    let constants = [
        ("E", Num::E),
        ("LN10", Num::LN10),
        ("LN2", Num::LN2),
        ("LOG10E", Num::LOG10E),
        ("LOG2E", Num::LOG2E),
        ("PI", Num::PI),
        ("SQRT1_2", Num::SQRT1_2),
        ("SQRT2", Num::SQRT2),
    ];
    for (name, value) in constants {
        math.put_key(JsStr::from(name), Value::Num(value), 0);
    }
    let functions: &[(&str, usize, NativeFn)] = &[
        ("abs", 1, abs),
        ("acos", 1, acos),
        ("acosh", 1, acosh),
        ("asin", 1, asin),
        ("asinh", 1, asinh),
        ("atan", 1, atan),
        ("atanh", 1, atanh),
        ("atan2", 2, atan2),
        ("cbrt", 1, cbrt),
        ("ceil", 1, ceil),
        ("clz32", 1, clz32),
        ("cos", 1, cos),
        ("cosh", 1, cosh),
        ("exp", 1, exp),
        ("expm1", 1, expm1),
        ("floor", 1, floor),
        ("fround", 1, fround),
        ("hypot", 2, hypot),
        ("imul", 2, imul),
        ("log", 1, log),
        ("log1p", 1, log1p),
        ("log10", 1, log10),
        ("log2", 1, log2),
        ("max", 2, max),
        ("min", 2, min),
        ("pow", 2, pow),
        ("random", 0, random),
        ("round", 1, round),
        ("sign", 1, sign),
        ("sin", 1, sin),
        ("sinh", 1, sinh),
        ("sqrt", 1, sqrt),
        ("tan", 1, tan),
        ("tanh", 1, tanh),
        ("trunc", 1, trunc),
    ];
    for (name, length, function) in functions {
        ctx.method(&math, name, *length, *function);
    }
    let tag = ctx.realm.symbols.to_string_tag.clone();
    math.put_key(tag, Value::str("Math"), CONFIGURABLE);
    ctx.realm.global.put_hidden("Math", Value::Obj(math));
}
