//! `RegExp`, and the regular-expression paths of `String.prototype`
//! (`replace`, `split`, `match`, `matchAll`, `search`).
//!
//! Matching is `regex::Regex`, a backtracking machine with a step bound;
//! this file speaks the language's protocol around it: `lastIndex`, the
//! `g` and `y` flags, the result array with `index`, `input`, `groups`
//! and, under `d`, `indices`. Positions are UTF-16 units, as scripts
//! count them; the machine works on characters, and `Subject` keeps the
//! three numberings of one text (characters, UTF-16 units, bytes) side by
//! side, built once per call rather than once per match.

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use super::arg;
use super::string::substitute;
use crate::web::js::interp::{Ctx, R};
use crate::web::js::ops;
use crate::web::js::regex::{Input, Regex};
use crate::web::js::value::*;

/// One text, prepared for matching.
struct Subject {
    text: JsStr,
    input: Input,
    /// Byte offset of each character, and of the end.
    bytes: Vec<usize>,
}

impl Subject {
    fn new(text: &JsStr) -> Subject {
        let mut bytes: Vec<usize> = text.char_indices().map(|(b, _)| b).collect();
        bytes.push(text.len());
        Subject { text: text.clone(), input: Input::new(text), bytes }
    }

    fn slice(&self, from: usize, to: usize) -> &str {
        &self.text[self.bytes[from.min(self.bytes.len() - 1)]..self.bytes[to.min(self.bytes.len() - 1)]]
    }

    fn unit(&self, index: usize) -> usize {
        self.input.unit(index)
    }
}

type Captures = Vec<Option<(usize, usize)>>;

fn regex_of(value: &Value) -> Option<Rc<Regex>> {
    match value {
        Value::Obj(object) => match &object.borrow().kind {
            Kind::RegExp(regex) => Some(regex.clone()),
            _ => None,
        },
        _ => None,
    }
}

fn this_regex(ctx: &mut Ctx, this: &Value, method: &str) -> R<Rc<Regex>> {
    match regex_of(this) {
        Some(regex) => Ok(regex),
        None => {
            let message = alloc::format!("RegExp.prototype.{} requires that 'this' be a RegExp object", method);
            ctx.throw_type(&message)
        }
    }
}

/// Makes a regular expression object; a bad pattern is a SyntaxError.
pub fn create(ctx: &mut Ctx, pattern: &str, flags: &str) -> R<Value> {
    let proto = ctx.realm.protos.regexp.clone();
    create_with(ctx, pattern, flags, proto)
}

fn create_with(ctx: &mut Ctx, pattern: &str, flags: &str, proto: Obj) -> R<Value> {
    let regex = match Regex::new(pattern, flags) {
        Ok(regex) => regex,
        Err(message) => {
            let message = alloc::format!("Invalid regular expression: /{}/: {}", pattern, message);
            return ctx.throw_error("SyntaxError", &message);
        }
    };
    let object = ctx.alloc(Kind::RegExp(Rc::new(regex)), Some(proto), "RegExp");
    object.put_key(JsStr::from("lastIndex"), Value::num(0), WRITABLE);
    Ok(Value::Obj(object))
}

fn last_index(ctx: &mut Ctx, regexp: &Value) -> R<usize> {
    let value = ops::get(ctx, regexp, "lastIndex")?;
    Ok(ops::to_integer(ctx, &value)?.max(0) as usize)
}

fn set_last_index(ctx: &mut Ctx, regexp: &Value, index: usize) -> R<()> {
    ops::set(ctx, regexp, JsStr::from("lastIndex"), Value::num(index as i64))
}

/// One match from character `start`, under the step bound of the machine.
fn exec_at(ctx: &mut Ctx, regex: &Regex, subject: &Subject, start: usize) -> R<Option<Captures>> {
    ctx.tick()?;
    if start > subject.input.len() {
        return Ok(None);
    }
    Ok(regex.exec_chars(&subject.input, start))
}

/// `RegExpBuiltinExec`: honours and updates `lastIndex` for `g` and `y`.
fn builtin_exec(ctx: &mut Ctx, regexp: &Value, regex: &Regex, subject: &Subject) -> R<Option<Captures>> {
    let tracking = regex.flags.global || regex.flags.sticky;
    let start_unit = if tracking { last_index(ctx, regexp)? } else { 0 };
    if start_unit > subject.text.units() {
        if tracking {
            set_last_index(ctx, regexp, 0)?;
        }
        return Ok(None);
    }
    let start = subject.input.index_of_unit(start_unit);
    let found = exec_at(ctx, regex, subject, start)?;
    match &found {
        None => {
            if tracking {
                set_last_index(ctx, regexp, 0)?;
            }
        }
        Some(captures) => {
            if tracking {
                let end = captures[0].map_or(start, |(_, e)| e);
                set_last_index(ctx, regexp, subject.unit(end))?;
            }
        }
    }
    Ok(found)
}

/// The `groups` object of a match, or undefined without named groups.
fn groups_object(ctx: &mut Ctx, regex: &Regex, subject: &Subject, captures: &Captures) -> Value {
    if regex.names.is_empty() {
        return Value::Undefined;
    }
    let groups = ctx.alloc(Kind::Ordinary, None, "Object");
    for (name, index) in &regex.names {
        let value = match captures.get(*index).copied().flatten() {
            Some((a, b)) => Value::str(subject.slice(a, b)),
            None => Value::Undefined,
        };
        groups.put(name, value);
    }
    Value::Obj(groups)
}

fn capture_values(subject: &Subject, captures: &Captures) -> Vec<Value> {
    captures
        .iter()
        .map(|c| match c {
            Some((a, b)) => Value::str(subject.slice(*a, *b)),
            None => Value::Undefined,
        })
        .collect()
}

/// The array `exec` returns.
fn result_array(ctx: &mut Ctx, regex: &Regex, subject: &Subject, captures: &Captures) -> Value {
    let values = capture_values(subject, captures);
    let array = ctx.new_array(values);
    let start = captures[0].map_or(0, |(a, _)| a);
    array.put("index", Value::num(subject.unit(start) as i64));
    array.put("input", Value::Str(subject.text.clone()));
    let groups = groups_object(ctx, regex, subject, captures);
    array.put("groups", groups);
    if regex.flags.has_indices {
        let pairs: Vec<Value> = captures
            .iter()
            .map(|c| match c {
                Some((a, b)) => {
                    let pair = ctx.new_array(alloc::vec![Value::num(subject.unit(*a) as i64), Value::num(subject.unit(*b) as i64)]);
                    Value::Obj(pair)
                }
                None => Value::Undefined,
            })
            .collect();
        let indices = ctx.new_array(pairs);
        array.put("indices", Value::Obj(indices));
    }
    Value::Obj(array)
}

fn exec(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let regex = this_regex(ctx, this, "exec")?;
    let text = ops::to_string(ctx, &arg(args, 0))?;
    let subject = Subject::new(&text);
    match builtin_exec(ctx, this, &regex, &subject)? {
        Some(captures) => Ok(result_array(ctx, &regex, &subject, &captures)),
        None => Ok(Value::Null),
    }
}

fn test(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let regex = this_regex(ctx, this, "test")?;
    let text = ops::to_string(ctx, &arg(args, 0))?;
    let subject = Subject::new(&text);
    Ok(Value::Bool(builtin_exec(ctx, this, &regex, &subject)?.is_some()))
}

/// Every match from the start, advancing past empty matches -- what the
/// global paths of `replace`, `match` and `matchAll` walk.
fn all_matches(ctx: &mut Ctx, regex: &Regex, subject: &Subject, limit: usize) -> R<Vec<Captures>> {
    let mut out = Vec::new();
    let mut position = 0;
    while position <= subject.input.len() && out.len() < limit {
        let Some(captures) = exec_at(ctx, regex, subject, position)? else {
            break;
        };
        let (start, end) = captures[0].unwrap_or((position, position));
        position = if end == start { end + 1 } else { end };
        if regex.flags.sticky && start != out.last().and_then(|c: &Captures| c[0]).map_or(0, |(_, e)| e) {
            break;
        }
        out.push(captures);
    }
    Ok(out)
}

pub fn replace(ctx: &mut Ctx, regexp: &Value, s: &JsStr, replacement: &Value, all: bool) -> R<Value> {
    let Some(regex) = regex_of(regexp) else {
        return ctx.throw_type("not a RegExp");
    };
    if all && !regex.flags.global {
        return ctx.throw_type("replaceAll must be called with a global RegExp");
    }
    let subject = Subject::new(s);
    let matches = if regex.flags.global {
        set_last_index(ctx, regexp, 0)?;
        all_matches(ctx, &regex, &subject, usize::MAX)?
    } else {
        builtin_exec(ctx, regexp, &regex, &subject)?.into_iter().collect()
    };
    let functional = replacement.as_obj().is_some_and(|r| r.is_callable());
    let template = if functional { JsStr::from("") } else { ops::to_string(ctx, replacement)? };
    let mut out = String::new();
    let mut last = 0;
    for captures in matches {
        let Some((start, end)) = captures[0] else {
            continue;
        };
        out.push_str(subject.slice(last, start));
        let values = capture_values(&subject, &captures);
        let groups = groups_object(ctx, &regex, &subject, &captures);
        let piece = if functional {
            let mut call_args = values.clone();
            call_args.push(Value::num(subject.unit(start) as i64));
            call_args.push(Value::Str(s.clone()));
            if !groups.is_undefined() {
                call_args.push(groups);
            }
            let result = ctx.call(replacement, Value::Undefined, &call_args)?;
            String::from(ops::to_string(ctx, &result)?.as_str())
        } else {
            substitute(
                subject.slice(start, end),
                subject.slice(0, start),
                subject.slice(end, subject.input.len()),
                &values[1..],
                &groups,
                &template,
            )
        };
        out.push_str(&piece);
        ops::check_string(ctx, out.len())?;
        last = end;
    }
    out.push_str(subject.slice(last, subject.input.len()));
    Ok(Value::Str(JsStr::from(out)))
}

pub fn split(ctx: &mut Ctx, regexp: &Value, s: &JsStr, limit: usize) -> R<Value> {
    let Some(regex) = regex_of(regexp) else {
        return ctx.throw_type("not a RegExp");
    };
    let mut parts: Vec<Value> = Vec::new();
    if limit == 0 {
        return Ok(Value::Obj(ctx.new_array(parts)));
    }
    let subject = Subject::new(s);
    let length = subject.input.len();
    if length == 0 {
        if exec_at(ctx, &regex, &subject, 0)?.is_none() {
            parts.push(Value::Str(s.clone()));
        }
        return Ok(Value::Obj(ctx.new_array(parts)));
    }
    // The specification's walk: `last` is where the current piece
    // starts, `q` where the separator is looked for. A match ending where
    // the piece starts (an empty one there) cuts nothing.
    let mut last = 0;
    let mut q = 0;
    while q < length {
        let Some(captures) = exec_at(ctx, &regex, &subject, q)? else {
            break;
        };
        let Some((start, end)) = captures[0] else {
            break;
        };
        if start >= length {
            break;
        }
        if end == last {
            q = start + 1;
            continue;
        }
        parts.push(Value::str(subject.slice(last, start)));
        if parts.len() >= limit {
            return Ok(Value::Obj(ctx.new_array(parts)));
        }
        for value in capture_values(&subject, &captures).into_iter().skip(1) {
            parts.push(value);
            if parts.len() >= limit {
                return Ok(Value::Obj(ctx.new_array(parts)));
            }
        }
        last = end;
        q = end;
    }
    parts.push(Value::str(subject.slice(last, length)));
    Ok(Value::Obj(ctx.new_array(parts)))
}

pub fn match_string(ctx: &mut Ctx, regexp: &Value, s: &JsStr) -> R<Value> {
    let Some(regex) = regex_of(regexp) else {
        return ctx.throw_type("not a RegExp");
    };
    let subject = Subject::new(s);
    if !regex.flags.global {
        return Ok(match builtin_exec(ctx, regexp, &regex, &subject)? {
            Some(captures) => result_array(ctx, &regex, &subject, &captures),
            None => Value::Null,
        });
    }
    set_last_index(ctx, regexp, 0)?;
    let matches = all_matches(ctx, &regex, &subject, usize::MAX)?;
    if matches.is_empty() {
        return Ok(Value::Null);
    }
    let values: Vec<Value> = matches
        .iter()
        .filter_map(|c| c[0])
        .map(|(a, b)| Value::str(subject.slice(a, b)))
        .collect();
    Ok(Value::Obj(ctx.new_array(values)))
}

/// `matchAll`: computed at once, handed out by an iterator.
pub fn match_all(ctx: &mut Ctx, regexp: &Value, s: &JsStr) -> R<Value> {
    let Some(regex) = regex_of(regexp) else {
        return ctx.throw_type("not a RegExp");
    };
    let subject = Subject::new(s);
    let start = subject.input.index_of_unit(last_index(ctx, regexp)?);
    let mut results = Vec::new();
    let mut position = start;
    while position <= subject.input.len() {
        let Some(captures) = exec_at(ctx, &regex, &subject, position)? else {
            break;
        };
        let (a, b) = captures[0].unwrap_or((position, position));
        results.push(result_array(ctx, &regex, &subject, &captures));
        position = if a == b { b + 1 } else { b };
        if !regex.flags.global {
            break;
        }
    }
    let proto = ctx.realm.protos.iterator.clone();
    Ok(Value::Obj(ctx.alloc(Kind::Iterator(IterState::List(results, 0)), Some(proto), "RegExp String Iterator")))
}

pub fn search(ctx: &mut Ctx, regexp: &Value, s: &JsStr) -> R<Value> {
    let Some(regex) = regex_of(regexp) else {
        return ctx.throw_type("not a RegExp");
    };
    let subject = Subject::new(s);
    Ok(match exec_at(ctx, &regex, &subject, 0)? {
        Some(captures) => Value::num(captures[0].map_or(-1, |(a, _)| subject.unit(a) as i64)),
        None => Value::num(-1),
    })
}

// ------------------------------------------------------------
//  Constructor and accessors
// ------------------------------------------------------------

fn regexp_constructor(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let pattern = arg(args, 0);
    let flags = arg(args, 1);
    if ctx.new_target.is_none() {
        if let Some(_) = regex_of(&pattern) {
            if flags.is_undefined() {
                return Ok(pattern);
            }
        }
    }
    let (source, inherited) = match regex_of(&pattern) {
        Some(regex) => (String::from(regex.source.as_str()), Some(flags_text(&regex))),
        None => match &pattern {
            Value::Undefined => (String::new(), None),
            other => (String::from(ops::to_string(ctx, other)?.as_str()), None),
        },
    };
    let flags = match flags {
        Value::Undefined => inherited.unwrap_or_default(),
        other => String::from(ops::to_string(ctx, &other)?.as_str()),
    };
    let proto = ctx.proto_for_new(ctx.realm.protos.regexp.clone())?;
    create_with(ctx, &source, &flags, proto)
}

fn flags_text(regex: &Regex) -> String {
    let f = regex.flags;
    let mut out = String::new();
    for (on, letter) in [
        (f.has_indices, 'd'),
        (f.global, 'g'),
        (f.ignore_case, 'i'),
        (f.multiline, 'm'),
        (f.dot_all, 's'),
        (f.unicode, 'u'),
        (f.sticky, 'y'),
    ] {
        if on {
            out.push(letter);
        }
    }
    out
}

/// `source`, escaped so that `/${source}/` reads back as the same
/// expression.
fn source_text(regex: &Regex) -> String {
    if regex.source.is_empty() {
        return String::from("(?:)");
    }
    let mut out = String::new();
    let mut escaped = false;
    let mut in_class = false;
    for c in regex.source.chars() {
        match c {
            '/' if !escaped && !in_class => out.push_str("\\/"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ => out.push(c),
        }
        if !escaped {
            match c {
                '[' => in_class = true,
                ']' => in_class = false,
                _ => {}
            }
        }
        escaped = c == '\\' && !escaped;
    }
    out
}

fn source(ctx: &mut Ctx, this: &Value, _: &[Value], _: &Value) -> R<Value> {
    if let Value::Obj(object) = this {
        if object.ptr_eq(&ctx.realm.protos.regexp) {
            return Ok(Value::str("(?:)"));
        }
    }
    let regex = this_regex(ctx, this, "source")?;
    Ok(Value::Str(JsStr::from(source_text(&regex))))
}

fn flags(ctx: &mut Ctx, this: &Value, _: &[Value], _: &Value) -> R<Value> {
    if let Value::Obj(object) = this {
        if object.ptr_eq(&ctx.realm.protos.regexp) {
            return Ok(Value::str(""));
        }
    }
    let regex = this_regex(ctx, this, "flags")?;
    Ok(Value::Str(JsStr::from(flags_text(&regex))))
}

/// The boolean flag accessors; data says which flag.
fn flag(ctx: &mut Ctx, this: &Value, _: &[Value], data: &Value) -> R<Value> {
    if let Value::Obj(object) = this {
        if object.ptr_eq(&ctx.realm.protos.regexp) {
            return Ok(Value::Undefined);
        }
    }
    let regex = this_regex(ctx, this, "flag")?;
    let letter = match data {
        Value::Str(s) => s.chars().next().unwrap_or(' '),
        _ => ' ',
    };
    Ok(Value::Bool(flags_text(&regex).contains(letter)))
}

fn to_string(ctx: &mut Ctx, this: &Value, _: &[Value], _: &Value) -> R<Value> {
    let source = ops::get(ctx, this, "source")?;
    let source = ops::to_string(ctx, &source)?;
    let flags = ops::get(ctx, this, "flags")?;
    let flags = ops::to_string(ctx, &flags)?;
    Ok(Value::Str(JsStr::from(alloc::format!("/{}/{}", source.as_str(), flags.as_str()))))
}

/// `RegExp.escape` (ES2025): every character that means something in a
/// pattern, escaped.
fn escape(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let Value::Str(text) = arg(args, 0) else {
        return ctx.throw_type("RegExp.escape requires a string");
    };
    let mut out = String::new();
    for (index, c) in text.chars().enumerate() {
        if index == 0 && c.is_ascii_alphanumeric() {
            out.push_str(&alloc::format!("\\x{:02x}", c as u32));
        } else if "^$\\.*+?()[]{}|/".contains(c) {
            out.push('\\');
            out.push(c);
        } else if ",-=<>#&!%:;@~'`\"".contains(c) || c.is_whitespace() {
            out.push_str(&alloc::format!("\\x{:02x}", c as u32 & 0xff));
        } else {
            out.push(c);
        }
    }
    Ok(Value::Str(JsStr::from(out)))
}

pub fn install(ctx: &mut Ctx) {
    let proto = ctx.realm.protos.regexp.clone();
    let constructor = super::constructor(ctx, "RegExp", 2, regexp_constructor, &proto);
    ctx.method(&constructor, "escape", 1, escape);
    ctx.method(&proto, "exec", 1, exec);
    ctx.method(&proto, "test", 1, test);
    ctx.method(&proto, "toString", 0, to_string);
    ctx.accessor(&proto, "source", Some(source), None);
    ctx.accessor(&proto, "flags", Some(flags), None);
    for (name, letter) in [
        ("global", "g"),
        ("ignoreCase", "i"),
        ("multiline", "m"),
        ("dotAll", "s"),
        ("unicode", "u"),
        ("sticky", "y"),
        ("hasIndices", "d"),
    ] {
        let getter = ctx.native_with(name, 0, flag, Value::str(letter));
        proto.borrow_mut().props.set(JsStr::from(name), Slot::Accessor(Some(getter), None, CONFIGURABLE));
    }
}
