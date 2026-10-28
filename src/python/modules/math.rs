//! `math`: the functions of C's libm, on soft-float doubles.
//!
//! `exp`, `log`, `log2`, `log10`, the trigonometric functions come from
//! `Num`, computed in 116-bit fixed point and rounded once: correctly
//! rounded in all but vanishingly rare cases, as glibc's are. `fsum` is
//! exact (Shewchuk's partials, CPython's algorithm). Integer functions
//! (`factorial`, `comb`, `gcd`, `isqrt`) work on unbounded ints.

use alloc::rc::Rc;
use alloc::vec::Vec;

use super::super::args;
use super::super::int::Int;
use super::super::types::num::{float_of, float_to_int};
use super::super::value::*;
use super::super::vm::{PyResult, Vm};
use crate::web::js::num::Num;

fn arg(vm: &mut Vm, a: &[Value], i: usize, function: &str) -> PyResult<Num> {
    match a.get(i) {
        Some(v) => match v {
            Value::Float(f) => Ok(*f),
            Value::Int(_) | Value::Bool(_) | Value::Instance(_) => float_of(vm, v),
            other => {
                let name = vm.type_name(other);
                let message = alloc::format!("must be real number, not {}", name);
                vm.type_error(&message)
            }
        },
        None => {
            let message = alloc::format!("{}() takes exactly one argument (0 given)", function);
            vm.type_error(&message)
        }
    }
}

fn domain<T>(vm: &mut Vm) -> PyResult<T> {
    vm.value_error("math domain error")
}

fn range_error<T>(vm: &mut Vm) -> PyResult<T> {
    vm.overflow("math range error")
}

/// A result: NaN from a non-NaN input is a domain error, an infinity
/// from a finite input a range error, as CPython's `math_1`.
fn checked(vm: &mut Vm, input: Num, r: Num) -> PyResult<Value> {
    if r.is_nan() && !input.is_nan() {
        return domain(vm);
    }
    if r.is_infinite() && input.is_finite() {
        return range_error(vm);
    }
    Ok(Value::Float(r))
}

fn int_arg(vm: &mut Vm, a: &[Value], i: usize, function: &str) -> PyResult<Int> {
    match a.get(i) {
        Some(Value::Int(n)) => Ok(n.clone()),
        Some(Value::Bool(b)) => Ok(Int::Small(*b as i64)),
        Some(v @ Value::Instance(_)) => match vm.call_special(v, "__index__", &[])? {
            Some(Value::Int(n)) => Ok(n),
            _ => {
                let name = vm.type_name(v);
                let message = alloc::format!("'{}' object cannot be interpreted as an integer", name);
                vm.type_error(&message)
            }
        },
        Some(other) => {
            let name = vm.type_name(other);
            let message = alloc::format!("'{}' object cannot be interpreted as an integer", name);
            vm.type_error(&message)
        }
        None => {
            let message = alloc::format!("{}() missing required argument", function);
            vm.type_error(&message)
        }
    }
}

fn sqrt(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let x = arg(vm, a, 0, "sqrt")?;
    if x.is_negative() && !x.is_zero() {
        return domain(vm);
    }
    Ok(Value::Float(x.sqrt()))
}

fn exp(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let x = arg(vm, a, 0, "exp")?;
    checked(vm, x, x.exp())
}

fn exp2(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let x = arg(vm, a, 0, "exp2")?;
    checked(vm, x, Num::TWO.pow(x))
}

/// ln of any positive number, ints beyond the double range included.
fn ln_of(vm: &mut Vm, value: &Value) -> PyResult<Num> {
    if let Value::Int(i) = value {
        if i.is_negative() || i.is_zero() {
            return domain(vm);
        }
        let bits = i.bit_length();
        if bits > 1000 {
            // i = m * 2^shift with m a double: ln m + shift ln 2.
            let shift = bits - 60;
            let m = i.shr(shift as u64).to_float();
            return Ok(m.ln().add(Num::from_i64(shift as i64).mul(Num::LN2)));
        }
    }
    let x = match value {
        Value::Float(f) => *f,
        other => float_of(vm, other)?,
    };
    if x.is_nan() {
        return Ok(x);
    }
    if x.is_negative() || x.is_zero() {
        return domain(vm);
    }
    Ok(x.ln())
}

fn log(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let Some(x) = a.first() else {
        return vm.type_error("log expected at least 1 argument, got 0");
    };
    let numerator = ln_of(vm, x)?;
    match a.get(1) {
        None => Ok(Value::Float(numerator)),
        Some(base) => {
            let denominator = ln_of(vm, base)?;
            if denominator.is_zero() {
                return vm.zero_division("float division by zero");
            }
            Ok(Value::Float(numerator.div(denominator)))
        }
    }
}

fn log_exact(vm: &mut Vm, a: &[Value], ten: bool) -> PyResult<Value> {
    let Some(x) = a.first() else {
        return vm.type_error("expected 1 argument, got 0");
    };
    if let Value::Int(i) = x {
        if i.bit_length() > 1000 {
            let ln = ln_of(vm, x)?;
            return Ok(Value::Float(ln.div(if ten { Num::LN10 } else { Num::LN2 })));
        }
    }
    let v = match x {
        Value::Float(f) => *f,
        other => float_of(vm, other)?,
    };
    if v.is_nan() {
        return Ok(Value::Float(v));
    }
    if v.is_negative() || v.is_zero() {
        return domain(vm);
    }
    Ok(Value::Float(if ten { v.log10() } else { v.log2() }))
}

fn log2(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    log_exact(vm, a, false)
}

fn log10(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    log_exact(vm, a, true)
}

fn log1p(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let x = arg(vm, a, 0, "log1p")?;
    if x.partial_cmp(Num::ONE.neg()) != Some(core::cmp::Ordering::Greater) {
        return domain(vm);
    }
    // For small x, ln(1 + x) loses x's low bits: the series instead.
    if x.abs().lt(Num::ONE.scale2(-20)) {
        let x2 = x.mul(x);
        return Ok(Value::Float(x.sub(x2.div(Num::TWO)).add(x2.mul(x).div(Num::from_i64(3)))));
    }
    Ok(Value::Float(Num::ONE.add(x).ln()))
}

fn expm1(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let x = arg(vm, a, 0, "expm1")?;
    if x.abs().lt(Num::ONE.scale2(-20)) {
        let x2 = x.mul(x);
        return Ok(Value::Float(x.add(x2.div(Num::TWO)).add(x2.mul(x).div(Num::from_i64(6)))));
    }
    checked(vm, x, x.exp().sub(Num::ONE))
}

fn trig(vm: &mut Vm, a: &[Value], name: &str, f: fn(Num) -> Num) -> PyResult<Value> {
    let x = arg(vm, a, 0, name)?;
    if x.is_infinite() && matches!(name, "sin" | "cos" | "tan") {
        return domain(vm);
    }
    checked(vm, x, f(x))
}

fn sin(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    trig(vm, a, "sin", Num::sin)
}

fn cos(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    trig(vm, a, "cos", Num::cos)
}

fn tan(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    trig(vm, a, "tan", Num::tan)
}

fn asin(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    trig(vm, a, "asin", Num::asin)
}

fn acos(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    trig(vm, a, "acos", Num::acos)
}

fn atan(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    trig(vm, a, "atan", Num::atan)
}

fn atan2(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let y = arg(vm, a, 0, "atan2")?;
    let x = arg(vm, a, 1, "atan2")?;
    Ok(Value::Float(y.atan2(x)))
}

fn sinh(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let x = arg(vm, a, 0, "sinh")?;
    let e = x.exp();
    checked(vm, x, e.sub(Num::ONE.div(e)).div(Num::TWO))
}

fn cosh(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let x = arg(vm, a, 0, "cosh")?;
    let e = x.exp();
    checked(vm, x, e.add(Num::ONE.div(e)).div(Num::TWO))
}

fn tanh(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let x = arg(vm, a, 0, "tanh")?;
    if x.abs().partial_cmp(Num::from_i64(20)) == Some(core::cmp::Ordering::Greater) {
        return Ok(Value::Float(if x.is_negative() { Num::ONE.neg() } else { Num::ONE }));
    }
    let e2 = x.mul(Num::TWO).exp();
    Ok(Value::Float(e2.sub(Num::ONE).div(e2.add(Num::ONE))))
}

fn floor_or_ceil(vm: &mut Vm, a: &[Value], ceil: bool) -> PyResult<Value> {
    let name = if ceil { "ceil" } else { "floor" };
    let Some(x) = a.first() else {
        let message = alloc::format!("{}() takes exactly one argument (0 given)", name);
        return vm.type_error(&message);
    };
    match x {
        Value::Int(_) => Ok(x.clone()),
        Value::Bool(b) => Ok(Value::int(*b as i64)),
        Value::Float(f) => float_to_int(vm, if ceil { f.ceil() } else { f.floor() }),
        Value::Instance(_) => {
            let dunder = if ceil { "__ceil__" } else { "__floor__" };
            if let Some(r) = vm.call_special(x, dunder, &[])? {
                return Ok(r);
            }
            let f = float_of(vm, x)?;
            float_to_int(vm, if ceil { f.ceil() } else { f.floor() })
        }
        other => {
            let name = vm.type_name(other);
            let message = alloc::format!("must be real number, not {}", name);
            vm.type_error(&message)
        }
    }
}

fn floor(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    floor_or_ceil(vm, a, false)
}

fn ceil(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    floor_or_ceil(vm, a, true)
}

fn trunc(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let Some(x) = a.first() else {
        return vm.type_error("trunc() takes exactly one argument (0 given)");
    };
    match x {
        Value::Int(_) => Ok(x.clone()),
        Value::Bool(b) => Ok(Value::int(*b as i64)),
        Value::Float(f) => float_to_int(vm, f.trunc()),
        Value::Instance(_) => match vm.call_special(x, "__trunc__", &[])? {
            Some(r) => Ok(r),
            None => {
                let name = vm.type_name(x);
                let message = alloc::format!("type {} doesn't define __trunc__ method", name);
                vm.type_error(&message)
            }
        },
        other => {
            let name = vm.type_name(other);
            let message = alloc::format!("type {} doesn't define __trunc__ method", name);
            vm.type_error(&message)
        }
    }
}

fn fabs(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let x = arg(vm, a, 0, "fabs")?;
    Ok(Value::Float(x.abs()))
}

fn pow(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let x = arg(vm, a, 0, "pow")?;
    let y = arg(vm, a, 1, "pow")?;
    if x.is_zero() && y.is_negative() && y.is_finite() {
        return domain(vm);
    }
    if x.is_negative() && x.is_finite() && y.is_finite() && !y.is_integer() {
        return domain(vm);
    }
    let r = x.pow(y);
    if r.is_infinite() && x.is_finite() && y.is_finite() {
        return range_error(vm);
    }
    Ok(Value::Float(r))
}

fn hypot(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let mut values = Vec::with_capacity(a.len());
    for i in 0..a.len() {
        values.push(arg(vm, a, i, "hypot")?.abs());
    }
    Ok(Value::Float(norm(&values)))
}

/// sqrt of the sum of squares, scaled against overflow.
fn norm(values: &[Num]) -> Num {
    if values.iter().any(|v| v.is_infinite()) {
        return Num::INFINITY;
    }
    if values.iter().any(|v| v.is_nan()) {
        return Num::NAN;
    }
    let max = values.iter().fold(Num::ZERO, |m, v| if m.lt(*v) { *v } else { m });
    if max.is_zero() {
        return Num::ZERO;
    }
    if values.len() == 1 {
        return max;
    }
    let mut sum = Num::ZERO;
    for v in values {
        let q = v.div(max);
        sum = sum.add(q.mul(q));
    }
    max.mul(sum.sqrt())
}

fn dist(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let (Some(p), Some(q)) = (a.first(), a.get(1)) else {
        return vm.type_error("dist expected 2 arguments");
    };
    let p = vm.collect(p)?;
    let q = vm.collect(q)?;
    if p.len() != q.len() {
        return vm.value_error("both points must have the same number of dimensions");
    }
    let mut diffs = Vec::with_capacity(p.len());
    for (x, y) in p.iter().zip(q.iter()) {
        let x = float_of(vm, x)?;
        let y = float_of(vm, y)?;
        diffs.push(x.sub(y).abs());
    }
    Ok(Value::Float(norm(&diffs)))
}

fn degrees(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let x = arg(vm, a, 0, "degrees")?;
    Ok(Value::Float(x.mul(Num::from_i64(180).div(Num::PI))))
}

fn radians(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let x = arg(vm, a, 0, "radians")?;
    Ok(Value::Float(x.mul(Num::PI.div(Num::from_i64(180)))))
}

fn isfinite(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let x = arg(vm, a, 0, "isfinite")?;
    Ok(Value::Bool(x.is_finite()))
}

fn isinf(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let x = arg(vm, a, 0, "isinf")?;
    Ok(Value::Bool(x.is_infinite()))
}

fn isnan(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let x = arg(vm, a, 0, "isnan")?;
    Ok(Value::Bool(x.is_nan()))
}

fn isclose(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let x = arg(vm, a, 0, "isclose")?;
    let y = arg(vm, a, 1, "isclose")?;
    let rel = match args::keyword(kw, "rel_tol") {
        Some(v) => float_of(vm, v)?,
        None => Num::from_decimal(b"1", -9),
    };
    let abs_tol = match args::keyword(kw, "abs_tol") {
        Some(v) => float_of(vm, v)?,
        None => Num::ZERO,
    };
    if rel.is_negative() || abs_tol.is_negative() {
        return vm.value_error("tolerances must be non-negative");
    }
    if x.equals(y) {
        return Ok(Value::Bool(true));
    }
    if x.is_infinite() || y.is_infinite() {
        return Ok(Value::Bool(false));
    }
    let diff = x.sub(y).abs();
    let close = !rel.mul(y).abs().lt(diff) || !rel.mul(x).abs().lt(diff) || !abs_tol.lt(diff);
    Ok(Value::Bool(close && !diff.is_nan()))
}

fn copysign(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let x = arg(vm, a, 0, "copysign")?;
    let y = arg(vm, a, 1, "copysign")?;
    let magnitude = x.abs();
    Ok(Value::Float(if y.is_sign_negative() { magnitude.neg() } else { magnitude }))
}

fn fmod(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let x = arg(vm, a, 0, "fmod")?;
    let y = arg(vm, a, 1, "fmod")?;
    if y.is_zero() || x.is_infinite() {
        if x.is_nan() || y.is_nan() {
            return Ok(Value::Float(Num::NAN));
        }
        return domain(vm);
    }
    Ok(Value::Float(x.rem(y)))
}

fn modf(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let x = arg(vm, a, 0, "modf")?;
    if x.is_infinite() {
        let zero = if x.is_negative() { Num::NEG_ZERO } else { Num::ZERO };
        return Ok(Value::tuple(alloc::vec![Value::Float(zero), Value::Float(x)]));
    }
    let whole = x.trunc();
    let mut fraction = x.sub(whole);
    if fraction.is_zero() && x.is_sign_negative() {
        fraction = Num::NEG_ZERO;
    }
    Ok(Value::tuple(alloc::vec![Value::Float(fraction), Value::Float(whole)]))
}

fn frexp(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let x = arg(vm, a, 0, "frexp")?;
    if x.is_zero() || !x.is_finite() {
        return Ok(Value::tuple(alloc::vec![Value::Float(x), Value::int(0)]));
    }
    let (_, m, e) = x.decompose();
    // x = m * 2^e; mantissa in [0.5, 1): shift by m's bit length.
    let bits = 64 - m.leading_zeros() as i32;
    let exponent = e + bits;
    Ok(Value::tuple(alloc::vec![Value::Float(x.scale2(-exponent)), Value::int(exponent as i64)]))
}

fn ldexp(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let x = arg(vm, a, 0, "ldexp")?;
    let n = int_arg(vm, a, 1, "ldexp")?;
    let n = n.to_i64().unwrap_or(if n.is_negative() { -100_000 } else { 100_000 }).clamp(-100_000, 100_000);
    let r = x.scale2(n as i32);
    if r.is_infinite() && x.is_finite() {
        return range_error(vm);
    }
    Ok(Value::Float(r))
}

/// `fsum`: exact sum through non-overlapping partials, rounded once.
fn fsum(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let Some(iterable) = a.first() else {
        return vm.type_error("fsum() takes exactly one argument (0 given)");
    };
    let items = vm.collect(iterable)?;
    let mut partials: Vec<Num> = Vec::new();
    let mut special = Num::ZERO;
    let mut inf_sum = Num::ZERO;
    for item in &items {
        let mut x = float_of(vm, item)?;
        if !x.is_finite() {
            if x.is_infinite() {
                inf_sum = inf_sum.add(x);
            }
            special = special.add(x);
            continue;
        }
        let mut i = 0;
        for j in 0..partials.len() {
            let mut y = partials[j];
            if x.abs().lt(y.abs()) {
                core::mem::swap(&mut x, &mut y);
            }
            let hi = x.add(y);
            let lo = y.sub(hi.sub(x));
            if !lo.is_zero() {
                partials[i] = lo;
                i += 1;
            }
            x = hi;
        }
        partials.truncate(i);
        partials.push(x);
    }
    if !special.is_zero() {
        if inf_sum.is_nan() {
            return vm.value_error("-inf + inf in fsum");
        }
        return Ok(Value::Float(special));
    }
    let mut n = partials.len();
    let mut hi = Num::ZERO;
    if n > 0 {
        n -= 1;
        hi = partials[n];
        let mut lo = Num::ZERO;
        while n > 0 {
            let x = hi;
            n -= 1;
            let y = partials[n];
            hi = x.add(y);
            let yr = hi.sub(x);
            lo = y.sub(yr);
            if !lo.is_zero() {
                break;
            }
        }
        if n > 0 && ((lo.is_negative() && partials[n - 1].is_negative()) || (!lo.is_negative() && !lo.is_zero() && !partials[n - 1].is_negative() && !partials[n - 1].is_zero())) {
            let y = lo.mul(Num::TWO);
            let x = hi.add(y);
            let yr = x.sub(hi);
            if y.equals(yr) {
                hi = x;
            }
        }
    }
    Ok(Value::Float(hi))
}

fn prod(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let Some(iterable) = a.first() else {
        return vm.type_error("prod() missing required argument 'iterable' (pos 1)");
    };
    let mut result = args::keyword(kw, "start").cloned().unwrap_or(Value::int(1));
    let items = vm.collect(iterable)?;
    for item in items {
        result = vm.binary(super::super::ast::BinOp::Mul, &result, &item, false)?;
    }
    Ok(result)
}

// ------------------------------------------------------------
//  Integers
// ------------------------------------------------------------

fn product(vm: &mut Vm, from: i64, to: i64) -> PyResult<Int> {
    let mut acc = Int::Small(1);
    let mut n = from;
    while n <= to {
        acc = match acc.mul(&Int::Small(n)) {
            Ok(v) => v,
            Err(e) => {
                let class = vm.types.memory_error.clone();
                return Err(vm.error(&class, e));
            }
        };
        n += 1;
    }
    Ok(acc)
}

fn factorial(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    if let Some(Value::Float(_)) = a.first() {
        return vm.type_error("'float' object cannot be interpreted as an integer");
    }
    let n = int_arg(vm, a, 0, "factorial")?;
    if n.is_negative() {
        return vm.value_error("factorial() not defined for negative values");
    }
    let Some(n) = n.to_i64().filter(|n| *n <= 100_000) else {
        return vm.overflow("factorial() argument should not exceed 100000");
    };
    Ok(Value::Int(product(vm, 2, n)?))
}

fn gcd_of(x: &Int, y: &Int) -> Int {
    let (mut a, mut b) = (x.abs(), y.abs());
    while !b.is_zero() {
        let r = a.divmod(&b).map(|(_, r)| r).unwrap_or(Int::zero());
        a = core::mem::replace(&mut b, r);
    }
    a
}

fn gcd(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let mut g = Int::zero();
    for i in 0..a.len() {
        let n = int_arg(vm, a, i, "gcd")?;
        g = gcd_of(&g, &n);
    }
    Ok(Value::Int(g))
}

fn lcm(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let mut l = Int::Small(1);
    for i in 0..a.len() {
        let n = int_arg(vm, a, i, "lcm")?;
        if n.is_zero() {
            return Ok(Value::int(0));
        }
        let g = gcd_of(&l, &n);
        let q = l.divmod(&g).map(|(q, _)| q).unwrap_or(Int::zero());
        l = q.mul(&n.abs()).unwrap_or(Int::zero());
    }
    Ok(Value::Int(l))
}

/// floor(sqrt(n)) of an int, by Newton's method.
pub fn isqrt_of(n: &Int) -> Int {
    if n.is_zero() {
        return Int::zero();
    }
    let bits = n.bit_length();
    let mut x = Int::Small(1).shl(bits.div_ceil(2) as u64).unwrap_or(Int::Small(1));
    loop {
        let q = n.divmod(&x).map(|(q, _)| q).unwrap_or(Int::zero());
        let y = x.add(&q).shr(1);
        if y.cmp(&x) != core::cmp::Ordering::Less {
            return x;
        }
        x = y;
    }
}

fn isqrt(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let n = int_arg(vm, a, 0, "isqrt")?;
    if n.is_negative() {
        return vm.value_error("isqrt() argument must be nonnegative");
    }
    Ok(Value::Int(isqrt_of(&n)))
}

fn comb(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let n = int_arg(vm, a, 0, "comb")?;
    let k = int_arg(vm, a, 1, "comb")?;
    if n.is_negative() {
        return vm.value_error("n must be a non-negative integer");
    }
    if k.is_negative() {
        return vm.value_error("k must be a non-negative integer");
    }
    let (Some(n), Some(k)) = (n.to_i64(), k.to_i64()) else {
        return vm.overflow("comb() arguments too large");
    };
    if k > n {
        return Ok(Value::int(0));
    }
    let k = k.min(n - k);
    // C(n, k) = prod (n - k + i) / i, exact at each step.
    let mut acc = Int::Small(1);
    for i in 1..=k {
        acc = acc.mul(&Int::Small(n - k + i)).unwrap_or(Int::zero());
        acc = acc.divmod(&Int::Small(i)).map(|(q, _)| q).unwrap_or(Int::zero());
    }
    Ok(Value::Int(acc))
}

fn perm(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let n = int_arg(vm, a, 0, "perm")?;
    let k = match a.get(1) {
        None | Some(Value::None) => n.clone(),
        Some(_) => int_arg(vm, a, 1, "perm")?,
    };
    if n.is_negative() {
        return vm.value_error("n must be a non-negative integer");
    }
    if k.is_negative() {
        return vm.value_error("k must be a non-negative integer");
    }
    let (Some(n), Some(k)) = (n.to_i64(), k.to_i64()) else {
        return vm.overflow("perm() arguments too large");
    };
    if k > n {
        return Ok(Value::int(0));
    }
    Ok(Value::Int(product(vm, n - k + 1, n)?))
}

pub fn module(vm: &mut Vm) -> Rc<Module> {
    let functions: &[(&'static str, NativeFn)] = &[
        ("sqrt", sqrt), ("exp", exp), ("exp2", exp2), ("log", log), ("log2", log2), ("log10", log10), ("log1p", log1p),
        ("expm1", expm1), ("sin", sin), ("cos", cos), ("tan", tan), ("asin", asin), ("acos", acos), ("atan", atan),
        ("atan2", atan2), ("sinh", sinh), ("cosh", cosh), ("tanh", tanh), ("floor", floor), ("ceil", ceil),
        ("trunc", trunc), ("fabs", fabs), ("pow", pow), ("hypot", hypot), ("dist", dist), ("degrees", degrees),
        ("radians", radians), ("isfinite", isfinite), ("isinf", isinf), ("isnan", isnan), ("isclose", isclose),
        ("copysign", copysign), ("fmod", fmod), ("modf", modf), ("frexp", frexp), ("ldexp", ldexp), ("fsum", fsum),
        ("prod", prod), ("factorial", factorial), ("gcd", gcd), ("lcm", lcm), ("isqrt", isqrt), ("comb", comb),
        ("perm", perm),
    ];
    let module = super::native_module(vm, "math", functions);
    super::set(&module, "pi", Value::Float(Num::PI));
    super::set(&module, "e", Value::Float(Num::E));
    super::set(&module, "tau", Value::Float(Num::PI.mul(Num::TWO)));
    super::set(&module, "inf", Value::Float(Num::INFINITY));
    super::set(&module, "nan", Value::Float(Num::NAN));
    module
}
