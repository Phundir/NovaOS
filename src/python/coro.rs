//! Coroutines (`async def`) and asynchronous generators.
//!
//! A coroutine is a generator in all but name: the same suspended frame,
//! resumed by `send`. `await x` is `yield from` on what `x` gives to
//! await: the coroutine itself, or the iterator of its `__await__` (an
//! `asyncio` future yields itself there, and the event loop resumes the
//! task when it is done).
//!
//! An asynchronous generator is a coroutine that also yields values. The
//! two kinds of suspension travel the same way, so its `yield` wraps the
//! value with a marker (`AsyncGenWrap`); `__anext__` -- written in Python,
//! `lib/_asyncgen.py` -- resumes the generator and returns the first
//! marked value, letting every unmarked suspension through to whoever
//! awaits it.

use alloc::rc::Rc;

use super::value::{Instance, Value};
use super::vm::{PyResult, Vm};

impl Vm {
    pub fn async_marker(&mut self) -> Value {
        if let Some(marker) = &self.async_marker {
            return marker.clone();
        }
        let marker = Value::Instance(Instance::new(self.types.object.clone()));
        self.async_marker = Some(marker.clone());
        marker
    }

    /// What `await value` delegates to.
    pub fn awaitable(&mut self, value: Value) -> PyResult<Value> {
        if let Value::Generator(g) = &value {
            if g.borrow().kind != 2 {
                return Ok(value);
            }
        }
        let name = self.type_name(&value);
        let refuse = |vm: &mut Vm| vm.type_error::<Value>(&alloc::format!("'{}' object can't be awaited", name));
        let Some(method) = self.special(&value, "__await__")? else {
            return refuse(self);
        };
        let iterator = self.call(&method, &[], &[])?;
        match &iterator {
            Value::Generator(_) | Value::Iter(_) => Ok(iterator),
            Value::Instance(_) if self.special(&iterator, "__next__")?.is_some() => Ok(iterator),
            _ => {
                let kind = self.type_name(&iterator);
                self.type_error(&alloc::format!("__await__() returned non-iterator of type '{}'", kind))
            }
        }
    }

    /// Calls a helper of `lib/_asyncgen.py`.
    pub fn async_helper(&mut self, name: &str, args: &[Value]) -> PyResult<Value> {
        let module = super::modules::import(self, "_asyncgen", 0, &Value::None)?;
        let function = self.get_attr(&module, name)?;
        self.call(&function, args, &[])
    }
}

pub fn unused(_: Rc<str>) {}
