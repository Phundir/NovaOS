//! `String`. Positions are UTF-16 units, as JavaScript counts them; the
//! text itself is UTF-8, and `JsStr` translates.

use alloc::string::String;
use alloc::vec::Vec;

use super::arg;
use crate::web::js::interp::{Ctx, R};
use crate::web::js::num::Num;
use crate::web::js::ops;
use crate::web::js::value::*;

/// `this` as a string (primitive, wrapper, or anything converted).
pub fn this_string(ctx: &mut Ctx, this: &Value) -> R<JsStr> {
    match this {
        Value::Str(s) => Ok(s.clone()),
        Value::Obj(object) => {
            if let Kind::String(s) = &object.borrow().kind {
                return Ok(s.clone());
            }
            ops::to_string(ctx, this)
        }
        Value::Undefined | Value::Null => ctx.throw_type("String.prototype method called on null or undefined"),
        other => ops::to_string(ctx, other),
    }
}

fn string_constructor(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let text = match args.first() {
        None => JsStr::from(""),
        Some(Value::Sym(key)) if ctx.new_target.is_none() => JsStr::from(alloc::format!("Symbol({})", symbol_description(key))),
        Some(value) => ops::to_string(ctx, value)?,
    };
    if ctx.new_target.is_some() {
        let proto = ctx.proto_for_new(ctx.realm.protos.string.clone())?;
        return Ok(Value::Obj(ctx.alloc(Kind::String(text), Some(proto), "String")));
    }
    Ok(Value::Str(text))
}

fn push_unit(out: &mut String, pending: &mut Option<u16>, unit: u16) {
    if let Some(high) = pending.take() {
        if (0xdc00..0xe000).contains(&unit) {
            let code = 0x10000 + (((high as u32) - 0xd800) << 10) + (unit as u32 - 0xdc00);
            out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
            return;
        }
        out.push('\u{fffd}');
    }
    if (0xd800..0xdc00).contains(&unit) {
        *pending = Some(unit);
    } else {
        out.push(char::from_u32(unit as u32).unwrap_or('\u{fffd}'));
    }
}

fn from_char_code(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let mut out = String::new();
    let mut pending = None;
    for value in args {
        let unit = ops::to_number(ctx, value)?.to_u32() as u16;
        push_unit(&mut out, &mut pending, unit);
    }
    if pending.is_some() {
        out.push('\u{fffd}');
    }
    Ok(Value::Str(JsStr::from(out)))
}

fn from_code_point(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let mut out = String::new();
    for value in args {
        let n = ops::to_number(ctx, value)?;
        let code = n.to_i64_exact().filter(|c| (0..=0x10ffff).contains(c));
        match code {
            Some(code) => out.push(char::from_u32(code as u32).unwrap_or('\u{fffd}')),
            None => return ctx.throw_range("Invalid code point"),
        }
    }
    Ok(Value::Str(JsStr::from(out)))
}

fn raw(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let strings = arg(args, 0);
    let raw = ops::get(ctx, &strings, "raw")?;
    let parts = ops::list_from_array_like(ctx, &raw)?;
    let mut out = String::new();
    for (index, part) in parts.iter().enumerate() {
        out.push_str(&ops::to_string(ctx, part)?);
        if index + 1 < parts.len() {
            if let Some(value) = args.get(index + 1) {
                out.push_str(&ops::to_string(ctx, value)?);
            }
        }
    }
    Ok(Value::Str(JsStr::from(out)))
}

fn char_at(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let s = this_string(ctx, this)?;
    let index = ops::to_integer(ctx, &arg(args, 0))?;
    if index < 0 {
        return Ok(Value::str(""));
    }
    Ok(ops::string_index(&s, index as usize).unwrap_or_else(|| Value::str("")))
}

fn char_code_at(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let s = this_string(ctx, this)?;
    let index = ops::to_integer(ctx, &arg(args, 0))?;
    if index < 0 {
        return Ok(Value::Num(Num::NAN));
    }
    Ok(s.unit_at(index as usize).map_or(Value::Num(Num::NAN), |u| Value::num(u as i64)))
}

fn code_point_at(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let s = this_string(ctx, this)?;
    let index = ops::to_integer(ctx, &arg(args, 0))?;
    if index < 0 || index as usize >= s.units() {
        return Ok(Value::Undefined);
    }
    let unit = s.unit_at(index as usize).unwrap_or(0);
    if (0xd800..0xdc00).contains(&unit) {
        if let Some(low) = s.unit_at(index as usize + 1).filter(|l| (0xdc00..0xe000).contains(l)) {
            return Ok(Value::num(0x10000 + (((unit as i64) - 0xd800) << 10) + (low as i64 - 0xdc00)));
        }
    }
    Ok(Value::num(unit as i64))
}

fn at(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let s = this_string(ctx, this)?;
    let index = ops::to_integer(ctx, &arg(args, 0))?;
    let index = if index < 0 { s.units() as i64 + index } else { index };
    if index < 0 {
        return Ok(Value::Undefined);
    }
    Ok(ops::string_index(&s, index as usize).unwrap_or(Value::Undefined))
}

fn index_of(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let s = this_string(ctx, this)?;
    let needle = ops::to_string(ctx, &arg(args, 0))?;
    let from = ops::to_integer(ctx, &arg(args, 1))?.clamp(0, s.units() as i64) as usize;
    let start = s.byte_of(from);
    Ok(match s[start..].find(needle.as_str()) {
        Some(found) => Value::num(s.unit_of(start + found) as i64),
        None => Value::num(-1),
    })
}

fn last_index_of(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let s = this_string(ctx, this)?;
    let needle = ops::to_string(ctx, &arg(args, 0))?;
    let position = ops::to_number(ctx, &arg(args, 1))?;
    let limit = if position.is_nan() { s.units() } else { position.to_i64_saturating().clamp(0, s.units() as i64) as usize };
    let end = (s.byte_of(limit) + needle.len()).min(s.len());
    Ok(match s[..end].rfind(needle.as_str()) {
        Some(found) => Value::num(s.unit_of(found) as i64),
        None => Value::num(-1),
    })
}

fn includes(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    if matches!(arg(args, 0), Value::Obj(ref o) if matches!(o.borrow().kind, Kind::RegExp(_))) {
        return ctx.throw_type("First argument to String.prototype.includes must not be a regular expression");
    }
    let found = index_of(ctx, this, args, &Value::Undefined)?;
    Ok(Value::Bool(!matches!(found, Value::Num(n) if n.is_sign_negative())))
}

fn starts_with(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let s = this_string(ctx, this)?;
    let needle = ops::to_string(ctx, &arg(args, 0))?;
    let from = ops::to_integer(ctx, &arg(args, 1))?.clamp(0, s.units() as i64) as usize;
    Ok(Value::Bool(s[s.byte_of(from)..].starts_with(needle.as_str())))
}

fn ends_with(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let s = this_string(ctx, this)?;
    let needle = ops::to_string(ctx, &arg(args, 0))?;
    let end = match arg(args, 1) {
        Value::Undefined => s.units(),
        other => ops::to_integer(ctx, &other)?.clamp(0, s.units() as i64) as usize,
    };
    Ok(Value::Bool(s[..s.byte_of(end)].ends_with(needle.as_str())))
}

fn slice(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let s = this_string(ctx, this)?;
    let length = s.units();
    let start = ops::relative_index(ctx, args.first(), length, 0)?;
    let end = ops::relative_index(ctx, args.get(1), length, length)?;
    Ok(Value::Str(s.slice(start, end)))
}

fn substring(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let s = this_string(ctx, this)?;
    let length = s.units() as i64;
    let start = ops::to_integer(ctx, &arg(args, 0))?.clamp(0, length) as usize;
    let end = match arg(args, 1) {
        Value::Undefined => length as usize,
        other => ops::to_integer(ctx, &other)?.clamp(0, length) as usize,
    };
    Ok(Value::Str(s.slice(start.min(end), start.max(end))))
}

fn substr(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let s = this_string(ctx, this)?;
    let length = s.units();
    let start = ops::relative_index(ctx, args.first(), length, 0)?;
    let count = match arg(args, 1) {
        Value::Undefined => length - start,
        other => ops::to_integer(ctx, &other)?.clamp(0, (length - start) as i64) as usize,
    };
    Ok(Value::Str(s.slice(start, start + count)))
}

fn to_upper_case(ctx: &mut Ctx, this: &Value, _: &[Value], _: &Value) -> R<Value> {
    let s = this_string(ctx, this)?;
    Ok(Value::Str(JsStr::from(s.to_uppercase())))
}

fn to_lower_case(ctx: &mut Ctx, this: &Value, _: &[Value], _: &Value) -> R<Value> {
    let s = this_string(ctx, this)?;
    Ok(Value::Str(JsStr::from(s.to_lowercase())))
}

fn js_space(c: char) -> bool {
    c.is_whitespace() || c == '\u{feff}'
}

fn trim(ctx: &mut Ctx, this: &Value, _: &[Value], _: &Value) -> R<Value> {
    let s = this_string(ctx, this)?;
    Ok(Value::str(s.trim_matches(js_space)))
}

fn trim_start(ctx: &mut Ctx, this: &Value, _: &[Value], _: &Value) -> R<Value> {
    let s = this_string(ctx, this)?;
    Ok(Value::str(s.trim_start_matches(js_space)))
}

fn trim_end(ctx: &mut Ctx, this: &Value, _: &[Value], _: &Value) -> R<Value> {
    let s = this_string(ctx, this)?;
    Ok(Value::str(s.trim_end_matches(js_space)))
}

fn pad(ctx: &mut Ctx, this: &Value, args: &[Value], at_start: bool) -> R<Value> {
    let s = this_string(ctx, this)?;
    let target = ops::to_integer(ctx, &arg(args, 0))?.max(0) as usize;
    let filler = match arg(args, 1) {
        Value::Undefined => JsStr::from(" "),
        other => ops::to_string(ctx, &other)?,
    };
    if target <= s.units() || filler.is_empty() {
        return Ok(Value::Str(s));
    }
    if target > ops::MAX_STRING {
        return ctx.throw_range("Invalid string length");
    }
    let missing = target - s.units();
    let mut padding = String::new();
    let filler_chars: Vec<char> = filler.chars().collect();
    let mut count = 0;
    while count < missing {
        let c = filler_chars[count % filler_chars.len()];
        padding.push(c);
        count += c.len_utf16();
    }
    Ok(Value::Str(JsStr::from(if at_start { padding + &s } else { String::from(s.as_str()) + &padding })))
}

fn pad_start(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    pad(ctx, this, args, true)
}

fn pad_end(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    pad(ctx, this, args, false)
}

fn repeat(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let s = this_string(ctx, this)?;
    let count = ops::to_number(ctx, &arg(args, 0))?;
    if count.is_sign_negative() && !count.is_zero() || count.is_infinite() {
        return ctx.throw_range("Invalid count value");
    }
    let count = count.to_i64_saturating().max(0) as usize;
    if s.len().saturating_mul(count) > ops::MAX_STRING {
        return ctx.throw_range("Invalid string length");
    }
    Ok(Value::Str(JsStr::from(s.repeat(count))))
}

fn concat(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let mut out = String::from(this_string(ctx, this)?.as_str());
    for value in args {
        out.push_str(&ops::to_string(ctx, value)?);
        if out.len() > ops::MAX_STRING {
            return ctx.throw_range("Invalid string length");
        }
    }
    Ok(Value::Str(JsStr::from(out)))
}

fn locale_compare(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let s = this_string(ctx, this)?;
    let other = ops::to_string(ctx, &arg(args, 0))?;
    // Case-insensitive first, then case: close to a locale collation.
    let key = |t: &str| t.to_lowercase();
    let order = key(&s).cmp(&key(&other)).then_with(|| other.as_str().cmp(s.as_str()));
    Ok(Value::num(match order {
        core::cmp::Ordering::Less => -1,
        core::cmp::Ordering::Equal => 0,
        core::cmp::Ordering::Greater => 1,
    }))
}

fn identity(ctx: &mut Ctx, this: &Value, _: &[Value], _: &Value) -> R<Value> {
    Ok(Value::Str(this_string(ctx, this)?))
}

fn value_of(ctx: &mut Ctx, this: &Value, _: &[Value], _: &Value) -> R<Value> {
    match this {
        Value::Str(s) => Ok(Value::Str(s.clone())),
        Value::Obj(object) => match &object.borrow().kind {
            Kind::String(s) => Ok(Value::Str(s.clone())),
            _ => ctx.throw_type("String.prototype.valueOf requires that 'this' be a String"),
        },
        _ => ctx.throw_type("String.prototype.valueOf requires that 'this' be a String"),
    }
}

fn is_well_formed(_: &mut Ctx, _: &Value, _: &[Value], _: &Value) -> R<Value> {
    Ok(Value::Bool(true))
}

fn is_regexp(value: &Value) -> bool {
    matches!(value, Value::Obj(o) if matches!(o.borrow().kind, Kind::RegExp(_)))
}

/// `GetSubstitution`: `$&`, `$1`..`$99`, `$<name>`, `` $` ``, `$'`, `$$`.
pub fn substitute(matched: &str, before: &str, after: &str, captures: &[Value], named: &Value, replacement: &str) -> String {
    let mut out = String::new();
    let bytes = replacement.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let b = bytes[index];
        if b != b'$' || index + 1 >= bytes.len() {
            let c = replacement[index..].chars().next().unwrap_or('$');
            out.push(c);
            index += c.len_utf8();
            continue;
        }
        let next = bytes[index + 1];
        match next {
            b'$' => {
                out.push('$');
                index += 2;
            }
            b'&' => {
                out.push_str(matched);
                index += 2;
            }
            b'`' => {
                out.push_str(before);
                index += 2;
            }
            b'\'' => {
                out.push_str(after);
                index += 2;
            }
            b'0'..=b'9' => {
                let one = (next - b'0') as usize;
                let two = bytes.get(index + 2).filter(|d| d.is_ascii_digit()).map(|d| one * 10 + (d - b'0') as usize);
                let (number, used) = match two {
                    Some(n) if n >= 1 && n <= captures.len() => (n, 3),
                    _ => (one, 2),
                };
                if number >= 1 && number <= captures.len() {
                    if let Value::Str(text) = &captures[number - 1] {
                        out.push_str(text);
                    }
                    index += used;
                } else {
                    out.push('$');
                    index += 1;
                }
            }
            b'<' if !named.is_undefined() => match replacement[index + 2..].find('>') {
                Some(end) => {
                    let name = &replacement[index + 2..index + 2 + end];
                    if let Value::Obj(groups) = named {
                        if let Some(Value::Str(text)) = groups.own_value(name) {
                            out.push_str(&text);
                        }
                    }
                    index += 3 + end;
                }
                None => {
                    out.push('$');
                    index += 1;
                }
            },
            _ => {
                out.push('$');
                index += 1;
            }
        }
    }
    out
}

fn replace_impl(ctx: &mut Ctx, this: &Value, args: &[Value], all: bool) -> R<Value> {
    let pattern = arg(args, 0);
    if is_regexp(&pattern) {
        let s = this_string(ctx, this)?;
        return super::regexp::replace(ctx, &pattern, &s, &arg(args, 1), all);
    }
    let s = this_string(ctx, this)?;
    let needle = ops::to_string(ctx, &pattern)?;
    let replacement = arg(args, 1);
    let functional = replacement.as_obj().is_some_and(|r| r.is_callable());
    let replacement_text = if functional { JsStr::from("") } else { ops::to_string(ctx, &replacement)? };
    let mut positions = Vec::new();
    if needle.is_empty() {
        if all {
            let mut byte = 0;
            loop {
                positions.push(byte);
                match s[byte..].chars().next() {
                    Some(c) => byte += c.len_utf8(),
                    None => break,
                }
            }
        } else {
            positions.push(0);
        }
    } else {
        let mut from = 0;
        while let Some(found) = s[from..].find(needle.as_str()) {
            positions.push(from + found);
            from += found + needle.len();
            if !all {
                break;
            }
        }
    }
    if positions.is_empty() {
        return Ok(Value::Str(s));
    }
    let mut out = String::new();
    let mut last = 0;
    for position in positions {
        ctx.tick()?;
        out.push_str(&s[last..position]);
        let piece = if functional {
            let result = ctx.call(
                &replacement,
                Value::Undefined,
                &[Value::Str(needle.clone()), Value::num(s.unit_of(position) as i64), Value::Str(s.clone())],
            )?;
            String::from(ops::to_string(ctx, &result)?.as_str())
        } else {
            substitute(&needle, &s[..position], &s[position + needle.len()..], &[], &Value::Undefined, &replacement_text)
        };
        out.push_str(&piece);
        last = position + needle.len();
        if out.len() > ops::MAX_STRING {
            return ctx.throw_range("Invalid string length");
        }
    }
    out.push_str(&s[last..]);
    Ok(Value::Str(JsStr::from(out)))
}

fn replace(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    replace_impl(ctx, this, args, false)
}

fn replace_all(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    replace_impl(ctx, this, args, true)
}

fn split(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let separator = arg(args, 0);
    let limit = match arg(args, 1) {
        Value::Undefined => u32::MAX as usize,
        other => ops::to_number(ctx, &other)?.to_u32() as usize,
    };
    if is_regexp(&separator) {
        let s = this_string(ctx, this)?;
        return super::regexp::split(ctx, &separator, &s, limit);
    }
    let s = this_string(ctx, this)?;
    let mut parts: Vec<Value> = Vec::new();
    if limit == 0 {
        return Ok(Value::Obj(ctx.new_array(parts)));
    }
    if separator.is_undefined() {
        parts.push(Value::Str(s));
        return Ok(Value::Obj(ctx.new_array(parts)));
    }
    let separator = ops::to_string(ctx, &separator)?;
    if separator.is_empty() {
        for index in 0..s.units().min(limit) {
            parts.push(ops::string_index(&s, index).unwrap_or(Value::Undefined));
        }
        return Ok(Value::Obj(ctx.new_array(parts)));
    }
    for piece in s.split(separator.as_str()) {
        if parts.len() >= limit {
            break;
        }
        parts.push(Value::str(piece));
    }
    Ok(Value::Obj(ctx.new_array(parts)))
}

fn to_regexp(ctx: &mut Ctx, value: &Value, flags: &str) -> R<Value> {
    if is_regexp(value) {
        return Ok(value.clone());
    }
    let pattern = match value {
        Value::Undefined => JsStr::from("(?:)"),
        other => ops::to_string(ctx, other)?,
    };
    super::regexp::create(ctx, &pattern, flags)
}

fn match_(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let s = this_string(ctx, this)?;
    let regexp = to_regexp(ctx, &arg(args, 0), "")?;
    super::regexp::match_string(ctx, &regexp, &s)
}

fn match_all(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let s = this_string(ctx, this)?;
    let pattern = arg(args, 0);
    if is_regexp(&pattern) {
        let flags = ops::get(ctx, &pattern, "flags")?;
        if !ops::to_display(&flags).contains('g') {
            return ctx.throw_type("String.prototype.matchAll called with a non-global RegExp argument");
        }
    }
    let regexp = to_regexp(ctx, &pattern, "g")?;
    super::regexp::match_all(ctx, &regexp, &s)
}

fn search(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let s = this_string(ctx, this)?;
    let regexp = to_regexp(ctx, &arg(args, 0), "")?;
    super::regexp::search(ctx, &regexp, &s)
}

fn normalize(ctx: &mut Ctx, this: &Value, _: &[Value], _: &Value) -> R<Value> {
    // Composition tables are not carried: the text is returned as is.
    Ok(Value::Str(this_string(ctx, this)?))
}

/// Legacy HTML methods: `"x".bold()` and friends.
fn html_wrap(ctx: &mut Ctx, this: &Value, _: &[Value], data: &Value) -> R<Value> {
    let s = this_string(ctx, this)?;
    let tag = ops::to_display(data);
    Ok(Value::Str(JsStr::from(alloc::format!("<{0}>{1}</{0}>", tag, s.as_str()))))
}

pub fn install(ctx: &mut Ctx) {
    let proto = ctx.realm.protos.string.clone();
    let constructor = super::constructor(ctx, "String", 1, string_constructor, &proto);
    ctx.method(&constructor, "fromCharCode", 1, from_char_code);
    ctx.method(&constructor, "fromCodePoint", 1, from_code_point);
    ctx.method(&constructor, "raw", 1, raw);
    let methods: &[(&str, usize, NativeFn)] = &[
        ("charAt", 1, char_at),
        ("charCodeAt", 1, char_code_at),
        ("codePointAt", 1, code_point_at),
        ("at", 1, at),
        ("indexOf", 1, index_of),
        ("lastIndexOf", 1, last_index_of),
        ("includes", 1, includes),
        ("startsWith", 1, starts_with),
        ("endsWith", 1, ends_with),
        ("slice", 2, slice),
        ("substring", 2, substring),
        ("substr", 2, substr),
        ("toUpperCase", 0, to_upper_case),
        ("toLowerCase", 0, to_lower_case),
        ("toLocaleUpperCase", 0, to_upper_case),
        ("toLocaleLowerCase", 0, to_lower_case),
        ("trim", 0, trim),
        ("trimStart", 0, trim_start),
        ("trimEnd", 0, trim_end),
        ("trimLeft", 0, trim_start),
        ("trimRight", 0, trim_end),
        ("padStart", 2, pad_start),
        ("padEnd", 2, pad_end),
        ("repeat", 1, repeat),
        ("concat", 1, concat),
        ("localeCompare", 1, locale_compare),
        ("toString", 0, value_of),
        ("valueOf", 0, value_of),
        ("replace", 2, replace),
        ("replaceAll", 2, replace_all),
        ("split", 2, split),
        ("match", 1, match_),
        ("matchAll", 1, match_all),
        ("search", 1, search),
        ("normalize", 0, normalize),
        ("isWellFormed", 0, is_well_formed),
        ("toWellFormed", 0, identity),
    ];
    for (name, length, function) in methods {
        ctx.method(&proto, name, *length, *function);
    }
    for (name, tag) in [("bold", "b"), ("italics", "i"), ("big", "big"), ("small", "small"), ("strike", "strike"), ("sub", "sub"), ("sup", "sup")] {
        let function = ctx.native_with(name, 0, html_wrap, Value::str(tag));
        proto.put_hidden(name, Value::Obj(function));
    }
    let symbol = ctx.realm.symbols.iterator.clone();
    let iterator = ctx.native("[Symbol.iterator]", 0, super::array::values);
    proto.put_key(symbol, Value::Obj(iterator), HIDDEN);
}
