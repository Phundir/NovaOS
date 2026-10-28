//! The semantics of operations: attributes, operators, comparisons,
//! subscripts, iteration, hashing, truth, `repr` and `str`.
//!
//! Each operation has a fast path for the built-in types and, for user
//! classes, the protocol of the data model: `a + b` tries `a.__add__(b)`,
//! then `b.__radd__(a)` (first when b's class derives from a's), and a
//! `NotImplemented` from both is the `TypeError` CPython gives, word for
//! word.

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::cmp::Ordering;

use super::ast::{BinOp, CmpOp, UnaryOp};
use super::dict::{str_hash, Dict, SetTable};
use super::int::Int;
use super::value::*;
use super::vm::{PyErr, PyResult, Vm};
use crate::web::js::num::Num;

/// Dunder names of the binary operators: (normal, reflected, in-place).
fn dunders(op: BinOp) -> (&'static str, &'static str, &'static str) {
    match op {
        BinOp::Add => ("__add__", "__radd__", "__iadd__"),
        BinOp::Sub => ("__sub__", "__rsub__", "__isub__"),
        BinOp::Mul => ("__mul__", "__rmul__", "__imul__"),
        BinOp::MatMul => ("__matmul__", "__rmatmul__", "__imatmul__"),
        BinOp::Div => ("__truediv__", "__rtruediv__", "__itruediv__"),
        BinOp::FloorDiv => ("__floordiv__", "__rfloordiv__", "__ifloordiv__"),
        BinOp::Mod => ("__mod__", "__rmod__", "__imod__"),
        BinOp::Pow => ("__pow__", "__rpow__", "__ipow__"),
        BinOp::LShift => ("__lshift__", "__rlshift__", "__ilshift__"),
        BinOp::RShift => ("__rshift__", "__rrshift__", "__irshift__"),
        BinOp::BitOr => ("__or__", "__ror__", "__ior__"),
        BinOp::BitXor => ("__xor__", "__rxor__", "__ixor__"),
        BinOp::BitAnd => ("__and__", "__rand__", "__iand__"),
    }
}

fn compare_dunders(op: CmpOp) -> (&'static str, &'static str, &'static str) {
    match op {
        CmpOp::Eq => ("__eq__", "__eq__", "=="),
        CmpOp::NotEq => ("__ne__", "__ne__", "!="),
        CmpOp::Lt => ("__lt__", "__gt__", "<"),
        CmpOp::LtE => ("__le__", "__ge__", "<="),
        CmpOp::Gt => ("__gt__", "__lt__", ">"),
        CmpOp::GtE => ("__ge__", "__le__", ">="),
        _ => ("", "", ""),
    }
}

/// A number as a float, if it is one (bool and int included).
fn as_float(value: &Value) -> Option<Num> {
    match value {
        Value::Float(f) => Some(*f),
        Value::Int(i) => Some(i.to_float()),
        Value::Bool(b) => Some(if *b { Num::ONE } else { Num::ZERO }),
        _ => None,
    }
}

fn as_int(value: &Value) -> Option<Int> {
    match value {
        Value::Int(i) => Some(i.clone()),
        Value::Bool(b) => Some(Int::Small(*b as i64)),
        _ => None,
    }
}

impl Vm {
    // --------------------------------------------------------
    //  Types
    // --------------------------------------------------------

    /// The class of any value.
    pub fn class_of(&self, value: &Value) -> Rc<Class> {
        let t = &self.types;
        match value {
            Value::None => t.none_type.clone(),
            Value::NotImplemented => t.not_implemented_type.clone(),
            Value::Ellipsis => t.ellipsis_type.clone(),
            Value::Bool(_) => t.bool.clone(),
            Value::Int(_) => t.int.clone(),
            Value::Float(_) => t.float.clone(),
            Value::Str(_) => t.str.clone(),
            Value::Bytes(_) => t.bytes.clone(),
            Value::List(_) => t.list.clone(),
            Value::Tuple(_) => t.tuple.clone(),
            Value::Dict(_) => t.dict.clone(),
            Value::Set(_) => t.set.clone(),
            Value::FrozenSet(_) => t.frozenset.clone(),
            Value::View(v) => match v.kind {
                0 => t.dict_keys.clone(),
                1 => t.dict_values.clone(),
                _ => t.dict_items.clone(),
            },
            Value::Range(_) => t.range.clone(),
            Value::Slice(_) => t.slice.clone(),
            Value::Function(_) => t.function.clone(),
            Value::Builtin(_) => t.builtin_function.clone(),
            Value::Method(_) => t.method.clone(),
            Value::Class(_) => t.type_.clone(),
            Value::Instance(i) => i.class(),
            Value::Iter(_) => t.iterator.clone(),
            Value::Generator(_) => t.generator.clone(),
            Value::Module(_) => t.module.clone(),
            Value::Property(_) => t.property.clone(),
            Value::StaticMethod(_) => t.staticmethod.clone(),
            Value::ClassMethod(_) => t.classmethod.clone(),
            Value::Super(_) => t.super_.clone(),
            Value::File(_) => t.file.clone(),
            Value::Code(_) | Value::Cell(_) => t.object.clone(),
        }
    }

    pub fn type_name(&self, value: &Value) -> Rc<str> {
        self.class_of(value).name.clone()
    }

    /// The built-in value inside an instance of a subclass of a built-in
    /// type, or the value itself.
    pub fn unwrap_builtin(&self, value: &Value) -> Value {
        if let Value::Instance(i) = value {
            if !matches!(i.class().base, Base::Object | Base::Exception | Base::Other) {
                return i.inner.borrow().clone();
            }
        }
        value.clone()
    }

    // --------------------------------------------------------
    //  Attributes
    // --------------------------------------------------------

    /// Binds what a class attribute lookup found to `receiver`.
    fn bind(&mut self, found: Value, receiver: &Value, owner: &Rc<Class>) -> PyResult<Value> {
        Ok(match found {
            Value::Function(_) => Value::Method(Rc::new((receiver.clone(), found))),
            Value::Builtin(ref b) if b.method => Value::Method(Rc::new((receiver.clone(), found))),
            Value::StaticMethod(inner) => (*inner).clone(),
            Value::ClassMethod(inner) => Value::Method(Rc::new((Value::Class(owner.clone()), (*inner).clone()))),
            Value::Property(p) => {
                if p[0].is_none() {
                    return self.attribute_error("property has no getter");
                }
                self.call(&p[0], core::slice::from_ref(receiver), &[])?
            }
            other => other,
        })
    }

    pub fn get_attr(&mut self, object: &Value, name: &str) -> PyResult<Value> {
        match object {
            Value::Instance(instance) => {
                let class = instance.class();
                let found = class.lookup(name);
                // Data descriptors (properties, objects with `__get__` and
                // `__set__`) win over the instance.
                if let Some(Value::Property(_)) = &found {
                    return self.bind(found.unwrap_or(Value::None), object, &class);
                }
                if let Some(descriptor @ Value::Instance(d)) = &found {
                    let kind = d.class();
                    if kind.lookup("__get__").is_some() && (kind.lookup("__set__").is_some() || kind.lookup("__delete__").is_some()) {
                        let descriptor = descriptor.clone();
                        return Ok(self.call_special(&descriptor, "__get__", &[object.clone(), Value::Class(class.clone())])?.unwrap_or(Value::None));
                    }
                }
                if let Some(v) = instance.dict.borrow().get_str(name) {
                    return Ok(v);
                }
                match name {
                    "__class__" => return Ok(Value::Class(class)),
                    "__dict__" => return Ok(Value::Dict(instance.dict.clone())),
                    _ => {}
                }
                if class.base == Base::Exception {
                    match name {
                        "__traceback__" => return Ok(Value::None),
                        "__cause__" => return Ok(instance.cause.borrow().clone()),
                        "__context__" => return Ok(instance.context.borrow().clone()),
                        _ => {}
                    }
                }
                if let Some(found) = found {
                    // A non-data descriptor: after the instance's own dict.
                    if let Value::Instance(d) = &found {
                        if d.class().lookup("__get__").is_some() {
                            return Ok(self.call_special(&found, "__get__", &[object.clone(), Value::Class(class.clone())])?.unwrap_or(Value::None));
                        }
                    }
                    // Methods of a built-in base act on the value inside.
                    if let Value::Builtin(b) = &found {
                        if b.method && !matches!(class.base, Base::Object | Base::Exception | Base::Other) {
                            let inner = instance.inner.borrow().clone();
                            if !inner.is_none() {
                                return Ok(Value::Method(Rc::new((inner, found))));
                            }
                        }
                    }
                    return self.bind(found, object, &class);
                }
                if let Some(getattr) = class.lookup("__getattr__") {
                    return self.call(&getattr, &[object.clone(), Value::str(name)], &[]);
                }
                let message = alloc::format!("'{}' object has no attribute '{}'", class.name, name);
                self.attribute_error(&message)
            }
            Value::Class(class) => {
                match name {
                    "__name__" => return Ok(Value::Str(class.name.clone())),
                    "__qualname__" => return Ok(Value::Str(class.qualname.borrow().clone())),
                    "__mro__" => return Ok(Value::tuple(class.mro.borrow().iter().map(|c| Value::Class(c.clone())).collect())),
                    "__bases__" => {
                        let bases: Vec<Value> = if class.bases.is_empty() && class.id != self.types.object.id {
                            alloc::vec![Value::Class(self.types.object.clone())]
                        } else {
                            class.bases.iter().map(|c| Value::Class(c.clone())).collect()
                        };
                        return Ok(Value::tuple(bases));
                    }
                    "__dict__" => return Ok(Value::Dict(shared(class.dict.borrow().clone()))),
                    "__class__" => return Ok(Value::Class(self.types.type_.clone())),
                    _ => {}
                }
                match class.lookup(name) {
                    Some(Value::ClassMethod(inner)) => Ok(Value::Method(Rc::new((object.clone(), (*inner).clone())))),
                    Some(Value::StaticMethod(inner)) => Ok((*inner).clone()),
                    Some(descriptor @ Value::Instance(_)) => {
                        let has_get = matches!(&descriptor, Value::Instance(d) if d.class().lookup("__get__").is_some());
                        if has_get {
                            return Ok(self.call_special(&descriptor, "__get__", &[Value::None, object.clone()])?.unwrap_or(Value::None));
                        }
                        Ok(descriptor)
                    }
                    Some(other) => Ok(other),
                    None => {
                        // Methods of `type` itself (`mro`, `__subclasses__`).
                        if let Some(found) = self.types.type_.lookup(name) {
                            return self.bind(found, object, class);
                        }
                        let message = alloc::format!("type object '{}' has no attribute '{}'", class.name, name);
                        self.attribute_error(&message)
                    }
                }
            }
            Value::Module(module) => {
                if let Some(v) = module.dict.borrow().get_str(name) {
                    return Ok(v);
                }
                if name == "__dict__" {
                    return Ok(Value::Dict(module.dict.clone()));
                }
                let message = alloc::format!("module '{}' has no attribute '{}'", module.name, name);
                self.attribute_error(&message)
            }
            Value::Function(function) => match name {
                "__name__" => Ok(Value::Str(function.name.borrow().clone())),
                "__qualname__" => Ok(Value::Str(function.qualname.borrow().clone())),
                "__doc__" => Ok(function.doc.borrow().clone()),
                "__module__" => Ok(function.globals.dict.borrow().get_str("__name__").unwrap_or(Value::None)),
                "__defaults__" => Ok(if function.defaults.is_empty() { Value::None } else { Value::tuple(function.defaults.clone()) }),
                "__dict__" => Ok(Value::Dict(shared(function.dict.borrow().clone()))),
                "__code__" => Ok(Value::Code(function.code.clone())),
                _ => {
                    if let Some(v) = function.dict.borrow().get_str(name) {
                        return Ok(v);
                    }
                    self.generic_attr(object, name)
                }
            },
            Value::Builtin(b) if name == "__name__" => Ok(Value::str(b.name)),
            Value::Method(m) if name == "__self__" => Ok(m.0.clone()),
            Value::Method(m) if name == "__func__" => Ok(m.1.clone()),
            Value::Method(m) if matches!(name, "__name__" | "__qualname__" | "__doc__") => {
                let function = m.1.clone();
                self.get_attr(&function, name)
            }
            Value::Super(s) => {
                let (start, receiver) = (&s.0, &s.1);
                let owner = match receiver {
                    Value::Class(c) => c.clone(),
                    other => self.class_of(other),
                };
                let mro = owner.mro.borrow().clone();
                let position = mro.iter().position(|c| c.id == start.id).map_or(mro.len(), |p| p + 1);
                for class in &mro[position..] {
                    let found = class.dict.borrow().get_str(name);
                    if let Some(found) = found {
                        return match (&found, receiver) {
                            (Value::Function(_), Value::Class(_)) => Ok(found),
                            _ => self.bind(found, receiver, &owner),
                        };
                    }
                }
                let message = alloc::format!("'super' object has no attribute '{}'", name);
                self.attribute_error(&message)
            }
            _ => self.generic_attr(object, name),
        }
    }

    /// Attributes of built-in values: their class's methods.
    fn generic_attr(&mut self, object: &Value, name: &str) -> PyResult<Value> {
        let class = self.class_of(object);
        if name == "__class__" {
            return Ok(Value::Class(class));
        }
        if let Some(found) = class.lookup(name) {
            return self.bind(found, object, &class);
        }
        if let Some(value) = super::types::special_attr(self, object, name)? {
            return Ok(value);
        }
        let message = alloc::format!("'{}' object has no attribute '{}'", class.name, name);
        self.attribute_error(&message)
    }

    pub fn set_attr(&mut self, object: &Value, name: &str, value: Value) -> PyResult<()> {
        match object {
            Value::Instance(instance) => {
                let class = instance.class();
                if let Some(setattr) = class.lookup("__setattr__") {
                    if matches!(setattr, Value::Function(_)) {
                        self.call(&setattr, &[object.clone(), Value::str(name), value], &[])?;
                        return Ok(());
                    }
                }
                match class.lookup(name) {
                    Some(Value::Property(p)) => {
                        if p[1].is_none() {
                            let message = alloc::format!("property '{}' of '{}' object has no setter", name, class.name);
                            return self.attribute_error(&message);
                        }
                        self.call(&p[1], &[object.clone(), value], &[])?;
                        return Ok(());
                    }
                    Some(descriptor @ Value::Instance(_)) => {
                        if let Some(set) = self.special(&descriptor, "__set__")? {
                            self.call(&set, &[object.clone(), value], &[])?;
                            return Ok(());
                        }
                    }
                    _ => {}
                }
                match name {
                    "__class__" => {
                        if let Value::Class(c) = value {
                            *instance.class.borrow_mut() = c;
                            return Ok(());
                        }
                        return self.type_error("__class__ must be set to a class");
                    }
                    "__cause__" => {
                        *instance.cause.borrow_mut() = value;
                        return Ok(());
                    }
                    "__context__" => {
                        *instance.context.borrow_mut() = value;
                        return Ok(());
                    }
                    _ => {}
                }
                instance.dict.borrow_mut().set_str(name, value);
                Ok(())
            }
            Value::Class(class) => {
                class.dict.borrow_mut().set_str(name, value);
                Ok(())
            }
            Value::Module(module) => {
                module.dict.borrow_mut().set_str(name, value);
                Ok(())
            }
            Value::Function(function) => {
                match name {
                    "__name__" => {
                        if let Value::Str(s) = &value {
                            *function.name.borrow_mut() = s.clone();
                        }
                    }
                    "__qualname__" => {
                        if let Value::Str(s) = &value {
                            *function.qualname.borrow_mut() = s.clone();
                        }
                    }
                    "__doc__" => *function.doc.borrow_mut() = value,
                    _ => function.dict.borrow_mut().set_str(name, value),
                }
                Ok(())
            }
            other => {
                let class = self.class_of(other);
                let message = alloc::format!("'{}' object has no attribute '{}'", class.name, name);
                self.attribute_error(&message)
            }
        }
    }

    pub fn del_attr(&mut self, object: &Value, name: &str) -> PyResult<()> {
        let removed = match object {
            Value::Instance(i) => {
                let class = i.class();
                match class.lookup(name) {
                    Some(Value::Property(p)) => {
                        if !p[2].is_none() {
                            self.call(&p[2], core::slice::from_ref(object), &[])?;
                            return Ok(());
                        }
                    }
                    Some(descriptor @ Value::Instance(_)) => {
                        if let Some(delete) = self.special(&descriptor, "__delete__")? {
                            self.call(&delete, core::slice::from_ref(object), &[])?;
                            return Ok(());
                        }
                    }
                    _ => {}
                }
                i.dict.borrow_mut().remove_str(name).is_some()
            }
            Value::Class(c) => c.dict.borrow_mut().remove_str(name).is_some(),
            Value::Module(m) => m.dict.borrow_mut().remove_str(name).is_some(),
            Value::Function(f) => f.dict.borrow_mut().remove_str(name).is_some(),
            _ => false,
        };
        if !removed {
            let class = self.class_of(object);
            let message = alloc::format!("'{}' object has no attribute '{}'", class.name, name);
            return self.attribute_error(&message);
        }
        Ok(())
    }

    /// A method found on the class of `value` (not the instance), bound:
    /// what the data model's implicit lookups use (`__len__`...).
    pub fn special(&mut self, value: &Value, name: &str) -> PyResult<Option<Value>> {
        let class = self.class_of(value);
        if matches!(class.base, Base::Object | Base::Exception | Base::Other) || matches!(value, Value::Instance(_)) {
            if let Some(found) = class.lookup(name) {
                return Ok(Some(self.bind(found, value, &class)?));
            }
        }
        Ok(None)
    }

    /// What a metaclass would do for a class (`for m in Color`, `len(Color)`):
    /// the class's own `__class_iter__`... called with the class.
    pub fn class_hook(&mut self, class: &Rc<Class>, name: &str, args: &[Value]) -> PyResult<Option<Value>> {
        let Some(hook) = class.lookup(name) else {
            return Ok(None);
        };
        let function = match hook {
            Value::ClassMethod(inner) => (*inner).clone(),
            other => other,
        };
        let mut full = alloc::vec![Value::Class(class.clone())];
        full.extend_from_slice(args);
        Ok(Some(self.call(&function, &full, &[])?))
    }

    /// Calls `value.name(args)` through the class; None if absent.
    pub fn call_special(&mut self, value: &Value, name: &str, args: &[Value]) -> PyResult<Option<Value>> {
        match self.special(value, name)? {
            Some(method) => Ok(Some(self.call(&method, args, &[])?)),
            None => Ok(None),
        }
    }

    // --------------------------------------------------------
    //  Truth, length
    // --------------------------------------------------------

    pub fn truthy(&mut self, value: &Value) -> PyResult<bool> {
        Ok(match value {
            Value::None => false,
            Value::Bool(b) => *b,
            Value::Int(i) => !i.is_zero(),
            Value::Float(f) => !f.is_zero(),
            Value::Str(s) => !s.is_empty(),
            Value::Bytes(b) => !b.is_empty(),
            Value::List(l) => !l.borrow().is_empty(),
            Value::Tuple(t) => !t.is_empty(),
            Value::Dict(d) => !d.borrow().is_empty(),
            Value::Set(s) => !s.borrow().is_empty(),
            Value::FrozenSet(s) => !s.is_empty(),
            Value::View(v) => !v.dict.borrow().is_empty(),
            Value::Range(r) => r.len() > 0,
            Value::Instance(i) => {
                let class = i.class();
                if class.lookup("__bool__").is_some() {
                    let r = self.call_special(value, "__bool__", &[])?.unwrap_or(Value::Bool(true));
                    return match r {
                        Value::Bool(b) => Ok(b),
                        _ => self.type_error("__bool__ should return bool"),
                    };
                }
                if class.lookup("__len__").is_some() {
                    return Ok(self.len(value)? > 0);
                }
                if !matches!(class.base, Base::Object | Base::Exception | Base::Other) {
                    let inner = i.inner.borrow().clone();
                    return self.truthy(&inner);
                }
                true
            }
            _ => true,
        })
    }

    pub fn len(&mut self, value: &Value) -> PyResult<usize> {
        Ok(match value {
            Value::Str(s) => s.chars().count(),
            Value::Bytes(b) => b.len(),
            Value::List(l) => l.borrow().len(),
            Value::Tuple(t) => t.len(),
            Value::Dict(d) => d.borrow().len(),
            Value::Set(s) => s.borrow().len(),
            Value::FrozenSet(s) => s.len(),
            Value::View(v) => v.dict.borrow().len(),
            Value::Range(r) => r.len().max(0) as usize,
            Value::Class(class) => {
                if let Some(r) = self.class_hook(class, "__class_len__", &[])? {
                    return Ok(super::args::int(self, &r)?.max(0) as usize);
                }
                return self.type_error("object of type 'type' has no len()");
            }
            Value::Instance(i) => {
                if let Some(r) = self.call_special(value, "__len__", &[])? {
                    return match r {
                        Value::Int(Int::Small(n)) if n >= 0 => Ok(n as usize),
                        Value::Int(_) => self.value_error("__len__() should return >= 0"),
                        other => {
                            let name = self.type_name(&other);
                            let message = alloc::format!("'{}' object cannot be interpreted as an integer", name);
                            self.type_error(&message)
                        }
                    };
                }
                let inner = i.inner.borrow().clone();
                if !inner.is_none() {
                    return self.len(&inner);
                }
                let message = alloc::format!("object of type '{}' has no len()", i.class().name);
                return self.type_error(&message);
            }
            other => {
                let name = self.type_name(other);
                let message = alloc::format!("object of type '{}' has no len()", name);
                return self.type_error(&message);
            }
        })
    }

    // --------------------------------------------------------
    //  Operators
    // --------------------------------------------------------

    pub fn binary(&mut self, op: BinOp, a: &Value, b: &Value, inplace: bool) -> PyResult<Value> {
        if let Some(result) = self.binary_builtin(op, a, b, inplace)? {
            return Ok(result);
        }
        let (name, rname, iname) = dunders(op);
        if inplace {
            if let Some(r) = self.call_special(a, iname, core::slice::from_ref(b))? {
                if !matches!(r, Value::NotImplemented) {
                    return Ok(r);
                }
            }
        }
        let a_class = self.class_of(a);
        let b_class = self.class_of(b);
        // The reflected method first when b's class is a subclass of a's
        // and overrides it.
        let b_first = b_class.id != a_class.id && b_class.is_subclass(&a_class) && b_class.lookup(rname).is_some();
        if b_first {
            if let Some(r) = self.call_special(b, rname, core::slice::from_ref(a))? {
                if !matches!(r, Value::NotImplemented) {
                    return Ok(r);
                }
            }
        }
        if let Some(r) = self.call_special(a, name, core::slice::from_ref(b))? {
            if !matches!(r, Value::NotImplemented) {
                return Ok(r);
            }
        }
        if !b_first && b_class.id != a_class.id {
            if let Some(r) = self.call_special(b, rname, core::slice::from_ref(a))? {
                if !matches!(r, Value::NotImplemented) {
                    return Ok(r);
                }
            }
        }
        // A subclass of a built-in type: the values inside.
        let (ua, ub) = (self.unwrap_builtin(a), self.unwrap_builtin(b));
        if !ua.is(a) || !ub.is(b) {
            if let Some(result) = self.binary_builtin(op, &ua, &ub, inplace)? {
                return Ok(result);
            }
        }
        // The sequence protocol's own messages.
        let sequence = |v: &Value| matches!(v, Value::Str(_) | Value::List(_) | Value::Tuple(_) | Value::Bytes(_));
        if op == BinOp::Add && sequence(&ua) && !matches!(ub, Value::Instance(_)) {
            let message = alloc::format!("can only concatenate {} (not \"{}\") to {}", a_class.name, b_class.name, a_class.name);
            return self.type_error(&message);
        }
        if op == BinOp::Mul && (sequence(&ua) || sequence(&ub)) {
            let other = if sequence(&ua) { &b_class.name } else { &a_class.name };
            let message = alloc::format!("can't multiply sequence by non-int of type '{}'", other);
            return self.type_error(&message);
        }
        let symbol = super::ast::display(op);
        let message = alloc::format!(
            "unsupported operand type(s) for {}{}: '{}' and '{}'",
            symbol,
            if inplace { "=" } else { "" },
            a_class.name,
            b_class.name
        );
        self.type_error(&message)
    }

    pub fn binary_builtin(&mut self, op: BinOp, a: &Value, b: &Value, inplace: bool) -> PyResult<Option<Value>> {
        // Integers (bools are ints).
        if let (Some(x), Some(y)) = (as_int(a), as_int(b)) {
            if !matches!(a, Value::Float(_)) && !matches!(b, Value::Float(_)) {
                return self.int_op(op, &x, &y).map(Some);
            }
        }
        // Floats with ints.
        if matches!(a, Value::Float(_)) || matches!(b, Value::Float(_)) {
            if let (Some(x), Some(y)) = (as_float(a), as_float(b)) {
                return self.float_op(op, x, y).map(Some);
            }
        }
        match (op, a, b) {
            (BinOp::Add, Value::Str(x), Value::Str(y)) => {
                let mut s = String::with_capacity(x.len() + y.len());
                s.push_str(x);
                s.push_str(y);
                return self.checked_str(s).map(Some);
            }
            (BinOp::Add, Value::Bytes(x), Value::Bytes(y)) => {
                let mut v = x.to_vec();
                v.extend_from_slice(y);
                return Ok(Some(Value::Bytes(Rc::from(v))));
            }
            (BinOp::Add, Value::List(x), Value::List(y)) => {
                if inplace {
                    let items = y.borrow().clone();
                    x.borrow_mut().extend(items);
                    return Ok(Some(a.clone()));
                }
                let mut v = x.borrow().clone();
                v.extend(y.borrow().iter().cloned());
                return Ok(Some(Value::list(v)));
            }
            (BinOp::Add, Value::List(x), other) if inplace && !matches!(other, Value::Instance(_)) => {
                let items = self.collect(other)?;
                x.borrow_mut().extend(items);
                return Ok(Some(a.clone()));
            }
            (BinOp::Add, Value::Tuple(x), Value::Tuple(y)) => {
                let mut v = x.to_vec();
                v.extend(y.iter().cloned());
                return Ok(Some(Value::tuple(v)));
            }
            (BinOp::Mul, seq, n) | (BinOp::Mul, n, seq) if matches!(seq, Value::Str(_) | Value::List(_) | Value::Tuple(_) | Value::Bytes(_)) && as_int(n).is_some() && !matches!(n, Value::Float(_)) => {
                let count = as_int(n).and_then(|i| i.to_i64()).unwrap_or(0).max(0) as usize;
                return self.repeat(seq, count, inplace).map(Some);
            }
            (BinOp::Mod, Value::Str(format), args) => {
                return super::format::percent(self, format, args).map(|s| Some(Value::str(&s)));
            }
            (BinOp::BitOr, Value::Dict(x), Value::Dict(y)) => {
                if inplace {
                    let pairs: Vec<(Value, Value)> = y.borrow().iter().map(|e| (e.key.clone(), e.value.clone())).collect();
                    for (k, v) in pairs {
                        self.dict_insert(x, k, v)?;
                    }
                    return Ok(Some(a.clone()));
                }
                let copy = shared(x.borrow().clone());
                let pairs: Vec<(Value, Value)> = y.borrow().iter().map(|e| (e.key.clone(), e.value.clone())).collect();
                for (k, v) in pairs {
                    self.dict_insert(&copy, k, v)?;
                }
                return Ok(Some(Value::Dict(copy)));
            }
            _ => {}
        }
        // `int | None`: a union of types (annotations, isinstance).
        if op == BinOp::BitOr && self.is_type_like(a) && self.is_type_like(b) && (matches!(a, Value::Class(_)) || matches!(b, Value::Class(_)) || self.is_union(a) || self.is_union(b)) {
            return self.make_union(a, b).map(Some);
        }
        // Views of keys and items are sets for these operators.
        if matches!(op, BinOp::BitOr | BinOp::BitAnd | BinOp::Sub | BinOp::BitXor) {
            let is_view = |v: &Value| matches!(v, Value::View(view) if view.kind != 1);
            if is_view(a) || is_view(b) {
                let x = self.as_set(a)?;
                let y = self.as_set(b)?;
                if let (Some(x), Some(y)) = (x, y) {
                    return super::types::set_operator(self, op, &x, &y, false).map(Some);
                }
            }
        }
        // Sets.
        let a_set = matches!(a, Value::Set(_) | Value::FrozenSet(_));
        let b_set = matches!(b, Value::Set(_) | Value::FrozenSet(_));
        if a_set && b_set && matches!(op, BinOp::BitOr | BinOp::BitAnd | BinOp::Sub | BinOp::BitXor) {
            return super::types::set_operator(self, op, a, b, inplace).map(Some);
        }
        Ok(None)
    }

    fn repeat(&mut self, seq: &Value, count: usize, inplace: bool) -> PyResult<Value> {
        let length = match seq {
            Value::Str(s) => s.len(),
            Value::Bytes(b) => b.len(),
            Value::List(l) => l.borrow().len(),
            Value::Tuple(t) => t.len(),
            _ => 0,
        };
        if length.saturating_mul(count) > 1 << 26 || !self.host.can_allocate(length.saturating_mul(count).saturating_mul(16)) {
            let class = self.types.memory_error.clone();
            return Err(self.error(&class, "repetition trop grande"));
        }
        Ok(match seq {
            Value::Str(s) => Value::str(&s.repeat(count)),
            Value::Bytes(b) => Value::Bytes(Rc::from(b.repeat(count))),
            Value::List(l) => {
                let items = l.borrow().clone();
                let mut out = Vec::with_capacity(items.len() * count);
                for _ in 0..count {
                    out.extend(items.iter().cloned());
                }
                if inplace {
                    *l.borrow_mut() = out;
                    seq.clone()
                } else {
                    Value::list(out)
                }
            }
            Value::Tuple(t) => {
                let mut out = Vec::with_capacity(t.len() * count);
                for _ in 0..count {
                    out.extend(t.iter().cloned());
                }
                Value::tuple(out)
            }
            _ => Value::None,
        })
    }

    pub fn checked_str(&mut self, s: String) -> PyResult<Value> {
        if s.len() > 1 << 28 {
            let class = self.types.memory_error.clone();
            return Err(self.error(&class, "chaine trop grande"));
        }
        Ok(Value::str(&s))
    }

    pub fn int_op(&mut self, op: BinOp, x: &Int, y: &Int) -> PyResult<Value> {
        let too_big = |vm: &mut Vm, e: &str| -> PyResult<Value> {
            let class = vm.types.memory_error.clone();
            Err(vm.error(&class, e))
        };
        Ok(Value::Int(match op {
            BinOp::Add => x.add(y),
            BinOp::Sub => x.sub(y),
            BinOp::Mul => match x.mul(y) {
                Ok(v) => v,
                Err(e) => return too_big(self, e),
            },
            BinOp::FloorDiv | BinOp::Mod => match x.divmod(y) {
                Some((q, r)) => {
                    if op == BinOp::FloorDiv {
                        q
                    } else {
                        r
                    }
                }
                None => {
                    return self.zero_division(if op == BinOp::FloorDiv { "integer division or modulo by zero" } else { "integer modulo by zero" });
                }
            },
            BinOp::Div => {
                if y.is_zero() {
                    return self.zero_division("division by zero");
                }
                return Ok(Value::Float(super::types::int_true_div(x, y)));
            }
            BinOp::Pow => {
                if y.is_negative() {
                    if x.is_zero() {
                        return self.zero_division("zero to a negative power");
                    }
                    return Ok(Value::Float(x.to_float().pow(y.to_float())));
                }
                let Some(e) = y.to_i64() else {
                    return too_big(self, "exposant trop grand");
                };
                match x.pow(e as u64) {
                    Ok(v) => v,
                    Err(m) => return too_big(self, m),
                }
            }
            BinOp::LShift | BinOp::RShift => {
                if y.is_negative() {
                    return self.value_error("negative shift count");
                }
                let shift = y.to_i64().unwrap_or(i64::MAX) as u64;
                if op == BinOp::LShift {
                    match x.shl(shift) {
                        Ok(v) => v,
                        Err(m) => return too_big(self, m),
                    }
                } else {
                    x.shr(shift)
                }
            }
            BinOp::BitAnd => x.bitop(y, b'&'),
            BinOp::BitOr => x.bitop(y, b'|'),
            BinOp::BitXor => x.bitop(y, b'^'),
            BinOp::MatMul => return self.type_error("unsupported operand type(s) for @: 'int' and 'int'"),
        }))
    }

    pub fn float_op(&mut self, op: BinOp, x: Num, y: Num) -> PyResult<Value> {
        Ok(Value::Float(match op {
            BinOp::Add => x.add(y),
            BinOp::Sub => x.sub(y),
            BinOp::Mul => x.mul(y),
            BinOp::Div => {
                if y.is_zero() {
                    return self.zero_division("division by zero");
                }
                x.div(y)
            }
            BinOp::FloorDiv | BinOp::Mod => {
                if y.is_zero() {
                    return self.zero_division(if op == BinOp::FloorDiv { "float floor division by zero" } else { "float modulo" });
                }
                let (q, r) = super::types::float_divmod(x, y);
                if op == BinOp::FloorDiv { q } else { r }
            }
            BinOp::Pow => {
                if x.is_zero() && y.is_negative() {
                    return self.zero_division("zero to a negative power");
                }
                if x.is_negative() && !y.is_integer() && x.is_finite() && y.is_finite() {
                    return self.value_error("math domain error");
                }
                let r = x.pow(y);
                if r.is_infinite() && x.is_finite() && y.is_finite() {
                    return self.overflow("(34, 'Numerical result out of range')");
                }
                r
            }
            _ => {
                let symbol = super::ast::display(op);
                let message = alloc::format!("unsupported operand type(s) for {}: 'float' and 'float'", symbol);
                return self.type_error(&message);
            }
        }))
    }

    pub fn unary(&mut self, op: UnaryOp, a: &Value) -> PyResult<Value> {
        match (op, a) {
            (UnaryOp::Not, _) => return Ok(Value::Bool(!self.truthy(a)?)),
            (UnaryOp::Neg, Value::Int(i)) => return Ok(Value::Int(i.neg())),
            (UnaryOp::Neg, Value::Bool(b)) => return Ok(Value::int(-(*b as i64))),
            (UnaryOp::Neg, Value::Float(f)) => return Ok(Value::Float(f.neg())),
            (UnaryOp::Pos, Value::Int(_) | Value::Float(_)) => return Ok(a.clone()),
            (UnaryOp::Pos, Value::Bool(b)) => return Ok(Value::int(*b as i64)),
            (UnaryOp::Invert, Value::Int(i)) => return Ok(Value::Int(i.invert())),
            (UnaryOp::Invert, Value::Bool(b)) => return Ok(Value::int(!(*b as i64))),
            _ => {}
        }
        let name = match op {
            UnaryOp::Neg => "__neg__",
            UnaryOp::Pos => "__pos__",
            UnaryOp::Invert => "__invert__",
            UnaryOp::Not => "__bool__",
        };
        if let Some(r) = self.call_special(a, name, &[])? {
            return Ok(r);
        }
        let inner = self.unwrap_builtin(a);
        if !inner.is(a) {
            return self.unary(op, &inner);
        }
        let symbol = match op {
            UnaryOp::Neg => "-",
            UnaryOp::Pos => "+",
            _ => "~",
        };
        let tn = self.type_name(a);
        let message = alloc::format!("bad operand type for unary {}: '{}'", symbol, tn);
        self.type_error(&message)
    }

    // --------------------------------------------------------
    //  Comparisons
    // --------------------------------------------------------

    pub fn compare(&mut self, op: CmpOp, a: &Value, b: &Value) -> PyResult<Value> {
        if let Some(ordering) = self.builtin_compare(a, b, op)? {
            let r = match op {
                CmpOp::Eq => ordering == Some(Ordering::Equal),
                CmpOp::NotEq => ordering != Some(Ordering::Equal),
                CmpOp::Lt => ordering == Some(Ordering::Less),
                CmpOp::LtE => matches!(ordering, Some(Ordering::Less | Ordering::Equal)),
                CmpOp::Gt => ordering == Some(Ordering::Greater),
                CmpOp::GtE => matches!(ordering, Some(Ordering::Greater | Ordering::Equal)),
                _ => false,
            };
            return Ok(Value::Bool(r));
        }
        let (name, reflected, symbol) = compare_dunders(op);
        let a_class = self.class_of(a);
        let b_class = self.class_of(b);
        let b_first = b_class.id != a_class.id && b_class.is_subclass(&a_class);
        let mut order: Vec<(Value, &str, Value)> = alloc::vec![(a.clone(), name, b.clone()), (b.clone(), reflected, a.clone())];
        if b_first {
            order.swap(0, 1);
        }
        for (x, method, y) in order {
            if let Some(r) = self.call_special(&x, method, core::slice::from_ref(&y))? {
                if !matches!(r, Value::NotImplemented) {
                    return Ok(r);
                }
            }
        }
        // A subclass of a built-in type: the values inside.
        let (ua, ub) = (self.unwrap_builtin(a), self.unwrap_builtin(b));
        if !ua.is(a) || !ub.is(b) {
            return self.compare(op, &ua, &ub);
        }
        match op {
            CmpOp::Eq => Ok(Value::Bool(a.is(b))),
            CmpOp::NotEq => {
                // Default __ne__: the inverse of __eq__.
                let eq = self.compare(CmpOp::Eq, a, b)?;
                Ok(Value::Bool(!self.truthy(&eq)?))
            }
            _ => {
                let message = alloc::format!("'{}' not supported between instances of '{}' and '{}'", symbol, a_class.name, b_class.name);
                self.type_error(&message)
            }
        }
    }

    /// Ordering of built-in values: None when the types do not compare
    /// that way (`Some(None)`: unordered, as NaN).
    fn builtin_compare(&mut self, a: &Value, b: &Value, op: CmpOp) -> PyResult<Option<Option<Ordering>>> {
        let equality = matches!(op, CmpOp::Eq | CmpOp::NotEq);
        Ok(Some(match (a, b) {
            (Value::Int(_) | Value::Bool(_), Value::Int(_) | Value::Bool(_)) => {
                let (x, y) = (as_int(a).unwrap_or(Int::zero()), as_int(b).unwrap_or(Int::zero()));
                Some(x.cmp(&y))
            }
            (Value::Float(_), Value::Float(_) | Value::Int(_) | Value::Bool(_)) | (Value::Int(_) | Value::Bool(_), Value::Float(_)) => {
                super::types::compare_numbers(a, b)
            }
            (Value::Str(x), Value::Str(y)) => Some(x.as_ref().cmp(y.as_ref())),
            (Value::Bytes(x), Value::Bytes(y)) => Some(x.as_ref().cmp(y.as_ref())),
            (Value::List(x), Value::List(y)) => {
                let (x, y) = (x.borrow().clone(), y.borrow().clone());
                return self.sequence_compare(&x, &y, op);
            }
            (Value::Tuple(x), Value::Tuple(y)) => {
                let (x, y) = (x.to_vec(), y.to_vec());
                return self.sequence_compare(&x, &y, op);
            }
            (Value::None, Value::None) if equality => Some(Ordering::Equal),
            (Value::Dict(x), Value::Dict(y)) if equality => {
                if Rc::ptr_eq(x, y) {
                    Some(Ordering::Equal)
                } else {
                    let equal = self.dict_equal(x, y)?;
                    if equal { Some(Ordering::Equal) } else { Some(Ordering::Less) }
                }
            }
            (Value::Set(_) | Value::FrozenSet(_), Value::Set(_) | Value::FrozenSet(_)) => {
                return super::types::set_compare(self, a, b, op).map(Some);
            }
            (Value::View(v), Value::Set(_) | Value::FrozenSet(_) | Value::View(_)) | (Value::Set(_) | Value::FrozenSet(_), Value::View(v)) if v.kind != 1 => {
                let (x, y) = (self.as_set(a)?, self.as_set(b)?);
                return match (x, y) {
                    (Some(x), Some(y)) => super::types::set_compare(self, &x, &y, op).map(Some),
                    _ => Ok(Some(if a.is(b) { Some(Ordering::Equal) } else { Some(Ordering::Less) })),
                };
            }
            (Value::Range(x), Value::Range(y)) if equality => {
                let same = x.len() == y.len() && (x.len() == 0 || (x.start == y.start && (x.len() == 1 || x.step == y.step)));
                Some(if same { Ordering::Equal } else { Ordering::Less })
            }
            (Value::Instance(_), _) | (_, Value::Instance(_)) => return Ok(None),
            _ if equality => {
                // Different built-in types: equal only if the same object.
                Some(if a.is(b) { Ordering::Equal } else { Ordering::Less })
            }
            _ => return Ok(None),
        }))
    }

    fn sequence_compare(&mut self, x: &[Value], y: &[Value], op: CmpOp) -> PyResult<Option<Option<Ordering>>> {
        if matches!(op, CmpOp::Eq | CmpOp::NotEq) && x.len() != y.len() {
            return Ok(Some(Some(Ordering::Less)));
        }
        for (a, b) in x.iter().zip(y.iter()) {
            if a.is(b) {
                continue;
            }
            if !self.eq(a, b)? {
                if matches!(op, CmpOp::Eq | CmpOp::NotEq) {
                    return Ok(Some(Some(Ordering::Less)));
                }
                // The first difference decides, with the operator.
                let less = self.compare(CmpOp::Lt, a, b)?;
                return Ok(Some(Some(if self.truthy(&less)? { Ordering::Less } else { Ordering::Greater })));
            }
        }
        Ok(Some(Some(x.len().cmp(&y.len()))))
    }

    /// `a == b` as a bool.
    pub fn eq(&mut self, a: &Value, b: &Value) -> PyResult<bool> {
        if a.is(b) && !matches!(a, Value::Float(f) if f.is_nan()) {
            return Ok(true);
        }
        let r = self.compare(CmpOp::Eq, a, b)?;
        self.truthy(&r)
    }

    fn dict_equal(&mut self, x: &Rc<RefCell<Dict>>, y: &Rc<RefCell<Dict>>) -> PyResult<bool> {
        if x.borrow().len() != y.borrow().len() {
            return Ok(false);
        }
        let entries: Vec<(i64, Value, Value)> = x.borrow().iter().map(|e| (e.hash, e.key.clone(), e.value.clone())).collect();
        for (hash, key, value) in entries {
            let other = {
                let table = core::mem::take(&mut *y.borrow_mut());
                let result = {
                    let mut eq = |p: &Value, q: &Value| self.eq(p, q);
                    table.get(hash, &key, &mut eq)
                };
                *y.borrow_mut() = table;
                result?
            };
            match other {
                Some(v) => {
                    if !self.eq(&value, &v)? {
                        return Ok(false);
                    }
                }
                None => return Ok(false),
            }
        }
        Ok(true)
    }

    pub fn contains(&mut self, container: &Value, item: &Value) -> PyResult<bool> {
        match container {
            Value::Str(s) => match item {
                Value::Str(needle) => Ok(s.contains(needle.as_ref())),
                other => {
                    let name = self.type_name(other);
                    let message = alloc::format!("'in <string>' requires string as left operand, not {}", name);
                    self.type_error(&message)
                }
            },
            Value::Bytes(b) => match item {
                Value::Bytes(needle) => Ok(needle.is_empty() || b.windows(needle.len()).any(|w| w == needle.as_ref())),
                Value::Int(Int::Small(n)) => Ok(b.contains(&(*n as u8))),
                _ => self.type_error("a bytes-like object is required"),
            },
            Value::List(l) => {
                let items = l.borrow().clone();
                for v in items.iter() {
                    if self.eq(v, item)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
            Value::Tuple(t) => {
                for v in t.iter() {
                    if self.eq(v, item)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
            Value::Dict(d) => Ok(self.dict_get(d, item)?.is_some()),
            Value::View(v) => match v.kind {
                0 => Ok(self.dict_get(&v.dict, item)?.is_some()),
                1 => {
                    let values = v.dict.borrow().values();
                    for value in &values {
                        if self.eq(value, item)? {
                            return Ok(true);
                        }
                    }
                    Ok(false)
                }
                _ => {
                    let Value::Tuple(pair) = item else {
                        return Ok(false);
                    };
                    if pair.len() != 2 {
                        return Ok(false);
                    }
                    match self.dict_get(&v.dict, &pair[0])? {
                        Some(value) => self.eq(&value, &pair[1]),
                        None => Ok(false),
                    }
                }
            },
            Value::Set(s) => {
                let hash = self.hash(item)?;
                // Taken out while a user __eq__ may run (see dict_get).
                let table = core::mem::take(&mut *s.borrow_mut());
                let result = {
                    let mut eq = |p: &Value, q: &Value| self.eq(p, q);
                    table.contains(hash, item, &mut eq)
                };
                *s.borrow_mut() = table;
                result
            }
            Value::FrozenSet(s) => {
                let hash = self.hash(item)?;
                let mut eq = |p: &Value, q: &Value| self.eq(p, q);
                s.contains(hash, item, &mut eq)
            }
            Value::Range(r) => {
                let Some(n) = as_int(item).and_then(|i| i.to_i64()) else {
                    return Ok(false);
                };
                if matches!(item, Value::Float(_)) {
                    return Ok(false);
                }
                let inside = if r.step > 0 { n >= r.start && n < r.stop } else { n <= r.start && n > r.stop };
                Ok(inside && (n - r.start) % r.step == 0)
            }
            Value::Class(class) => match self.class_hook(class, "__class_contains__", core::slice::from_ref(item))? {
                Some(r) => self.truthy(&r),
                None => self.type_error("argument of type 'type' is not iterable"),
            },
            _ => {
                if let Some(r) = self.call_special(container, "__contains__", core::slice::from_ref(item))? {
                    return self.truthy(&r);
                }
                if let Value::Instance(i) = container {
                    let inner = i.inner.borrow().clone();
                    if !inner.is_none() {
                        return self.contains(&inner, item);
                    }
                }
                let iterator = self.get_iter(container)?;
                while let Some(v) = self.next(&iterator)? {
                    if self.eq(&v, item)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
        }
    }

    // --------------------------------------------------------
    //  Hashing, dicts and sets
    // --------------------------------------------------------

    pub fn hash(&mut self, value: &Value) -> PyResult<i64> {
        Ok(match value {
            Value::None => 0xFCA8_6420,
            Value::Bool(b) => *b as i64,
            Value::Int(i) => i.hash(),
            Value::Float(f) => super::types::float_hash(*f, value),
            Value::Str(s) => str_hash(s),
            Value::Bytes(b) => super::dict::bytes_hash(b),
            Value::Tuple(items) => {
                // CPython's tuplehash (xxHash-like).
                const P1: u64 = 11400714785074694791;
                const P2: u64 = 14029467366897019727;
                const P5: u64 = 2870177450012600261;
                let mut acc = P5;
                for item in items.iter() {
                    let lane = self.hash(item)? as u64;
                    acc = acc.wrapping_add(lane.wrapping_mul(P2));
                    acc = acc.rotate_left(31);
                    acc = acc.wrapping_mul(P1);
                }
                acc = acc.wrapping_add((items.len() as u64) ^ (P5 ^ 3527539));
                if acc == u64::MAX {
                    1546275796
                } else {
                    acc as i64
                }
            }
            Value::FrozenSet(s) => {
                let mut h: u64 = 0;
                for (hash, _) in s.entries() {
                    let x = hash as u64;
                    h ^= ((x ^ 89869747) ^ (x << 16)).wrapping_mul(3644798167);
                }
                h as i64
            }
            Value::List(_) | Value::Dict(_) | Value::Set(_) => {
                let name = self.type_name(value);
                let message = alloc::format!("unhashable type: '{}'", name);
                return self.type_error(&message);
            }
            Value::Instance(i) => {
                let class = i.class();
                match class.lookup("__hash__") {
                    Some(Value::None) => {
                        let message = alloc::format!("unhashable type: '{}'", class.name);
                        return self.type_error(&message);
                    }
                    // A __hash__ result that fits a machine word is kept as
                    // is (-1 is -2); a larger one is hashed, as in CPython.
                    Some(h) => match self.call(&h, core::slice::from_ref(value), &[])? {
                        Value::Int(Int::Small(-1)) => -2,
                        Value::Int(Int::Small(v)) => v,
                        Value::Int(i) => i.hash(),
                        Value::Bool(b) => b as i64,
                        _ => return self.type_error("__hash__ method should return an integer"),
                    },
                    None => {
                        // __eq__ without __hash__: unhashable.
                        if class.lookup("__eq__").is_some_and(|e| matches!(e, Value::Function(_))) {
                            let message = alloc::format!("unhashable type: '{}'", class.name);
                            return self.type_error(&message);
                        }
                        let inner = i.inner.borrow().clone();
                        if !inner.is_none() && !matches!(class.base, Base::Object | Base::Exception | Base::Other) {
                            return self.hash(&inner);
                        }
                        (value.pointer().unwrap_or(0) >> 4) as i64
                    }
                }
            }
            other => (other.pointer().unwrap_or(0) >> 4) as i64,
        })
    }

    /// `dict[key] = value`; the former value.
    pub fn dict_set(&mut self, dict: &mut Dict, key: Value, value: Value) -> PyResult<Option<Value>> {
        let hash = self.hash(&key)?;
        let mut eq = |p: &Value, q: &Value| self.eq(p, q);
        dict.insert(hash, key, value, &mut eq)
    }

    pub fn dict_insert(&mut self, dict: &Rc<RefCell<Dict>>, key: Value, value: Value) -> PyResult<Option<Value>> {
        // The table is taken out while user __eq__ may run.
        let mut table = core::mem::take(&mut *dict.borrow_mut());
        let result = self.dict_set(&mut table, key, value);
        *dict.borrow_mut() = table;
        result
    }

    /// `dict[key]`. The table is moved out of its cell for the lookup (a
    /// pointer swap, not a copy): a user `__eq__` called on the way may
    /// touch the same dict, and must find a cell it can borrow -- it sees
    /// the dict empty, rather than the kernel panicking on a borrow.
    pub fn dict_get(&mut self, dict: &Rc<RefCell<Dict>>, key: &Value) -> PyResult<Option<Value>> {
        let hash = self.hash(key)?;
        let table = core::mem::take(&mut *dict.borrow_mut());
        let result = {
            let mut eq = |p: &Value, q: &Value| self.eq(p, q);
            table.get(hash, key, &mut eq)
        };
        *dict.borrow_mut() = table;
        result
    }

    pub fn dict_remove(&mut self, dict: &Rc<RefCell<Dict>>, key: &Value) -> PyResult<Option<Value>> {
        let hash = self.hash(key)?;
        let mut table = core::mem::take(&mut *dict.borrow_mut());
        let result = {
            let mut eq = |p: &Value, q: &Value| self.eq(p, q);
            table.remove(hash, key, &mut eq)
        };
        *dict.borrow_mut() = table;
        result
    }

    pub fn new_set(&mut self, items: &[Value]) -> PyResult<Value> {
        let set = Value::Set(shared(SetTable::new()));
        for item in items {
            self.set_add(&set, item.clone())?;
        }
        Ok(set)
    }

    pub fn is_union(&self, value: &Value) -> bool {
        matches!(value, Value::Instance(i) if i.class().id == self.types.union_type.id)
    }

    fn is_type_like(&self, value: &Value) -> bool {
        matches!(value, Value::Class(_) | Value::None) || self.is_union(value)
    }

    /// The members of a union (or the type itself), None as NoneType.
    pub fn union_members(&self, value: &Value) -> Vec<Value> {
        match value {
            Value::Instance(i) if self.is_union(value) => match i.dict.borrow().get_str("__args__") {
                Some(Value::Tuple(t)) => t.to_vec(),
                _ => Vec::new(),
            },
            Value::None => alloc::vec![Value::Class(self.types.none_type.clone())],
            other => alloc::vec![other.clone()],
        }
    }

    fn make_union(&mut self, a: &Value, b: &Value) -> PyResult<Value> {
        let mut members: Vec<Value> = Vec::new();
        for m in self.union_members(a).into_iter().chain(self.union_members(b)) {
            if !members.iter().any(|x| x.is(&m)) {
                members.push(m);
            }
        }
        let instance = Instance::new(self.types.union_type.clone());
        instance.dict.borrow_mut().set_str("__args__", Value::tuple(members));
        Ok(Value::Instance(instance))
    }

    /// A set of what an iterable gives, for set operators on views (None
    /// for what cannot be one: a view of values, a non-iterable).
    pub fn as_set(&mut self, value: &Value) -> PyResult<Option<Value>> {
        match value {
            Value::Set(_) | Value::FrozenSet(_) => Ok(Some(value.clone())),
            Value::View(v) if v.kind == 1 => Ok(None),
            Value::View(_) => {
                let items = self.collect(value)?;
                self.new_set(&items).map(Some)
            }
            _ => Ok(None),
        }
    }

    pub fn set_add(&mut self, set: &Value, item: Value) -> PyResult<bool> {
        let Value::Set(s) = set else {
            return Ok(false);
        };
        let hash = self.hash(&item)?;
        let mut table = core::mem::take(&mut *s.borrow_mut());
        let result = {
            let mut eq = |p: &Value, q: &Value| self.eq(p, q);
            table.add(hash, item, &mut eq)
        };
        *s.borrow_mut() = table;
        result
    }

    /// The (key, value) pairs of a mapping (`**kwargs`, `dict.update`).
    pub fn mapping_items(&mut self, value: &Value) -> PyResult<Vec<(Value, Value)>> {
        match value {
            Value::Dict(d) => Ok(d.borrow().iter().map(|e| (e.key.clone(), e.value.clone())).collect()),
            other => {
                let keys = self.get_attr(other, "keys").ok();
                match keys {
                    Some(keys) => {
                        let keys = self.call(&keys, &[], &[])?;
                        let keys = self.collect(&keys)?;
                        let mut out = Vec::new();
                        for k in keys {
                            let v = self.get_item(other, &k)?;
                            out.push((k, v));
                        }
                        Ok(out)
                    }
                    None => {
                        let name = self.type_name(other);
                        let message = alloc::format!("'{}' object is not a mapping", name);
                        self.type_error(&message)
                    }
                }
            }
        }
    }

    // --------------------------------------------------------
    //  Iteration
    // --------------------------------------------------------

    pub fn get_iter(&mut self, value: &Value) -> PyResult<Value> {
        let iter = match value {
            Value::List(_) | Value::Tuple(_) | Value::Bytes(_) => Iter::Seq { seq: value.clone(), index: 0 },
            Value::Str(s) => Iter::Str { text: s.clone(), byte: 0 },
            Value::Range(r) => Iter::Range { next: r.start, stop: r.stop, step: r.step },
            Value::Dict(d) => Iter::Keys { dict: value.clone(), index: 0, kind: 0, size: d.borrow().len() },
            Value::View(v) => Iter::Keys { dict: Value::Dict(v.dict.clone()), index: 0, kind: v.kind, size: v.dict.borrow().len() },
            Value::Set(s) => Iter::Seq { seq: Value::tuple(s.borrow().keys()), index: 0 },
            Value::FrozenSet(s) => Iter::Seq { seq: Value::tuple(s.keys()), index: 0 },
            Value::Iter(_) | Value::Generator(_) => return Ok(value.clone()),
            Value::Class(class) => {
                if let Some(r) = self.class_hook(class, "__class_iter__", &[])? {
                    return self.get_iter(&r);
                }
                let message = alloc::format!("'type' object is not iterable");
                return self.type_error(&message);
            }
            Value::File(_) => Iter::Object(value.clone()),
            Value::Instance(i) => {
                if let Some(it) = self.call_special(value, "__iter__", &[])? {
                    return match &it {
                        Value::Instance(_) => Ok(Value::Iter(shared(Iter::Object(it)))),
                        _ => Ok(it),
                    };
                }
                if i.class().lookup("__getitem__").is_some() {
                    // The old protocol: indices until IndexError.
                    return Ok(Value::Iter(shared(Iter::Seq { seq: value.clone(), index: 0 })));
                }
                let inner = i.inner.borrow().clone();
                if !inner.is_none() {
                    return self.get_iter(&inner);
                }
                let message = alloc::format!("'{}' object is not iterable", i.class().name);
                return self.type_error(&message);
            }
            other => {
                let name = self.type_name(other);
                let message = alloc::format!("'{}' object is not iterable", name);
                return self.type_error(&message);
            }
        };
        Ok(Value::Iter(shared(iter)))
    }

    /// The next item, or None at the end.
    pub fn next(&mut self, iterator: &Value) -> PyResult<Option<Value>> {
        match iterator {
            Value::Generator(g) => {
                let g = g.clone();
                match self.resume(&g, Value::None, None) {
                    Ok(v) => Ok(Some(v)),
                    Err(e) if self.is_instance_of(&e, &self.types.stop_iteration.clone()) => Ok(None),
                    Err(e) => Err(e),
                }
            }
            Value::Iter(state) => super::types::iter_next(self, state),
            Value::Instance(_) => match self.call_special(iterator, "__next__", &[]) {
                Ok(Some(v)) => Ok(Some(v)),
                Ok(None) => {
                    let name = self.type_name(iterator);
                    let message = alloc::format!("'{}' object is not an iterator", name);
                    self.type_error(&message)
                }
                Err(e) if self.is_instance_of(&e, &self.types.stop_iteration.clone()) => Ok(None),
                Err(e) => Err(e),
            },
            other => {
                let name = self.type_name(other);
                let message = alloc::format!("'{}' object is not an iterator", name);
                self.type_error(&message)
            }
        }
    }

    /// Everything an iterable gives, in a vector.
    pub fn collect(&mut self, value: &Value) -> PyResult<Vec<Value>> {
        match value {
            Value::List(l) => return Ok(l.borrow().clone()),
            Value::Tuple(t) => return Ok(t.to_vec()),
            _ => {}
        }
        let iterator = self.get_iter(value)?;
        let mut out = Vec::new();
        while let Some(v) = self.next(&iterator)? {
            if out.len() & 0xfff == 0xfff && !self.host.can_allocate(out.len() * 32) {
                let class = self.types.memory_error.clone();
                return Err(self.error(&class, "memoire du noyau insuffisante"));
            }
            out.push(v);
        }
        Ok(out)
    }

    // --------------------------------------------------------
    //  Subscripts
    // --------------------------------------------------------

    pub fn get_item(&mut self, object: &Value, index: &Value) -> PyResult<Value> {
        super::types::get_item(self, object, index)
    }

    pub fn set_item(&mut self, object: &Value, index: &Value, value: Value) -> PyResult<()> {
        super::types::set_item(self, object, index, value)
    }

    pub fn del_item(&mut self, object: &Value, index: &Value) -> PyResult<()> {
        super::types::del_item(self, object, index)
    }

    // --------------------------------------------------------
    //  Text
    // --------------------------------------------------------

    pub fn repr(&mut self, value: &Value) -> PyResult<String> {
        super::format::repr(self, value)
    }

    pub fn str_of(&mut self, value: &Value) -> PyResult<String> {
        match value {
            Value::Str(s) => Ok(String::from(&**s)),
            _ => super::format::str_of(self, value),
        }
    }

    pub fn format(&mut self, value: &Value, spec: &str) -> PyResult<String> {
        super::format::format(self, value, spec)
    }

    pub fn exc_message(&mut self, err: &PyErr) -> String {
        match err {
            PyErr::Exc(v) => {
                let name = self.type_name(v);
                let text = self.str_of(v).unwrap_or_default();
                if text.is_empty() { String::from(&*name) } else { alloc::format!("{}: {}", name, text) }
            }
            PyErr::Halt(reason) => reason.clone(),
        }
    }
}
