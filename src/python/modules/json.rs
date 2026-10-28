//! `json`: `dumps`/`loads` with CPython's output and errors.
//!
//! `dumps` writes what `json.encoder` writes: `", "` and `": "` between
//! items (`","` when indenting), floats by `float.__repr__`, `NaN` and
//! `Infinity` allowed, non-ASCII escaped as `\uXXXX` unless
//! `ensure_ascii=False`, keys converted (`1` -> `"1"`, `None` -> `"null"`).
//! `loads` is strict JSON plus `NaN`/`Infinity`, with the messages of
//! `json.decoder` ("Expecting value: line 1 column 1 (char 0)").

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use super::super::args;
use super::super::dict::Dict;
use super::super::types::class;
use super::super::value::*;
use super::super::vm::{PyErr, PyResult, Vm};
use crate::web::js::num::Num;

struct Encoder {
    indent: Option<String>,
    item_separator: String,
    key_separator: String,
    sort_keys: bool,
    ensure_ascii: bool,
    allow_nan: bool,
    skip_keys: bool,
    default: Value,
    stack: Vec<usize>,
}

fn quote(text: &str, ensure_ascii: bool, out: &mut String) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&alloc::format!("\\u{:04x}", c as u32)),
            c if ensure_ascii && (c as u32) > 0x7e => {
                let code = c as u32;
                if code > 0xffff {
                    let v = code - 0x10000;
                    out.push_str(&alloc::format!("\\u{:04x}\\u{:04x}", 0xd800 + (v >> 10), 0xdc00 + (v & 0x3ff)));
                } else {
                    out.push_str(&alloc::format!("\\u{:04x}", code));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

impl Encoder {
    fn float(&self, vm: &mut Vm, f: Num) -> PyResult<String> {
        if f.is_nan() || f.is_infinite() {
            if !self.allow_nan {
                let shown = super::super::format::float_repr(f);
                let message = alloc::format!("Out of range float values are not JSON compliant: {}", shown);
                return vm.value_error(&message);
            }
            return Ok(String::from(if f.is_nan() {
                "NaN"
            } else if f.is_negative() {
                "-Infinity"
            } else {
                "Infinity"
            }));
        }
        Ok(super::super::format::float_repr(f))
    }

    fn newline(&self, out: &mut String, level: usize) {
        if let Some(indent) = &self.indent {
            out.push('\n');
            for _ in 0..level {
                out.push_str(indent);
            }
        }
    }

    fn encode(&mut self, vm: &mut Vm, value: &Value, out: &mut String, level: usize) -> PyResult<()> {
        vm.enter_native()?;
        let result = self.encode_inner(vm, value, out, level);
        vm.leave_native();
        result
    }

    fn encode_inner(&mut self, vm: &mut Vm, value: &Value, out: &mut String, level: usize) -> PyResult<()> {
        let value = match value {
            Value::Instance(i) if !matches!(i.class().base, Base::Object | Base::Exception | Base::Other) => i.inner.borrow().clone(),
            other => other.clone(),
        };
        match &value {
            Value::None => out.push_str("null"),
            Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Value::Int(i) => out.push_str(&super::super::format::int_text(vm, i)?),
            Value::Float(f) => out.push_str(&self.float(vm, *f)?),
            Value::Str(s) => quote(s, self.ensure_ascii, out),
            Value::List(_) | Value::Tuple(_) => {
                let items = vm.collect(&value)?;
                if items.is_empty() {
                    out.push_str("[]");
                    return Ok(());
                }
                let id = value.pointer().unwrap_or(0);
                if self.stack.contains(&id) {
                    return vm.value_error("Circular reference detected");
                }
                self.stack.push(id);
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(&self.item_separator);
                    }
                    self.newline(out, level + 1);
                    self.encode(vm, item, out, level + 1)?;
                }
                self.newline(out, level);
                out.push(']');
                self.stack.pop();
            }
            Value::Dict(d) => {
                let mut pairs: Vec<(Value, Value)> = d.borrow().iter().map(|e| (e.key.clone(), e.value.clone())).collect();
                if pairs.is_empty() {
                    out.push_str("{}");
                    return Ok(());
                }
                let id = value.pointer().unwrap_or(0);
                if self.stack.contains(&id) {
                    return vm.value_error("Circular reference detected");
                }
                if self.sort_keys {
                    // Sorted by key, stably.
                    let keys: Vec<Value> = pairs.iter().map(|(k, _)| k.clone()).collect();
                    let mut indexed: Vec<(Value, (Value, Value))> = keys.into_iter().zip(pairs).collect();
                    let mut sorted = Vec::with_capacity(indexed.len());
                    let key_values: Vec<Value> = indexed.iter().map(|(k, _)| k.clone()).collect();
                    let order = super::super::types::seq::sort_values(vm, key_values, &Value::None, false)?;
                    for key in order {
                        if let Some(position) = indexed.iter().position(|(k, _)| k.is(&key)) {
                            sorted.push(indexed.remove(position).1);
                        }
                    }
                    pairs = sorted;
                }
                self.stack.push(id);
                out.push('{');
                let mut first = true;
                for (key, item) in pairs.iter() {
                    let key_text = match key {
                        Value::Str(s) => String::from(&**s),
                        Value::Int(i) => super::super::format::int_text(vm, i)?,
                        Value::Bool(b) => String::from(if *b { "true" } else { "false" }),
                        Value::None => String::from("null"),
                        Value::Float(f) => self.float(vm, *f)?,
                        other => {
                            if self.skip_keys {
                                continue;
                            }
                            let name = vm.type_name(other);
                            let message = alloc::format!("keys must be str, int, float, bool or None, not {}", name);
                            return vm.type_error(&message);
                        }
                    };
                    if !first {
                        out.push_str(&self.item_separator);
                    }
                    first = false;
                    self.newline(out, level + 1);
                    quote(&key_text, self.ensure_ascii, out);
                    out.push_str(&self.key_separator);
                    self.encode(vm, item, out, level + 1)?;
                }
                self.newline(out, level);
                out.push('}');
                self.stack.pop();
            }
            other => {
                if self.default.is_none() {
                    let name = vm.type_name(other);
                    let message = alloc::format!("Object of type {} is not JSON serializable", name);
                    return vm.type_error(&message);
                }
                let id = other.pointer().unwrap_or(0);
                if self.stack.contains(&id) {
                    return vm.value_error("Circular reference detected");
                }
                self.stack.push(id);
                let default = self.default.clone();
                let replaced = vm.call(&default, core::slice::from_ref(other), &[])?;
                self.encode(vm, &replaced, out, level)?;
                self.stack.pop();
            }
        }
        Ok(())
    }
}

fn encoder(vm: &mut Vm, kw: &[(Rc<str>, Value)]) -> PyResult<Encoder> {
    let flag = |vm: &mut Vm, name: &str, default: bool| -> PyResult<bool> {
        match args::keyword(kw, name) {
            Some(v) => vm.truthy(v),
            None => Ok(default),
        }
    };
    let indent = match args::keyword(kw, "indent") {
        None | Some(Value::None) => None,
        Some(Value::Int(i)) => Some(" ".repeat(i.to_i64().unwrap_or(0).max(0) as usize)),
        Some(Value::Str(s)) => Some(String::from(&**s)),
        Some(other) => {
            let name = vm.type_name(other);
            let message = alloc::format!("indent must be an int or a str, not {}", name);
            return vm.type_error(&message);
        }
    };
    let (item_separator, key_separator) = match args::keyword(kw, "separators") {
        None | Some(Value::None) => (String::from(if indent.is_some() { "," } else { ", " }), String::from(": ")),
        Some(v) => {
            let parts = vm.collect(v)?;
            if parts.len() != 2 {
                return vm.value_error("separators must be a (item_separator, key_separator) tuple");
            }
            (vm.str_of(&parts[0])?, vm.str_of(&parts[1])?)
        }
    };
    Ok(Encoder {
        indent,
        item_separator,
        key_separator,
        sort_keys: flag(vm, "sort_keys", false)?,
        ensure_ascii: flag(vm, "ensure_ascii", true)?,
        allow_nan: flag(vm, "allow_nan", true)?,
        skip_keys: flag(vm, "skipkeys", false)?,
        default: args::keyword(kw, "default").cloned().unwrap_or(Value::None),
        stack: Vec::new(),
    })
}

fn dumps(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let Some(value) = a.first() else {
        return vm.type_error("dumps() missing 1 required positional argument: 'obj'");
    };
    let mut e = encoder(vm, kw)?;
    let mut out = String::new();
    e.encode(vm, value, &mut out, 0)?;
    Ok(Value::str(&out))
}

fn dump(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let (Some(value), Some(file)) = (a.first(), a.get(1)) else {
        return vm.type_error("dump() missing 1 required positional argument: 'fp'");
    };
    let text = dumps(vm, core::slice::from_ref(value), kw)?;
    let write = vm.get_attr(file, "write")?;
    vm.call(&write, &[text], &[])?;
    Ok(Value::None)
}

// ------------------------------------------------------------
//  Decoding
// ------------------------------------------------------------

struct Decoder<'a> {
    chars: Vec<char>,
    pos: usize,
    text: &'a str,
}

fn decode_error(vm: &mut Vm, message: &str, doc: &str, pos: usize) -> PyErr {
    let chars: Vec<char> = doc.chars().take(pos).collect();
    let line = chars.iter().filter(|c| **c == '\n').count() + 1;
    let column = match chars.iter().rposition(|c| *c == '\n') {
        Some(i) => pos - i,
        None => pos + 1,
    };
    let class = json_error_class(vm);
    let full = alloc::format!("{}: line {} column {} (char {})", message, line, column, pos);
    let err = vm.error(&class, &full);
    if let PyErr::Exc(Value::Instance(i)) = &err {
        let mut d = i.dict.borrow_mut();
        d.set_str("msg", Value::str(message));
        d.set_str("doc", Value::str(doc));
        d.set_str("pos", Value::int(pos as i64));
        d.set_str("lineno", Value::int(line as i64));
        d.set_str("colno", Value::int(column as i64));
    }
    err
}

fn json_error_class(vm: &mut Vm) -> Rc<Class> {
    if let Some(Value::Module(m)) = vm.modules.iter().find(|(n, _)| &**n == "json").map(|(_, m)| m.clone()) {
        if let Some(Value::Class(c)) = m.dict.borrow().get_str("JSONDecodeError") {
            return c;
        }
    }
    vm.types.value_error.clone()
}

impl Decoder<'_> {
    fn skip_space(&mut self) {
        while self.pos < self.chars.len() && matches!(self.chars[self.pos], ' ' | '\t' | '\n' | '\r') {
            self.pos += 1;
        }
    }

    fn fail<T>(&self, vm: &mut Vm, message: &str, pos: usize) -> PyResult<T> {
        Err(decode_error(vm, message, self.text, pos))
    }

    fn starts(&self, word: &str) -> bool {
        let w: Vec<char> = word.chars().collect();
        self.chars.len() >= self.pos + w.len() && self.chars[self.pos..self.pos + w.len()] == w[..]
    }

    fn value(&mut self, vm: &mut Vm, depth: usize) -> PyResult<Value> {
        if depth > 500 {
            let class = vm.types.recursion_error.clone();
            return Err(vm.error(&class, "maximum recursion depth exceeded while decoding a JSON document"));
        }
        self.skip_space();
        let Some(&c) = self.chars.get(self.pos) else {
            return self.fail(vm, "Expecting value", self.pos);
        };
        match c {
            '{' => self.object(vm, depth),
            '[' => self.array(vm, depth),
            '"' => {
                let s = self.string(vm)?;
                Ok(Value::str(&s))
            }
            'n' if self.starts("null") => {
                self.pos += 4;
                Ok(Value::None)
            }
            't' if self.starts("true") => {
                self.pos += 4;
                Ok(Value::Bool(true))
            }
            'f' if self.starts("false") => {
                self.pos += 5;
                Ok(Value::Bool(false))
            }
            'N' if self.starts("NaN") => {
                self.pos += 3;
                Ok(Value::Float(Num::NAN))
            }
            'I' if self.starts("Infinity") => {
                self.pos += 8;
                Ok(Value::Float(Num::INFINITY))
            }
            '-' if self.starts("-Infinity") => {
                self.pos += 9;
                Ok(Value::Float(Num::NEG_INFINITY))
            }
            '-' | '0'..='9' => self.number(vm),
            _ => self.fail(vm, "Expecting value", self.pos),
        }
    }

    fn number(&mut self, vm: &mut Vm) -> PyResult<Value> {
        let start = self.pos;
        if self.chars.get(self.pos) == Some(&'-') {
            self.pos += 1;
        }
        let digits_start = self.pos;
        match self.chars.get(self.pos) {
            Some('0') => self.pos += 1,
            Some('1'..='9') => {
                while self.chars.get(self.pos).is_some_and(|c| c.is_ascii_digit()) {
                    self.pos += 1;
                }
            }
            _ => return self.fail(vm, "Expecting value", start),
        }
        let _ = digits_start;
        let mut is_float = false;
        if self.chars.get(self.pos) == Some(&'.') && self.chars.get(self.pos + 1).is_some_and(|c| c.is_ascii_digit()) {
            is_float = true;
            self.pos += 1;
            while self.chars.get(self.pos).is_some_and(|c| c.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        if matches!(self.chars.get(self.pos), Some('e' | 'E')) {
            let save = self.pos;
            self.pos += 1;
            if matches!(self.chars.get(self.pos), Some('+' | '-')) {
                self.pos += 1;
            }
            if self.chars.get(self.pos).is_some_and(|c| c.is_ascii_digit()) {
                is_float = true;
                while self.chars.get(self.pos).is_some_and(|c| c.is_ascii_digit()) {
                    self.pos += 1;
                }
            } else {
                self.pos = save;
            }
        }
        let text: String = self.chars[start..self.pos].iter().collect();
        if is_float {
            Ok(Value::Float(Num::parse(&text)))
        } else {
            super::super::types::num::parse_int(vm, &text, 10)
        }
    }

    fn string(&mut self, vm: &mut Vm) -> PyResult<String> {
        let start = self.pos;
        self.pos += 1;
        let mut out = String::new();
        loop {
            let Some(&c) = self.chars.get(self.pos) else {
                return self.fail(vm, "Unterminated string starting at", start);
            };
            match c {
                '"' => {
                    self.pos += 1;
                    return Ok(out);
                }
                '\\' => {
                    let Some(&e) = self.chars.get(self.pos + 1) else {
                        return self.fail(vm, "Unterminated string starting at", start);
                    };
                    let simple = match e {
                        '"' => Some('"'),
                        '\\' => Some('\\'),
                        '/' => Some('/'),
                        'b' => Some('\u{8}'),
                        'f' => Some('\u{c}'),
                        'n' => Some('\n'),
                        'r' => Some('\r'),
                        't' => Some('\t'),
                        _ => None,
                    };
                    if let Some(s) = simple {
                        out.push(s);
                        self.pos += 2;
                        continue;
                    }
                    if e != 'u' {
                        return self.fail(vm, "Invalid \\escape", self.pos);
                    }
                    let hex = |d: &Decoder, at: usize| -> Option<u32> {
                        let s: String = d.chars.get(at..at + 4)?.iter().collect();
                        if s.len() != 4 {
                            return None;
                        }
                        u32::from_str_radix(&s, 16).ok()
                    };
                    let Some(mut code) = hex(self, self.pos + 2) else {
                        return self.fail(vm, "Invalid \\uXXXX escape", self.pos + 1);
                    };
                    self.pos += 6;
                    if (0xd800..0xdc00).contains(&code) && self.chars.get(self.pos) == Some(&'\\') && self.chars.get(self.pos + 1) == Some(&'u') {
                        if let Some(low) = hex(self, self.pos + 2) {
                            if (0xdc00..0xe000).contains(&low) {
                                code = 0x10000 + ((code - 0xd800) << 10) + (low - 0xdc00);
                                self.pos += 6;
                            }
                        }
                    }
                    out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                }
                c if (c as u32) < 0x20 => return self.fail(vm, "Invalid control character at", self.pos),
                c => {
                    out.push(c);
                    self.pos += 1;
                }
            }
        }
    }

    fn array(&mut self, vm: &mut Vm, depth: usize) -> PyResult<Value> {
        self.pos += 1;
        let mut items = Vec::new();
        self.skip_space();
        if self.chars.get(self.pos) == Some(&']') {
            self.pos += 1;
            return Ok(Value::list(items));
        }
        loop {
            items.push(self.value(vm, depth + 1)?);
            self.skip_space();
            match self.chars.get(self.pos) {
                Some(',') => {
                    self.pos += 1;
                    self.skip_space();
                    if self.chars.get(self.pos) == Some(&']') {
                        return self.fail(vm, "Illegal trailing comma before end of array", self.pos - 1);
                    }
                }
                Some(']') => {
                    self.pos += 1;
                    return Ok(Value::list(items));
                }
                _ => return self.fail(vm, "Expecting ',' delimiter", self.pos),
            }
        }
    }

    fn object(&mut self, vm: &mut Vm, depth: usize) -> PyResult<Value> {
        self.pos += 1;
        let dict = shared_dict(Dict::new());
        self.skip_space();
        if self.chars.get(self.pos) == Some(&'}') {
            self.pos += 1;
            return Ok(Value::Dict(dict));
        }
        loop {
            self.skip_space();
            if self.chars.get(self.pos) != Some(&'"') {
                return self.fail(vm, "Expecting property name enclosed in double quotes", self.pos);
            }
            let key = self.string(vm)?;
            self.skip_space();
            if self.chars.get(self.pos) != Some(&':') {
                return self.fail(vm, "Expecting ':' delimiter", self.pos);
            }
            self.pos += 1;
            let value = self.value(vm, depth + 1)?;
            vm.dict_insert(&dict, Value::str(&key), value)?;
            self.skip_space();
            match self.chars.get(self.pos) {
                Some(',') => {
                    self.pos += 1;
                    self.skip_space();
                    if self.chars.get(self.pos) == Some(&'}') {
                        return self.fail(vm, "Illegal trailing comma before end of object", self.pos - 1);
                    }
                }
                Some('}') => {
                    self.pos += 1;
                    return Ok(Value::Dict(dict));
                }
                _ => return self.fail(vm, "Expecting ',' delimiter", self.pos),
            }
        }
    }
}

fn loads(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let text = match a.first() {
        Some(Value::Str(s)) => s.clone(),
        Some(Value::Bytes(b)) => Rc::from(String::from_utf8_lossy(b).as_ref()),
        Some(other) => {
            let name = vm.type_name(other);
            let message = alloc::format!("the JSON object must be str, bytes or bytearray, not {}", name);
            return vm.type_error(&message);
        }
        None => return vm.type_error("loads() missing 1 required positional argument: 's'"),
    };
    let mut d = Decoder { chars: text.chars().collect(), pos: 0, text: &text };
    let value = d.value(vm, 0)?;
    d.skip_space();
    if d.pos < d.chars.len() {
        return d.fail(vm, "Extra data", d.pos);
    }
    Ok(value)
}

fn load(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let Some(file) = a.first() else {
        return vm.type_error("load() missing 1 required positional argument: 'fp'");
    };
    let read = vm.get_attr(file, "read")?;
    let text = vm.call(&read, &[], &[])?;
    loads(vm, &[text], kw)
}

pub fn module(vm: &mut Vm) -> Rc<Module> {
    let functions: &[(&'static str, NativeFn)] = &[("dumps", dumps), ("dump", dump), ("loads", loads), ("load", load)];
    let module = super::native_module(vm, "json", functions);
    let error = class("JSONDecodeError", alloc::vec![vm.types.value_error.clone()], Base::Exception);
    error.dict.borrow_mut().set_str("__module__", Value::str("json.decoder"));
    super::set(&module, "JSONDecodeError", Value::Class(error));
    module
}
