//! `complex`: arithmetic, display, hashing and the constructor, with
//! CPython's own algorithms (complexobject.c) over the soft-float `Num`:
//! Smith's division, powers by repeated squaring for small integer
//! exponents and by the polar form otherwise, the repr that drops `.0`.

use alloc::string::String;

use super::super::ast::BinOp;
use super::super::value::*;
use super::super::vm::{PyResult, Vm};
use crate::web::js::num::Num;

pub type C = (Num, Num);

/// `x` as a complex, if it is a number.
pub fn as_complex(value: &Value) -> Option<C> {
    match value {
        Value::Complex(r, i) => Some((*r, *i)),
        Value::Float(f) => Some((*f, Num::ZERO)),
        Value::Int(i) => Some((i.to_float(), Num::ZERO)),
        Value::Bool(b) => Some((Num::from_i64(*b as i64), Num::ZERO)),
        _ => None,
    }
}

pub fn hypot(x: Num, y: Num) -> Num {
    let (x, y) = (x.abs(), y.abs());
    if x.is_infinite() || y.is_infinite() {
        return Num::INFINITY;
    }
    if x.is_nan() || y.is_nan() {
        return Num::NAN;
    }
    let (big, small) = if x.lt(y) { (y, x) } else { (x, y) };
    if big.is_zero() {
        return Num::ZERO;
    }
    let ratio = small.div(big);
    big.mul(Num::ONE.add(ratio.mul(ratio)).sqrt())
}

fn prod(a: C, b: C) -> C {
    (a.0.mul(b.0).sub(a.1.mul(b.1)), a.0.mul(b.1).add(a.1.mul(b.0)))
}

/// `a / b`, None for a division by zero.
pub fn quot(a: C, b: C) -> Option<C> {
    let abs_breal = b.0.abs();
    let abs_bimag = b.1.abs();
    if !abs_breal.lt(abs_bimag) {
        if abs_breal.is_zero() {
            return None;
        }
        let ratio = b.1.div(b.0);
        let denom = b.0.add(b.1.mul(ratio));
        Some((a.0.add(a.1.mul(ratio)).div(denom), a.1.sub(a.0.mul(ratio)).div(denom)))
    } else if !abs_bimag.lt(abs_breal) {
        let ratio = b.0.div(b.1);
        let denom = b.0.mul(ratio).add(b.1);
        Some((a.0.mul(ratio).add(a.1).div(denom), a.1.mul(ratio).sub(a.0).div(denom)))
    } else {
        Some((Num::NAN, Num::NAN))
    }
}

fn powu(x: C, n: i64) -> C {
    let mut r = (Num::ONE, Num::ZERO);
    let mut p = x;
    let mut mask: i64 = 1;
    while mask > 0 && n >= mask {
        if n & mask != 0 {
            r = prod(r, p);
        }
        mask <<= 1;
        p = prod(p, p);
    }
    r
}

/// `a ** b`; Err(()) for zero to a negative or complex power.
pub fn pow(a: C, b: C) -> Result<C, ()> {
    if b.1.is_zero() && b.0.equals(b.0.floor()) && !Num::from_i64(100).lt(b.0.abs()) {
        let n = b.0.to_i64_saturating();
        if n >= 0 {
            return Ok(powu(a, n));
        }
        if a.0.is_zero() && a.1.is_zero() {
            return Err(());
        }
        return quot((Num::ONE, Num::ZERO), powu(a, -n)).ok_or(());
    }
    if b.0.is_zero() && b.1.is_zero() {
        return Ok((Num::ONE, Num::ZERO));
    }
    if a.0.is_zero() && a.1.is_zero() {
        if !b.1.is_zero() || b.0.is_negative() {
            return Err(());
        }
        return Ok((Num::ZERO, Num::ZERO));
    }
    let vabs = hypot(a.0, a.1);
    let mut len = vabs.pow(b.0);
    let at = a.1.atan2(a.0);
    let mut phase = at.mul(b.0);
    if !b.1.is_zero() {
        len = len.div(at.mul(b.1).exp());
        phase = phase.add(b.1.mul(vabs.ln()));
    }
    Ok((len.mul(phase.cos()), len.mul(phase.sin())))
}

/// Arithmetic with at least one complex operand.
pub fn binary(vm: &mut Vm, op: BinOp, a: &Value, b: &Value) -> PyResult<Option<Value>> {
    let (Some(x), Some(y)) = (as_complex(a), as_complex(b)) else {
        return Ok(None);
    };
    let result = match op {
        BinOp::Add => (x.0.add(y.0), x.1.add(y.1)),
        BinOp::Sub => (x.0.sub(y.0), x.1.sub(y.1)),
        BinOp::Mul => prod(x, y),
        BinOp::Div => match quot(x, y) {
            Some(q) => q,
            None => return vm.zero_division("complex division by zero"),
        },
        BinOp::Pow => match pow(x, y) {
            Ok(p) => p,
            Err(()) => return vm.zero_division("0.0 to a negative or complex power"),
        },
        _ => return Ok(None),
    };
    Ok(Some(Value::Complex(result.0, result.1)))
}

/// A part of a complex as its repr shows it: the float repr without a
/// trailing `.0`.
fn part(x: Num) -> String {
    let text = super::super::format::float_repr(x);
    match text.strip_suffix(".0") {
        Some(short) => String::from(short),
        None => text,
    }
}

pub fn repr(real: Num, imag: Num) -> String {
    if real.is_zero() && !real.is_sign_negative() {
        return alloc::format!("{}j", part(imag));
    }
    let im = part(imag);
    let sign = if im.starts_with('-') { "" } else { "+" };
    alloc::format!("({}{}{}j)", part(real), sign, im)
}

pub fn hash(real: Num, imag: Num, value: &Value) -> i64 {
    let hr = super::float_hash(real, value) as u64;
    let hi = super::float_hash(imag, value) as u64;
    let combined = hr.wrapping_add(1_000_003u64.wrapping_mul(hi)) as i64;
    if combined == -1 { -2 } else { combined }
}

/// `complex(text)`: `1+2j`, `-3.5j`, `(1-2j)`, `4`.
fn parse(text: &str) -> Option<C> {
    let mut s = text.trim();
    if let Some(inner) = s.strip_prefix('(').and_then(|r| r.strip_suffix(')')) {
        s = inner.trim();
    }
    if s.is_empty() {
        return None;
    }
    let number = |t: &str| -> Option<Num> {
        let t = t.trim();
        let ok = !t.is_empty()
            && t.bytes().all(|b| b.is_ascii_digit() || matches!(b, b'.' | b'e' | b'E' | b'+' | b'-' | b'_'))
            || matches!(t.to_ascii_lowercase().trim_start_matches(['+', '-']), "inf" | "infinity" | "nan");
        if !ok {
            return None;
        }
        Some(Num::parse(t))
    };
    let imaginary_unit = |t: &str| -> Option<Num> {
        match t {
            "" | "+" => Some(Num::ONE),
            "-" => Some(Num::ONE.neg()),
            other => number(other),
        }
    };
    if let Some(body) = s.strip_suffix(['j', 'J']) {
        // The split between the real and the imaginary part: the last
        // sign not following an exponent letter.
        let bytes = body.as_bytes();
        let mut split = None;
        for i in (1..bytes.len()).rev() {
            if (bytes[i] == b'+' || bytes[i] == b'-') && !matches!(bytes[i - 1], b'e' | b'E') {
                split = Some(i);
                break;
            }
        }
        return match split {
            Some(i) => Some((number(&body[..i])?, imaginary_unit(&body[i..])?)),
            None => Some((Num::ZERO, imaginary_unit(body)?)),
        };
    }
    Some((number(s)?, Num::ZERO))
}

pub fn new_complex(vm: &mut Vm, _class: &std_rc::Rc<Class>, a: &[Value], kw: &[(std_rc::Rc<str>, Value)]) -> PyResult<Value> {
    let mut real = a.first().cloned();
    let mut imag = a.get(1).cloned();
    for (key, value) in kw {
        match &**key {
            "real" => real = Some(value.clone()),
            "imag" => imag = Some(value.clone()),
            other => return vm.type_error(&alloc::format!("complex() got an unexpected keyword argument '{}'", other)),
        }
    }
    if let Some(Value::Str(text)) = &real {
        if imag.is_some() {
            return vm.type_error("complex() can't take second arg if first is a string");
        }
        return match parse(text) {
            Some((r, i)) => Ok(Value::Complex(r, i)),
            None => vm.value_error("complex() arg is a malformed string"),
        };
    }
    let part = |vm: &mut Vm, v: &Option<Value>, which: &str| -> PyResult<C> {
        match v {
            None => Ok((Num::ZERO, Num::ZERO)),
            Some(v) => match as_complex(v) {
                Some(c) => Ok(c),
                None => {
                    if let Some(method) = vm.special(v, "__complex__")? {
                        let result = vm.call(&method, &[], &[])?;
                        if let Some(c) = as_complex(&result) {
                            return Ok(c);
                        }
                    }
                    if let Some(method) = vm.special(v, "__float__")? {
                        let result = vm.call(&method, &[], &[])?;
                        if let Some(c) = as_complex(&result) {
                            return Ok(c);
                        }
                    }
                    let name = vm.type_name(v);
                    vm.type_error(&alloc::format!(
                        "complex() {} must be a number, not '{}'",
                        if which == "real" { "first argument" } else { "second argument" },
                        name
                    ))
                }
            },
        }
    };
    let r = part(vm, &real, "real")?;
    let i = part(vm, &imag, "imag")?;
    // complex(a, b) = a + b*1j, each possibly complex; a real part is
    // taken as it is (so that complex(0.0, -0.0) keeps its sign).
    let real_complex = matches!(real, Some(Value::Complex(..)));
    let imag_complex = matches!(imag, Some(Value::Complex(..)));
    let mut re = r.0;
    let mut im = i.0;
    if imag_complex {
        re = re.sub(i.1);
    }
    if real_complex {
        im = if imag.is_some() { im.add(r.1) } else { r.1 };
    }
    Ok(Value::Complex(re, im))
}

mod std_rc {
    pub use alloc::rc::Rc;
}

pub fn conjugate(vm: &mut Vm, a: &[Value], _: &[(alloc::rc::Rc<str>, Value)]) -> PyResult<Value> {
    match a.first() {
        Some(Value::Complex(r, i)) => Ok(Value::Complex(*r, i.neg())),
        _ => vm.type_error("descriptor 'conjugate' requires a 'complex' object"),
    }
}

/// `format(z, spec)`: each part formatted with the spec, the imaginary
/// one signed, as CPython's complex.__format__.
pub fn format(vm: &mut Vm, real: Num, imag: Num, spec: &str) -> PyResult<String> {
    if spec.is_empty() {
        return Ok(repr(real, imag));
    }
    // Fill, alignment and width apply to the whole result.
    let chars: alloc::vec::Vec<char> = spec.chars().collect();
    let is_align = |c: char| matches!(c, '<' | '>' | '^' | '=');
    let (fill, align, skip) = if chars.len() >= 2 && is_align(chars[1]) {
        (chars[0], chars[1], 2)
    } else if chars.first().is_some_and(|c| is_align(*c)) {
        (' ', chars[0], 1)
    } else {
        (' ', '>', 0)
    };
    let rest: String = chars[skip..].iter().collect();
    let rest = rest.as_str();
    let (width, inner) = {
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        (digits.parse::<usize>().unwrap_or(0), &rest[digits.len()..])
    };
    let has_type = inner.chars().last().is_some_and(|c| c.is_ascii_alphabetic());
    let body = if !has_type {
        let r = super::super::format::format(vm, &Value::Float(real), inner)?;
        let mut i = super::super::format::format(vm, &Value::Float(imag), inner)?;
        if !i.starts_with('-') {
            i.insert(0, '+');
        }
        let r = r.strip_suffix(".0").map(String::from).unwrap_or(r);
        let i = i.strip_suffix(".0").map(String::from).unwrap_or(i);
        if real.is_zero() && !real.is_sign_negative() {
            alloc::format!("{}j", i.trim_start_matches('+'))
        } else {
            alloc::format!("({}{}j)", r, i)
        }
    } else {
        let r = super::super::format::format(vm, &Value::Float(real), inner)?;
        let mut i = super::super::format::format(vm, &Value::Float(imag), inner)?;
        if !i.starts_with('-') {
            i.insert(0, '+');
        }
        alloc::format!("{}{}j", r, i)
    };
    let length = body.chars().count();
    if width <= length {
        return Ok(body);
    }
    let pad = width - length;
    let fill: String = core::iter::repeat_n(fill, pad).collect();
    Ok(match align {
        '<' => alloc::format!("{}{}", body, fill),
        '^' => {
            let left: String = core::iter::repeat_n(fill.chars().next().unwrap_or(' '), pad / 2).collect();
            let right: String = core::iter::repeat_n(fill.chars().next().unwrap_or(' '), pad - pad / 2).collect();
            alloc::format!("{}{}{}", left, body, right)
        }
        _ => alloc::format!("{}{}", fill, body),
    })
}
