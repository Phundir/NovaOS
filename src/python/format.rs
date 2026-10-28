//! Text of values: `repr`, `str`, `format` (the format-spec mini-language),
//! `%` formatting and `str.format`.
//!
//! The output is CPython's character for character, because it is what a
//! program prints and what its user compares: `repr(0.1)` is `0.1` (the
//! shortest digits that read back), `1e16` prints `1e+16`, `{:,}` groups
//! by three, and a string's repr picks its quotes as CPython does.

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use super::int::{Int, MAX_STR_DIGITS};
use super::types::num::round_decimal;
use super::value::*;
use super::vm::{PyErr, PyResult, Vm};
use crate::web::js::num::Num;

// ============================================================
//  repr and str
// ============================================================

/// The text of an int, refused beyond 4300 digits as CPython does.
pub fn int_text(vm: &mut Vm, i: &Int) -> PyResult<String> {
    let limit = || {
        alloc::format!(
            "Exceeds the limit ({}) for integer string conversion; use sys.set_int_max_str_digits() to increase the limit",
            MAX_STR_DIGITS
        )
    };
    if i.bit_length() > 14_300 {
        return vm.value_error(&limit());
    }
    let text = i.to_radix(10);
    if text.trim_start_matches('-').len() > MAX_STR_DIGITS {
        return vm.value_error(&limit());
    }
    Ok(text)
}

/// `repr(float)`: the shortest digits that read back, fixed notation
/// between 1e-4 and 1e16.
pub fn float_repr(x: Num) -> String {
    if x.is_nan() {
        return String::from("nan");
    }
    if x.is_infinite() {
        return String::from(if x.is_negative() { "-inf" } else { "inf" });
    }
    let sign = if x.is_sign_negative() { "-" } else { "" };
    if x.is_zero() {
        return alloc::format!("{}0.0", sign);
    }
    let (digits, point) = x.abs().shortest();
    let text = |d: &[u8]| String::from(core::str::from_utf8(d).unwrap_or("0"));
    let body = if point <= -4 || point > 16 {
        let mut s = text(&digits[..1]);
        if digits.len() > 1 {
            s.push('.');
            s.push_str(&text(&digits[1..]));
        }
        let exponent = point - 1;
        alloc::format!("{}e{}{:02}", s, if exponent < 0 { '-' } else { '+' }, exponent.abs())
    } else if point <= 0 {
        alloc::format!("0.{}{}", "0".repeat((-point) as usize), text(&digits))
    } else if point as usize >= digits.len() {
        alloc::format!("{}{}.0", text(&digits), "0".repeat(point as usize - digits.len()))
    } else {
        alloc::format!("{}.{}", text(&digits[..point as usize]), text(&digits[point as usize..]))
    };
    alloc::format!("{}{}", sign, body)
}

/// A string's repr: single quotes unless it holds one and no double.
pub fn str_repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || (0x7f..0xa1).contains(&(c as u32)) || c as u32 == 0xad => {
                out.push_str(&alloc::format!("\\x{:02x}", c as u32));
            }
            c if matches!(c as u32, 0x2028 | 0x2029 | 0x200b..=0x200f | 0xfeff) => {
                out.push_str(&alloc::format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

pub fn bytes_repr(b: &[u8]) -> String {
    let quote = if b.contains(&b'\'') && !b.contains(&b'"') { b'"' } else { b'\'' };
    let mut out = String::from("b");
    out.push(quote as char);
    for &byte in b {
        match byte {
            b'\\' => out.push_str("\\\\"),
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            b'\t' => out.push_str("\\t"),
            q if q == quote => {
                out.push('\\');
                out.push(q as char);
            }
            0x20..=0x7e => out.push(byte as char),
            _ => out.push_str(&alloc::format!("\\x{:02x}", byte)),
        }
    }
    out.push(quote as char);
    out
}

/// `ascii()`: a repr with every non-ASCII character escaped.
pub fn ascii(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        let code = c as u32;
        if code < 0x80 {
            out.push(c);
        } else if code < 0x100 {
            out.push_str(&alloc::format!("\\x{:02x}", code));
        } else if code < 0x10000 {
            out.push_str(&alloc::format!("\\u{:04x}", code));
        } else {
            out.push_str(&alloc::format!("\\U{:08x}", code));
        }
    }
    out
}

fn class_text(class: &Class) -> String {
    match class.dict.borrow().get_str("__module__") {
        Some(Value::Str(m)) if &*m != "builtins" => alloc::format!("<class '{}.{}'>", m, class.qualname.borrow()),
        _ => alloc::format!("<class '{}'>", class.qualname.borrow()),
    }
}

/// Items joined, with a guard against containers inside themselves.
fn items_repr(vm: &mut Vm, id: usize, items: &[Value], open: &str, close: &str, cycle: &str) -> PyResult<String> {
    if vm.repr_stack.contains(&id) {
        return Ok(String::from(cycle));
    }
    vm.repr_stack.push(id);
    let mut out = String::from(open);
    let mut result = Ok(());
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        match repr(vm, item) {
            Ok(text) => out.push_str(&text),
            Err(e) => {
                result = Err(e);
                break;
            }
        }
    }
    vm.repr_stack.retain(|x| *x != id);
    result?;
    out.push_str(close);
    Ok(out)
}

fn dict_repr(vm: &mut Vm, id: usize, pairs: &[(Value, Value)]) -> PyResult<String> {
    if vm.repr_stack.contains(&id) {
        return Ok(String::from("{...}"));
    }
    vm.repr_stack.push(id);
    let mut out = String::from("{");
    let mut result = Ok(());
    for (i, (k, v)) in pairs.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        let text = repr(vm, k).and_then(|k| Ok((k, repr(vm, v)?)));
        match text {
            Ok((k, v)) => {
                out.push_str(&k);
                out.push_str(": ");
                out.push_str(&v);
            }
            Err(e) => {
                result = Err(e);
                break;
            }
        }
    }
    vm.repr_stack.retain(|x| *x != id);
    result?;
    out.push('}');
    Ok(out)
}

pub fn repr(vm: &mut Vm, value: &Value) -> PyResult<String> {
    vm.enter_native()?;
    let result = repr_inner(vm, value);
    vm.leave_native();
    result
}

fn repr_inner(vm: &mut Vm, value: &Value) -> PyResult<String> {
    let at = || value.pointer().unwrap_or(0);
    Ok(match value {
        Value::None => String::from("None"),
        Value::NotImplemented => String::from("NotImplemented"),
        Value::Ellipsis => String::from("Ellipsis"),
        Value::Bool(b) => String::from(if *b { "True" } else { "False" }),
        Value::Int(i) => int_text(vm, i)?,
        Value::Float(f) => float_repr(*f),
        Value::Str(s) => str_repr(s),
        Value::Bytes(b) => bytes_repr(b),
        Value::List(l) => {
            let items = l.borrow().clone();
            items_repr(vm, at(), &items, "[", "]", "[...]")?
        }
        Value::Tuple(t) => {
            if t.len() == 1 {
                let inner = items_repr(vm, at(), t, "(", "", "(...)")?;
                alloc::format!("{},)", inner)
            } else {
                items_repr(vm, at(), t, "(", ")", "(...)")?
            }
        }
        Value::Dict(d) => {
            let pairs: Vec<(Value, Value)> = d.borrow().iter().map(|e| (e.key.clone(), e.value.clone())).collect();
            dict_repr(vm, at(), &pairs)?
        }
        Value::Set(s) => {
            let items = s.borrow().keys();
            if items.is_empty() { String::from("set()") } else { items_repr(vm, at(), &items, "{", "}", "set(...)")? }
        }
        Value::FrozenSet(s) => {
            let items = s.keys();
            if items.is_empty() { String::from("frozenset()") } else { items_repr(vm, at(), &items, "frozenset({", "})", "frozenset(...)")? }
        }
        Value::View(v) => {
            let (name, items): (&str, Vec<Value>) = match v.kind {
                0 => ("dict_keys", v.dict.borrow().keys()),
                1 => ("dict_values", v.dict.borrow().values()),
                _ => ("dict_items", v.dict.borrow().iter().map(|e| Value::tuple(alloc::vec![e.key.clone(), e.value.clone()])).collect()),
            };
            let open = alloc::format!("{}([", name);
            items_repr(vm, at(), &items, &open, "])", "...")?
        }
        Value::Range(r) => {
            if r.step == 1 {
                alloc::format!("range({}, {})", r.start, r.stop)
            } else {
                alloc::format!("range({}, {}, {})", r.start, r.stop, r.step)
            }
        }
        Value::Slice(s) => {
            let parts = [repr(vm, &s[0])?, repr(vm, &s[1])?, repr(vm, &s[2])?];
            alloc::format!("slice({}, {}, {})", parts[0], parts[1], parts[2])
        }
        Value::Function(f) => alloc::format!("<function {} at {:#x}>", f.qualname.borrow(), at()),
        Value::Builtin(b) => alloc::format!("<built-in function {}>", b.name),
        Value::Method(m) => {
            let (receiver, function) = (&m.0, &m.1);
            match function {
                Value::Builtin(b) => {
                    let kind = vm.type_name(receiver);
                    alloc::format!("<built-in method {} of {} object at {:#x}>", b.name, kind, receiver.pointer().unwrap_or(0))
                }
                Value::Function(f) => {
                    let shown = repr(vm, receiver)?;
                    alloc::format!("<bound method {} of {}>", f.qualname.borrow(), shown)
                }
                other => {
                    let shown = repr(vm, other)?;
                    alloc::format!("<bound method {}>", shown)
                }
            }
        }
        Value::Class(c) => {
            // A metaclass's repr (`<enum 'Color'>`), through its hook.
            if c.lookup("__class_repr__").is_some() {
                let c = c.clone();
                if let Some(Value::Str(s)) = vm.class_hook(&c, "__class_repr__", &[])? {
                    return Ok(String::from(&*s));
                }
            }
            class_text(c)
        }
        Value::Instance(_) => {
            let r = vm.call_special(value, "__repr__", &[])?;
            match r {
                Some(Value::Str(s)) => String::from(&*s),
                Some(other) => {
                    let name = vm.type_name(&other);
                    let message = alloc::format!("__repr__ returned non-string (type {})", name);
                    return vm.type_error(&message);
                }
                None => super::types::object::default_repr(vm, value),
            }
        }
        Value::Iter(state) => {
            let name = match &*state.borrow() {
                Iter::Seq { seq: Value::List(_), .. } => "list_iterator",
                Iter::Seq { seq: Value::Tuple(_), .. } => "tuple_iterator",
                Iter::Seq { .. } => "iterator",
                Iter::Range { .. } => "range_iterator",
                Iter::Str { .. } => "str_ascii_iterator",
                Iter::Keys { kind: 0, .. } => "dict_keyiterator",
                Iter::Keys { kind: 1, .. } => "dict_valueiterator",
                Iter::Keys { .. } => "dict_itemiterator",
                Iter::Enumerate { .. } => "enumerate",
                Iter::Zip { .. } => "zip",
                Iter::Map { .. } => "map",
                Iter::Filter { .. } => "filter",
                Iter::Reversed { .. } => "list_reverseiterator",
                Iter::Callable { .. } => "callable_iterator",
                Iter::Object(_) | Iter::Done => "iterator",
            };
            alloc::format!("<{} object at {:#x}>", name, at())
        }
        Value::Generator(g) => {
            let name = g.borrow().name.clone();
            alloc::format!("<generator object {} at {:#x}>", name, at())
        }
        Value::Module(m) => {
            if m.file.is_empty() {
                alloc::format!("<module '{}' (built-in)>", m.name)
            } else {
                alloc::format!("<module '{}' from '{}'>", m.name, m.file)
            }
        }
        Value::Property(_) => alloc::format!("<property object at {:#x}>", at()),
        Value::StaticMethod(f) => alloc::format!("<staticmethod({})>", repr(vm, f)?),
        Value::ClassMethod(f) => alloc::format!("<classmethod({})>", repr(vm, f)?),
        Value::Super(s) => {
            let shown = repr(vm, &s.1)?;
            alloc::format!("<super: {}, {}>", class_text(&s.0), shown)
        }
        Value::File(f) => {
            let f = f.borrow();
            let mode = match (f.readable, f.writable, f.append) {
                (_, true, true) => "a",
                (true, true, false) => "r+",
                (false, true, false) => "w",
                _ => "r",
            };
            if f.binary {
                alloc::format!("<_io.BufferedReader name='{}'>", f.path)
            } else {
                alloc::format!("<_io.TextIOWrapper name='{}' mode='{}' encoding='utf-8'>", f.path, mode)
            }
        }
        Value::Code(c) => alloc::format!("<code object {} at {:#x}, file \"{}\", line {}>", c.name, at(), c.filename, c.first_line),
        Value::Cell(_) => alloc::format!("<cell at {:#x}>", at()),
    })
}

/// `str()` of anything but a str.
pub fn str_of(vm: &mut Vm, value: &Value) -> PyResult<String> {
    match value {
        Value::Str(s) => Ok(String::from(&**s)),
        Value::Instance(_) => {
            vm.enter_native()?;
            let r = vm.call_special(value, "__str__", &[]);
            vm.leave_native();
            match r? {
                Some(Value::Str(s)) => Ok(String::from(&*s)),
                Some(other) => {
                    let name = vm.type_name(&other);
                    let message = alloc::format!("__str__ returned non-string (type {})", name);
                    vm.type_error(&message)
                }
                None => repr(vm, value),
            }
        }
        other => repr(vm, other),
    }
}

// ============================================================
//  The format-spec mini-language
// ============================================================

#[derive(Clone)]
struct Spec {
    fill: char,
    align: Option<char>,
    sign: char,
    z: bool,
    alternate: bool,
    zero: bool,
    width: usize,
    grouping: Option<char>,
    precision: Option<usize>,
    kind: Option<char>,
}

fn parse_spec(vm: &mut Vm, spec: &str, type_name: &str) -> PyResult<Spec> {
    let chars: Vec<char> = spec.chars().collect();
    let mut s = Spec { fill: ' ', align: None, sign: '-', z: false, alternate: false, zero: false, width: 0, grouping: None, precision: None, kind: None };
    let mut i = 0;
    let is_align = |c: char| matches!(c, '<' | '>' | '=' | '^');
    if chars.len() >= 2 && is_align(chars[1]) {
        s.fill = chars[0];
        s.align = Some(chars[1]);
        i = 2;
    } else if !chars.is_empty() && is_align(chars[0]) {
        s.align = Some(chars[0]);
        i = 1;
    }
    if let Some(&c @ ('+' | '-' | ' ')) = chars.get(i) {
        s.sign = c;
        i += 1;
    }
    if chars.get(i) == Some(&'z') {
        s.z = true;
        i += 1;
    }
    if chars.get(i) == Some(&'#') {
        s.alternate = true;
        i += 1;
    }
    if chars.get(i) == Some(&'0') {
        s.zero = true;
        i += 1;
    }
    let start = i;
    while i < chars.len() && chars[i].is_ascii_digit() {
        i += 1;
    }
    if i > start {
        let digits: String = chars[start..i].iter().collect();
        s.width = digits.parse().unwrap_or(0);
    }
    if let Some(&c @ (',' | '_')) = chars.get(i) {
        s.grouping = Some(c);
        i += 1;
        if let Some(&(',' | '_')) = chars.get(i) {
            return vm.value_error("Cannot specify both ',' and '_'.");
        }
    }
    if chars.get(i) == Some(&'.') {
        i += 1;
        let start = i;
        while i < chars.len() && chars[i].is_ascii_digit() {
            i += 1;
        }
        if i == start {
            return vm.value_error("Format specifier missing precision");
        }
        let digits: String = chars[start..i].iter().collect();
        s.precision = Some(digits.parse().unwrap_or(0));
    }
    if i < chars.len() {
        if i + 1 < chars.len() {
            let message = alloc::format!("Invalid format specifier '{}' for object of type '{}'", spec, type_name);
            return vm.value_error(&message);
        }
        s.kind = Some(chars[i]);
    }
    if s.zero && s.align.is_none() {
        s.fill = '0';
        s.align = Some('=');
    }
    Ok(s)
}

/// Pads `sign + prefix + body` to the spec's width.
fn pad(spec: &Spec, sign: &str, body: &str, default_align: char) -> String {
    let len = sign.chars().count() + body.chars().count();
    if spec.width <= len {
        return alloc::format!("{}{}", sign, body);
    }
    let room = spec.width - len;
    let fill: String = core::iter::repeat_n(spec.fill, room).collect();
    match spec.align.unwrap_or(default_align) {
        '<' => alloc::format!("{}{}{}", sign, body, fill),
        '^' => {
            let left: String = core::iter::repeat_n(spec.fill, room / 2).collect();
            let right: String = core::iter::repeat_n(spec.fill, room - room / 2).collect();
            alloc::format!("{}{}{}{}", left, sign, body, right)
        }
        '=' => alloc::format!("{}{}{}", sign, fill, body),
        _ => alloc::format!("{}{}{}", fill, sign, body),
    }
}

/// Inserts `separator` every `every` digits from the right.
fn group(digits: &str, separator: char, every: usize) -> String {
    let n = digits.len();
    let mut out = String::with_capacity(n + n / every);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (n - i) % every == 0 {
            out.push(separator);
        }
        out.push(c);
    }
    out
}

/// Grouped digits, zero-padded through the groups up to `target`.
fn group_padded(digits: &str, separator: char, every: usize, target: usize, zero: bool) -> String {
    let mut d = String::from(digits);
    let mut g = group(&d, separator, every);
    while zero && g.chars().count() < target {
        d.insert(0, '0');
        g = group(&d, separator, every);
    }
    g
}

fn sign_text(negative: bool, sign: char) -> &'static str {
    match (negative, sign) {
        (true, _) => "-",
        (false, '+') => "+",
        (false, ' ') => " ",
        _ => "",
    }
}

fn format_int(vm: &mut Vm, i: &Int, spec: &Spec, type_name: &str) -> PyResult<String> {
    let kind = spec.kind.unwrap_or('d');
    if matches!(kind, 'e' | 'E' | 'f' | 'F' | 'g' | 'G' | '%') {
        let f = i.to_float();
        return format_float(vm, f, spec);
    }
    if spec.precision.is_some() {
        return vm.value_error("Precision not allowed in integer format specifier");
    }
    if spec.z {
        return vm.value_error("Negative zero coercion (z) not allowed in integer format specifier");
    }
    let (radix, prefix) = match kind {
        'd' | 'n' => (10, ""),
        'b' => (2, "0b"),
        'o' => (8, "0o"),
        'x' => (16, "0x"),
        'X' => (16, "0X"),
        'c' => {
            if spec.sign != '-' {
                return vm.value_error("Sign not allowed with integer format specifier 'c'");
            }
            let code = i.to_i64().unwrap_or(-1);
            let Some(c) = u32::try_from(code).ok().and_then(char::from_u32) else {
                return vm.overflow("%c arg not in range(0x110000)");
            };
            let mut buffer = [0u8; 4];
            return Ok(pad(spec, "", c.encode_utf8(&mut buffer), '<'));
        }
        other => {
            let message = alloc::format!("Unknown format code '{}' for object of type '{}'", other, type_name);
            return vm.value_error(&message);
        }
    };
    if spec.grouping == Some(',') && radix != 10 {
        let message = alloc::format!("Cannot specify ',' with '{}'.", kind);
        return vm.value_error(&message);
    }
    let mut digits = if radix == 10 { int_text(vm, &i.abs())? } else { i.abs().to_radix(radix) };
    if kind == 'X' {
        digits = digits.to_uppercase();
    }
    let sign = sign_text(i.is_negative(), spec.sign);
    let prefix = if spec.alternate { prefix } else { "" };
    let body = match spec.grouping {
        Some(separator) => {
            let every = if radix == 10 { 3 } else { 4 };
            let zero_fill = spec.align == Some('=') && spec.fill == '0';
            let target = spec.width.saturating_sub(sign.len() + prefix.len());
            group_padded(&digits, separator, every, target, zero_fill)
        }
        None => digits,
    };
    // '=' pads between the sign and prefix and the digits: 0x000000ff.
    Ok(pad(spec, &alloc::format!("{}{}", sign, prefix), &body, '>'))
}

/// Digits of a positive finite float rounded to `count` significant
/// digits, ties to even: (digits, point).
fn significant(x: Num, count: usize) -> (Vec<u8>, i32) {
    let (_, point) = x.exact_decimal();
    let (mut digits, point) = round_decimal(x, count as i64 - point as i64);
    digits.truncate(count);
    while digits.len() < count {
        digits.push(b'0');
    }
    (digits, point)
}

fn text(d: &[u8]) -> String {
    String::from(core::str::from_utf8(d).unwrap_or("0"))
}

/// Fixed notation with `decimals` digits after the point.
pub fn fixed(x: Num, decimals: usize) -> String {
    if x.is_zero() {
        return if decimals == 0 { String::from("0") } else { alloc::format!("0.{}", "0".repeat(decimals)) };
    }
    let (digits, point) = round_decimal(x, decimals as i64);
    let all: Vec<u8> = if digits.is_empty() { alloc::vec![b'0'] } else { digits };
    // all = 0.ALL * 10^point
    let (int_part, frac_part) = if point <= 0 {
        (String::from("0"), alloc::format!("{}{}", "0".repeat((-point) as usize), text(&all)))
    } else if point as usize >= all.len() {
        (alloc::format!("{}{}", text(&all), "0".repeat(point as usize - all.len())), String::new())
    } else {
        (text(&all[..point as usize]), text(&all[point as usize..]))
    };
    let mut frac = frac_part;
    frac.truncate(decimals);
    while frac.len() < decimals {
        frac.push('0');
    }
    let int_part = if int_part.is_empty() { String::from("0") } else { int_part };
    if decimals == 0 { int_part } else { alloc::format!("{}.{}", int_part, frac) }
}

/// Scientific notation with `decimals` digits after the point.
pub fn exponent(x: Num, decimals: usize, upper: bool) -> String {
    let (digits, point) = if x.is_zero() { (alloc::vec![b'0'; decimals + 1], 1) } else { significant(x, decimals + 1) };
    let mut s = text(&digits[..1]);
    if decimals > 0 {
        s.push('.');
        s.push_str(&text(&digits[1..]));
    }
    let e = if x.is_zero() { 0 } else { point - 1 };
    alloc::format!("{}{}{}{:02}", s, if upper { 'E' } else { 'e' }, if e < 0 { '-' } else { '+' }, e.abs())
}

/// `g` formatting of a positive finite number.
fn general(x: Num, precision: usize, alternate: bool, upper: bool, at_least_one: bool) -> String {
    let p = precision.max(1);
    let exp = if x.is_zero() { 0 } else { significant(x, p).1 - 1 };
    let mut out = if exp >= -4 && exp < p as i32 {
        fixed(x, (p as i32 - 1 - exp) as usize)
    } else {
        exponent(x, p - 1, upper)
    };
    if !alternate {
        // Trailing zeros of the fraction go.
        let (mantissa, rest) = match out.find(['e', 'E']) {
            Some(i) => (String::from(&out[..i]), String::from(&out[i..])),
            None => (out.clone(), String::new()),
        };
        let mantissa = if mantissa.contains('.') {
            let t = mantissa.trim_end_matches('0');
            String::from(t.trim_end_matches('.'))
        } else {
            mantissa
        };
        out = alloc::format!("{}{}", mantissa, rest);
    }
    if at_least_one && !out.contains(['.', 'e', 'E']) {
        out.push_str(".0");
    }
    out
}

fn format_float(vm: &mut Vm, f: Num, spec: &Spec) -> PyResult<String> {
    let kind = spec.kind;
    if matches!(kind, Some('d' | 'b' | 'o' | 'x' | 'X' | 'c')) {
        let message = alloc::format!("Unknown format code '{}' for object of type 'float'", kind.unwrap_or('?'));
        return vm.value_error(&message);
    }
    let upper = matches!(kind, Some('E' | 'F' | 'G'));
    let mut negative = f.is_sign_negative() && !f.is_nan();
    let x = f.abs();
    let mut body = if x.is_nan() || x.is_infinite() {
        let t = if x.is_nan() { "nan" } else { "inf" };
        let t = if upper { t.to_uppercase() } else { String::from(t) };
        if kind == Some('%') { alloc::format!("{}%", t) } else { t }
    } else {
        match kind {
            Some('f' | 'F') => fixed(x, spec.precision.unwrap_or(6)),
            Some('e' | 'E') => exponent(x, spec.precision.unwrap_or(6), upper),
            Some('g' | 'G' | 'n') => general(x, spec.precision.unwrap_or(6), spec.alternate, upper, false),
            Some('%') => alloc::format!("{}%", fixed(x.mul(Num::from_i64(100)), spec.precision.unwrap_or(6))),
            None => match spec.precision {
                None => {
                    let r = float_repr(x);
                    r
                }
                Some(p) => general(x, p, spec.alternate, false, true),
            },
            Some(other) => {
                let message = alloc::format!("Unknown format code '{}' for object of type 'float'", other);
                return vm.value_error(&message);
            }
        }
    };
    if spec.alternate && !body.contains('.') && body.bytes().next().is_some_and(|b| b.is_ascii_digit()) {
        match body.find(['e', 'E', '%']) {
            Some(i) => body.insert(i, '.'),
            None => body.push('.'),
        }
    }
    if spec.z && negative && body.bytes().all(|b| matches!(b, b'0' | b'.' | b'%' | b'e' | b'E' | b'+' | b'-')) {
        negative = false;
    }
    let sign = sign_text(negative, spec.sign);
    let body = match spec.grouping {
        Some(separator) => {
            let end = body.find(['.', 'e', 'E', '%']).unwrap_or(body.len());
            let (int_part, rest) = body.split_at(end);
            let zero_fill = spec.align == Some('=') && spec.fill == '0';
            let target = spec.width.saturating_sub(sign.len() + rest.len());
            alloc::format!("{}{}", group_padded(int_part, separator, 3, target, zero_fill), rest)
        }
        None => body,
    };
    Ok(pad(spec, sign, &body, '>'))
}

fn format_str(vm: &mut Vm, s: &str, spec: &Spec) -> PyResult<String> {
    if let Some(k) = spec.kind {
        if k != 's' {
            let message = alloc::format!("Unknown format code '{}' for object of type 'str'", k);
            return vm.value_error(&message);
        }
    }
    if spec.sign != '-' {
        return vm.value_error("Sign not allowed in string format specifier");
    }
    if spec.alternate {
        return vm.value_error("Alternate form (#) not allowed in string format specifier");
    }
    if spec.align == Some('=') {
        return vm.value_error("'=' alignment not allowed in string format specifier");
    }
    if spec.grouping.is_some() {
        let message = alloc::format!("Cannot specify '{}' with 's'.", spec.grouping.unwrap_or(','));
        return vm.value_error(&message);
    }
    let body: String = match spec.precision {
        Some(p) => s.chars().take(p).collect(),
        None => String::from(s),
    };
    Ok(pad(spec, "", &body, '<'))
}

/// `format(value, spec)`.
pub fn format(vm: &mut Vm, value: &Value, spec: &str) -> PyResult<String> {
    if let Value::Instance(_) = value {
        let r = vm.call_special(value, "__format__", &[Value::str(spec)])?;
        return match r {
            Some(Value::Str(s)) => Ok(String::from(&*s)),
            Some(other) => {
                let name = vm.type_name(&other);
                let message = alloc::format!("__format__ must return a str, not {}", name);
                vm.type_error(&message)
            }
            None => str_of(vm, value),
        };
    }
    if spec.is_empty() {
        return str_of(vm, value);
    }
    let type_name = vm.type_name(value);
    let parsed = parse_spec(vm, spec, &type_name)?;
    match value {
        Value::Int(i) => format_int(vm, i, &parsed, &type_name),
        Value::Bool(b) => format_int(vm, &Int::Small(*b as i64), &parsed, &type_name),
        Value::Float(f) => format_float(vm, *f, &parsed),
        Value::Str(s) => format_str(vm, s, &parsed),
        other => {
            let name = vm.type_name(other);
            let message = alloc::format!("unsupported format string passed to {}.__format__", name);
            vm.type_error(&message)
        }
    }
}

// ============================================================
//  printf-style: "%d" % x
// ============================================================

fn number_for(vm: &mut Vm, value: &Value, conversion: char) -> PyResult<Value> {
    match value {
        Value::Int(_) | Value::Bool(_) | Value::Float(_) => Ok(value.clone()),
        Value::Instance(_) => {
            let inner = vm.unwrap_builtin(value);
            if !inner.is(value) {
                return number_for(vm, &inner, conversion);
            }
            if let Some(r) = vm.call_special(value, if conversion == 'f' { "__float__" } else { "__int__" }, &[])? {
                return Ok(r);
            }
            if let Some(r) = vm.call_special(value, "__index__", &[])? {
                return Ok(r);
            }
            let name = vm.type_name(value);
            let message = alloc::format!("%{} format: a real number is required, not {}", conversion, name);
            vm.type_error(&message)
        }
        other => {
            let name = vm.type_name(other);
            let message = if matches!(conversion, 'x' | 'X' | 'o' | 'c') {
                alloc::format!("%{} format: an integer is required, not {}", conversion, name)
            } else if matches!(conversion, 'd' | 'i' | 'u') {
                alloc::format!("%{} format: a real number is required, not {}", conversion, name)
            } else {
                alloc::format!("must be real number, not {}", name)
            };
            vm.type_error(&message)
        }
    }
}

/// `format % args`.
pub fn percent(vm: &mut Vm, format_text: &str, args: &Value) -> PyResult<String> {
    let mapping = match args {
        Value::Dict(_) => Some(args.clone()),
        Value::Instance(_) if vm.get_attr(args, "keys").is_ok() && !matches!(vm.unwrap_builtin(args), Value::Tuple(_)) => Some(args.clone()),
        _ => None,
    };
    let positional: Vec<Value> = match args {
        Value::Tuple(t) => t.to_vec(),
        other => match vm.unwrap_builtin(other) {
            Value::Tuple(t) => t.to_vec(),
            _ => alloc::vec![other.clone()],
        },
    };
    let chars: Vec<char> = format_text.chars().collect();
    let mut out = String::new();
    let mut next = 0;
    let mut used_mapping = false;
    let mut i = 0;
    let take = |vm: &mut Vm, next: &mut usize| -> PyResult<Value> {
        match positional.get(*next) {
            Some(v) => {
                *next += 1;
                Ok(v.clone())
            }
            None => vm.type_error("not enough arguments for format string"),
        }
    };
    while i < chars.len() {
        let c = chars[i];
        if c != '%' {
            out.push(c);
            i += 1;
            continue;
        }
        i += 1;
        if i >= chars.len() {
            return vm.value_error("incomplete format");
        }
        // %(name)
        let mut keyed = None;
        if chars[i] == '(' {
            let Some(mapping) = &mapping else {
                return vm.type_error("format requires a mapping");
            };
            let mut depth = 1;
            let start = i + 1;
            i += 1;
            while i < chars.len() && depth > 0 {
                match chars[i] {
                    '(' => depth += 1,
                    ')' => depth -= 1,
                    _ => {}
                }
                i += 1;
            }
            if depth > 0 {
                return vm.value_error("incomplete format key");
            }
            let key: String = chars[start..i - 1].iter().collect();
            keyed = Some(vm.get_item(mapping, &Value::str(&key))?);
            used_mapping = true;
        }
        // Flags.
        let mut spec = Spec { fill: ' ', align: Some('>'), sign: '-', z: false, alternate: false, zero: false, width: 0, grouping: None, precision: None, kind: None };
        while i < chars.len() {
            match chars[i] {
                '-' => spec.align = Some('<'),
                '+' => spec.sign = '+',
                ' ' => {
                    if spec.sign != '+' {
                        spec.sign = ' ';
                    }
                }
                '#' => spec.alternate = true,
                '0' => spec.zero = true,
                _ => break,
            }
            i += 1;
        }
        // Width.
        if i < chars.len() && chars[i] == '*' {
            let w = take(vm, &mut next)?;
            let w = super::args::int(vm, &w)?;
            if w < 0 {
                spec.align = Some('<');
            }
            spec.width = w.unsigned_abs() as usize;
            i += 1;
        } else {
            let start = i;
            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }
            if i > start {
                spec.width = chars[start..i].iter().collect::<String>().parse().unwrap_or(0);
            }
        }
        // Precision.
        if i < chars.len() && chars[i] == '.' {
            i += 1;
            if i < chars.len() && chars[i] == '*' {
                let p = take(vm, &mut next)?;
                spec.precision = Some(super::args::int(vm, &p)?.max(0) as usize);
                i += 1;
            } else {
                let start = i;
                while i < chars.len() && chars[i].is_ascii_digit() {
                    i += 1;
                }
                spec.precision = Some(chars[start..i].iter().collect::<String>().parse().unwrap_or(0));
            }
        }
        // Length modifiers are ignored.
        while i < chars.len() && matches!(chars[i], 'h' | 'l' | 'L') {
            i += 1;
        }
        if i >= chars.len() {
            return vm.value_error("incomplete format");
        }
        let conversion = chars[i];
        i += 1;
        if conversion == '%' {
            out.push('%');
            continue;
        }
        let value = match keyed {
            Some(v) => v,
            None => take(vm, &mut next)?,
        };
        if spec.zero && spec.align != Some('<') {
            spec.fill = '0';
            spec.align = Some('=');
        }
        let piece = match conversion {
            's' | 'r' | 'a' => {
                let mut t = match conversion {
                    's' => str_of(vm, &value)?,
                    'r' => repr(vm, &value)?,
                    _ => ascii(&repr(vm, &value)?),
                };
                if let Some(p) = spec.precision {
                    t = t.chars().take(p).collect();
                }
                let plain = Spec { fill: ' ', align: if spec.align == Some('<') { Some('<') } else { Some('>') }, ..spec.clone() };
                pad(&plain, "", &t, '>')
            }
            'd' | 'i' | 'u' => {
                let n = number_for(vm, &value, conversion)?;
                let i = match n {
                    Value::Float(f) => match Int::from_float(f) {
                        Some(i) => i,
                        None => return vm.overflow("cannot convert float infinity to integer"),
                    },
                    Value::Bool(b) => Int::Small(b as i64),
                    Value::Int(i) => i,
                    _ => Int::zero(),
                };
                let digits = int_text(vm, &i.abs())?;
                let digits = match spec.precision {
                    Some(p) if p > digits.len() => alloc::format!("{}{}", "0".repeat(p - digits.len()), digits),
                    _ => digits,
                };
                pad(&spec, sign_text(i.is_negative(), spec.sign), &digits, '>')
            }
            'x' | 'X' | 'o' => {
                let n = number_for(vm, &value, conversion)?;
                let Value::Int(i) = super::types::num::to_int(&n) else {
                    let name = vm.type_name(&n);
                    let message = alloc::format!("%{} format: an integer is required, not {}", conversion, name);
                    return vm.type_error(&message);
                };
                let radix = if conversion == 'o' { 8 } else { 16 };
                let mut digits = i.abs().to_radix(radix);
                if conversion == 'X' {
                    digits = digits.to_uppercase();
                }
                if let Some(p) = spec.precision {
                    if p > digits.len() {
                        digits = alloc::format!("{}{}", "0".repeat(p - digits.len()), digits);
                    }
                }
                let prefix = match (spec.alternate, conversion) {
                    (true, 'x') => "0x",
                    (true, 'X') => "0X",
                    (true, _) => "0o",
                    _ => "",
                };
                let sign = alloc::format!("{}{}", sign_text(i.is_negative(), spec.sign), prefix);
                pad(&spec, &sign, &digits, '>')
            }
            'e' | 'E' | 'f' | 'F' | 'g' | 'G' => {
                let n = number_for(vm, &value, conversion)?;
                let f = match n {
                    Value::Float(f) => f,
                    Value::Int(i) => i.to_float(),
                    Value::Bool(b) => if b { Num::ONE } else { Num::ZERO },
                    _ => Num::ZERO,
                };
                let mut fs = Spec { kind: Some(conversion), precision: Some(spec.precision.unwrap_or(6)), ..spec.clone() };
                if matches!(conversion, 'g' | 'G') && fs.precision == Some(0) {
                    fs.precision = Some(1);
                }
                format_float(vm, f, &fs)?
            }
            'c' => {
                let t = match &value {
                    Value::Str(s) if super::types::str::char_len(s) == 1 => String::from(&**s),
                    Value::Int(i) => {
                        let code = i.to_i64().unwrap_or(-1);
                        match u32::try_from(code).ok().and_then(char::from_u32) {
                            Some(c) => String::from(c),
                            None => return vm.overflow("%c arg not in range(0x110000)"),
                        }
                    }
                    _ => return vm.type_error("%c requires an int or a unicode character, not a string of length 2"),
                };
                pad(&Spec { fill: ' ', ..spec.clone() }, "", &t, '>')
            }
            other => {
                let message = alloc::format!("unsupported format character '{}' (0x{:x}) at index {}", other, other as u32, i - 1);
                return vm.value_error(&message);
            }
        };
        out.push_str(&piece);
    }
    if mapping.is_none() && next < positional.len() && !used_mapping {
        // A single non-tuple argument used zero times is still an error.
        return vm.type_error("not all arguments converted during string formatting");
    }
    Ok(out)
}

// ============================================================
//  str.format
// ============================================================

/// Resolves a field name (`0`, `name`, `0.attr`, `name[key]`).
fn field_value(vm: &mut Vm, field: &str, args: &[Value], kwargs: &[(Rc<str>, Value)], auto: &mut Option<usize>, manual: &mut bool) -> PyResult<Value> {
    let end = field.find(['.', '[']).unwrap_or(field.len());
    let (first, mut rest) = field.split_at(end);
    let mut value = if first.is_empty() {
        if *manual {
            return vm.value_error("cannot switch from manual field specification to automatic field numbering");
        }
        let n = auto.unwrap_or(0);
        *auto = Some(n + 1);
        match args.get(n) {
            Some(v) => v.clone(),
            None => {
                let message = alloc::format!("Replacement index {} out of range for positional args tuple", n);
                return vm.index_error(&message);
            }
        }
    } else if first.bytes().all(|b| b.is_ascii_digit()) {
        if auto.is_some() {
            return vm.value_error("cannot switch from automatic field numbering to manual field specification");
        }
        *manual = true;
        let n: usize = first.parse().unwrap_or(0);
        match args.get(n) {
            Some(v) => v.clone(),
            None => {
                let message = alloc::format!("Replacement index {} out of range for positional args tuple", n);
                return vm.index_error(&message);
            }
        }
    } else {
        match kwargs.iter().find(|(k, _)| &**k == first) {
            Some((_, v)) => v.clone(),
            None => return vm.key_error(Value::str(first)),
        }
    };
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix('.') {
            let end = after.find(['.', '[']).unwrap_or(after.len());
            let (name, tail) = after.split_at(end);
            value = vm.get_attr(&value, name)?;
            rest = tail;
        } else if let Some(after) = rest.strip_prefix('[') {
            let Some(close) = after.find(']') else {
                return vm.value_error("Missing ']' in format string");
            };
            let key = &after[..close];
            let index = if !key.is_empty() && key.bytes().all(|b| b.is_ascii_digit()) {
                Value::int(key.parse().unwrap_or(0))
            } else {
                Value::str(key)
            };
            value = vm.get_item(&value, &index)?;
            rest = &after[close + 1..];
        } else {
            return vm.value_error("Only '.' or '[' may follow ']' in format field specifier");
        }
    }
    Ok(value)
}

pub fn str_format(vm: &mut Vm, template: &str, args: &[Value], kwargs: &[(Rc<str>, Value)]) -> PyResult<String> {
    let mut auto = None;
    let mut manual = false;
    format_with(vm, template, args, kwargs, &mut auto, &mut manual, 2)
}

fn format_with(vm: &mut Vm, template: &str, args: &[Value], kwargs: &[(Rc<str>, Value)], auto: &mut Option<usize>, manual: &mut bool, depth: u32) -> PyResult<String> {
    if depth == 0 {
        return vm.value_error("Max string recursion exceeded");
    }
    let chars: Vec<char> = template.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '}' {
            if chars.get(i + 1) == Some(&'}') {
                out.push('}');
                i += 2;
                continue;
            }
            return vm.value_error("Single '}' encountered in format string");
        }
        if c != '{' {
            out.push(c);
            i += 1;
            continue;
        }
        if chars.get(i + 1) == Some(&'{') {
            out.push('{');
            i += 2;
            continue;
        }
        // A field up to its matching '}'.
        let start = i + 1;
        let mut nesting = 1;
        let mut j = start;
        let mut in_brackets = false;
        while j < chars.len() {
            match chars[j] {
                '[' => in_brackets = true,
                ']' => in_brackets = false,
                '{' if !in_brackets => nesting += 1,
                '}' if !in_brackets => {
                    nesting -= 1;
                    if nesting == 0 {
                        break;
                    }
                }
                _ => {}
            }
            j += 1;
        }
        if j >= chars.len() {
            return vm.value_error(if nesting > 0 { "expected '}' before end of string" } else { "Single '{' encountered in format string" });
        }
        let field: String = chars[start..j].iter().collect();
        i = j + 1;
        // name[!conversion][:spec]
        let mut name_end = field.len();
        let mut bracket = false;
        for (k, ch) in field.char_indices() {
            match ch {
                '[' => bracket = true,
                ']' => bracket = false,
                '!' | ':' if !bracket => {
                    name_end = k;
                    break;
                }
                _ => {}
            }
        }
        let name = &field[..name_end];
        let mut rest = &field[name_end..];
        let mut conversion = None;
        if let Some(after) = rest.strip_prefix('!') {
            let mut it = after.chars();
            conversion = it.next();
            rest = it.as_str();
            if !(rest.is_empty() || rest.starts_with(':')) {
                return vm.value_error("expected ':' after conversion specifier");
            }
            if !matches!(conversion, Some('r' | 's' | 'a')) {
                let message = alloc::format!("Unknown conversion specifier {}", conversion.map(String::from).unwrap_or_default());
                return vm.value_error(&message);
            }
        }
        // The field's value first: it takes its automatic number before
        // the fields nested in its spec.
        let value = field_value(vm, name, args, kwargs, auto, manual)?;
        let spec_template = rest.strip_prefix(':').unwrap_or("");
        let spec = if spec_template.contains('{') {
            format_with(vm, spec_template, args, kwargs, auto, manual, depth - 1)?
        } else {
            String::from(spec_template)
        };
        let value = match conversion {
            Some('r') => Value::str(&repr(vm, &value)?),
            Some('s') => Value::str(&str_of(vm, &value)?),
            Some('a') => Value::str(&ascii(&repr(vm, &value)?)),
            _ => value,
        };
        out.push_str(&format(vm, &value, &spec)?);
    }
    Ok(out)
}

/// The message of an error being printed (`str(e)`), never failing.
pub fn safe_str(vm: &mut Vm, value: &Value) -> String {
    match str_of(vm, value) {
        Ok(s) => s,
        Err(PyErr::Halt(reason)) => reason,
        Err(_) => String::from("<exception str() failed>"),
    }
}
