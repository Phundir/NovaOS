//! Arguments of native functions: by position or by name, typed.

use alloc::rc::Rc;
use alloc::string::String;

use super::int::Int;
use super::value::Value;
use super::vm::{PyResult, Vm};

/// The argument at `index`, or the keyword `name`.
pub fn get<'a>(args: &'a [Value], kwargs: &'a [(Rc<str>, Value)], index: usize, name: &str) -> Option<&'a Value> {
    args.get(index).or_else(|| kwargs.iter().find(|(k, _)| &**k == name).map(|(_, v)| v))
}

/// A keyword argument only.
pub fn keyword<'a>(kwargs: &'a [(Rc<str>, Value)], name: &str) -> Option<&'a Value> {
    kwargs.iter().find(|(k, _)| &**k == name).map(|(_, v)| v)
}

/// Refuses keyword arguments a function does not take.
pub fn no_keywords(vm: &mut Vm, function: &str, kwargs: &[(Rc<str>, Value)]) -> PyResult<()> {
    if let Some((k, _)) = kwargs.first() {
        let message = alloc::format!("{}() got an unexpected keyword argument '{}'", function, k);
        return vm.type_error(&message);
    }
    Ok(())
}

/// Checks the count of positional arguments.
pub fn count(vm: &mut Vm, function: &str, args: &[Value], min: usize, max: usize) -> PyResult<()> {
    if args.len() < min || args.len() > max {
        let message = if min == max {
            alloc::format!("{}() takes exactly {} argument{} ({} given)", function, min, if min == 1 { "" } else { "s" }, args.len())
        } else if args.len() < min {
            alloc::format!("{}() takes at least {} argument{} ({} given)", function, min, if min == 1 { "" } else { "s" }, args.len())
        } else {
            alloc::format!("{}() takes at most {} argument{} ({} given)", function, max, if max == 1 { "" } else { "s" }, args.len())
        };
        return vm.type_error(&message);
    }
    Ok(())
}

/// An int argument (bools count, `__index__` too).
pub fn int(vm: &mut Vm, value: &Value) -> PyResult<i64> {
    match value {
        Value::Int(Int::Small(i)) => Ok(*i),
        Value::Bool(b) => Ok(*b as i64),
        Value::Int(_) => vm.overflow("Python int too large to convert to C ssize_t"),
        Value::Instance(_) => match vm.call_special(value, "__index__", &[])? {
            Some(Value::Int(Int::Small(i))) => Ok(i),
            _ => {
                let name = vm.type_name(value);
                let message = alloc::format!("'{}' object cannot be interpreted as an integer", name);
                vm.type_error(&message)
            }
        },
        other => {
            let name = vm.type_name(other);
            let message = alloc::format!("'{}' object cannot be interpreted as an integer", name);
            vm.type_error(&message)
        }
    }
}

/// An optional int: None (or absent) gives `default`.
pub fn opt_int(vm: &mut Vm, value: Option<&Value>, default: i64) -> PyResult<i64> {
    match value {
        None | Some(Value::None) => Ok(default),
        Some(v) => int(vm, v),
    }
}

pub fn string(vm: &mut Vm, value: &Value, what: &str) -> PyResult<Rc<str>> {
    match value {
        Value::Str(s) => Ok(s.clone()),
        Value::Instance(i) => match &*i.inner.borrow() {
            Value::Str(s) => Ok(s.clone()),
            _ => {
                let name = vm.type_name(value);
                let message = alloc::format!("{} must be str, not {}", what, name);
                vm.type_error(&message)
            }
        },
        other => {
            let name = vm.type_name(other);
            let message = alloc::format!("{} must be str, not {}", what, name);
            vm.type_error(&message)
        }
    }
}

/// `self` of a str method.
pub fn this_str(vm: &mut Vm, args: &[Value], method: &str) -> PyResult<Rc<str>> {
    match args.first() {
        Some(Value::Str(s)) => Ok(s.clone()),
        Some(other) => {
            let inner = vm.unwrap_builtin(other);
            match inner {
                Value::Str(s) => Ok(s),
                _ => {
                    let name = vm.type_name(other);
                    let message = alloc::format!("descriptor '{}' for 'str' objects doesn't apply to a '{}' object", method, name);
                    vm.type_error(&message)
                }
            }
        }
        None => {
            let message = alloc::format!("descriptor '{}' of 'str' object needs an argument", method);
            vm.type_error(&message)
        }
    }
}

pub fn text(value: &str) -> String {
    String::from(value)
}
