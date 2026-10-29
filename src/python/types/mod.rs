//! The built-in types: their classes, their constructors, their methods.
//!
//! Every built-in type is a `Class` whose dictionary holds native methods
//! (`Builtin` with `method: true`), so `"a".upper` is found and bound like
//! a user method. The exception hierarchy is built here too, class by
//! class as CPython's, so `except LookupError` catches a `KeyError`.
//!
//! The methods live by type: `str.rs`, `seq.rs` (list, tuple, range,
//! bytes), `map.rs` (dict, set), `num.rs` (int, float, bool), `object.rs`
//! (object, type, exceptions, generators, files and the descriptors).

pub mod bytes;
pub mod complex;
pub mod map;
pub mod num;
pub mod object;
pub mod seq;
pub mod str;

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::cmp::Ordering;

use super::ast::{BinOp, CmpOp};
use super::dict::{Dict, SetTable};
use super::int::Int;
use super::value::*;
use super::vm::{PyResult, Vm};
use crate::web::js::num::Num;

pub type Constructor = fn(&mut Vm, &Rc<Class>, &[Value], &[(Rc<str>, Value)]) -> PyResult<Value>;

macro_rules! types {
    ($($field:ident),* $(,)?) => {
        /// The built-in classes.
        pub struct Types {
            $(pub $field: Rc<Class>,)*
        }
    };
}

types!(
    object, type_, none_type, not_implemented_type, ellipsis_type, bool, int, float, complex, str, bytes, bytearray, list, tuple, dict,
    set, frozenset, dict_keys, dict_values, dict_items, union_type, range, slice, function, builtin_function, method, iterator, generator, coroutine, async_generator, module, property,
    staticmethod, classmethod, super_, file, base_exception, exception, system_exit, keyboard_interrupt,
    generator_exit, stop_iteration, arithmetic_error, zero_division_error, overflow_error, assertion_error,
    attribute_error, lookup_error, index_error, key_error, name_error, unbound_local_error, type_error,
    value_error, runtime_error, not_implemented_error, recursion_error, memory_error, import_error,
    module_not_found_error, os_error, file_not_found_error, file_exists_error, permission_error,
    is_a_directory_error, not_a_directory_error, eof_error, syntax_error, unicode_error, warning,
    stop_async_iteration, timeout_error, unicode_decode_error, unicode_encode_error,
    indentation_error, tab_error, system_error, buffer_error, floating_point_error, reference_error, unicode_translate_error, python_finalization_error, blocking_io_error, child_process_error, connection_error, broken_pipe_error, connection_aborted_error, connection_refused_error, connection_reset_error, interrupted_error, process_lookup_error, user_warning, deprecation_warning, pending_deprecation_warning, syntax_warning, runtime_warning, future_warning, import_warning, unicode_warning, bytes_warning, resource_warning, encoding_warning,
);

/// A class with its MRO computed.
pub fn class(name: &str, bases: Vec<Rc<Class>>, base: Base) -> Rc<Class> {
    let class = Class::new(name, bases, base);
    let mro = compute_mro(&class).unwrap_or_else(|_| alloc::vec![class.clone()]);
    *class.mro.borrow_mut() = mro;
    class
}

/// Puts a native method on a class.
pub fn method(class: &Rc<Class>, name: &'static str, call: NativeFn) {
    class.dict.borrow_mut().set_str(name, Value::Builtin(Rc::new(Builtin { name, call, method: true, data: Value::None })));
}

pub fn native(name: &'static str, call: NativeFn) -> Value {
    Value::Builtin(Rc::new(Builtin { name, call, method: false, data: Value::None }))
}

pub fn native_with(name: &'static str, call: NativeFn, data: Value) -> Value {
    Value::Builtin(Rc::new(Builtin { name, call, method: false, data }))
}

impl Types {
    pub fn new() -> Types {
        let object = class("object", Vec::new(), Base::Object);
        let o = || alloc::vec![object.clone()];
        let type_ = class("type", o(), Base::Other);
        let int = class("int", o(), Base::Int);
        let bool = class("bool", alloc::vec![int.clone()], Base::Bool);
        let base_exception = class("BaseException", o(), Base::Exception);
        let e = |name: &str, parent: &Rc<Class>| class(name, alloc::vec![parent.clone()], Base::Exception);
        let exception = e("Exception", &base_exception);
        let arithmetic_error = e("ArithmeticError", &exception);
        let lookup_error = e("LookupError", &exception);
        let name_error = e("NameError", &exception);
        let os_error = e("OSError", &exception);
        let import_error = e("ImportError", &exception);
        let runtime_error = e("RuntimeError", &exception);
        let value_error = e("ValueError", &exception);
        let warning = e("Warning", &exception);
        let unicode_error = e("UnicodeError", &value_error);
        let syntax_error = e("SyntaxError", &exception);
        let indentation_error = e("IndentationError", &syntax_error);
        let connection_error = e("ConnectionError", &os_error);
        Types {
            type_,
            none_type: class("NoneType", o(), Base::Other),
            not_implemented_type: class("NotImplementedType", o(), Base::Other),
            ellipsis_type: class("ellipsis", o(), Base::Other),
            bool,
            int,
            float: class("float", o(), Base::Float),
            complex: class("complex", o(), Base::Other),
            bytearray: class("bytearray", o(), Base::Other),
            str: class("str", o(), Base::Str),
            bytes: class("bytes", o(), Base::Bytes),
            list: class("list", o(), Base::List),
            tuple: class("tuple", o(), Base::Tuple),
            dict: class("dict", o(), Base::Dict),
            set: class("set", o(), Base::Set),
            frozenset: class("frozenset", o(), Base::FrozenSet),
            dict_keys: class("dict_keys", o(), Base::Other),
            dict_values: class("dict_values", o(), Base::Other),
            dict_items: class("dict_items", o(), Base::Other),
            union_type: class("UnionType", o(), Base::Other),
            range: class("range", o(), Base::Other),
            slice: class("slice", o(), Base::Other),
            function: class("function", o(), Base::Other),
            builtin_function: class("builtin_function_or_method", o(), Base::Other),
            method: class("method", o(), Base::Other),
            iterator: class("iterator", o(), Base::Other),
            generator: class("generator", o(), Base::Other),
            coroutine: class("coroutine", o(), Base::Other),
            async_generator: class("async_generator", o(), Base::Other),
            module: class("module", o(), Base::Other),
            property: class("property", o(), Base::Other),
            staticmethod: class("staticmethod", o(), Base::Other),
            classmethod: class("classmethod", o(), Base::Other),
            super_: class("super", o(), Base::Other),
            file: class("TextIOWrapper", o(), Base::Other),
            system_exit: e("SystemExit", &base_exception),
            keyboard_interrupt: e("KeyboardInterrupt", &base_exception),
            generator_exit: e("GeneratorExit", &base_exception),
            stop_iteration: e("StopIteration", &exception),
            stop_async_iteration: e("StopAsyncIteration", &exception),
            timeout_error: e("TimeoutError", &os_error),
            zero_division_error: e("ZeroDivisionError", &arithmetic_error),
            overflow_error: e("OverflowError", &arithmetic_error),
            assertion_error: e("AssertionError", &exception),
            attribute_error: e("AttributeError", &exception),
            index_error: e("IndexError", &lookup_error),
            key_error: e("KeyError", &lookup_error),
            unbound_local_error: e("UnboundLocalError", &name_error),
            type_error: e("TypeError", &exception),
            not_implemented_error: e("NotImplementedError", &runtime_error),
            recursion_error: e("RecursionError", &runtime_error),
            memory_error: e("MemoryError", &exception),
            module_not_found_error: e("ModuleNotFoundError", &import_error),
            file_not_found_error: e("FileNotFoundError", &os_error),
            file_exists_error: e("FileExistsError", &os_error),
            permission_error: e("PermissionError", &os_error),
            is_a_directory_error: e("IsADirectoryError", &os_error),
            not_a_directory_error: e("NotADirectoryError", &os_error),
            eof_error: e("EOFError", &exception),
            tab_error: e("TabError", &indentation_error),
            system_error: e("SystemError", &exception),
            buffer_error: e("BufferError", &exception),
            floating_point_error: e("FloatingPointError", &arithmetic_error),
            reference_error: e("ReferenceError", &exception),
            unicode_translate_error: e("UnicodeTranslateError", &unicode_error),
            python_finalization_error: e("PythonFinalizationError", &runtime_error),
            blocking_io_error: e("BlockingIOError", &os_error),
            child_process_error: e("ChildProcessError", &os_error),
            broken_pipe_error: e("BrokenPipeError", &connection_error),
            connection_aborted_error: e("ConnectionAbortedError", &connection_error),
            connection_refused_error: e("ConnectionRefusedError", &connection_error),
            connection_reset_error: e("ConnectionResetError", &connection_error),
            interrupted_error: e("InterruptedError", &os_error),
            process_lookup_error: e("ProcessLookupError", &os_error),
            user_warning: e("UserWarning", &warning),
            deprecation_warning: e("DeprecationWarning", &warning),
            pending_deprecation_warning: e("PendingDeprecationWarning", &warning),
            syntax_warning: e("SyntaxWarning", &warning),
            runtime_warning: e("RuntimeWarning", &warning),
            future_warning: e("FutureWarning", &warning),
            import_warning: e("ImportWarning", &warning),
            unicode_warning: e("UnicodeWarning", &warning),
            bytes_warning: e("BytesWarning", &warning),
            resource_warning: e("ResourceWarning", &warning),
            encoding_warning: e("EncodingWarning", &warning),
            syntax_error,
            indentation_error,
            connection_error,
            unicode_decode_error: e("UnicodeDecodeError", &unicode_error),
            unicode_encode_error: e("UnicodeEncodeError", &unicode_error),
            unicode_error,
            base_exception,
            exception,
            arithmetic_error,
            lookup_error,
            name_error,
            os_error,
            import_error,
            runtime_error,
            value_error,
            warning,
            object,
        }
    }

    /// Every built-in class with its Python name, for the builtins module.
    pub fn named(&self) -> Vec<Rc<Class>> {
        alloc::vec![
            self.object.clone(), self.type_.clone(), self.bool.clone(), self.int.clone(), self.float.clone(), self.complex.clone(), self.bytearray.clone(),
            self.str.clone(), self.bytes.clone(), self.list.clone(), self.tuple.clone(), self.dict.clone(),
            self.set.clone(), self.frozenset.clone(), self.range.clone(), self.slice.clone(), self.property.clone(),
            self.staticmethod.clone(), self.classmethod.clone(), self.super_.clone(), self.base_exception.clone(),
            self.exception.clone(), self.system_exit.clone(), self.keyboard_interrupt.clone(),
            self.generator_exit.clone(), self.stop_iteration.clone(), self.arithmetic_error.clone(),
            self.zero_division_error.clone(), self.overflow_error.clone(), self.assertion_error.clone(),
            self.attribute_error.clone(), self.lookup_error.clone(), self.index_error.clone(),
            self.key_error.clone(), self.name_error.clone(), self.unbound_local_error.clone(),
            self.type_error.clone(), self.value_error.clone(), self.runtime_error.clone(),
            self.not_implemented_error.clone(), self.recursion_error.clone(), self.memory_error.clone(),
            self.import_error.clone(), self.module_not_found_error.clone(), self.os_error.clone(),
            self.file_not_found_error.clone(), self.file_exists_error.clone(), self.permission_error.clone(),
            self.is_a_directory_error.clone(), self.not_a_directory_error.clone(), self.eof_error.clone(),
            self.syntax_error.clone(), self.unicode_error.clone(), self.warning.clone(),
            self.stop_async_iteration.clone(), self.timeout_error.clone(), self.unicode_decode_error.clone(),
            self.unicode_encode_error.clone(),
            self.indentation_error.clone(),
            self.tab_error.clone(),
            self.system_error.clone(),
            self.buffer_error.clone(),
            self.floating_point_error.clone(),
            self.reference_error.clone(),
            self.unicode_translate_error.clone(),
            self.python_finalization_error.clone(),
            self.blocking_io_error.clone(),
            self.child_process_error.clone(),
            self.connection_error.clone(),
            self.broken_pipe_error.clone(),
            self.connection_aborted_error.clone(),
            self.connection_refused_error.clone(),
            self.connection_reset_error.clone(),
            self.interrupted_error.clone(),
            self.process_lookup_error.clone(),
            self.user_warning.clone(),
            self.deprecation_warning.clone(),
            self.pending_deprecation_warning.clone(),
            self.syntax_warning.clone(),
            self.runtime_warning.clone(),
            self.future_warning.clone(),
            self.import_warning.clone(),
            self.unicode_warning.clone(),
            self.bytes_warning.clone(),
            self.resource_warning.clone(),
            self.encoding_warning.clone(),
        ]
    }
}

impl Default for Types {
    fn default() -> Self {
        Types::new()
    }
}

/// Installs the methods of every built-in type.
pub fn install(vm: &mut Vm) {
    for class in vm.types.named() {
        class.dict.borrow_mut().set_str("__module__", Value::str("builtins"));
    }
    str::install(vm);
    seq::install(vm);
    map::install(vm);
    num::install(vm);
    object::install(vm);
    bytes::install(vm);
}

/// Is `class` exactly one of the built-in types (not a user subclass)?
pub fn is_builtin(vm: &Vm, class: &Rc<Class>) -> bool {
    let t = &vm.types;
    [
        &t.int, &t.bool, &t.float, &t.complex, &t.bytearray, &t.str, &t.bytes, &t.list, &t.tuple, &t.dict, &t.set, &t.frozenset, &t.range,
        &t.slice, &t.object, &t.type_, &t.property, &t.staticmethod, &t.classmethod, &t.super_,
    ]
    .iter()
    .any(|c| c.id == class.id)
}

/// The native constructor of a class or of its built-in base.
pub fn constructor(vm: &Vm, class: &Rc<Class>) -> Option<Constructor> {
    let t = &vm.types;
    let id = class.id;
    if id == t.range.id {
        return Some(seq::new_range);
    }
    if id == t.slice.id {
        return Some(seq::new_slice);
    }
    if id == t.type_.id {
        return Some(object::new_type);
    }
    if id == t.property.id {
        return Some(object::new_property);
    }
    if id == t.staticmethod.id {
        return Some(object::new_staticmethod);
    }
    if id == t.classmethod.id {
        return Some(object::new_classmethod);
    }
    if id == t.super_.id {
        return Some(object::new_super);
    }
    if id == t.object.id {
        return Some(object::new_object);
    }
    if id == t.complex.id {
        return Some(complex::new_complex);
    }
    if id == t.bytearray.id {
        return Some(bytes::new_bytearray);
    }
    Some(match class.base {
        Base::Int => num::new_int,
        Base::Bool => num::new_bool,
        Base::Float => num::new_float,
        Base::Str => str::new_str,
        Base::Bytes => seq::new_bytes,
        Base::List => seq::new_list,
        Base::Tuple => seq::new_tuple,
        Base::Dict => map::new_dict,
        Base::Set => map::new_set,
        Base::FrozenSet => map::new_frozenset,
        _ => return None,
    })
}

/// The end of a class body: the class from its namespace.
pub fn make_class(
    vm: &mut Vm,
    name: Rc<str>,
    bases: Vec<Rc<Class>>,
    namespace: Rc<RefCell<Dict>>,
    body_result: Value,
    keywords: Vec<(Rc<str>, Value)>,
) -> PyResult<Value> {
    let bases = if bases.is_empty() { alloc::vec![vm.types.object.clone()] } else { bases };
    // `metaclass=` was dealt with by the caller; the other keywords go to
    // `__init_subclass__`.
    let keywords: Vec<(Rc<str>, Value)> = keywords.into_iter().filter(|(k, _)| &**k != "metaclass").collect();
    let base = bases.iter().map(|b| b.base).find(|b| *b != Base::Object).unwrap_or(Base::Object);
    let base = if base == Base::Bool { Base::Int } else { base };
    // The class of a class: inherited from the bases.
    let inherited = bases.iter().filter_map(|b| b.metaclass.borrow().clone()).fold(None, |best: Option<Rc<Class>>, m| match best {
        Some(b) if b.is_subclass(&m) => Some(b),
        _ => Some(m),
    });
    let class = Class::new(&name, bases, base);
    *class.metaclass.borrow_mut() = inherited;
    *class.dict.borrow_mut() = namespace.borrow().clone();
    // A class that defines `__eq__` without `__hash__` is unhashable.
    let has_eq = class.dict.borrow().get_str("__eq__").is_some();
    if has_eq && class.dict.borrow().get_str("__hash__").is_none() {
        class.dict.borrow_mut().set_str("__hash__", Value::None);
    }
    if let Some(Value::Str(q)) = class.dict.borrow().get_str("__qualname__") {
        *class.qualname.borrow_mut() = q;
    }
    match compute_mro(&class) {
        Ok(mro) => *class.mro.borrow_mut() = mro,
        Err(message) => return vm.type_error(&message),
    }
    // `__new__` and `__init_subclass__` are implicitly static/class
    // methods.
    let new = class.dict.borrow().get_str("__new__");
    if let Some(f @ Value::Function(_)) = new {
        class.dict.borrow_mut().set_str("__new__", Value::StaticMethod(Rc::new(f)));
    }
    for name in ["__init_subclass__", "__class_getitem__"] {
        let hook = class.dict.borrow().get_str(name);
        if let Some(f @ Value::Function(_)) = hook {
            class.dict.borrow_mut().set_str(name, Value::ClassMethod(Rc::new(f)));
        }
    }
    if let Value::Cell(cell) = body_result {
        *cell.borrow_mut() = Some(Value::Class(class.clone()));
    }
    // `__set_name__` of descriptors.
    let entries: Vec<(Value, Value)> = class.dict.borrow().iter().map(|e| (e.key.clone(), e.value.clone())).collect();
    for (key, value) in entries {
        if let Value::Instance(_) = &value {
            if let Some(set_name) = vm.special(&value, "__set_name__")? {
                vm.call(&set_name, &[Value::Class(class.clone()), key], &[])?;
            }
        }
    }
    // `__init_subclass__` of a parent.
    let parent_hook = class.mro.borrow().iter().skip(1).find_map(|c| c.dict.borrow().get_str("__init_subclass__"));
    if let Some(hook) = parent_hook {
        let function = match hook {
            Value::ClassMethod(inner) => (*inner).clone(),
            other => other,
        };
        vm.call(&function, &[Value::Class(class.clone())], &keywords)?;
    }
    Ok(Value::Class(class))
}

/// Attributes of built-in values that are not methods (`real`, `start`).
pub fn special_attr(vm: &mut Vm, object: &Value, name: &str) -> PyResult<Option<Value>> {
    Ok(match (object, name) {
        (Value::Int(_) | Value::Bool(_), "real" | "numerator") => Some(num::to_int(object)),
        (Value::Int(_) | Value::Bool(_), "imag") => Some(Value::int(0)),
        (Value::Int(_) | Value::Bool(_), "denominator") => Some(Value::int(1)),
        (Value::Float(_), "real") => Some(object.clone()),
        (Value::Float(_), "imag") => Some(Value::Float(Num::ZERO)),
        (Value::Complex(r, _), "real") => Some(Value::Float(*r)),
        (Value::Complex(_, i), "imag") => Some(Value::Float(*i)),
        (Value::Range(r), "start") => Some(Value::int(r.start)),
        (Value::Range(r), "stop") => Some(Value::int(r.stop)),
        (Value::Range(r), "step") => Some(Value::int(r.step)),
        (Value::Slice(s), "start") => Some(s[0].clone()),
        (Value::Slice(s), "stop") => Some(s[1].clone()),
        (Value::Slice(s), "step") => Some(s[2].clone()),
        (Value::Generator(g), "__name__") => Some(Value::Str(g.borrow().name.clone())),
        (Value::File(f), "name") => Some(Value::Str(f.borrow().path.clone())),
        (Value::File(f), "closed") => Some(Value::Bool(f.borrow().closed)),
        (Value::Property(p), "fget") => Some(p[0].clone()),
        (Value::Property(p), "fset") => Some(p[1].clone()),
        (Value::Property(p), "fdel") => Some(p[2].clone()),
        (Value::StaticMethod(f) | Value::ClassMethod(f), "__func__") => Some((**f).clone()),
        (_, "__doc__") => Some(Value::None),
        _ => {
            let _ = vm;
            None
        }
    })
}

// ============================================================
//  Numbers
// ============================================================

/// `x / y` of two ints, correctly rounded (CPython's long_true_divide):
/// exact when both are below 2^53, through a big quotient otherwise.
pub fn int_true_div(x: &Int, y: &Int) -> Num {
    if let (Some(a), Some(b)) = (x.to_i64(), y.to_i64()) {
        if a.unsigned_abs() < 1 << 53 && b.unsigned_abs() < 1 << 53 {
            return Num::from_i64(a).div(Num::from_i64(b));
        }
    }
    // Scale so the quotient has 64+ significant bits, then let the
    // float conversion round once, the remainder as a sticky bit.
    let negative = x.is_negative() != y.is_negative();
    let (a, b) = (x.abs(), y.abs());
    let shift = 66i64 - (a.bit_length() as i64 - b.bit_length() as i64);
    let (scaled_a, scaled_b) = if shift >= 0 { (a.shl(shift as u64).unwrap_or(a.clone()), b.clone()) } else { (a.clone(), b.shl((-shift) as u64).unwrap_or(b.clone())) };
    let (q, r) = scaled_a.divmod(&scaled_b).unwrap_or((Int::zero(), Int::zero()));
    let q = if r.is_zero() { q } else { q.shl(1).unwrap_or(q.clone()).add(&Int::Small(1)) };
    let extra = if r.is_zero() { 0 } else { 1 };
    let f = q.to_float().scale2(-(shift as i32) - extra);
    if negative { f.neg() } else { f }
}

/// Python's float floor division and modulo (the remainder takes the
/// divisor's sign).
pub fn float_divmod(x: Num, y: Num) -> (Num, Num) {
    let mut m = x.rem(y);
    let mut div = x.sub(m).div(y);
    if !m.is_zero() {
        if y.is_negative() != m.is_negative() {
            m = m.add(y);
            div = div.sub(Num::ONE);
        }
    } else {
        m = if y.is_negative() { Num::NEG_ZERO } else { Num::ZERO };
    }
    let floordiv = if !div.is_zero() {
        let f = div.floor();
        if Num::HALF.lt(div.sub(f)) { f.add(Num::ONE) } else { f }
    } else if x.div(y).is_negative() {
        Num::NEG_ZERO
    } else {
        Num::ZERO
    };
    (floordiv, m)
}

/// Exact comparison of numbers of mixed types (an int beyond 2^53 is
/// not rounded to a float first).
pub fn compare_numbers(a: &Value, b: &Value) -> Option<Ordering> {
    let float_of = |v: &Value| match v {
        Value::Float(f) => Some(*f),
        _ => None,
    };
    let int_of = |v: &Value| match v {
        Value::Int(i) => Some(i.clone()),
        Value::Bool(b) => Some(Int::Small(*b as i64)),
        _ => None,
    };
    match (float_of(a), float_of(b)) {
        (Some(x), Some(y)) => x.partial_cmp(y),
        (Some(x), None) => compare_float_int(x, &int_of(b)?),
        (None, Some(y)) => compare_float_int(y, &int_of(a)?).map(|o| o.reverse()),
        _ => None,
    }
}

fn compare_float_int(x: Num, i: &Int) -> Option<Ordering> {
    if x.is_nan() {
        return None;
    }
    if x.is_infinite() {
        return Some(if x.is_negative() { Ordering::Less } else { Ordering::Greater });
    }
    let floor = Int::from_float(x.floor())?;
    match floor.cmp(i) {
        Ordering::Equal => Some(if x.is_integer() { Ordering::Equal } else { Ordering::Greater }),
        other => Some(other),
    }
}

/// CPython's hash of a float: the value modulo 2^61 - 1.
pub fn float_hash(x: Num, value: &Value) -> i64 {
    if x.is_nan() {
        return (value.pointer().unwrap_or(0) >> 4) as i64;
    }
    if x.is_infinite() {
        return if x.is_negative() { -314159 } else { 314159 };
    }
    if x.is_integer() {
        if let Some(i) = Int::from_float(x) {
            return i.hash();
        }
    }
    const P: u128 = (1 << 61) - 1;
    let (negative, m, e) = x.decompose();
    // value = m * 2^e; 2^61 = 1 mod P, so 2^e = 2^(e mod 61).
    let shift = e.rem_euclid(61) as u32;
    let h = ((m as u128 % P) << shift) % P;
    let mut h = h as i64;
    if negative {
        h = -h;
    }
    if h == -1 { -2 } else { h }
}

// ============================================================
//  Subscripts
// ============================================================

/// Normalises an index into a sequence of `len` items.
pub fn index_of(vm: &mut Vm, index: &Value, len: usize, what: &str) -> PyResult<usize> {
    let i = match index {
        Value::Int(Int::Small(i)) => *i,
        Value::Bool(b) => *b as i64,
        Value::Int(_) => {
            let message = alloc::format!("cannot fit 'int' into an index-sized integer");
            return vm.index_error(&message);
        }
        Value::Instance(_) => match vm.call_special(index, "__index__", &[])? {
            Some(Value::Int(Int::Small(i))) => i,
            _ => {
                let name = vm.type_name(index);
                let message = alloc::format!("{} indices must be integers or slices, not {}", what, name);
                return vm.type_error(&message);
            }
        },
        other => {
            let name = vm.type_name(other);
            let message = alloc::format!("{} indices must be integers or slices, not {}", what, name);
            return vm.type_error(&message);
        }
    };
    let actual = if i < 0 { i + len as i64 } else { i };
    if actual < 0 || actual >= len as i64 {
        let message = alloc::format!("{} index out of range", what);
        return vm.index_error(&message);
    }
    Ok(actual as usize)
}

/// The indices a slice selects in a sequence of `len` items:
/// (start, stop, step, count), as `slice.indices` computes them.
pub fn slice_indices(vm: &mut Vm, slice: &[Value; 3], len: usize) -> PyResult<(i64, i64, i64, usize)> {
    let len = len as i64;
    let get = |vm: &mut Vm, v: &Value| -> PyResult<Option<i64>> {
        match v {
            Value::None => Ok(None),
            Value::Int(Int::Small(i)) => Ok(Some(*i)),
            Value::Int(i) => Ok(Some(if i.is_negative() { i64::MIN / 2 } else { i64::MAX / 2 })),
            Value::Bool(b) => Ok(Some(*b as i64)),
            other => match vm.call_special(other, "__index__", &[])? {
                Some(Value::Int(Int::Small(i))) => Ok(Some(i)),
                _ => vm.type_error("slice indices must be integers or None or have an __index__ method"),
            },
        }
    };
    let step = get(vm, &slice[2])?.unwrap_or(1);
    if step == 0 {
        return vm.value_error("slice step cannot be zero");
    }
    let (lower, upper) = if step < 0 { (-1, len - 1) } else { (0, len) };
    let clamp = |v: Option<i64>, default: i64| -> i64 {
        match v {
            None => default,
            Some(v) => {
                let v = if v < 0 { v + len } else { v };
                v.clamp(lower, upper)
            }
        }
    };
    let start = clamp(get(vm, &slice[0])?, if step < 0 { upper } else { lower });
    let stop = clamp(get(vm, &slice[1])?, if step < 0 { lower } else { upper });
    let count = if step > 0 {
        if stop > start { ((stop - start - 1) / step + 1) as usize } else { 0 }
    } else if start > stop {
        ((start - stop - 1) / (-step) + 1) as usize
    } else {
        0
    };
    Ok((start, stop, step, count))
}

pub fn get_item(vm: &mut Vm, object: &Value, index: &Value) -> PyResult<Value> {
    match object {
        Value::ByteArray(b) => {
            let snapshot = Value::Bytes(Rc::from(b.borrow().as_slice()));
            match seq::get_item(vm, &snapshot, index)? {
                Value::Bytes(x) => Ok(bytes::new_bytearray_value(x.to_vec())),
                other => Ok(other),
            }
        }
        Value::List(_) | Value::Tuple(_) | Value::Str(_) | Value::Bytes(_) | Value::Range(_) => seq::get_item(vm, object, index),
        Value::Dict(d) => match vm.dict_get(d, index)? {
            Some(v) => Ok(v),
            None => vm.key_error(index.clone()),
        },
        Value::Class(c) => {
            // `list[int]`, `dict[str, int]`: annotations, the class itself.
            if let Some(hook) = c.lookup("__class_getitem__") {
                let function = match hook {
                    Value::ClassMethod(inner) => (*inner).clone(),
                    other => other,
                };
                return vm.call(&function, &[object.clone(), index.clone()], &[]);
            }
            // `list[int]`: a types.GenericAlias of the built-in generics.
            let generic = [&vm.types.list, &vm.types.dict, &vm.types.tuple, &vm.types.set, &vm.types.frozenset, &vm.types.type_]
                .iter()
                .any(|t| c.is_subclass(t));
            if generic {
                let alias = match vm.lazy_builtin("_nova_generic_alias")? {
                    Some(f) => f,
                    None => return Ok(object.clone()),
                };
                return vm.call(&alias, &[object.clone(), index.clone()], &[]);
            }
            let message = alloc::format!("type '{}' is not subscriptable", c.name);
            vm.type_error(&message)
        }
        Value::Instance(i) => {
            if let Some(r) = vm.call_special(object, "__getitem__", core::slice::from_ref(index))? {
                return Ok(r);
            }
            let inner = i.inner.borrow().clone();
            if !inner.is_none() {
                // A dict subclass with `__missing__`.
                if let Value::Dict(d) = &inner {
                    if let Some(v) = vm.dict_get(d, index)? {
                        return Ok(v);
                    }
                    if let Some(r) = vm.call_special(object, "__missing__", core::slice::from_ref(index))? {
                        return Ok(r);
                    }
                    return vm.key_error(index.clone());
                }
                return get_item(vm, &inner, index);
            }
            let message = alloc::format!("'{}' object is not subscriptable", i.class().name);
            vm.type_error(&message)
        }
        other => {
            let name = vm.type_name(other);
            let message = alloc::format!("'{}' object is not subscriptable", name);
            vm.type_error(&message)
        }
    }
}

pub fn set_item(vm: &mut Vm, object: &Value, index: &Value, value: Value) -> PyResult<()> {
    match object {
        Value::ByteArray(b) => {
            let len = b.borrow().len();
            if let Value::Slice(s) = index {
                let (start, _, step, count) = slice_indices(vm, s, len)?;
                let replacement = bytes::build(vm, core::slice::from_ref(&value), &[])?;
                if step == 1 {
                    let start = start as usize;
                    b.borrow_mut().splice(start..start + count, replacement);
                    return Ok(());
                }
                if replacement.len() != count {
                    return vm.value_error(&alloc::format!(
                        "attempt to assign bytes of size {} to extended slice of size {}",
                        replacement.len(),
                        count
                    ));
                }
                let mut position = start;
                for byte in replacement {
                    b.borrow_mut()[position as usize] = byte;
                    position += step;
                }
                return Ok(());
            }
            let at = index_of(vm, index, len, "bytearray")?;
            let n = super::args::int(vm, &value)?;
            if !(0..=255).contains(&n) {
                return vm.value_error("byte must be in range(0, 256)");
            }
            b.borrow_mut()[at] = n as u8;
            Ok(())
        }
        Value::List(_) => seq::set_list_item(vm, object, index, value),
        Value::Dict(d) => {
            vm.dict_insert(d, index.clone(), value)?;
            Ok(())
        }
        Value::Instance(i) => {
            if vm.call_special(object, "__setitem__", &[index.clone(), value.clone()])?.is_some() {
                return Ok(());
            }
            let inner = i.inner.borrow().clone();
            if !inner.is_none() {
                return set_item(vm, &inner, index, value);
            }
            let message = alloc::format!("'{}' object does not support item assignment", i.class().name);
            vm.type_error(&message)
        }
        other => {
            let name = vm.type_name(other);
            let message = alloc::format!("'{}' object does not support item assignment", name);
            vm.type_error(&message)
        }
    }
}

pub fn del_item(vm: &mut Vm, object: &Value, index: &Value) -> PyResult<()> {
    match object {
        Value::ByteArray(b) => {
            let len = b.borrow().len();
            if let Value::Slice(s) = index {
                let (start, _, step, count) = slice_indices(vm, s, len)?;
                let mut positions: Vec<usize> = (0..count).map(|k| (start + k as i64 * step) as usize).collect();
                positions.sort_unstable();
                for position in positions.into_iter().rev() {
                    b.borrow_mut().remove(position);
                }
                return Ok(());
            }
            let at = index_of(vm, index, len, "bytearray")?;
            b.borrow_mut().remove(at);
            Ok(())
        }
        Value::List(_) => seq::del_list_item(vm, object, index),
        Value::Dict(d) => match vm.dict_remove(d, index)? {
            Some(_) => Ok(()),
            None => vm.key_error(index.clone()),
        },
        Value::Instance(i) => {
            if vm.call_special(object, "__delitem__", core::slice::from_ref(index))?.is_some() {
                return Ok(());
            }
            let inner = i.inner.borrow().clone();
            if !inner.is_none() {
                return del_item(vm, &inner, index);
            }
            let message = alloc::format!("'{}' object doesn't support item deletion", i.class().name);
            vm.type_error(&message)
        }
        other => {
            let name = vm.type_name(other);
            let message = alloc::format!("'{}' object doesn't support item deletion", name);
            vm.type_error(&message)
        }
    }
}

// ============================================================
//  Iterators
// ============================================================

pub fn iter_next(vm: &mut Vm, state: &Rc<RefCell<Iter>>) -> PyResult<Option<Value>> {
    // Each kind takes what it needs out of the state, so that Python
    // code called on the way (map's function) may use the iterator.
    let kind = core::mem::replace(&mut *state.borrow_mut(), Iter::Done);
    let (result, back) = match kind {
        Iter::Seq { seq, index } => {
            let item = match &seq {
                Value::List(l) => l.borrow().get(index).cloned(),
                Value::Tuple(t) => t.get(index).cloned(),
                Value::Bytes(b) => b.get(index).map(|b| Value::int(*b as i64)),
                Value::ByteArray(b) => b.borrow().get(index).map(|b| Value::int(*b as i64)),
                Value::Instance(_) => match vm.get_item(&seq, &Value::int(index as i64)) {
                    Ok(v) => Some(v),
                    Err(e) if vm.is_instance_of(&e, &vm.types.index_error.clone()) || vm.is_instance_of(&e, &vm.types.stop_iteration.clone()) => None,
                    Err(e) => return Err(e),
                },
                _ => None,
            };
            match item {
                Some(v) => (Some(v), Iter::Seq { seq, index: index + 1 }),
                None => (None, Iter::Done),
            }
        }
        Iter::Range { next, stop, step } => {
            if (step > 0 && next < stop) || (step < 0 && next > stop) {
                (Some(Value::int(next)), Iter::Range { next: next + step, stop, step })
            } else {
                (None, Iter::Done)
            }
        }
        Iter::Str { text, byte } => match text[byte..].chars().next() {
            Some(c) => {
                let mut buffer = [0u8; 4];
                let s: &str = c.encode_utf8(&mut buffer);
                (Some(Value::str(s)), Iter::Str { byte: byte + c.len_utf8(), text })
            }
            None => (None, Iter::Done),
        },
        Iter::Keys { dict, index, kind, size } => {
            let Value::Dict(d) = &dict else {
                return Ok(None);
            };
            if d.borrow().len() != size {
                let class = vm.types.runtime_error.clone();
                return Err(vm.error(&class, "dictionary changed size during iteration"));
            }
            let mut i = index;
            let found = loop {
                let d = d.borrow();
                if i >= d.raw_len() {
                    break None;
                }
                if let Some(entry) = d.entry_at(i) {
                    break Some(match kind {
                        0 => entry.key.clone(),
                        1 => entry.value.clone(),
                        _ => Value::tuple(alloc::vec![entry.key.clone(), entry.value.clone()]),
                    });
                }
                i += 1;
            };
            match found {
                Some(v) => (Some(v), Iter::Keys { dict: dict.clone(), index: i + 1, kind, size }),
                None => (None, Iter::Done),
            }
        }
        Iter::Enumerate { inner, count } => match vm.next(&inner)? {
            Some(v) => {
                let next = count.add(&Int::Small(1));
                (Some(Value::tuple(alloc::vec![Value::Int(count), v])), Iter::Enumerate { inner, count: next })
            }
            None => (None, Iter::Done),
        },
        Iter::Zip { inners, strict } => {
            let mut items = Vec::with_capacity(inners.len());
            let mut ended = None;
            for (i, inner) in inners.iter().enumerate() {
                match vm.next(inner)? {
                    Some(v) => items.push(v),
                    None => {
                        ended = Some(i);
                        break;
                    }
                }
            }
            match ended {
                None if !inners.is_empty() => (Some(Value::tuple(items)), Iter::Zip { inners, strict }),
                Some(i) if strict && (i > 0 || inners.iter().skip(1).any(|_| true)) => {
                    // Strict: all must end together.
                    let mut longer = i > 0;
                    if i == 0 {
                        for inner in inners.iter().skip(1) {
                            if vm.next(inner)?.is_some() {
                                longer = true;
                                break;
                            }
                        }
                    }
                    if longer {
                        let message = alloc::format!("zip() argument {} is {} than argument{} 1{}", if i == 0 { 2 } else { i + 1 }, if i == 0 { "longer" } else { "shorter" }, if i > 1 { "s" } else { "" }, if i > 1 { alloc::format!("-{}", i) } else { String::new() });
                        return vm.value_error(&message);
                    }
                    (None, Iter::Done)
                }
                _ => (None, Iter::Done),
            }
        }
        Iter::Map { function, inners } => {
            let mut args = Vec::with_capacity(inners.len());
            let mut ended = false;
            for inner in &inners {
                match vm.next(inner)? {
                    Some(v) => args.push(v),
                    None => {
                        ended = true;
                        break;
                    }
                }
            }
            if ended {
                (None, Iter::Done)
            } else {
                let v = vm.call(&function, &args, &[])?;
                (Some(v), Iter::Map { function, inners })
            }
        }
        Iter::Filter { function, inner } => {
            let mut found = None;
            while let Some(v) = vm.next(&inner)? {
                let keep = if function.is_none() { vm.truthy(&v)? } else {
                    let r = vm.call(&function, core::slice::from_ref(&v), &[])?;
                    vm.truthy(&r)?
                };
                if keep {
                    found = Some(v);
                    break;
                }
            }
            match found {
                Some(v) => (Some(v), Iter::Filter { function, inner }),
                None => (None, Iter::Done),
            }
        }
        Iter::Reversed { seq, index } => {
            if index == 0 {
                (None, Iter::Done)
            } else {
                let item = match &seq {
                    Value::List(l) => l.borrow().get(index - 1).cloned(),
                    Value::Tuple(t) => t.get(index - 1).cloned(),
                    other => Some(vm.get_item(other, &Value::int(index as i64 - 1))?),
                };
                match item {
                    Some(v) => (Some(v), Iter::Reversed { seq, index: index - 1 }),
                    None => (None, Iter::Done),
                }
            }
        }
        Iter::Callable { function, sentinel } => {
            let v = vm.call(&function, &[], &[])?;
            if vm.eq(&v, &sentinel)? {
                (None, Iter::Done)
            } else {
                (Some(v), Iter::Callable { function, sentinel })
            }
        }
        Iter::Object(object) => match vm.call_special(&object, "__next__", &[]) {
            Ok(Some(v)) => (Some(v), Iter::Object(object)),
            Ok(None) => (None, Iter::Done),
            Err(e) if vm.is_instance_of(&e, &vm.types.stop_iteration.clone()) => (None, Iter::Done),
            Err(e) => {
                *state.borrow_mut() = Iter::Object(object);
                return Err(e);
            }
        },
        Iter::Done => (None, Iter::Done),
    };
    *state.borrow_mut() = back;
    Ok(result)
}

// ============================================================
//  Sets: CPython's algorithms (`Objects/setobject.c`), so that the
//  tables -- and the order a set prints in -- come out the same
// ============================================================

fn set_table(value: &Value) -> SetTable {
    match value {
        Value::Set(s) => s.borrow().clone(),
        Value::FrozenSet(s) => (**s).clone(),
        _ => SetTable::new(),
    }
}

/// `set_merge`: `other`'s keys into `so`.
pub fn table_merge(vm: &mut Vm, so: &mut SetTable, other: &SetTable) -> PyResult<()> {
    let mut eq = |p: &Value, q: &Value| vm.eq(p, q);
    so.merge(other, &mut eq)
}

pub fn table_add(vm: &mut Vm, so: &mut SetTable, hash: i64, key: Value) -> PyResult<bool> {
    let mut eq = |p: &Value, q: &Value| vm.eq(p, q);
    so.add(hash, key, &mut eq)
}

fn table_contains(vm: &mut Vm, table: &SetTable, hash: i64, key: &Value) -> PyResult<bool> {
    let mut eq = |p: &Value, q: &Value| vm.eq(p, q);
    table.contains(hash, key, &mut eq)
}

fn table_discard(vm: &mut Vm, so: &mut SetTable, hash: i64, key: &Value) -> PyResult<bool> {
    let mut eq = |p: &Value, q: &Value| vm.eq(p, q);
    so.discard(hash, key, &mut eq)
}

/// `set_copy`: a new table, merged from `t`.
pub fn set_copy(vm: &mut Vm, t: &SetTable) -> PyResult<SetTable> {
    let mut out = SetTable::new();
    table_merge(vm, &mut out, t)?;
    Ok(out)
}

/// `set_update_internal`: a set is merged, a dict pre-sizes the table,
/// anything else is added key by key.
pub fn set_update_from(vm: &mut Vm, so: &mut SetTable, iterable: &Value) -> PyResult<()> {
    match iterable {
        Value::Set(_) | Value::FrozenSet(_) => {
            let other = set_table(iterable);
            table_merge(vm, so, &other)
        }
        Value::Dict(d) => {
            let keys: Vec<(i64, Value)> = d.borrow().iter().map(|e| (e.hash, e.key.clone())).collect();
            so.presize(keys.len());
            for (hash, key) in keys {
                table_add(vm, so, hash, key)?;
            }
            Ok(())
        }
        other => {
            let inner = vm.unwrap_builtin(other);
            if matches!(inner, Value::Set(_) | Value::FrozenSet(_) | Value::Dict(_)) && !inner.is(other) {
                return set_update_from(vm, so, &inner);
            }
            let iterator = vm.get_iter(other)?;
            while let Some(key) = vm.next(&iterator)? {
                let hash = vm.hash(&key)?;
                table_add(vm, so, hash, key)?;
            }
            Ok(())
        }
    }
}

/// `set_intersection`: the smaller table walked, the larger probed.
pub fn set_intersection(vm: &mut Vm, so: &SetTable, other: &SetTable) -> PyResult<SetTable> {
    let (larger, smaller) = if other.len() > so.len() { (other, so) } else { (so, other) };
    let mut out = SetTable::new();
    for (hash, key) in smaller.entries() {
        if table_contains(vm, larger, hash, &key)? {
            table_add(vm, &mut out, hash, key)?;
        }
    }
    Ok(out)
}

/// `set_difference_update_internal`.
pub fn set_difference_update(vm: &mut Vm, so: &mut SetTable, other: &SetTable) -> PyResult<()> {
    let other = if (other.len() >> 3) > so.len() { set_intersection(vm, so, other)? } else { other.clone() };
    for (hash, key) in other.entries() {
        table_discard(vm, so, hash, &key)?;
    }
    so.drop_dummies();
    Ok(())
}

/// `set_difference`: copied then emptied when the other set is small,
/// built afresh otherwise.
pub fn set_difference(vm: &mut Vm, so: &SetTable, other: &SetTable) -> PyResult<SetTable> {
    if (so.len() >> 2) > other.len() {
        let mut out = set_copy(vm, so)?;
        set_difference_update(vm, &mut out, other)?;
        return Ok(out);
    }
    let mut out = SetTable::new();
    for (hash, key) in so.entries() {
        if !table_contains(vm, other, hash, &key)? {
            table_add(vm, &mut out, hash, key)?;
        }
    }
    Ok(out)
}

/// `set_symmetric_difference_update`: each key of `other` toggled.
pub fn set_symmetric_update(vm: &mut Vm, so: &mut SetTable, other: &SetTable) -> PyResult<()> {
    for (hash, key) in other.entries() {
        if !table_discard(vm, so, hash, &key)? {
            table_add(vm, so, hash, key)?;
        }
    }
    Ok(())
}

/// `|`, `&`, `-`, `^` of sets, and their in-place forms.
pub fn set_operator(vm: &mut Vm, op: BinOp, a: &Value, b: &Value, inplace: bool) -> PyResult<Value> {
    let x = set_table(a);
    let y = set_table(b);
    let same = a.is(b);
    let out = match op {
        BinOp::BitOr => {
            let mut out = if inplace { x } else { set_copy(vm, &x)? };
            if !same || !inplace {
                table_merge(vm, &mut out, &y)?;
            }
            out
        }
        BinOp::BitAnd => {
            if same { set_copy(vm, &x)? } else { set_intersection(vm, &x, &y)? }
        }
        BinOp::Sub => {
            if inplace {
                if same {
                    SetTable::new()
                } else {
                    let mut out = x;
                    set_difference_update(vm, &mut out, &y)?;
                    out
                }
            } else {
                set_difference(vm, &x, &y)?
            }
        }
        _ => {
            if inplace {
                if same {
                    SetTable::new()
                } else {
                    let mut out = x;
                    set_symmetric_update(vm, &mut out, &y)?;
                    out
                }
            } else {
                // A copy of the right operand, toggled by the left one.
                let mut out = set_copy(vm, &y)?;
                set_symmetric_update(vm, &mut out, &x)?;
                out
            }
        }
    };
    if inplace {
        if let Value::Set(s) = a {
            *s.borrow_mut() = out;
            return Ok(a.clone());
        }
    }
    Ok(match a {
        Value::FrozenSet(_) => Value::FrozenSet(Rc::new(out)),
        _ => Value::set(out),
    })
}

/// Set comparisons: equality and inclusion.
pub fn set_compare(vm: &mut Vm, a: &Value, b: &Value, op: CmpOp) -> PyResult<Option<Ordering>> {
    let x = set_table(a);
    let y = set_table(b);
    let mut x_in_y = true;
    for (hash, key) in x.entries() {
        if !table_contains(vm, &y, hash, &key)? {
            x_in_y = false;
            break;
        }
    }
    let mut y_in_x = true;
    for (hash, key) in y.entries() {
        if !table_contains(vm, &x, hash, &key)? {
            y_in_x = false;
            break;
        }
    }
    let _ = op;
    Ok(match (x_in_y, y_in_x) {
        (true, true) => Some(Ordering::Equal),
        (true, false) => Some(Ordering::Less),
        (false, true) => Some(Ordering::Greater),
        (false, false) => None,
    })
}
