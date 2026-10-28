//! The functions of the global object that belong to no constructor:
//! `console`, the timers, `queueMicrotask`, `structuredClone`, the URI
//! and base64 functions.
//!
//! What a browser adds besides (`window`, `document`, `location`,
//! `localStorage`...) is the DOM's, in `dom.rs`: a script run by
//! `dev interpret` has a console and timers but no document.
//!
//! Timers do not run by themselves: the realm keeps them with their due
//! time, and the host runs those that are due (`Ctx::run_timers`) when it
//! gets control -- between two frames of the browser, or when a script
//! under `dev interpret` has finished and its timers are drained.

use alloc::string::String;
use alloc::vec::Vec;

use super::arg;
use crate::web::js::interp::{Ctx, Job, Timer, R};
use crate::web::js::num::Num;
use crate::web::js::ops;
use crate::web::js::value::*;

// ------------------------------------------------------------
//  console
// ------------------------------------------------------------

/// `console.log` formatting: `%s`, `%d`/`%i`, `%f`, `%o`/`%O`, `%c` in
/// the first argument when it is a string, then the rest joined by
/// spaces. Strings print raw, everything else as the console shows it.
pub fn format_args(ctx: &mut Ctx, args: &[Value]) -> R<String> {
    let mut out = String::new();
    let mut rest = 0;
    if let Some(Value::Str(first)) = args.first() {
        rest = 1;
        let mut chars = first.chars().peekable();
        while let Some(c) = chars.next() {
            if c != '%' {
                out.push(c);
                continue;
            }
            match chars.peek().copied() {
                Some('%') => {
                    chars.next();
                    out.push('%');
                }
                Some(spec @ ('s' | 'd' | 'i' | 'f' | 'o' | 'O' | 'c' | 'j')) => {
                    chars.next();
                    if spec == 'c' {
                        // CSS for the console: nothing to style here.
                        rest += 1;
                        continue;
                    }
                    let Some(value) = args.get(rest) else {
                        out.push('%');
                        out.push(spec);
                        continue;
                    };
                    rest += 1;
                    match spec {
                        's' => match value {
                            Value::Str(s) => out.push_str(s),
                            other => out.push_str(&ops::to_display(other)),
                        },
                        'd' | 'i' => {
                            let n = ops::to_number(ctx, value)?;
                            out.push_str(&ops::number_string(if n.is_finite() { n.trunc() } else { n }));
                        }
                        'f' => {
                            let n = ops::to_number(ctx, value)?;
                            out.push_str(&ops::number_string(n));
                        }
                        _ => out.push_str(&ops::to_display(value)),
                    }
                }
                _ => out.push('%'),
            }
        }
    }
    for value in args.iter().skip(rest) {
        if !out.is_empty() || rest > 0 {
            out.push(' ');
        }
        match value {
            Value::Str(s) => out.push_str(s),
            other => out.push_str(&ops::to_display(other)),
        }
    }
    Ok(out)
}

/// One line on the console of the realm, and to the host's log.
pub fn console_line(ctx: &mut Ctx, level: &str, text: &str) {
    let line = if level.is_empty() {
        String::from(text)
    } else {
        alloc::format!("[{}] {}", level, text)
    };
    let indent = "  ".repeat(group_depth(ctx));
    let line = alloc::format!("{}{}", indent, line);
    ctx.host.log(&line);
    if ctx.realm.console.len() < 2000 {
        ctx.realm.console.push(line);
    }
}

fn console_object(ctx: &mut Ctx) -> Option<Obj> {
    match ctx.realm.global.own_value("console") {
        Some(Value::Obj(console)) => Some(console),
        _ => None,
    }
}

fn group_depth(ctx: &mut Ctx) -> usize {
    console_object(ctx)
        .and_then(|c| match c.borrow().props.get(&crate::web::js::interp::private_key("depth")) {
            Some(Slot::Data(Value::Num(n), _)) => Some(n.to_i64_saturating().max(0) as usize),
            _ => None,
        })
        .unwrap_or(0)
}

fn set_group_depth(ctx: &mut Ctx, depth: usize) {
    if let Some(console) = console_object(ctx) {
        console.put_key(crate::web::js::interp::private_key("depth"), Value::num(depth as i64), WRITABLE);
    }
}

/// `log`, `info`, `warn`, `error`, `debug`, `trace`, `dir`: data is the
/// level written before the line ("" for log).
fn console_log(ctx: &mut Ctx, _: &Value, args: &[Value], data: &Value) -> R<Value> {
    let text = format_args(ctx, args)?;
    let level = match data {
        Value::Str(s) => String::from(s.as_str()),
        _ => String::new(),
    };
    console_line(ctx, &level, &text);
    Ok(Value::Undefined)
}

fn console_assert(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    if arg(args, 0).truthy() {
        return Ok(Value::Undefined);
    }
    let rest = if args.len() > 1 { &args[1..] } else { &[][..] };
    let text = format_args(ctx, rest)?;
    let text = if text.is_empty() { String::from("Assertion failed") } else { alloc::format!("Assertion failed: {}", text) };
    console_line(ctx, "erreur", &text);
    Ok(Value::Undefined)
}

/// Counters and timers of the console live in a hidden table on it.
fn console_table(ctx: &mut Ctx, name: &str) -> Option<Obj> {
    let console = console_object(ctx)?;
    let key = crate::web::js::interp::private_key(name);
    if let Some(Slot::Data(Value::Obj(table), _)) = console.borrow().props.get(&key) {
        return Some(table.clone());
    }
    let table = ctx.alloc(Kind::Ordinary, None, "Table");
    console.put_key(key, Value::Obj(table.clone()), WRITABLE);
    Some(table)
}

fn label_of(ctx: &mut Ctx, args: &[Value]) -> R<JsStr> {
    match arg(args, 0) {
        Value::Undefined => Ok(JsStr::from("default")),
        other => ops::to_string(ctx, &other),
    }
}

fn console_count(ctx: &mut Ctx, _: &Value, args: &[Value], data: &Value) -> R<Value> {
    let label = label_of(ctx, args)?;
    let Some(table) = console_table(ctx, "counts") else {
        return Ok(Value::Undefined);
    };
    if data.truthy() {
        table.put_key(label, Value::num(0), DEFAULT);
        return Ok(Value::Undefined);
    }
    let count = match table.own_value(&label) {
        Some(Value::Num(n)) => n.to_i64_saturating() + 1,
        _ => 1,
    };
    table.put_key(label.clone(), Value::num(count), DEFAULT);
    console_line(ctx, "", &alloc::format!("{}: {}", label.as_str(), count));
    Ok(Value::Undefined)
}

/// `time` (data 0), `timeLog` (1), `timeEnd` (2).
fn console_time(ctx: &mut Ctx, _: &Value, args: &[Value], data: &Value) -> R<Value> {
    let label = label_of(ctx, args)?;
    let mode = match data {
        Value::Num(n) => n.to_i64_saturating(),
        _ => 0,
    };
    let now = ctx.now();
    let Some(table) = console_table(ctx, "timers") else {
        return Ok(Value::Undefined);
    };
    if mode == 0 {
        table.put_key(label, Value::num(now), DEFAULT);
        return Ok(Value::Undefined);
    }
    match table.own_value(&label) {
        Some(Value::Num(start)) => {
            let elapsed = now - start.to_i64_saturating();
            console_line(ctx, "", &alloc::format!("{}: {} ms", label.as_str(), elapsed));
            if mode == 2 {
                table.borrow_mut().props.remove(&label);
            }
        }
        _ => console_line(ctx, "avertissement", &alloc::format!("Timer '{}' does not exist", label.as_str())),
    }
    Ok(Value::Undefined)
}

fn console_group(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    if !args.is_empty() {
        let text = format_args(ctx, args)?;
        console_line(ctx, "", &text);
    }
    let depth = group_depth(ctx);
    set_group_depth(ctx, depth + 1);
    Ok(Value::Undefined)
}

fn console_group_end(ctx: &mut Ctx, _: &Value, _: &[Value], _: &Value) -> R<Value> {
    let depth = group_depth(ctx);
    set_group_depth(ctx, depth.saturating_sub(1));
    Ok(Value::Undefined)
}

/// `console.table`: one line per row, the columns joined by `|`.
fn console_table_fn(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let data = arg(args, 0);
    let Value::Obj(object) = &data else {
        let text = format_args(ctx, args)?;
        console_line(ctx, "", &text);
        return Ok(Value::Undefined);
    };
    let keys = ops::own_keys(object, false, true);
    for key in keys.iter().take(100) {
        let row = ops::get_key(ctx, &data, key)?;
        let cells = match &row {
            Value::Obj(row_object) => {
                let mut cells = Vec::new();
                for column in ops::own_keys(row_object, false, true).iter().take(20) {
                    let cell = ops::get_key(ctx, &row, column)?;
                    cells.push(alloc::format!("{}: {}", column.as_str(), ops::to_display(&cell)));
                }
                cells.join(" | ")
            }
            other => ops::to_display(other),
        };
        console_line(ctx, "", &alloc::format!("{} | {}", key.as_str(), cells));
    }
    Ok(Value::Undefined)
}

fn console_noop(_: &mut Ctx, _: &Value, _: &[Value], _: &Value) -> R<Value> {
    Ok(Value::Undefined)
}

// ------------------------------------------------------------
//  Timers and jobs
// ------------------------------------------------------------

/// A timer's callback: a function, or source text compiled as one.
fn timer_callback(ctx: &mut Ctx, value: &Value) -> R<Value> {
    match value {
        Value::Obj(object) if object.is_callable() => Ok(value.clone()),
        other => {
            let source = ops::to_string(ctx, other)?;
            ctx.eval_global(&alloc::format!("(function() {{\n{}\n}})", source.as_str()))
        }
    }
}

/// `setTimeout` (data false) and `setInterval` (data true).
fn set_timer(ctx: &mut Ctx, _: &Value, args: &[Value], data: &Value) -> R<Value> {
    let callback = timer_callback(ctx, &arg(args, 0))?;
    let delay = ops::to_number(ctx, &arg(args, 1))?;
    let delay = if delay.is_finite() { delay.to_i64_saturating().clamp(0, i32::MAX as i64) } else { 0 };
    let id = ctx.realm.next_timer;
    ctx.realm.next_timer = ctx.realm.next_timer.wrapping_add(1).max(1);
    let due = ctx.now() + delay;
    let extra: Vec<Value> = args.iter().skip(2).cloned().collect();
    if ctx.realm.timers.len() >= 10_000 {
        return ctx.throw_range("trop de minuteries actives");
    }
    ctx.realm.timers.push(Timer {
        id,
        due,
        interval: if data.truthy() { Some(delay.max(4)) } else { None },
        callback,
        args: extra,
    });
    Ok(Value::num(id as i64))
}

fn clear_timer(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let id = ops::to_number(ctx, &arg(args, 0))?;
    if id.is_finite() {
        let id = id.to_i64_saturating();
        ctx.realm.timers.retain(|t| t.id as i64 != id);
    }
    Ok(Value::Undefined)
}

fn queue_microtask(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let callback = arg(args, 0);
    ops::require_callable(ctx, &callback, "queueMicrotask argument")?;
    ctx.realm.jobs.push_back(Job::Call(callback, Vec::new()));
    Ok(Value::Undefined)
}

/// `eval`, indirect: the text runs in the global scope. A direct `eval`
/// that sees the caller's variables is not provided -- the same choice
/// as a strict-mode page that never calls `eval` by name.
fn eval(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    match arg(args, 0) {
        Value::Str(source) => ctx.eval_global(&source),
        other => Ok(other),
    }
}

// ------------------------------------------------------------
//  structuredClone
// ------------------------------------------------------------

fn clone_value(ctx: &mut Ctx, value: &Value, memo: &mut Vec<(Obj, Obj)>) -> R<Value> {
    let Value::Obj(object) = value else {
        if let Value::Sym(_) = value {
            return ctx.throw_error("Error", "DataCloneError: Symbol() could not be cloned");
        }
        return Ok(value.clone());
    };
    if let Some((_, copy)) = memo.iter().find(|(o, _)| o.ptr_eq(object)) {
        return Ok(Value::Obj(copy.clone()));
    }
    if crate::scheduler::stack_headroom() < 48 * 1024 {
        return ctx.throw_range("structuredClone: imbrication trop profonde");
    }
    ctx.tick()?;
    enum Shape {
        Plain,
        Array(Vec<Value>),
        Date(Num),
        Map(Vec<(Value, Value)>, bool),
        Boxed(Kind),
        Error,
        Uncloneable,
    }
    let shape = match &object.borrow().kind {
        Kind::Ordinary => Shape::Plain,
        Kind::Arguments => Shape::Plain,
        Kind::Array(items) => Shape::Array(items.clone()),
        Kind::Date(t) => Shape::Date(*t),
        Kind::Map(data) => Shape::Map(data.entries.iter().flatten().cloned().collect(), false),
        Kind::Set(data) => Shape::Map(data.entries.iter().flatten().cloned().collect(), true),
        Kind::Boolean(b) => Shape::Boxed(Kind::Boolean(*b)),
        Kind::Number(n) => Shape::Boxed(Kind::Number(*n)),
        Kind::String(s) => Shape::Boxed(Kind::String(s.clone())),
        Kind::RegExp(r) => Shape::Boxed(Kind::RegExp(r.clone())),
        Kind::Error => Shape::Error,
        _ => Shape::Uncloneable,
    };
    let copy = match shape {
        Shape::Uncloneable => {
            let message = alloc::format!("DataCloneError: {} could not be cloned", ops::to_display(value));
            return ctx.throw_error("Error", &message);
        }
        Shape::Array(items) => {
            let copy = ctx.new_array(Vec::new());
            memo.push((object.clone(), copy.clone()));
            let mut cloned = Vec::with_capacity(items.len());
            for item in &items {
                cloned.push(clone_value(ctx, item, memo)?);
            }
            copy.borrow_mut().kind = Kind::Array(cloned);
            return Ok(Value::Obj(copy));
        }
        Shape::Date(t) => {
            let proto = ctx.realm.protos.date.clone();
            ctx.alloc(Kind::Date(t), Some(proto), "Date")
        }
        Shape::Map(entries, is_set) => {
            let proto = if is_set { ctx.realm.protos.set.clone() } else { ctx.realm.protos.map.clone() };
            let kind = if is_set { Kind::Set(MapData::default()) } else { Kind::Map(MapData::default()) };
            let copy = ctx.alloc(kind, Some(proto), if is_set { "Set" } else { "Map" });
            memo.push((object.clone(), copy.clone()));
            for (key, item) in entries {
                let key = clone_value(ctx, &key, memo)?;
                let item = clone_value(ctx, &item, memo)?;
                if let Kind::Map(data) | Kind::Set(data) = &mut copy.borrow_mut().kind {
                    data.set(key, item);
                }
            }
            return Ok(Value::Obj(copy));
        }
        Shape::Boxed(kind) => {
            let proto = object.proto();
            let class = object.class_name();
            ctx.alloc(kind, proto, class)
        }
        Shape::Error => {
            let proto = object.proto();
            let copy = ctx.alloc(Kind::Error, proto, "Error");
            for name in ["message", "stack", "name", "cause"] {
                if let Some(v) = object.own_value(name) {
                    copy.put_hidden(name, v);
                }
            }
            return Ok(Value::Obj(copy));
        }
        Shape::Plain => ctx.new_object(),
    };
    memo.push((object.clone(), copy.clone()));
    for key in ops::own_keys(object, false, true) {
        let item = ops::get_key(ctx, value, &key)?;
        if item.as_obj().is_some_and(|o| o.is_callable()) {
            return ctx.throw_error("Error", "DataCloneError: function could not be cloned");
        }
        let cloned = clone_value(ctx, &item, memo)?;
        copy.put_key(key, cloned, DEFAULT);
    }
    Ok(Value::Obj(copy))
}

fn structured_clone(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let mut memo = Vec::new();
    clone_value(ctx, &arg(args, 0), &mut memo)
}

// ------------------------------------------------------------
//  URI and base64
// ------------------------------------------------------------

const URI_UNRESERVED: &str = "-_.!~*'()";
const URI_RESERVED: &str = ";/?:@&=+$,#";

/// `encodeURIComponent` (data false) and `encodeURI` (data true).
fn encode_uri(ctx: &mut Ctx, _: &Value, args: &[Value], data: &Value) -> R<Value> {
    let text = ops::to_string(ctx, &arg(args, 0))?;
    let keep_reserved = data.truthy();
    let mut out = String::new();
    for c in text.chars() {
        if c.is_ascii_alphanumeric() || URI_UNRESERVED.contains(c) || (keep_reserved && URI_RESERVED.contains(c)) {
            out.push(c);
        } else {
            let mut buffer = [0u8; 4];
            for byte in c.encode_utf8(&mut buffer).bytes() {
                out.push_str(&alloc::format!("%{:02X}", byte));
            }
        }
    }
    Ok(Value::Str(JsStr::from(out)))
}

/// `decodeURIComponent` (data false) and `decodeURI` (data true, which
/// leaves the escapes of reserved characters as they are).
fn decode_uri(ctx: &mut Ctx, _: &Value, args: &[Value], data: &Value) -> R<Value> {
    let text = ops::to_string(ctx, &arg(args, 0))?;
    let keep_reserved = data.truthy();
    let bytes = text.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    let hex = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
    while i < bytes.len() {
        if bytes[i] != b'%' {
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        let Some(value) = bytes.get(i + 1).and_then(|b| hex(*b)).zip(bytes.get(i + 2).and_then(|b| hex(*b))).map(|(h, l)| h * 16 + l) else {
            return ctx.throw_error("URIError", "URI malformed");
        };
        if value < 0x80 {
            if keep_reserved && URI_RESERVED.contains(value as char) {
                out.extend_from_slice(&bytes[i..i + 3]);
            } else {
                out.push(value);
            }
            i += 3;
            continue;
        }
        // A multi-byte sequence: every byte escaped, the whole valid.
        let length = match value {
            0xC2..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF4 => 4,
            _ => return ctx.throw_error("URIError", "URI malformed"),
        };
        let mut sequence = alloc::vec![value];
        for k in 1..length {
            let at = i + 3 * k;
            let byte = (bytes.get(at) == Some(&b'%'))
                .then(|| bytes.get(at + 1).and_then(|b| hex(*b)).zip(bytes.get(at + 2).and_then(|b| hex(*b))))
                .flatten()
                .map(|(h, l)| h * 16 + l);
            match byte {
                Some(b) if b & 0xC0 == 0x80 => sequence.push(b),
                _ => return ctx.throw_error("URIError", "URI malformed"),
            }
        }
        if core::str::from_utf8(&sequence).is_err() {
            return ctx.throw_error("URIError", "URI malformed");
        }
        out.extend_from_slice(&sequence);
        i += 3 * length;
    }
    match String::from_utf8(out) {
        Ok(text) => Ok(Value::Str(JsStr::from(text))),
        Err(_) => ctx.throw_error("URIError", "URI malformed"),
    }
}

/// Legacy `escape`.
fn escape(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let text = ops::to_string(ctx, &arg(args, 0))?;
    let mut out = String::new();
    for c in text.chars() {
        if c.is_ascii_alphanumeric() || "@*_+-./".contains(c) {
            out.push(c);
        } else {
            let mut units = [0u16; 2];
            for unit in c.encode_utf16(&mut units) {
                if *unit < 0x100 {
                    out.push_str(&alloc::format!("%{:02X}", unit));
                } else {
                    out.push_str(&alloc::format!("%u{:04X}", unit));
                }
            }
        }
    }
    Ok(Value::Str(JsStr::from(out)))
}

/// Legacy `unescape`.
fn unescape(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let text = ops::to_string(ctx, &arg(args, 0))?;
    let chars: Vec<char> = text.chars().collect();
    let mut units: Vec<u16> = Vec::new();
    let mut i = 0;
    let hex_value = |slice: &[char]| -> Option<u16> {
        let text: String = slice.iter().collect();
        u16::from_str_radix(&text, 16).ok()
    };
    while i < chars.len() {
        if chars[i] == '%' {
            if chars.get(i + 1) == Some(&'u') && i + 6 <= chars.len() {
                if let Some(v) = hex_value(&chars[i + 2..i + 6]) {
                    units.push(v);
                    i += 6;
                    continue;
                }
            }
            if i + 3 <= chars.len() {
                if let Some(v) = hex_value(&chars[i + 1..i + 3]) {
                    units.push(v);
                    i += 3;
                    continue;
                }
            }
        }
        let mut buffer = [0u16; 2];
        units.extend_from_slice(chars[i].encode_utf16(&mut buffer));
        i += 1;
    }
    Ok(Value::Str(JsStr::from(String::from_utf16_lossy(&units))))
}

const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn invalid_character(ctx: &mut Ctx, message: &str) -> R<Value> {
    let error = ctx.error_object(ctx.realm.protos.error.clone(), "InvalidCharacterError", message);
    error.put_hidden("name", Value::str("InvalidCharacterError"));
    Err(crate::web::js::interp::Ctrl::Throw(Value::Obj(error)))
}

/// `btoa`: the text must be Latin-1 (one byte per character).
fn btoa(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let text = ops::to_string(ctx, &arg(args, 0))?;
    let mut bytes = Vec::with_capacity(text.len());
    for c in text.chars() {
        if c as u32 > 0xFF {
            return invalid_character(ctx, "The string to be encoded contains characters outside of the Latin1 range.");
        }
        bytes.push(c as u8);
    }
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        for k in 0..4 {
            if k <= chunk.len() {
                out.push(BASE64[((n >> (18 - 6 * k)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    Ok(Value::Str(JsStr::from(out)))
}

/// `atob`: white space ignored, padding optional, a string of Latin-1
/// characters back.
fn atob(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let text = ops::to_string(ctx, &arg(args, 0))?;
    let mut clean: Vec<u8> = text.bytes().filter(|b| !b" \t\n\x0c\r".contains(b)).collect();
    if clean.len() % 4 == 0 {
        while clean.last() == Some(&b'=') {
            clean.pop();
        }
    }
    if clean.len() % 4 == 1 {
        return invalid_character(ctx, "The string to be decoded is not correctly encoded.");
    }
    let mut out = String::new();
    let mut accumulator: u32 = 0;
    let mut bits = 0;
    for byte in clean {
        let Some(value) = BASE64.iter().position(|b| *b == byte) else {
            return invalid_character(ctx, "The string to be decoded is not correctly encoded.");
        };
        accumulator = (accumulator << 6) | value as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(char::from(((accumulator >> bits) & 0xFF) as u8));
        }
    }
    Ok(Value::Str(JsStr::from(out)))
}

pub fn install(ctx: &mut Ctx) {
    let global = ctx.realm.global.clone();

    let console = ctx.new_object();
    let levels: [(&str, &str); 8] = [
        ("log", ""),
        ("info", "info"),
        ("warn", "avertissement"),
        ("error", "erreur"),
        ("debug", "debug"),
        ("trace", "trace"),
        ("dir", ""),
        ("dirxml", ""),
    ];
    for (name, level) in levels {
        let native = ctx.native_with(name, 0, console_log, Value::str(level));
        console.put_hidden(name, Value::Obj(native));
    }
    ctx.method(&console, "assert", 0, console_assert);
    let count = ctx.native_with("count", 0, console_count, Value::Bool(false));
    console.put_hidden("count", Value::Obj(count));
    let count_reset = ctx.native_with("countReset", 0, console_count, Value::Bool(true));
    console.put_hidden("countReset", Value::Obj(count_reset));
    for (name, mode) in [("time", 0), ("timeLog", 1), ("timeEnd", 2)] {
        let native = ctx.native_with(name, 0, console_time, Value::num(mode));
        console.put_hidden(name, Value::Obj(native));
    }
    ctx.method(&console, "group", 0, console_group);
    ctx.method(&console, "groupCollapsed", 0, console_group);
    ctx.method(&console, "groupEnd", 0, console_group_end);
    ctx.method(&console, "table", 1, console_table_fn);
    ctx.method(&console, "clear", 0, console_noop);
    ctx.method(&console, "profile", 0, console_noop);
    ctx.method(&console, "profileEnd", 0, console_noop);
    global.put_hidden("console", Value::Obj(console));

    let timeout = ctx.native_with("setTimeout", 2, set_timer, Value::Bool(false));
    global.put_hidden("setTimeout", Value::Obj(timeout));
    let interval = ctx.native_with("setInterval", 2, set_timer, Value::Bool(true));
    global.put_hidden("setInterval", Value::Obj(interval));
    ctx.method(&global, "clearTimeout", 1, clear_timer);
    ctx.method(&global, "clearInterval", 1, clear_timer);
    ctx.method(&global, "queueMicrotask", 1, queue_microtask);
    ctx.method(&global, "structuredClone", 1, structured_clone);
    ctx.method(&global, "eval", 1, eval);

    for (name, data, function) in [
        ("encodeURIComponent", false, encode_uri as NativeFn),
        ("encodeURI", true, encode_uri as NativeFn),
        ("decodeURIComponent", false, decode_uri as NativeFn),
        ("decodeURI", true, decode_uri as NativeFn),
    ] {
        let native = ctx.native_with(name, 1, function, Value::Bool(data));
        global.put_hidden(name, Value::Obj(native));
    }
    ctx.method(&global, "escape", 1, escape);
    ctx.method(&global, "unescape", 1, unescape);
    ctx.method(&global, "btoa", 1, btoa);
    ctx.method(&global, "atob", 1, atob);
}
