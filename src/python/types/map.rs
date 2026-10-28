//! Mappings and sets: `dict`, its views, `set`, `frozenset`.
//!
//! Every method that looks a key up goes through the VM (`dict_get`,
//! `set_add`...): keys may be user objects whose `__hash__` and `__eq__`
//! are Python code.

use alloc::rc::Rc;
use core::cell::RefCell;

use super::super::args;
use super::super::dict::{Dict, SetTable};
use super::super::value::*;
use super::super::vm::{PyResult, Vm};
use super::{method, set_copy, set_difference, set_difference_update, set_intersection, set_symmetric_update, set_update_from, table_add};

// ------------------------------------------------------------
//  Constructors
// ------------------------------------------------------------

/// Fills `dict` from a mapping or an iterable of pairs, then keywords
/// (`dict(...)`, `dict.update(...)`).
fn fill(vm: &mut Vm, dict: &Rc<RefCell<Dict>>, source: Option<&Value>, kw: &[(Rc<str>, Value)]) -> PyResult<()> {
    if let Some(source) = source {
        let is_mapping = match source {
            Value::Dict(_) => true,
            Value::Instance(_) => vm.get_attr(source, "keys").is_ok(),
            _ => false,
        };
        if is_mapping {
            for (k, v) in vm.mapping_items(source)? {
                vm.dict_insert(dict, k, v)?;
            }
        } else {
            let items = vm.collect(source)?;
            for (n, item) in items.iter().enumerate() {
                let pair = match item {
                    Value::Tuple(t) => t.to_vec(),
                    Value::List(l) => l.borrow().clone(),
                    Value::Str(_) | Value::Instance(_) | Value::Iter(_) | Value::Generator(_) => vm.collect(item)?,
                    other => {
                        let _ = other;
                        let message = alloc::format!("cannot convert dictionary update sequence element #{} to a sequence", n);
                        return vm.type_error(&message);
                    }
                };
                if pair.len() != 2 {
                    let message = alloc::format!("dictionary update sequence element #{} has length {}; 2 is required", n, pair.len());
                    return vm.value_error(&message);
                }
                vm.dict_insert(dict, pair[0].clone(), pair[1].clone())?;
            }
        }
    }
    for (k, v) in kw {
        vm.dict_insert(dict, Value::Str(k.clone()), v.clone())?;
    }
    Ok(())
}

pub fn new_dict(vm: &mut Vm, _class: &Rc<Class>, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    if a.len() > 1 {
        let message = alloc::format!("dict expected at most 1 argument, got {}", a.len());
        return vm.type_error(&message);
    }
    let dict = shared(Dict::new());
    fill(vm, &dict, a.first(), kw)?;
    Ok(Value::Dict(dict))
}

fn table_of(vm: &mut Vm, iterable: Option<&Value>) -> PyResult<SetTable> {
    let mut table = SetTable::new();
    if let Some(v) = iterable {
        set_update_from(vm, &mut table, v)?;
    }
    Ok(table)
}

pub fn new_set(vm: &mut Vm, _class: &Rc<Class>, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    args::no_keywords(vm, "set", kw)?;
    if a.len() > 1 {
        let message = alloc::format!("set expected at most 1 argument, got {}", a.len());
        return vm.type_error(&message);
    }
    let table = table_of(vm, a.first())?;
    Ok(Value::Set(shared(table)))
}

pub fn new_frozenset(vm: &mut Vm, _class: &Rc<Class>, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    args::no_keywords(vm, "frozenset", kw)?;
    if a.len() > 1 {
        let message = alloc::format!("frozenset expected at most 1 argument, got {}", a.len());
        return vm.type_error(&message);
    }
    if let Some(Value::FrozenSet(s)) = a.first() {
        return Ok(Value::FrozenSet(s.clone()));
    }
    let table = table_of(vm, a.first())?;
    Ok(Value::FrozenSet(Rc::new(table)))
}

// ------------------------------------------------------------
//  dict
// ------------------------------------------------------------

fn this_dict(vm: &mut Vm, a: &[Value]) -> PyResult<Rc<RefCell<Dict>>> {
    match a.first().map(|v| vm.unwrap_builtin(v)) {
        Some(Value::Dict(d)) => Ok(d),
        _ => vm.type_error("descriptor requires a 'dict' object"),
    }
}

fn view(vm: &mut Vm, a: &[Value], kind: u8) -> PyResult<Value> {
    let dict = this_dict(vm, a)?;
    Ok(Value::View(Rc::new(View { dict, kind })))
}

fn keys(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    view(vm, a, 0)
}

fn values(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    view(vm, a, 1)
}

fn items(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    view(vm, a, 2)
}

fn get(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let d = this_dict(vm, a)?;
    let Some(key) = a.get(1) else {
        return vm.type_error("get expected at least 1 argument, got 0");
    };
    Ok(vm.dict_get(&d, key)?.unwrap_or_else(|| a.get(2).cloned().unwrap_or(Value::None)))
}

fn setdefault(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let d = this_dict(vm, a)?;
    let Some(key) = a.get(1) else {
        return vm.type_error("setdefault expected at least 1 argument, got 0");
    };
    if let Some(v) = vm.dict_get(&d, key)? {
        return Ok(v);
    }
    let default = a.get(2).cloned().unwrap_or(Value::None);
    vm.dict_insert(&d, key.clone(), default.clone())?;
    Ok(default)
}

fn pop(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let d = this_dict(vm, a)?;
    let Some(key) = a.get(1) else {
        return vm.type_error("pop expected at least 1 argument, got 0");
    };
    match vm.dict_remove(&d, key)? {
        Some(v) => Ok(v),
        None => match a.get(2) {
            Some(default) => Ok(default.clone()),
            None => vm.key_error(key.clone()),
        },
    }
}

fn popitem(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let d = this_dict(vm, a)?;
    let last = d.borrow_mut().pop_last();
    match last {
        Some(entry) => Ok(Value::tuple(alloc::vec![entry.key, entry.value])),
        None => {
            let class = vm.types.key_error.clone();
            Err(vm.error(&class, "popitem(): dictionary is empty"))
        }
    }
}

fn update(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let d = this_dict(vm, a)?;
    if a.len() > 2 {
        let message = alloc::format!("update expected at most 1 argument, got {}", a.len() - 1);
        return vm.type_error(&message);
    }
    fill(vm, &d, a.get(1), kw)?;
    Ok(Value::None)
}

fn dict_init(vm: &mut Vm, a: &[Value], kw: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let d = this_dict(vm, a)?;
    fill(vm, &d, a.get(1), kw)?;
    Ok(Value::None)
}

fn clear(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let d = this_dict(vm, a)?;
    d.borrow_mut().clear();
    Ok(Value::None)
}

fn copy(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let d = this_dict(vm, a)?;
    let copy = d.borrow().clone();
    Ok(Value::Dict(shared(copy)))
}

/// `dict.fromkeys(iterable, value=None)`: a class method.
fn fromkeys(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let Some(iterable) = a.get(1) else {
        return vm.type_error("fromkeys expected at least 1 argument, got 0");
    };
    let value = a.get(2).cloned().unwrap_or(Value::None);
    let keys = vm.collect(iterable)?;
    let made = match a.first() {
        Some(class @ Value::Class(c)) if !super::is_builtin(vm, c) => vm.call(class, &[], &[])?,
        _ => Value::Dict(shared(Dict::new())),
    };
    for key in keys {
        vm.set_item(&made, &key, value.clone())?;
    }
    Ok(made)
}

/// `dict.__getitem__`: with `__missing__` for subclasses (defaultdict).
fn dict_getitem(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let d = this_dict(vm, a)?;
    let Some(key) = a.get(1) else {
        return vm.type_error("expected 1 argument, got 0");
    };
    if let Some(v) = vm.dict_get(&d, key)? {
        return Ok(v);
    }
    if let Some(receiver @ Value::Instance(_)) = a.first() {
        if let Some(r) = vm.call_special(receiver, "__missing__", core::slice::from_ref(key))? {
            return Ok(r);
        }
    }
    vm.key_error(key.clone())
}

fn dict_setitem(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let d = this_dict(vm, a)?;
    let (Some(key), Some(value)) = (a.get(1), a.get(2)) else {
        return vm.type_error("expected 2 arguments");
    };
    vm.dict_insert(&d, key.clone(), value.clone())?;
    Ok(Value::None)
}

fn dict_delitem(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let d = this_dict(vm, a)?;
    let Some(key) = a.get(1) else {
        return vm.type_error("expected 1 argument, got 0");
    };
    match vm.dict_remove(&d, key)? {
        Some(_) => Ok(Value::None),
        None => vm.key_error(key.clone()),
    }
}

fn dict_reversed(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let d = this_dict(vm, a)?;
    let keys = d.borrow().keys();
    let n = keys.len();
    Ok(Value::iter(Iter::Reversed { seq: Value::tuple(keys), index: n }))
}

fn dict_contains(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let d = this_dict(vm, a)?;
    let key = a.get(1).cloned().unwrap_or(Value::None);
    Ok(Value::Bool(vm.dict_get(&d, &key)?.is_some()))
}

// ------------------------------------------------------------
//  set and frozenset
// ------------------------------------------------------------

/// `self` of a set method: the set or frozenset value.
fn this_set(vm: &mut Vm, a: &[Value]) -> PyResult<Value> {
    match a.first().map(|v| vm.unwrap_builtin(v)) {
        Some(v @ (Value::Set(_) | Value::FrozenSet(_))) => Ok(v),
        _ => vm.type_error("descriptor requires a 'set' object"),
    }
}

fn this_mutable(vm: &mut Vm, a: &[Value]) -> PyResult<Rc<RefCell<SetTable>>> {
    match a.first().map(|v| vm.unwrap_builtin(v)) {
        Some(Value::Set(s)) => Ok(s),
        _ => vm.type_error("descriptor requires a 'set' object"),
    }
}

/// The table of a set argument, or of a set made from an iterable (dict
/// keys included).
fn table_arg(vm: &mut Vm, value: &Value) -> PyResult<SetTable> {
    match vm.unwrap_builtin(value) {
        Value::Set(t) => Ok(t.borrow().clone()),
        Value::FrozenSet(t) => Ok((*t).clone()),
        _ => table_of(vm, Some(value)),
    }
}

fn is_set(vm: &Vm, value: &Value) -> bool {
    matches!(vm.unwrap_builtin(value), Value::Set(_) | Value::FrozenSet(_))
}

/// A table back into a set value of `self`'s kind.
fn result_like(this: &Value, table: SetTable) -> Value {
    match this {
        Value::FrozenSet(_) => Value::FrozenSet(Rc::new(table)),
        _ => Value::set(table),
    }
}

fn table_of_value(this: &Value) -> SetTable {
    match this {
        Value::Set(t) => t.borrow().clone(),
        Value::FrozenSet(t) => (**t).clone(),
        _ => SetTable::new(),
    }
}

fn add(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let s = this_set(vm, a)?;
    let Some(item) = a.get(1) else {
        return vm.type_error("set.add() takes exactly one argument (0 given)");
    };
    vm.set_add(&s, item.clone())?;
    Ok(Value::None)
}

fn discard_item(vm: &mut Vm, table: &Rc<RefCell<SetTable>>, item: &Value) -> PyResult<bool> {
    let hash = vm.hash(item)?;
    let mut t = core::mem::take(&mut *table.borrow_mut());
    let result = {
        let mut eq = |p: &Value, q: &Value| vm.eq(p, q);
        t.discard(hash, item, &mut eq)
    };
    *table.borrow_mut() = t;
    result
}

fn discard(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let s = this_mutable(vm, a)?;
    let item = a.get(1).cloned().unwrap_or(Value::None);
    discard_item(vm, &s, &item)?;
    Ok(Value::None)
}

fn remove(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let s = this_mutable(vm, a)?;
    let item = a.get(1).cloned().unwrap_or(Value::None);
    if !discard_item(vm, &s, &item)? {
        return vm.key_error(item);
    }
    Ok(Value::None)
}

fn set_pop(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let s = this_mutable(vm, a)?;
    let popped = s.borrow_mut().pop();
    match popped {
        Some(v) => Ok(v),
        None => {
            let class = vm.types.key_error.clone();
            Err(vm.error(&class, "pop from an empty set"))
        }
    }
}

fn set_clear(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let s = this_mutable(vm, a)?;
    s.borrow_mut().clear();
    Ok(Value::None)
}

fn copy_method(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let s = this_set(vm, a)?;
    if let Value::FrozenSet(_) = s {
        return Ok(s);
    }
    let table = set_copy(vm, &table_of_value(&s))?;
    Ok(Value::set(table))
}

fn union(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let s = this_set(vm, a)?;
    let mut result = set_copy(vm, &table_of_value(&s))?;
    for other in &a[1..] {
        set_update_from(vm, &mut result, other)?;
    }
    Ok(result_like(&s, result))
}

/// `set_intersection` with any iterable: a set's table is probed, any
/// other iterable walked in its own order.
fn intersect_with(vm: &mut Vm, so: &SetTable, other: &Value) -> PyResult<SetTable> {
    if is_set(vm, other) {
        let t = table_arg(vm, other)?;
        return set_intersection(vm, so, &t);
    }
    let mut out = SetTable::new();
    let iterator = vm.get_iter(other)?;
    while let Some(key) = vm.next(&iterator)? {
        let hash = vm.hash(&key)?;
        let found = {
            let mut eq = |p: &Value, q: &Value| vm.eq(p, q);
            so.contains(hash, &key, &mut eq)?
        };
        if found {
            table_add(vm, &mut out, hash, key)?;
        }
    }
    Ok(out)
}

fn intersection_multi(vm: &mut Vm, s: &Value, others: &[Value]) -> PyResult<SetTable> {
    let mut result = table_of_value(s);
    if others.is_empty() {
        return set_copy(vm, &result);
    }
    for other in others {
        result = intersect_with(vm, &result, other)?;
    }
    Ok(result)
}

fn intersection(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let s = this_set(vm, a)?;
    let result = intersection_multi(vm, &s, &a[1..])?;
    Ok(result_like(&s, result))
}

/// `set_difference_update_internal` with any iterable.
fn difference_update_with(vm: &mut Vm, so: &mut SetTable, other: &Value) -> PyResult<()> {
    if is_set(vm, other) || matches!(other, Value::Dict(_)) {
        let t = table_arg(vm, other)?;
        return set_difference_update(vm, so, &t);
    }
    let iterator = vm.get_iter(other)?;
    while let Some(key) = vm.next(&iterator)? {
        let hash = vm.hash(&key)?;
        let mut eq = |p: &Value, q: &Value| vm.eq(p, q);
        so.discard(hash, &key, &mut eq)?;
    }
    so.drop_dummies();
    Ok(())
}

fn difference(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let s = this_set(vm, a)?;
    let table = table_of_value(&s);
    let Some(first) = a.get(1) else {
        let copy = set_copy(vm, &table)?;
        return Ok(result_like(&s, copy));
    };
    let mut result = if is_set(vm, first) || matches!(first, Value::Dict(_)) {
        let t = table_arg(vm, first)?;
        set_difference(vm, &table, &t)?
    } else {
        let mut copy = set_copy(vm, &table)?;
        difference_update_with(vm, &mut copy, first)?;
        copy
    };
    for other in &a[2..] {
        difference_update_with(vm, &mut result, other)?;
    }
    Ok(result_like(&s, result))
}

fn symmetric_difference(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let s = this_set(vm, a)?;
    let Some(other) = a.get(1) else {
        return vm.type_error("symmetric_difference() takes exactly one argument (0 given)");
    };
    // A copy of the other, toggled by this one.
    let mut result = table_of(vm, Some(other))?;
    set_symmetric_update(vm, &mut result, &table_of_value(&s))?;
    Ok(result_like(&s, result))
}

fn set_update(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let table = this_mutable(vm, a)?;
    let mut t = table.borrow().clone();
    for other in &a[1..] {
        set_update_from(vm, &mut t, other)?;
    }
    *table.borrow_mut() = t;
    Ok(Value::None)
}

fn set_init(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let table = this_mutable(vm, a)?;
    table.borrow_mut().clear();
    set_update(vm, a, &[])
}

fn intersection_update(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let table = this_mutable(vm, a)?;
    let s = Value::Set(table.clone());
    let result = intersection_multi(vm, &s, &a[1..])?;
    *table.borrow_mut() = result;
    Ok(Value::None)
}

fn difference_update(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let table = this_mutable(vm, a)?;
    let mut t = table.borrow().clone();
    for other in &a[1..] {
        if let Value::Set(o) = other {
            if Rc::ptr_eq(o, &table) {
                t = SetTable::new();
                continue;
            }
        }
        difference_update_with(vm, &mut t, other)?;
    }
    *table.borrow_mut() = t;
    Ok(Value::None)
}

fn symmetric_difference_update(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let table = this_mutable(vm, a)?;
    let Some(other) = a.get(1) else {
        return vm.type_error("symmetric_difference_update() takes exactly one argument (0 given)");
    };
    if let Value::Set(o) = other {
        if Rc::ptr_eq(o, &table) {
            table.borrow_mut().clear();
            return Ok(Value::None);
        }
    }
    let other = table_arg(vm, other)?;
    let mut t = table.borrow().clone();
    set_symmetric_update(vm, &mut t, &other)?;
    *table.borrow_mut() = t;
    Ok(Value::None)
}

fn inclusion(vm: &mut Vm, a: &[Value], subset: bool) -> PyResult<Value> {
    let s = this_set(vm, a)?;
    let Some(other) = a.get(1) else {
        return vm.type_error("expected 1 argument, got 0");
    };
    let other = Value::set(table_arg(vm, other)?);
    let (small, large) = if subset { (s, other) } else { (other, s) };
    for item in vm.collect(&small)? {
        if !vm.contains(&large, &item)? {
            return Ok(Value::Bool(false));
        }
    }
    Ok(Value::Bool(true))
}

fn issubset(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    inclusion(vm, a, true)
}

fn issuperset(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    inclusion(vm, a, false)
}

fn isdisjoint(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let s = this_set(vm, a)?;
    let Some(other) = a.get(1) else {
        return vm.type_error("isdisjoint() takes exactly one argument (0 given)");
    };
    for item in vm.collect(other)? {
        if vm.contains(&s, &item)? {
            return Ok(Value::Bool(false));
        }
    }
    Ok(Value::Bool(true))
}

fn set_contains(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let s = this_set(vm, a)?;
    let item = a.get(1).cloned().unwrap_or(Value::None);
    Ok(Value::Bool(vm.contains(&s, &item)?))
}

// ------------------------------------------------------------
//  Shared by the containers: what subclasses reach through super()
// ------------------------------------------------------------

fn container_len(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let inner = a.first().map(|v| vm.unwrap_builtin(v)).unwrap_or(Value::None);
    Ok(Value::int(vm.len(&inner)? as i64))
}

fn container_iter(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let inner = a.first().map(|v| vm.unwrap_builtin(v)).unwrap_or(Value::None);
    vm.get_iter(&inner)
}

pub fn install(vm: &mut Vm) {
    let dict = vm.types.dict.clone();
    let entries: &[(&'static str, NativeFn)] = &[
        ("keys", keys), ("values", values), ("items", items), ("get", get), ("setdefault", setdefault), ("pop", pop),
        ("popitem", popitem), ("update", update), ("clear", clear), ("copy", copy), ("__getitem__", dict_getitem),
        ("__setitem__", dict_setitem), ("__delitem__", dict_delitem), ("__contains__", dict_contains),
        ("__len__", container_len), ("__iter__", container_iter), ("__init__", dict_init), ("__reversed__", dict_reversed),
    ];
    for (name, f) in entries {
        method(&dict, name, *f);
    }
    dict.dict.borrow_mut().set_str("fromkeys", Value::ClassMethod(Rc::new(super::native("fromkeys", fromkeys))));

    let shared: &[(&'static str, NativeFn)] = &[
        ("copy", copy_method), ("union", union), ("intersection", intersection), ("difference", difference),
        ("symmetric_difference", symmetric_difference), ("issubset", issubset), ("issuperset", issuperset),
        ("isdisjoint", isdisjoint), ("__contains__", set_contains), ("__len__", container_len), ("__iter__", container_iter),
    ];
    let set = vm.types.set.clone();
    let frozenset = vm.types.frozenset.clone();
    for (name, f) in shared {
        method(&set, name, *f);
        method(&frozenset, name, *f);
    }
    let mutating: &[(&'static str, NativeFn)] = &[
        ("add", add), ("discard", discard), ("remove", remove), ("pop", set_pop), ("clear", set_clear),
        ("update", set_update), ("intersection_update", intersection_update), ("difference_update", difference_update),
        ("symmetric_difference_update", symmetric_difference_update), ("__init__", set_init),
    ];
    for (name, f) in mutating {
        method(&set, name, *f);
    }
    for class in [vm.types.dict_keys.clone(), vm.types.dict_values.clone(), vm.types.dict_items.clone()] {
        method(&class, "__len__", container_len);
        method(&class, "__iter__", container_iter);
    }
    let list = vm.types.list.clone();
    let tuple = vm.types.tuple.clone();
    for class in [&list, &tuple] {
        method(class, "__len__", container_len);
        method(class, "__iter__", container_iter);
    }
}
