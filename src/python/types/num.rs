//! Numbers: `int`, `bool`, `float` -- constructors, methods, rounding.
//!
//! Floats are the JavaScript engine's soft-float `Num` (the kernel has no
//! SSE): the same IEEE doubles as CPython's, correctly rounded, so the
//! digits a program prints are CPython's. `round` works on the exact
//! decimal expansion of the double, ties to even, as CPython does:
//! `round(2.675, 2)` is 2.67, because 2.675 is really 2.67499999...

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use super::super::args;
use super::super::int::{Int, MAX_STR_DIGITS};
use super::super::value::*;
use super::super::vm::{PyResult, Vm};
use super::method;
use crate::web::js::num::Num;

// ------------------------------------------------------------
//  int()
// ------------------------------------------------------------

/// An int or bool as an int value (`True.real` is 1).
pub fn to_int(value: &Value) -> Value {
    match value {
        Value::Bool(b) => Value::int(*b as i64),
        other => other.clone(),
    }
}

/// Parses an int literal as `int(text, base)` does.
pub fn parse_int(vm: &mut Vm, original: &str, base: u32) -> PyResult<Value> {
    let fail = |vm: &mut Vm| -> PyResult<Value> {
        let shown = vm.repr(&Value::str(original))?;
        let message = alloc::format!("invalid literal for int() with base {}: {}", base, shown);
        vm.value_error(&message)
    };
    let text = original.trim_matches(|c: char| c.is_whitespace());
    let (negative, rest) = match text.as_bytes().first() {
        Some(b'-') => (true, &text[1..]),
        Some(b'+') => (false, &text[1..]),
        _ => (false, text),
    };
    let lower = rest.to_ascii_lowercase();
    let prefixed = |p: &str| lower.starts_with(p) && lower.len() > 2;
    let (radix, digits) = match base {
        0 if prefixed("0x") => (16, &rest[2..]),
        0 if prefixed("0o") => (8, &rest[2..]),
        0 if prefixed("0b") => (2, &rest[2..]),
        0 => {
            // A decimal literal: no leading zero unless it is zero.
            let stripped: String = rest.chars().filter(|c| *c != '_').collect();
            if stripped.len() > 1 && stripped.starts_with('0') && stripped.bytes().any(|b| b != b'0') {
                return fail(vm);
            }
            (10, rest)
        }
        16 if prefixed("0x") => (16, &rest[2..]),
        8 if prefixed("0o") => (8, &rest[2..]),
        2 if prefixed("0b") => (2, &rest[2..]),
        b => (b, rest),
    };
    // Underscores: single, between digits (or right after a prefix).
    let had_prefix = digits.len() != rest.len();
    let bytes = digits.as_bytes();
    if bytes.is_empty() || bytes[bytes.len() - 1] == b'_' || (bytes[0] == b'_' && !had_prefix) || digits.contains("__") {
        return fail(vm);
    }
    let clean: String = digits.chars().filter(|c| *c != '_').collect();
    if clean.is_empty() || !clean.chars().all(|c| c.is_digit(radix)) {
        return fail(vm);
    }
    if radix == 10 && clean.len() > MAX_STR_DIGITS {
        let message = alloc::format!(
            "Exceeds the limit ({}) for integer string conversion: value has {} digits; use sys.set_int_max_str_digits() to increase the limit",
            MAX_STR_DIGITS,
            clean.len()
        );
        return vm.value_error(&message);
    }
    match Int::parse_radix(&clean, radix) {
        Some(i) => Ok(Value::Int(if negative { i.neg() } else { i })),
        None => fail(vm),
    }
}

/// `int(x)` for one argument.
pub fn int_of(vm: &mut Vm, value: &Value) -> PyResult<Value> {
    match value {
        Value::Int(_) => Ok(value.clone()),
        Value::Bool(b) => Ok(Value::int(*b as i64)),
        Value::Float(f) => float_to_int(vm, *f),
        Value::Str(s) => parse_int(vm, s, 10),
        Value::Bytes(b) => {
            let text = String::from_utf8_lossy(b).into_owned();
            parse_int(vm, &text, 10)
        }
        Value::Instance(i) => {
            for name in ["__int__", "__index__", "__trunc__"] {
                if let Some(r) = vm.call_special(value, name, &[])? {
                    return match r {
                        Value::Int(_) | Value::Bool(_) => Ok(to_int(&r)),
                        other => {
                            let t = vm.type_name(&other);
                            let message = alloc::format!("{} returned non-int (type {})", name, t);
                            vm.type_error(&message)
                        }
                    };
                }
            }
            let inner = i.inner.borrow().clone();
            if !inner.is_none() {
                return int_of(vm, &inner);
            }
            let message = alloc::format!(
                "int() argument must be a string, a bytes-like object or a real number, not '{}'",
                i.class().name
            );
            vm.type_error(&message)
        }
        other => {
            let name = vm.type_name(other);
            let message = alloc::format!("int() argument must be a string, a bytes-like object or a real number, not '{}'", name);
            vm.type_error(&message)
        }
    }
}

pub fn float_to_int(vm: &mut Vm, f: Num) -> PyResult<Value> {
    if f.is_nan() {
        return vm.value_error("cannot convert float NaN to integer");
    }
    if f.is_infinite() {
        return vm.overflow("cannot convert float infinity to integer");
    }
    Ok(Value::Int(Int::from_float(f).unwrap_or(Int::zero())))
}

pub fn new_int(vm: &mut Vm, _class: &Rc<Class>, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    if a.len() > 2 {
        let message = alloc::format!("int() takes at most 2 arguments ({} given)", a.len());
        return vm.type_error(&message);
    }
    let value = args::get(a, kw, 0, "x");
    let base = args::get(a, kw, 1, "base");
    match (value, base) {
        (None, None) => Ok(Value::int(0)),
        (None, Some(_)) => vm.type_error("int() missing string argument"),
        (Some(v), None) => int_of(vm, v),
        (Some(v), Some(b)) => {
            let b = args::int(vm, b)?;
            if b != 0 && !(2..=36).contains(&b) {
                return vm.value_error("int() base must be >= 2 and <= 36, or 0");
            }
            match vm.unwrap_builtin(v) {
                Value::Str(s) => parse_int(vm, &s, b as u32),
                Value::Bytes(bytes) => {
                    let text = String::from_utf8_lossy(&bytes).into_owned();
                    parse_int(vm, &text, b as u32)
                }
                _ => vm.type_error("int() can't convert non-string with explicit base"),
            }
        }
    }
}

pub fn new_bool(vm: &mut Vm, _class: &Rc<Class>, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    args::no_keywords(vm, "bool", kw)?;
    match a.first() {
        Some(v) => Ok(Value::Bool(vm.truthy(v)?)),
        None => Ok(Value::Bool(false)),
    }
}

// ------------------------------------------------------------
//  float()
// ------------------------------------------------------------

/// Parses a float literal as `float(text)` does: decimal with optional
/// exponent and underscores between digits, `inf`, `infinity`, `nan`.
pub fn parse_float(text: &str) -> Option<Num> {
    let text = text.trim_matches(|c: char| c.is_whitespace());
    let (negative, rest) = match text.as_bytes().first() {
        Some(b'-') => (true, &text[1..]),
        Some(b'+') => (false, &text[1..]),
        _ => (false, text),
    };
    let lower = rest.to_ascii_lowercase();
    let special = match lower.as_str() {
        "inf" | "infinity" => Some(Num::INFINITY),
        "nan" => Some(Num::NAN),
        _ => None,
    };
    if let Some(v) = special {
        return Some(if negative { v.neg() } else { v });
    }
    // Validate: digits [. digits] [e [sign] digits], underscores only
    // between two digits.
    let bytes = rest.as_bytes();
    let mut clean = String::with_capacity(rest.len());
    let mut i = 0;
    let mut mantissa_digits = 0;
    let mut previous_digit = false;
    let mut seen_point = false;
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            b'0'..=b'9' => {
                clean.push(b as char);
                mantissa_digits += 1;
                previous_digit = true;
            }
            b'_' => {
                let next_digit = bytes.get(i + 1).is_some_and(|n| n.is_ascii_digit());
                if !previous_digit || !next_digit {
                    return None;
                }
                previous_digit = false;
            }
            b'.' if !seen_point => {
                seen_point = true;
                clean.push('.');
                previous_digit = false;
            }
            b'e' | b'E' => break,
            _ => return None,
        }
        i += 1;
    }
    if mantissa_digits == 0 {
        return None;
    }
    if i < bytes.len() {
        clean.push('e');
        i += 1;
        if let Some(sign @ (b'+' | b'-')) = bytes.get(i) {
            clean.push(*sign as char);
            i += 1;
        }
        let start = i;
        let mut previous_digit = false;
        while i < bytes.len() {
            match bytes[i] {
                b'0'..=b'9' => {
                    clean.push(bytes[i] as char);
                    previous_digit = true;
                }
                b'_' if previous_digit && bytes.get(i + 1).is_some_and(|n| n.is_ascii_digit()) => previous_digit = false,
                _ => return None,
            }
            i += 1;
        }
        if i == start {
            return None;
        }
    }
    let value = Num::parse(&clean);
    Some(if negative { value.neg() } else { value })
}

/// `float(x)`.
pub fn float_of(vm: &mut Vm, value: &Value) -> PyResult<Num> {
    match value {
        Value::Float(f) => Ok(*f),
        Value::Int(i) => {
            let f = i.to_float();
            if f.is_infinite() {
                return vm.overflow("int too large to convert to float");
            }
            Ok(f)
        }
        Value::Bool(b) => Ok(if *b { Num::ONE } else { Num::ZERO }),
        Value::Str(s) => match parse_float(s) {
            Some(f) => Ok(f),
            None => {
                let shown = vm.repr(value)?;
                let message = alloc::format!("could not convert string to float: {}", shown);
                vm.value_error(&message)
            }
        },
        Value::Instance(i) => {
            if let Some(r) = vm.call_special(value, "__float__", &[])? {
                return match r {
                    Value::Float(f) => Ok(f),
                    other => {
                        let t = vm.type_name(&other);
                        let message = alloc::format!("{}.__float__ returned non-float (type {})", i.class().name, t);
                        vm.type_error(&message)
                    }
                };
            }
            if let Some(r) = vm.call_special(value, "__index__", &[])? {
                return float_of(vm, &r);
            }
            let inner = i.inner.borrow().clone();
            if !inner.is_none() {
                return float_of(vm, &inner);
            }
            let message = alloc::format!("float() argument must be a string or a real number, not '{}'", i.class().name);
            vm.type_error(&message)
        }
        other => {
            let name = vm.type_name(other);
            let message = alloc::format!("float() argument must be a string or a real number, not '{}'", name);
            vm.type_error(&message)
        }
    }
}

pub fn new_float(vm: &mut Vm, _class: &Rc<Class>, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    args::no_keywords(vm, "float", kw)?;
    if a.len() > 1 {
        let message = alloc::format!("float expected at most 1 argument, got {}", a.len());
        return vm.type_error(&message);
    }
    match a.first() {
        Some(v) => Ok(Value::Float(float_of(vm, v)?)),
        None => Ok(Value::Float(Num::ZERO)),
    }
}

// ------------------------------------------------------------
//  Rounding
// ------------------------------------------------------------

/// Rounds a float to the nearest integer, ties to even.
pub fn round_half_even(x: Num) -> Num {
    if !x.is_finite() || x.is_integer() {
        return x;
    }
    let floor = x.floor();
    let diff = x.sub(floor);
    let up = match diff.partial_cmp(Num::HALF) {
        Some(core::cmp::Ordering::Greater) => true,
        Some(core::cmp::Ordering::Less) => false,
        _ => {
            // A tie: to the even neighbour.
            let half_floor = floor.div(Num::TWO);
            !half_floor.is_integer()
        }
    };
    let r = if up { floor.add(Num::ONE) } else { floor };
    if r.is_zero() && x.is_negative() { Num::NEG_ZERO } else { r }
}

/// Exact decimal digits of a positive finite double rounded at
/// `point + ndigits` digits, ties to even: (digits, point) of the result.
pub fn round_decimal(x: Num, ndigits: i64) -> (Vec<u8>, i32) {
    let (digits, point) = x.exact_decimal();
    let keep = point as i64 + ndigits;
    if keep < 0 {
        return (Vec::new(), 0);
    }
    let keep = keep as usize;
    if digits.len() <= keep {
        return (digits, point);
    }
    let mut kept = digits[..keep].to_vec();
    let first_dropped = digits[keep];
    let rest_nonzero = digits[keep + 1..].iter().any(|d| *d != b'0');
    let last_odd = kept.last().is_some_and(|d| (d - b'0') % 2 == 1);
    let up = first_dropped > b'5' || (first_dropped == b'5' && (rest_nonzero || last_odd));
    let mut point = point;
    if up {
        let mut index = kept.len();
        loop {
            if index == 0 {
                kept.insert(0, b'1');
                point += 1;
                break;
            }
            index -= 1;
            if kept[index] == b'9' {
                kept[index] = b'0';
            } else {
                kept[index] += 1;
                break;
            }
        }
    }
    (kept, point)
}

/// `round(x, ndigits)` of a float.
pub fn round_float(x: Num, ndigits: i64) -> Num {
    if !x.is_finite() || x.is_zero() {
        return x;
    }
    if ndigits > 330 {
        return x;
    }
    if ndigits < -330 {
        return if x.is_negative() { Num::NEG_ZERO } else { Num::ZERO };
    }
    let (digits, point) = round_decimal(x.abs(), ndigits);
    let r = if digits.is_empty() || digits.iter().all(|d| *d == b'0') {
        Num::ZERO
    } else {
        Num::from_decimal(&digits, point - digits.len() as i32)
    };
    if x.is_negative() { r.neg() } else { r }
}

/// `round(i, ndigits)` of an int with a negative ndigits: to a multiple
/// of 10^-ndigits, ties to even.
pub fn round_int(i: &Int, ndigits: i64) -> Int {
    if ndigits >= 0 {
        return i.clone();
    }
    let Ok(power) = Int::Small(10).pow((-ndigits) as u64) else {
        return Int::zero();
    };
    let Some((q, r)) = i.divmod(&power) else {
        return i.clone();
    };
    // r in [0, power): compare 2r with power.
    let twice = r.add(&r);
    let q = match twice.cmp(&power) {
        core::cmp::Ordering::Greater => q.add(&Int::Small(1)),
        core::cmp::Ordering::Less => q,
        core::cmp::Ordering::Equal => {
            let odd = q.divmod(&Int::Small(2)).is_some_and(|(_, m)| !m.is_zero());
            if odd { q.add(&Int::Small(1)) } else { q }
        }
    };
    q.mul(&power).unwrap_or(Int::zero())
}

// ------------------------------------------------------------
//  int methods
// ------------------------------------------------------------

fn this_int(vm: &mut Vm, a: &[Value]) -> PyResult<Int> {
    match a.first().map(|v| vm.unwrap_builtin(v)) {
        Some(Value::Int(i)) => Ok(i),
        Some(Value::Bool(b)) => Ok(Int::Small(b as i64)),
        _ => vm.type_error("descriptor requires an 'int' object"),
    }
}

fn bit_length(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let i = this_int(vm, a)?;
    Ok(Value::int(i.bit_length() as i64))
}

fn bit_count(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let i = this_int(vm, a)?;
    let ones = i.abs().to_radix(2).bytes().filter(|b| *b == b'1').count();
    Ok(Value::int(ones as i64))
}

fn byteorder(vm: &mut Vm, value: Option<&Value>) -> PyResult<bool> {
    match value {
        None => Ok(true),
        Some(Value::Str(s)) if &**s == "big" => Ok(true),
        Some(Value::Str(s)) if &**s == "little" => Ok(false),
        Some(_) => vm.value_error("byteorder must be either 'little' or 'big'"),
    }
}

fn to_bytes(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let i = this_int(vm, a)?;
    let length = args::opt_int(vm, args::get(&a[1..], kw, 0, "length"), 1)?;
    let big = byteorder(vm, args::get(&a[1..], kw, 1, "byteorder"))?;
    let signed = match args::keyword(kw, "signed") {
        Some(v) => vm.truthy(v)?,
        None => false,
    };
    if length < 0 {
        return vm.value_error("length argument must be non-negative");
    }
    if i.is_negative() && !signed {
        return vm.overflow("can't convert negative int to unsigned");
    }
    let bits = 8 * length as u64;
    let modulus = Int::Small(1).shl(bits).unwrap_or(Int::zero());
    let fits = if signed {
        let half = Int::Small(1).shl(bits.saturating_sub(1)).unwrap_or(Int::zero());
        length > 0 && i.cmp(&half) == core::cmp::Ordering::Less && i.cmp(&half.neg()) != core::cmp::Ordering::Less
    } else {
        i.cmp(&modulus) == core::cmp::Ordering::Less
    } || (length == 0 && i.is_zero());
    if !fits {
        return vm.overflow("int too big to convert");
    }
    let mut v = if i.is_negative() { i.add(&modulus) } else { i };
    let mut out = Vec::with_capacity(length as usize);
    let byte = Int::Small(256);
    for _ in 0..length {
        let (q, r) = v.divmod(&byte).unwrap_or((Int::zero(), Int::zero()));
        out.push(r.to_i64().unwrap_or(0) as u8);
        v = q;
    }
    if big {
        out.reverse();
    }
    Ok(Value::Bytes(Rc::from(out)))
}

/// `int.from_bytes(bytes, byteorder='big', *, signed=False)`.
fn from_bytes(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let rest = if a.is_empty() { a } else { &a[1..] };
    let data = match args::get(rest, kw, 0, "bytes") {
        Some(Value::Bytes(b)) => b.to_vec(),
        Some(other) => {
            let items = vm.collect(other)?;
            let mut out = Vec::new();
            for item in items {
                out.push(args::int(vm, &item)? as u8);
            }
            out
        }
        None => return vm.type_error("from_bytes() missing required argument 'bytes' (pos 1)"),
    };
    let big = byteorder(vm, args::get(rest, kw, 1, "byteorder"))?;
    let signed = match args::keyword(kw, "signed") {
        Some(v) => vm.truthy(v)?,
        None => false,
    };
    let mut ordered = data.clone();
    if !big {
        ordered.reverse();
    }
    let mut v = Int::zero();
    for byte in &ordered {
        v = v.mul(&Int::Small(256)).unwrap_or(Int::zero()).add(&Int::Small(*byte as i64));
    }
    if signed && ordered.first().is_some_and(|b| b & 0x80 != 0) {
        let modulus = Int::Small(1).shl(8 * ordered.len() as u64).unwrap_or(Int::zero());
        v = v.sub(&modulus);
    }
    Ok(Value::Int(v))
}

fn int_conjugate(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    Ok(Value::Int(this_int(vm, a)?))
}

fn int_ratio(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let i = this_int(vm, a)?;
    Ok(Value::tuple(alloc::vec![Value::Int(i), Value::int(1)]))
}

fn int_is_integer(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    this_int(vm, a)?;
    Ok(Value::Bool(true))
}

// ------------------------------------------------------------
//  float methods
// ------------------------------------------------------------

fn this_float(vm: &mut Vm, a: &[Value]) -> PyResult<Num> {
    match a.first().map(|v| vm.unwrap_builtin(v)) {
        Some(Value::Float(f)) => Ok(f),
        _ => vm.type_error("descriptor requires a 'float' object"),
    }
}

fn float_is_integer(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let f = this_float(vm, a)?;
    Ok(Value::Bool(f.is_finite() && f.is_integer()))
}

fn float_ratio(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let f = this_float(vm, a)?;
    if f.is_nan() {
        return vm.value_error("cannot convert NaN to integer ratio");
    }
    if f.is_infinite() {
        return vm.overflow("cannot convert Infinity to integer ratio");
    }
    if f.is_zero() {
        return Ok(Value::tuple(alloc::vec![Value::int(0), Value::int(1)]));
    }
    let (negative, m, e) = f.decompose();
    let (numerator, denominator) = if e >= 0 {
        (Int::from_u128(m as u128).shl(e as u64).unwrap_or(Int::zero()), Int::Small(1))
    } else {
        let shift = (m.trailing_zeros() as i32).min(-e);
        let m = m >> shift;
        let d = Int::Small(1).shl((-e - shift) as u64).unwrap_or(Int::zero());
        (Int::from_u128(m as u128), d)
    };
    let numerator = if negative { numerator.neg() } else { numerator };
    Ok(Value::tuple(alloc::vec![Value::Int(numerator), Value::Int(denominator)]))
}

/// `float.hex()`: `0x1.8000000000000p+0`.
fn float_hex(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let f = this_float(vm, a)?;
    if f.is_nan() {
        return Ok(Value::str("nan"));
    }
    if f.is_infinite() {
        return Ok(Value::str(if f.is_negative() { "-inf" } else { "inf" }));
    }
    let sign = if f.is_sign_negative() { "-" } else { "" };
    if f.is_zero() {
        return Ok(Value::str(&alloc::format!("{}0x0.0p+0", sign)));
    }
    let bits = f.0;
    let field = ((bits >> 52) & 0x7ff) as i32;
    let fraction = bits & ((1 << 52) - 1);
    let (lead, exponent) = if field == 0 { (0, -1022) } else { (1, field - 1023) };
    let text = alloc::format!("{}0x{}.{:013x}p{}{}", sign, lead, fraction, if exponent >= 0 { "+" } else { "-" }, exponent.abs());
    Ok(Value::str(&text))
}

fn float_conjugate(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    Ok(Value::Float(this_float(vm, a)?))
}

pub fn install(vm: &mut Vm) {
    let int = vm.types.int.clone();
    let entries: &[(&'static str, NativeFn)] = &[
        ("bit_length", bit_length), ("bit_count", bit_count), ("to_bytes", to_bytes), ("conjugate", int_conjugate),
        ("as_integer_ratio", int_ratio), ("is_integer", int_is_integer),
    ];
    for (name, f) in entries {
        method(&int, name, *f);
    }
    int.dict.borrow_mut().set_str("from_bytes", Value::ClassMethod(Rc::new(super::native("from_bytes", from_bytes))));
    let float = vm.types.float.clone();
    let entries: &[(&'static str, NativeFn)] = &[
        ("is_integer", float_is_integer), ("as_integer_ratio", float_ratio), ("hex", float_hex), ("conjugate", float_conjugate),
    ];
    for (name, f) in entries {
        method(&float, name, *f);
    }
}
