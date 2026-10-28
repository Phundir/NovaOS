//! The runtime side of `match` (PEP 634): what a sequence and a mapping
//! are for a pattern, the attributes a class pattern reads, the keys a
//! mapping pattern looks up.
//!
//! CPython decides "sequence" and "mapping" by type flags that
//! `collections.abc` sets on registration. Here the built-in containers
//! answer for themselves, and a class written in Python (the `deque` and
//! the abstract classes of the library) says so by a class attribute,
//! `__match_sequence__` or `__match_mapping__`.

use alloc::rc::Rc;
use alloc::vec::Vec;

use super::dict::Dict;
use super::value::Value;
use super::vm::{PyResult, Vm};

impl Vm {
    /// Is `value` a sequence (kind 0) or a mapping (kind 1) for a
    /// pattern? `str`, `bytes` and `bytearray` are not sequences there.
    pub fn match_kind(&mut self, value: &Value, kind: u8) -> PyResult<bool> {
        let inner = self.unwrap_builtin(value);
        let builtin = match (&inner, kind) {
            (Value::List(_) | Value::Tuple(_) | Value::Range(_), 0) => true,
            (Value::Dict(_), 1) => true,
            _ => false,
        };
        if builtin {
            return Ok(true);
        }
        if let Value::Instance(instance) = value {
            let flag = if kind == 0 { "__match_sequence__" } else { "__match_mapping__" };
            if let Some(v) = instance.class().lookup(flag) {
                return self.truthy(&v);
            }
        }
        Ok(false)
    }

    /// The values a class pattern `cls(p1, .., pN, k1=.., ...)` matches
    /// its sub-patterns against, or None.
    pub fn match_class(&mut self, subject: &Value, class: &Value, positional: usize, names: &Value) -> PyResult<Value> {
        let Value::Class(class) = class else {
            return self.type_error("called match pattern must be a class");
        };
        if !self.class_of(subject).is_subclass(class) {
            return Ok(Value::None);
        }
        let keywords: Vec<Value> = match names {
            Value::Tuple(items) => items.to_vec(),
            _ => Vec::new(),
        };
        let mut attributes: Vec<Rc<str>> = Vec::new();
        let mut values = Vec::new();
        let t = &self.types;
        let self_matching = [&t.bool, &t.bytes, &t.dict, &t.float, &t.frozenset, &t.int, &t.list, &t.set, &t.str, &t.tuple]
            .iter()
            .any(|c| Rc::ptr_eq(c, class));
        let mut start = 0;
        if positional > 0 {
            if self_matching {
                if positional > 1 {
                    return self.type_error(&alloc::format!("{}() accepts 1 positional sub-pattern ({} given)", class.name, positional));
                }
                values.push(subject.clone());
                start = 1;
            } else {
                let args = match class.lookup("__match_args__") {
                    Some(Value::Tuple(items)) => items.to_vec(),
                    Some(_) => return self.type_error(&alloc::format!("{}.__match_args__ must be a tuple (got list)", class.name)),
                    None => Vec::new(),
                };
                if positional > args.len() {
                    let plural = if args.len() == 1 { "" } else { "s" };
                    return self.type_error(&alloc::format!(
                        "{}() accepts {} positional sub-pattern{} ({} given)",
                        class.name,
                        args.len(),
                        plural,
                        positional
                    ));
                }
                for arg in args.iter().take(positional) {
                    let Value::Str(name) = arg else {
                        return self.type_error("__match_args__ elements must be strings");
                    };
                    attributes.push(name.clone());
                }
            }
        }
        let _ = start;
        for keyword in keywords.iter() {
            if let Value::Str(name) = keyword {
                attributes.push(name.clone());
            }
        }
        for (i, name) in attributes.iter().enumerate() {
            if attributes[..i].contains(name) {
                return self.type_error(&alloc::format!("{}() got multiple sub-patterns for attribute '{}'", class.name, name));
            }
            match self.get_attr(subject, name) {
                Ok(value) => values.push(value),
                Err(error) => {
                    let attribute_error = self.types.attribute_error.clone();
                    if self.is_instance_of(&error, &attribute_error) {
                        return Ok(Value::None);
                    }
                    return Err(error);
                }
            }
        }
        Ok(Value::tuple(values))
    }

    /// The values of `keys` in the mapping `subject`, or None when one is
    /// missing.
    pub fn match_keys(&mut self, subject: &Value, keys: &Value) -> PyResult<Value> {
        let keys: Vec<Value> = match keys {
            Value::Tuple(items) => items.to_vec(),
            _ => Vec::new(),
        };
        for (i, key) in keys.iter().enumerate() {
            for earlier in keys[..i].iter() {
                if self.eq(earlier, key)? {
                    let text = self.repr(key)?;
                    return self.value_error(&alloc::format!("mapping pattern checks duplicate key ({})", text));
                }
            }
        }
        let inner = self.unwrap_builtin(subject);
        let mut values = Vec::new();
        if let (Value::Dict(dict), false) = (&inner, matches!(subject, Value::Instance(_))) {
            for key in keys.iter() {
                match self.dict_get(dict, key)? {
                    Some(value) => values.push(value),
                    None => return Ok(Value::None),
                }
            }
            return Ok(Value::tuple(values));
        }
        // Any other mapping: its `get`, with a marker for "missing".
        let marker = Value::Instance(super::value::Instance::new(self.types.object.clone()));
        let get = self.get_attr(subject, "get")?;
        for key in keys.iter() {
            let value = self.call(&get, &[key.clone(), marker.clone()], &[])?;
            if value.is(&marker) {
                return Ok(Value::None);
            }
            values.push(value);
        }
        Ok(Value::tuple(values))
    }

    /// `{**rest}`: the items of the mapping other than `keys`.
    pub fn copy_dict_without(&mut self, subject: &Value, keys: &Value) -> PyResult<Value> {
        let items = self.mapping_items(subject)?;
        let result = Rc::new(core::cell::RefCell::new(Dict::new()));
        for (key, value) in items {
            self.dict_insert(&result, key, value)?;
        }
        if let Value::Tuple(keys) = keys {
            for key in keys.iter() {
                self.dict_remove(&result, key)?;
            }
        }
        Ok(Value::Dict(result))
    }
}
