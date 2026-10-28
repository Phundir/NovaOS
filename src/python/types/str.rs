//! `str`: Python strings are sequences of code points. They are kept as
//! UTF-8 (`Rc<str>`); indices count characters, so an index into a
//! non-ASCII string walks it -- ASCII strings, nearly all of them, go
//! straight to the byte.

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use super::super::args::{self, this_str};
use super::super::value::*;
use super::super::vm::{PyResult, Vm};
use super::method;

/// Byte offset of character `index` (clamped to the end).
pub fn byte_of(text: &str, index: usize) -> usize {
    if text.is_ascii() {
        return index.min(text.len());
    }
    text.char_indices().nth(index).map_or(text.len(), |(b, _)| b)
}

pub fn char_len(text: &str) -> usize {
    if text.is_ascii() { text.len() } else { text.chars().count() }
}

/// `start`/`end` arguments of find/count/startswith, as byte offsets.
fn range(vm: &mut Vm, text: &str, start: Option<&Value>, end: Option<&Value>) -> PyResult<(usize, usize)> {
    let len = char_len(text) as i64;
    let clamp = |v: i64| -> usize {
        let v = if v < 0 { v + len } else { v };
        v.clamp(0, len) as usize
    };
    let start = clamp(args::opt_int(vm, start, 0)?);
    let end = clamp(args::opt_int(vm, end, len)?);
    Ok((byte_of(text, start), byte_of(text, end.max(start))))
}

pub fn new_str(vm: &mut Vm, _class: &Rc<Class>, args: &[Value], kwargs: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let Some(value) = args::get(args, kwargs, 0, "object") else {
        return Ok(Value::str(""));
    };
    if let Value::Bytes(b) = value {
        if args::get(args, kwargs, 1, "encoding").is_some() {
            return match core::str::from_utf8(b) {
                Ok(s) => Ok(Value::str(s)),
                Err(_) => {
                    let class = vm.types.unicode_error.clone();
                    Err(vm.error(&class, "'utf-8' codec can't decode bytes"))
                }
            };
        }
    }
    let text = vm.str_of(value)?;
    Ok(Value::str(&text))
}

fn ret(text: String) -> PyResult<Value> {
    Ok(Value::str(&text))
}

fn upper(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let s = this_str(vm, a, "upper")?;
    ret(s.to_uppercase())
}

fn lower(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let s = this_str(vm, a, "lower")?;
    ret(s.to_lowercase())
}

fn casefold(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let s = this_str(vm, a, "casefold")?;
    ret(s.to_lowercase().replace('\u{df}', "ss"))
}

fn swapcase(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let s = this_str(vm, a, "swapcase")?;
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_uppercase() {
            out.extend(c.to_lowercase());
        } else if c.is_lowercase() {
            out.extend(c.to_uppercase());
        } else {
            out.push(c);
        }
    }
    ret(out)
}

fn capitalize(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let s = this_str(vm, a, "capitalize")?;
    let mut chars = s.chars();
    let mut out = String::with_capacity(s.len());
    if let Some(first) = chars.next() {
        out.extend(first.to_uppercase());
        for c in chars {
            out.extend(c.to_lowercase());
        }
    }
    ret(out)
}

fn title(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let s = this_str(vm, a, "title")?;
    let mut out = String::with_capacity(s.len());
    let mut previous_cased = false;
    for c in s.chars() {
        if previous_cased {
            out.extend(c.to_lowercase());
        } else {
            out.extend(c.to_uppercase());
        }
        previous_cased = c.is_alphabetic();
    }
    ret(out)
}

fn strip_chars<'a>(vm: &mut Vm, a: &'a [Value]) -> PyResult<Option<Vec<char>>> {
    match a.get(1) {
        None | Some(Value::None) => Ok(None),
        Some(v) => {
            let chars = args::string(vm, v, "strip arg")?;
            Ok(Some(chars.chars().collect()))
        }
    }
}

fn strip_with(vm: &mut Vm, a: &[Value], left: bool, right: bool, name: &str) -> PyResult<Value> {
    let s = this_str(vm, a, name)?;
    let chars = strip_chars(vm, a)?;
    let matches = |c: char| match &chars {
        None => c.is_whitespace(),
        Some(set) => set.contains(&c),
    };
    let mut out: &str = &s;
    if left {
        out = out.trim_start_matches(matches);
    }
    if right {
        out = out.trim_end_matches(matches);
    }
    if out.len() == s.len() {
        return Ok(a[0].clone());
    }
    Ok(Value::str(out))
}

fn strip(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    strip_with(vm, a, true, true, "strip")
}

fn lstrip(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    strip_with(vm, a, true, false, "lstrip")
}

fn rstrip(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    strip_with(vm, a, false, true, "rstrip")
}

/// `split` and `rsplit`.
fn split_with(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)], reverse: bool) -> PyResult<Value> {
    let s = this_str(vm, a, "split")?;
    let sep = args::get(a, kw, 1, "sep").cloned().unwrap_or(Value::None);
    let maxsplit = args::opt_int(vm, args::get(a, kw, 2, "maxsplit"), -1)?;
    let mut parts: Vec<String> = Vec::new();
    match sep {
        Value::None => {
            // Runs of whitespace; no empty strings.
            let words: Vec<&str> = s.split_whitespace().collect();
            if maxsplit < 0 || (words.len() as i64) <= maxsplit + 1 {
                parts = words.iter().map(|w| String::from(*w)).collect();
            } else if !reverse {
                let mut rest: &str = s.trim_start();
                for _ in 0..maxsplit {
                    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
                    parts.push(String::from(&rest[..end]));
                    rest = rest[end..].trim_start();
                }
                if !rest.is_empty() {
                    parts.push(String::from(rest));
                }
            } else {
                let mut rest: &str = s.trim_end();
                let mut tail = Vec::new();
                for _ in 0..maxsplit {
                    let start = rest.rfind(char::is_whitespace).map_or(0, |i| i + rest[i..].chars().next().map_or(1, |c| c.len_utf8()));
                    tail.push(String::from(&rest[start..]));
                    rest = rest[..start].trim_end();
                }
                if !rest.is_empty() {
                    tail.push(String::from(rest));
                }
                tail.reverse();
                parts = tail;
            }
        }
        other => {
            let sep = args::string(vm, &other, "sep")?;
            if sep.is_empty() {
                return vm.value_error("empty separator");
            }
            if maxsplit < 0 {
                parts = s.split(&*sep).map(String::from).collect();
            } else if !reverse {
                parts = s.splitn(maxsplit as usize + 1, &*sep).map(String::from).collect();
            } else {
                parts = s.rsplitn(maxsplit as usize + 1, &*sep).map(String::from).collect();
                parts.reverse();
            }
        }
    }
    Ok(Value::list(parts.iter().map(|p| Value::str(p)).collect()))
}

fn split(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    split_with(vm, a, kw, false)
}

fn rsplit(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    split_with(vm, a, kw, true)
}

fn splitlines(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let s = this_str(vm, a, "splitlines")?;
    let keep = match args::get(a, kw, 1, "keepends") {
        Some(v) => vm.truthy(v)?,
        None => false,
    };
    let mut out = Vec::new();
    let mut line = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        let is_break = matches!(c, '\n' | '\r' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}' | '\u{2029}');
        if !is_break {
            line.push(c);
            continue;
        }
        let mut ending = String::from(c);
        if c == '\r' && chars.peek() == Some(&'\n') {
            chars.next();
            ending.push('\n');
        }
        if keep {
            line.push_str(&ending);
        }
        out.push(Value::str(&line));
        line.clear();
    }
    if !line.is_empty() {
        out.push(Value::str(&line));
    }
    Ok(Value::list(out))
}

fn join(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let s = this_str(vm, a, "join")?;
    let Some(iterable) = a.get(1) else {
        return vm.type_error("str.join() takes exactly one argument (0 given)");
    };
    let items = vm.collect(iterable)?;
    let mut out = String::new();
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            out.push_str(&s);
        }
        match item {
            Value::Str(t) => out.push_str(t),
            other => {
                let name = vm.type_name(other);
                let message = alloc::format!("sequence item {}: expected str instance, {} found", i, name);
                return vm.type_error(&message);
            }
        }
    }
    vm.checked_str(out)
}

fn replace(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let s = this_str(vm, a, "replace")?;
    let (Some(old), Some(new)) = (a.get(1), a.get(2)) else {
        return vm.type_error("replace() takes at least 2 arguments");
    };
    let old = args::string(vm, old, "replace() argument 1")?;
    let new = args::string(vm, new, "replace() argument 2")?;
    let count = args::opt_int(vm, args::get(a, kw, 3, "count"), -1)?;
    if count < 0 {
        if old.is_empty() {
            let mut out = String::from(&*new);
            for c in s.chars() {
                out.push(c);
                out.push_str(&new);
            }
            return vm.checked_str(out);
        }
        return vm.checked_str(s.replace(&*old, &new));
    }
    vm.checked_str(s.replacen(&*old, &new, count as usize))
}

/// find, rfind, index, rindex: (reverse, raise).
fn find_with(vm: &mut Vm, a: &[Value], reverse: bool, raise: bool) -> PyResult<Value> {
    let s = this_str(vm, a, "find")?;
    let Some(sub) = a.get(1) else {
        return vm.type_error("find() takes at least 1 argument (0 given)");
    };
    let sub = args::string(vm, sub, "must be str")?;
    let (start, end) = range(vm, &s, a.get(2), a.get(3))?;
    let window = &s[start..end];
    let found = if reverse { window.rfind(&*sub) } else { window.find(&*sub) };
    match found {
        Some(at) => {
            let index = char_len(&s[..start + at]);
            Ok(Value::int(index as i64))
        }
        None if raise => vm.value_error("substring not found"),
        None => Ok(Value::int(-1)),
    }
}

fn find(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    find_with(vm, a, false, false)
}

fn rfind(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    find_with(vm, a, true, false)
}

fn index(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    find_with(vm, a, false, true)
}

fn rindex(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    find_with(vm, a, true, true)
}

fn count(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let s = this_str(vm, a, "count")?;
    let Some(sub) = a.get(1) else {
        return vm.type_error("count() takes at least 1 argument (0 given)");
    };
    let sub = args::string(vm, sub, "must be str")?;
    let (start, end) = range(vm, &s, a.get(2), a.get(3))?;
    let window = &s[start..end];
    let n = if sub.is_empty() { char_len(window) + 1 } else { window.matches(&*sub).count() };
    Ok(Value::int(n as i64))
}

fn affix(vm: &mut Vm, a: &[Value], end: bool) -> PyResult<Value> {
    let s = this_str(vm, a, if end { "endswith" } else { "startswith" })?;
    let Some(needle) = a.get(1) else {
        return vm.type_error("startswith() takes at least 1 argument (0 given)");
    };
    let (start, stop) = range(vm, &s, a.get(2), a.get(3))?;
    let window = &s[start..stop];
    let candidates: Vec<Value> = match needle {
        Value::Tuple(t) => t.to_vec(),
        other => alloc::vec![other.clone()],
    };
    for candidate in candidates {
        let c = match &candidate {
            Value::Str(c) => c.clone(),
            other => {
                let name = vm.type_name(other);
                let message = alloc::format!("{} first arg must be str or a tuple of str, not {}", if end { "endswith" } else { "startswith" }, name);
                return vm.type_error(&message);
            }
        };
        if (end && window.ends_with(&*c)) || (!end && window.starts_with(&*c)) {
            return Ok(Value::Bool(true));
        }
    }
    Ok(Value::Bool(false))
}

fn startswith(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    affix(vm, a, false)
}

fn endswith(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    affix(vm, a, true)
}

fn predicate(vm: &mut Vm, a: &[Value], name: &str, test: fn(char) -> bool, empty: bool) -> PyResult<Value> {
    let s = this_str(vm, a, name)?;
    if s.is_empty() {
        return Ok(Value::Bool(empty));
    }
    Ok(Value::Bool(s.chars().all(test)))
}

fn isdigit(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    predicate(vm, a, "isdigit", |c| c.is_ascii_digit() || matches!(c, '\u{b2}' | '\u{b3}' | '\u{b9}') || (c.is_numeric() && c.to_digit(10).is_some()), false)
}

fn isdecimal(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    predicate(vm, a, "isdecimal", |c| c.is_ascii_digit(), false)
}

fn isnumeric(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    predicate(vm, a, "isnumeric", |c| c.is_numeric(), false)
}

fn isalpha(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    predicate(vm, a, "isalpha", |c| c.is_alphabetic(), false)
}

fn isalnum(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    predicate(vm, a, "isalnum", |c| c.is_alphanumeric(), false)
}

fn isspace(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    predicate(vm, a, "isspace", |c| c.is_whitespace(), false)
}

fn isascii(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    predicate(vm, a, "isascii", |c| c.is_ascii(), true)
}

fn isprintable(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    predicate(vm, a, "isprintable", |c| !c.is_control(), true)
}

fn isupper(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let s = this_str(vm, a, "isupper")?;
    Ok(Value::Bool(s.chars().any(|c| c.is_uppercase()) && !s.chars().any(|c| c.is_lowercase())))
}

fn islower(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let s = this_str(vm, a, "islower")?;
    Ok(Value::Bool(s.chars().any(|c| c.is_lowercase()) && !s.chars().any(|c| c.is_uppercase())))
}

fn istitle(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let s = this_str(vm, a, "istitle")?;
    let mut previous_cased = false;
    let mut any = false;
    for c in s.chars() {
        if c.is_uppercase() {
            if previous_cased {
                return Ok(Value::Bool(false));
            }
            previous_cased = true;
            any = true;
        } else if c.is_lowercase() {
            if !previous_cased {
                return Ok(Value::Bool(false));
            }
            previous_cased = true;
            any = true;
        } else {
            previous_cased = false;
        }
    }
    Ok(Value::Bool(any))
}

fn isidentifier(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let s = this_str(vm, a, "isidentifier")?;
    let mut chars = s.chars();
    let ok = match chars.next() {
        Some(c) if c == '_' || c.is_alphabetic() => chars.all(|c| c == '_' || c.is_alphanumeric()),
        _ => false,
    };
    Ok(Value::Bool(ok))
}

fn pad(vm: &mut Vm, a: &[Value], align: u8) -> PyResult<Value> {
    let s = this_str(vm, a, "center")?;
    let width = match a.get(1) {
        Some(w) => args::int(vm, w)?,
        None => return vm.type_error("center() takes at least 1 argument (0 given)"),
    };
    let fill = match a.get(2) {
        Some(f) => {
            let f = args::string(vm, f, "fill character")?;
            let mut chars = f.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => c,
                _ => return vm.type_error("The fill character must be exactly one character long"),
            }
        }
        None => ' ',
    };
    let len = char_len(&s) as i64;
    if width <= len {
        return Ok(a[0].clone());
    }
    let margin = (width - len) as usize;
    let (left, right) = match align {
        b'<' => (0, margin),
        b'>' => (margin, 0),
        _ => {
            let left = margin / 2 + (margin & width as usize & 1);
            (left, margin - left)
        }
    };
    let mut out = String::new();
    for _ in 0..left {
        out.push(fill);
    }
    out.push_str(&s);
    for _ in 0..right {
        out.push(fill);
    }
    ret(out)
}

fn center(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    pad(vm, a, b'^')
}

fn ljust(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    pad(vm, a, b'<')
}

fn rjust(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    pad(vm, a, b'>')
}

fn zfill(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let s = this_str(vm, a, "zfill")?;
    let width = match a.get(1) {
        Some(w) => args::int(vm, w)?,
        None => return vm.type_error("zfill() takes exactly one argument (0 given)"),
    };
    let len = char_len(&s) as i64;
    if width <= len {
        return Ok(a[0].clone());
    }
    let zeros = "0".repeat((width - len) as usize);
    let out = match s.chars().next() {
        Some(sign @ ('+' | '-')) => alloc::format!("{}{}{}", sign, zeros, &s[1..]),
        _ => alloc::format!("{}{}", zeros, s),
    };
    ret(out)
}

fn partition_with(vm: &mut Vm, a: &[Value], reverse: bool) -> PyResult<Value> {
    let s = this_str(vm, a, "partition")?;
    let Some(sep) = a.get(1) else {
        return vm.type_error("partition() takes exactly one argument (0 given)");
    };
    let sep = args::string(vm, sep, "must be str")?;
    if sep.is_empty() {
        return vm.value_error("empty separator");
    }
    let found = if reverse { s.rfind(&*sep) } else { s.find(&*sep) };
    let parts = match found {
        Some(i) => [Value::str(&s[..i]), Value::Str(sep.clone()), Value::str(&s[i + sep.len()..])],
        None if reverse => [Value::str(""), Value::str(""), Value::Str(s.clone())],
        None => [Value::Str(s.clone()), Value::str(""), Value::str("")],
    };
    Ok(Value::tuple(parts.to_vec()))
}

fn partition(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    partition_with(vm, a, false)
}

fn rpartition(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    partition_with(vm, a, true)
}

fn removeprefix(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let s = this_str(vm, a, "removeprefix")?;
    let prefix = match a.get(1) {
        Some(p) => args::string(vm, p, "removeprefix() argument")?,
        None => return vm.type_error("removeprefix() takes exactly one argument (0 given)"),
    };
    Ok(Value::str(s.strip_prefix(&*prefix).unwrap_or(&s)))
}

fn removesuffix(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let s = this_str(vm, a, "removesuffix")?;
    let suffix = match a.get(1) {
        Some(p) => args::string(vm, p, "removesuffix() argument")?,
        None => return vm.type_error("removesuffix() takes exactly one argument (0 given)"),
    };
    Ok(Value::str(s.strip_suffix(&*suffix).unwrap_or(&s)))
}

fn expandtabs(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let s = this_str(vm, a, "expandtabs")?;
    let size = args::opt_int(vm, args::get(a, kw, 1, "tabsize"), 8)?.max(0) as usize;
    let mut out = String::new();
    let mut column = 0;
    for c in s.chars() {
        match c {
            '\t' => {
                if size > 0 {
                    let n = size - column % size;
                    out.push_str(&" ".repeat(n));
                    column += n;
                }
            }
            '\n' | '\r' => {
                out.push(c);
                column = 0;
            }
            _ => {
                out.push(c);
                column += 1;
            }
        }
    }
    ret(out)
}

fn format(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let s = this_str(vm, a, "format")?;
    let text = super::super::format::str_format(vm, &s, &a[1..], kw)?;
    ret(text)
}

fn format_map(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let s = this_str(vm, a, "format_map")?;
    let Some(mapping) = a.get(1) else {
        return vm.type_error("format_map() takes exactly one argument (0 given)");
    };
    let pairs = vm.mapping_items(mapping)?;
    let mut kw = Vec::new();
    for (k, v) in pairs {
        if let Value::Str(k) = k {
            kw.push((k, v));
        }
    }
    let text = super::super::format::str_format(vm, &s, &[], &kw)?;
    ret(text)
}

fn encode(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let s = this_str(vm, a, "encode")?;
    Ok(Value::Bytes(Rc::from(s.as_bytes())))
}

fn translate(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let s = this_str(vm, a, "translate")?;
    let Some(table) = a.get(1).cloned() else {
        return vm.type_error("translate() takes exactly one argument (0 given)");
    };
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match vm.get_item(&table, &Value::int(c as i64)) {
            Ok(Value::None) => {}
            Ok(Value::Str(r)) => out.push_str(&r),
            Ok(Value::Int(i)) => out.push(i.to_i64().and_then(|n| char::from_u32(n as u32)).unwrap_or(c)),
            Ok(_) => out.push(c),
            Err(e) if vm.is_instance_of(&e, &vm.types.lookup_error.clone()) => out.push(c),
            Err(e) => return Err(e),
        }
    }
    ret(out)
}

fn maketrans(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    // A static method: no self.
    let dict = Value::Dict(Rc::new(core::cell::RefCell::new(super::super::dict::Dict::new())));
    let Value::Dict(d) = &dict else {
        return Ok(Value::None);
    };
    match (a.first(), a.get(1)) {
        (Some(Value::Dict(source)), None) => {
            let pairs: Vec<(Value, Value)> = source.borrow().iter().map(|e| (e.key.clone(), e.value.clone())).collect();
            for (k, v) in pairs {
                let key = match &k {
                    Value::Str(s) => Value::int(s.chars().next().map_or(0, |c| c as i64)),
                    other => other.clone(),
                };
                vm.dict_insert(d, key, v)?;
            }
        }
        (Some(Value::Str(x)), Some(Value::Str(y))) => {
            if char_len(x) != char_len(y) {
                return vm.value_error("the first two maketrans arguments must have equal length");
            }
            for (from, to) in x.chars().zip(y.chars()) {
                vm.dict_insert(d, Value::int(from as i64), Value::int(to as i64))?;
            }
            if let Some(Value::Str(z)) = a.get(2) {
                for c in z.chars() {
                    vm.dict_insert(d, Value::int(c as i64), Value::None)?;
                }
            }
        }
        _ => return vm.type_error("maketrans() arguments invalides"),
    }
    Ok(dict)
}

pub fn install(vm: &mut Vm) {
    let c = vm.types.str.clone();
    let entries: &[(&'static str, NativeFn)] = &[
        ("upper", upper), ("lower", lower), ("casefold", casefold), ("swapcase", swapcase), ("capitalize", capitalize),
        ("title", title), ("strip", strip), ("lstrip", lstrip), ("rstrip", rstrip), ("split", split), ("rsplit", rsplit),
        ("splitlines", splitlines), ("join", join), ("replace", replace), ("find", find), ("rfind", rfind),
        ("index", index), ("rindex", rindex), ("count", count), ("startswith", startswith), ("endswith", endswith),
        ("isdigit", isdigit), ("isdecimal", isdecimal), ("isnumeric", isnumeric), ("isalpha", isalpha),
        ("isalnum", isalnum), ("isspace", isspace), ("isascii", isascii), ("isprintable", isprintable),
        ("isupper", isupper), ("islower", islower), ("istitle", istitle), ("isidentifier", isidentifier),
        ("center", center), ("ljust", ljust), ("rjust", rjust), ("zfill", zfill), ("partition", partition),
        ("rpartition", rpartition), ("removeprefix", removeprefix), ("removesuffix", removesuffix),
        ("expandtabs", expandtabs), ("format", format), ("format_map", format_map), ("encode", encode),
        ("translate", translate),
    ];
    for (name, f) in entries {
        method(&c, name, *f);
    }
    c.dict.borrow_mut().set_str("maketrans", Value::StaticMethod(Rc::new(super::native("maketrans", maketrans))));
}
