//! `JSON`: `parse` on the reader of `crate::json`, `stringify` here.
//!
//! The reader is the one the update registry uses (phase 15), in its
//! lenient mode -- what the language specifies: the last duplicate key
//! wins, a lone surrogate is a character. Numbers keep their text there
//! and are converted here by `num::Num::parse`, which is exact.
//!
//! Both directions recurse with the data; both check the stack before
//! each level, and `stringify` refuses a cycle by name, as browsers do.

use alloc::string::String;
use alloc::vec::Vec;

use super::arg;
use crate::json;
use crate::web::js::interp::{Ctx, R};
use crate::web::js::num::Num;
use crate::web::js::ops;
use crate::web::js::value::*;

/// Stack kept free below each level of either direction.
const STACK_RESERVE: usize = 48 * 1024;

fn stack_ok() -> bool {
    crate::scheduler::stack_headroom() >= STACK_RESERVE
}

/// A parsed document as script values.
fn to_value(ctx: &mut Ctx, value: &json::Value) -> R<Value> {
    Ok(match value {
        json::Value::Null => Value::Null,
        json::Value::Bool(b) => Value::Bool(*b),
        json::Value::Number(n) => Value::Num(Num::parse(n.as_text())),
        json::Value::String(s) => Value::Str(JsStr::from(s.as_str())),
        json::Value::Array(items) => {
            if !stack_ok() {
                return ctx.throw_range("JSON.parse: imbrication trop profonde");
            }
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(to_value(ctx, item)?);
            }
            Value::Obj(ctx.new_array(out))
        }
        json::Value::Object(members) => {
            if !stack_ok() {
                return ctx.throw_range("JSON.parse: imbrication trop profonde");
            }
            let object = ctx.new_object();
            for (name, member) in members.iter() {
                let member = to_value(ctx, member)?;
                object.put_key(JsStr::from(name), member, DEFAULT);
            }
            Value::Obj(object)
        }
    })
}

/// `InternalizeJSONProperty`: the reviver sees every value, innermost
/// first, and what it returns replaces it (undefined deletes).
fn internalize(ctx: &mut Ctx, holder: &Value, name: JsStr, reviver: &Value) -> R<Value> {
    ctx.tick()?;
    if !stack_ok() {
        return ctx.throw_range("JSON.parse: imbrication trop profonde");
    }
    let value = ops::get_key(ctx, holder, &name)?;
    if let Value::Obj(object) = &value {
        let keys: Vec<JsStr> = if object.is_array() {
            let length = ops::get(ctx, &value, "length")?;
            let length = ops::to_integer(ctx, &length)?.max(0);
            (0..length).map(|i| ops::number_string(Num::from_i64(i))).collect()
        } else {
            ops::own_keys(object, false, true)
        };
        for key in keys {
            let element = internalize(ctx, &value, key.clone(), reviver)?;
            if element.is_undefined() {
                ops::delete(ctx, &value, &key)?;
            } else {
                ops::set(ctx, &value, key, element)?;
            }
        }
    }
    ctx.call(reviver, holder.clone(), &[Value::Str(name), value])
}

fn parse(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let text = ops::to_string(ctx, &arg(args, 0))?;
    let document = match json::parse_with(text.as_bytes(), &json::Options::LENIENT) {
        Ok(document) => document,
        Err(error) => {
            let message = alloc::format!("{} - JSON.parse", error.message());
            return ctx.throw_error("SyntaxError", &message);
        }
    };
    let result = to_value(ctx, &document)?;
    let reviver = arg(args, 1);
    if !reviver.as_obj().is_some_and(|r| r.is_callable()) {
        return Ok(result);
    }
    let root = ctx.new_object();
    root.put("", result);
    internalize(ctx, &Value::Obj(root), JsStr::from(""), &reviver)
}

// ------------------------------------------------------------
//  stringify
// ------------------------------------------------------------

struct Writer {
    replacer: Value,
    /// The property list of an array replacer.
    allowed: Option<Vec<JsStr>>,
    gap: String,
    indent: String,
    /// Objects being written, for cycle detection.
    stack: Vec<Obj>,
    out: String,
}

/// Unwraps `Number`, `String` and `Boolean` objects into their primitive.
fn unwrap_primitive(ctx: &mut Ctx, value: Value) -> R<Value> {
    let Value::Obj(object) = &value else {
        return Ok(value);
    };
    let kind = match &object.borrow().kind {
        Kind::Number(_) => 1,
        Kind::String(_) => 2,
        Kind::Boolean(b) => return Ok(Value::Bool(*b)),
        _ => 0,
    };
    Ok(match kind {
        1 => Value::Num(ops::to_number(ctx, &value)?),
        2 => Value::Str(ops::to_string(ctx, &value)?),
        _ => value,
    })
}

impl Writer {
    /// `SerializeJSONProperty`: writes the value of `holder[key]`, or
    /// returns false when it has no JSON form (undefined, a function, a
    /// symbol) and nothing was written.
    fn property(&mut self, ctx: &mut Ctx, holder: &Value, key: JsStr) -> R<bool> {
        let value = ops::get_key(ctx, holder, &key)?;
        self.value(ctx, holder, key, value)
    }

    fn value(&mut self, ctx: &mut Ctx, holder: &Value, key: JsStr, mut value: Value) -> R<bool> {
        ctx.tick()?;
        if let Value::Obj(_) = &value {
            let to_json = ops::get(ctx, &value, "toJSON")?;
            if to_json.as_obj().is_some_and(|f| f.is_callable()) {
                value = ctx.call(&to_json, value.clone(), &[Value::Str(key.clone())])?;
            }
        }
        if self.replacer.as_obj().is_some_and(|r| r.is_callable()) {
            let replacer = self.replacer.clone();
            value = ctx.call(&replacer, holder.clone(), &[Value::Str(key), value])?;
        }
        let value = unwrap_primitive(ctx, value)?;
        match &value {
            Value::Null => self.out.push_str("null"),
            Value::Bool(b) => self.out.push_str(if *b { "true" } else { "false" }),
            Value::Str(s) => json::write_string(s, &mut self.out),
            Value::Num(n) => {
                if n.is_finite() {
                    self.out.push_str(&ops::number_string(*n));
                } else {
                    self.out.push_str("null");
                }
            }
            Value::Obj(object) if !object.is_callable() => {
                if self.stack.iter().any(|o| o.ptr_eq(object)) {
                    return ctx.throw_type("Converting circular structure to JSON");
                }
                if !stack_ok() {
                    return ctx.throw_range("JSON.stringify: imbrication trop profonde");
                }
                self.stack.push(object.clone());
                let written = if object.is_array() { self.array(ctx, &value) } else { self.object(ctx, &value, object) };
                self.stack.pop();
                written?;
            }
            _ => return Ok(false),
        }
        ops::check_string(ctx, self.out.len())?;
        Ok(true)
    }

    fn open(&mut self, bracket: char) -> String {
        self.out.push(bracket);
        let outer = self.indent.clone();
        self.indent.push_str(&self.gap.clone());
        outer
    }

    fn separator(&mut self, first: bool) {
        if !first {
            self.out.push(',');
        }
        if !self.gap.is_empty() {
            self.out.push('\n');
            self.out.push_str(&self.indent.clone());
        }
    }

    fn close(&mut self, outer: String, bracket: char, empty: bool) {
        if !empty && !self.gap.is_empty() {
            self.out.push('\n');
            self.out.push_str(&outer);
        }
        self.indent = outer;
        self.out.push(bracket);
    }

    fn array(&mut self, ctx: &mut Ctx, value: &Value) -> R<()> {
        let length = ops::get(ctx, value, "length")?;
        let length = ops::to_integer(ctx, &length)?.max(0);
        let outer = self.open('[');
        for index in 0..length {
            self.separator(index == 0);
            if !self.property(ctx, value, ops::number_string(Num::from_i64(index)))? {
                self.out.push_str("null");
            }
        }
        self.close(outer, ']', length == 0);
        Ok(())
    }

    fn object(&mut self, ctx: &mut Ctx, value: &Value, object: &Obj) -> R<()> {
        let keys = match &self.allowed {
            Some(list) => list.clone(),
            None => ops::own_keys(object, false, true),
        };
        let outer = self.open('{');
        let mut first = true;
        for key in keys {
            // Written into a scratch buffer: a member without JSON form
            // leaves no trace, not even its name.
            let mark = self.out.len();
            self.separator(first);
            json::write_string(&key, &mut self.out);
            self.out.push(':');
            if !self.gap.is_empty() {
                self.out.push(' ');
            }
            if self.property(ctx, value, key)? {
                first = false;
            } else {
                self.out.truncate(mark);
            }
        }
        self.close(outer, '}', first);
        Ok(())
    }
}

fn stringify(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let replacer = arg(args, 1);
    let mut allowed = None;
    if let Value::Obj(list) = &replacer {
        if list.is_array() {
            let mut keys: Vec<JsStr> = Vec::new();
            for item in ops::list_from_array_like(ctx, &replacer)? {
                let item = unwrap_primitive(ctx, item)?;
                let key = match item {
                    Value::Str(s) => s,
                    Value::Num(n) => ops::number_string(n),
                    _ => continue,
                };
                if !keys.contains(&key) {
                    keys.push(key);
                }
            }
            allowed = Some(keys);
        }
    }
    let space = unwrap_primitive(ctx, arg(args, 2))?;
    let gap = match space {
        Value::Num(n) => " ".repeat(n.to_i64_saturating().clamp(0, 10) as usize),
        Value::Str(s) => String::from(s.slice(0, 10).as_str()),
        _ => String::new(),
    };
    let mut writer = Writer { replacer, allowed, gap, indent: String::new(), stack: Vec::new(), out: String::new() };
    let root = ctx.new_object();
    root.put("", arg(args, 0));
    let written = writer.property(ctx, &Value::Obj(root), JsStr::from(""))?;
    Ok(if written { Value::Str(JsStr::from(writer.out)) } else { Value::Undefined })
}

/// Serializes a value the way `JSON.stringify(value)` does, for the
/// console and `structuredClone`.
pub fn stringify_value(ctx: &mut Ctx, value: &Value) -> R<Option<String>> {
    let mut writer = Writer {
        replacer: Value::Undefined,
        allowed: None,
        gap: String::new(),
        indent: String::new(),
        stack: Vec::new(),
        out: String::new(),
    };
    let root = ctx.new_object();
    root.put("", value.clone());
    let written = writer.property(ctx, &Value::Obj(root), JsStr::from(""))?;
    Ok(written.then_some(writer.out))
}

/// Reads text as `JSON.parse(text)` would, without reviver.
pub fn parse_text(ctx: &mut Ctx, text: &str) -> R<Value> {
    match json::parse_with(text.as_bytes(), &json::Options::LENIENT) {
        Ok(document) => to_value(ctx, &document),
        Err(error) => {
            let message = error.message();
            ctx.throw_error("SyntaxError", &message)
        }
    }
}

pub fn install(ctx: &mut Ctx) {
    let object = ctx.new_object();
    ctx.method(&object, "parse", 2, parse);
    ctx.method(&object, "stringify", 3, stringify);
    let tag = ctx.realm.symbols.to_string_tag.clone();
    object.put_key(tag, Value::str("JSON"), CONFIGURABLE);
    ctx.realm.global.put_hidden("JSON", Value::Obj(object));
}
