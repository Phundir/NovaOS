//! Values and objects.
//!
//! A value is a small enum; objects are reference-counted cells, shared
//! the way JavaScript shares them. Reference counting does not collect
//! cycles, and closures make cycles all the time (a function reachable
//! from the scope it closes over). Rather than a tracing collector, the
//! realm keeps a weak list of every object it made and **empties them
//! all when the page goes away**: the cycles break, the counts fall to
//! zero, the memory comes back. Garbage made while the page lives stays
//! until then -- the price of simplicity, bounded by the realm's memory
//! budget.
//!
//! Strings are UTF-8 with their UTF-16 length computed once: JavaScript
//! indexes strings in UTF-16 units, and a loop over `s.length` with
//! `charCodeAt` must not rescan the whole string at every step.

use alloc::rc::{Rc, Weak};
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;
use core::cmp::Ordering;

use super::ast::Func;
use super::num::Num;
use super::regex::Regex;
use super::interp::{Ctx, Scope};

// ============================================================
//  Strings
// ============================================================

pub struct StrBox {
    text: String,
    /// All ASCII: byte index = UTF-16 index.
    ascii: bool,
    units: usize,
}

#[derive(Clone)]
pub struct JsStr(Rc<StrBox>);

impl JsStr {
    pub fn new(text: String) -> JsStr {
        let ascii = text.is_ascii();
        let units = if ascii { text.len() } else { text.chars().map(char::len_utf16).sum() };
        JsStr(Rc::new(StrBox { text, ascii, units }))
    }

    pub fn as_str(&self) -> &str {
        &self.0.text
    }

    pub fn is_ascii(&self) -> bool {
        self.0.ascii
    }

    /// Length in UTF-16 code units, as `length` reports it.
    pub fn units(&self) -> usize {
        self.0.units
    }

    pub fn ptr_eq(&self, other: &JsStr) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }

    /// The UTF-16 unit at `index`.
    pub fn unit_at(&self, index: usize) -> Option<u16> {
        if index >= self.0.units {
            return None;
        }
        if self.0.ascii {
            return Some(self.0.text.as_bytes()[index] as u16);
        }
        let mut position = 0;
        for c in self.0.text.chars() {
            let mut buffer = [0u16; 2];
            let encoded = c.encode_utf16(&mut buffer);
            if index < position + encoded.len() {
                return Some(encoded[index - position]);
            }
            position += encoded.len();
        }
        None
    }

    /// Byte offset of UTF-16 index `index` (clamped to the end). An index
    /// inside a surrogate pair goes to the start of the character.
    pub fn byte_of(&self, index: usize) -> usize {
        if self.0.ascii {
            return index.min(self.0.text.len());
        }
        let mut position = 0;
        for (byte, c) in self.0.text.char_indices() {
            if position >= index {
                return byte;
            }
            position += c.len_utf16();
            if position > index {
                return byte;
            }
        }
        self.0.text.len()
    }

    /// UTF-16 index of a byte offset.
    pub fn unit_of(&self, byte: usize) -> usize {
        if self.0.ascii {
            return byte;
        }
        self.0.text[..byte.min(self.0.text.len())].chars().map(char::len_utf16).sum()
    }

    /// Substring by UTF-16 indices.
    pub fn slice(&self, start: usize, end: usize) -> JsStr {
        if start >= end {
            return JsStr::from("");
        }
        let a = self.byte_of(start);
        let b = self.byte_of(end);
        JsStr::from(&self.0.text[a..b.max(a)])
    }
}

impl From<&str> for JsStr {
    fn from(text: &str) -> JsStr {
        JsStr::new(String::from(text))
    }
}

impl From<String> for JsStr {
    fn from(text: String) -> JsStr {
        JsStr::new(text)
    }
}

impl core::ops::Deref for JsStr {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0.text
    }
}

impl PartialEq for JsStr {
    fn eq(&self, other: &JsStr) -> bool {
        Rc::ptr_eq(&self.0, &other.0) || self.0.text == other.0.text
    }
}

impl Eq for JsStr {}

impl PartialOrd for JsStr {
    fn partial_cmp(&self, other: &JsStr) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for JsStr {
    fn cmp(&self, other: &JsStr) -> Ordering {
        self.0.text.cmp(&other.0.text)
    }
}

impl core::borrow::Borrow<str> for JsStr {
    fn borrow(&self) -> &str {
        &self.0.text
    }
}

impl core::fmt::Debug for JsStr {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{:?}", self.0.text)
    }
}

/// Symbols are property keys that no string can spell: their key text
/// starts with a NUL, which the enumeration of keys skips.
pub fn symbol_key(id: usize, description: &str) -> JsStr {
    JsStr::from(alloc::format!("\0@{}:{}", id, description))
}

pub fn is_symbol_key(key: &str) -> bool {
    key.starts_with('\0')
}

pub fn symbol_description(key: &str) -> &str {
    key.split_once(':').map_or("", |(_, rest)| rest)
}

// ============================================================
//  Values
// ============================================================

#[derive(Clone)]
pub enum Value {
    Undefined,
    Null,
    Bool(bool),
    Num(Num),
    Str(JsStr),
    /// A symbol: its key text (see `symbol_key`).
    Sym(JsStr),
    Obj(Obj),
}

impl Value {
    pub fn num(value: i64) -> Value {
        Value::Num(Num::from_i64(value))
    }

    pub fn str(text: &str) -> Value {
        Value::Str(JsStr::from(text))
    }

    pub fn is_nullish(&self) -> bool {
        matches!(self, Value::Undefined | Value::Null)
    }

    pub fn is_undefined(&self) -> bool {
        matches!(self, Value::Undefined)
    }

    pub fn as_obj(&self) -> Option<&Obj> {
        match self {
            Value::Obj(object) => Some(object),
            _ => None,
        }
    }

    pub fn truthy(&self) -> bool {
        match self {
            Value::Undefined | Value::Null => false,
            Value::Bool(b) => *b,
            Value::Num(n) => !(n.is_zero() || n.is_nan()),
            Value::Str(s) => !s.is_empty(),
            Value::Sym(_) | Value::Obj(_) => true,
        }
    }

    pub fn type_of(&self) -> &'static str {
        match self {
            Value::Undefined => "undefined",
            Value::Null => "object",
            Value::Bool(_) => "boolean",
            Value::Num(_) => "number",
            Value::Str(_) => "string",
            Value::Sym(_) => "symbol",
            Value::Obj(object) => {
                if object.is_callable() {
                    "function"
                } else {
                    "object"
                }
            }
        }
    }

    /// `===`.
    pub fn strict_eq(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::Undefined, Value::Undefined) | (Value::Null, Value::Null) => true,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Num(a), Value::Num(b)) => a.eq(*b),
            (Value::Str(a), Value::Str(b)) | (Value::Sym(a), Value::Sym(b)) => a == b,
            (Value::Obj(a), Value::Obj(b)) => a.ptr_eq(b),
            _ => false,
        }
    }

    /// `SameValueZero`: `===` except that NaN equals NaN.
    pub fn same_value_zero(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::Num(a), Value::Num(b)) if a.is_nan() && b.is_nan() => true,
            _ => self.strict_eq(other),
        }
    }

    /// `Object.is`.
    pub fn same_value(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::Num(a), Value::Num(b)) => a.same_value(*b),
            _ => self.strict_eq(other),
        }
    }
}

impl core::fmt::Debug for Value {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Value::Undefined => f.write_str("undefined"),
            Value::Null => f.write_str("null"),
            Value::Bool(b) => write!(f, "{}", b),
            Value::Num(n) => write!(f, "{:?}", n),
            Value::Str(s) => write!(f, "{:?}", s),
            Value::Sym(s) => write!(f, "Symbol({})", symbol_description(s)),
            Value::Obj(o) => write!(f, "[object {}]", o.class_name()),
        }
    }
}

// ============================================================
//  Objects
// ============================================================

/// Property attributes.
pub const ENUMERABLE: u8 = 1;
pub const WRITABLE: u8 = 2;
pub const CONFIGURABLE: u8 = 4;
/// Ordinary data property: all three.
pub const DEFAULT: u8 = ENUMERABLE | WRITABLE | CONFIGURABLE;
/// Built-in methods: not enumerable.
pub const HIDDEN: u8 = WRITABLE | CONFIGURABLE;

#[derive(Clone)]
pub enum Slot {
    Data(Value, u8),
    Accessor(Option<Obj>, Option<Obj>, u8),
}

impl Slot {
    pub fn flags(&self) -> u8 {
        match self {
            Slot::Data(_, flags) | Slot::Accessor(_, _, flags) => *flags,
        }
    }
}

/// Properties in insertion order, with an index once there are many.
#[derive(Default, Clone)]
pub struct Props {
    entries: Vec<(JsStr, Slot)>,
    index: Option<alloc::collections::BTreeMap<JsStr, usize>>,
}

const INDEX_FROM: usize = 12;

impl Props {
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn position(&self, key: &str) -> Option<usize> {
        if let Some(index) = &self.index {
            return index.get(key).copied();
        }
        self.entries.iter().position(|(k, _)| k.as_str() == key)
    }

    fn position_key(&self, key: &JsStr) -> Option<usize> {
        if let Some(index) = &self.index {
            return index.get(key).copied();
        }
        self.entries.iter().position(|(k, _)| k == key)
    }

    pub fn get(&self, key: &JsStr) -> Option<&Slot> {
        self.position_key(key).map(|i| &self.entries[i].1)
    }

    pub fn get_str(&self, key: &str) -> Option<&Slot> {
        self.position(key).map(|i| &self.entries[i].1)
    }

    pub fn get_mut(&mut self, key: &JsStr) -> Option<&mut Slot> {
        self.position_key(key).map(move |i| &mut self.entries[i].1)
    }

    pub fn set(&mut self, key: JsStr, slot: Slot) {
        if let Some(i) = self.position_key(&key) {
            self.entries[i].1 = slot;
            return;
        }
        if let Some(index) = &mut self.index {
            index.insert(key.clone(), self.entries.len());
        }
        self.entries.push((key, slot));
        if self.index.is_none() && self.entries.len() > INDEX_FROM {
            self.rebuild_index();
        }
    }

    fn rebuild_index(&mut self) {
        let mut index = alloc::collections::BTreeMap::new();
        for (position, (key, _)) in self.entries.iter().enumerate() {
            index.insert(key.clone(), position);
        }
        self.index = Some(index);
    }

    pub fn remove(&mut self, key: &JsStr) -> bool {
        match self.position_key(key) {
            Some(i) => {
                self.entries.remove(i);
                if self.index.is_some() {
                    self.rebuild_index();
                }
                true
            }
            None => false,
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &(JsStr, Slot)> {
        self.entries.iter()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.index = None;
    }
}

/// A native function: receives the context, `this`, the arguments, and
/// the data captured when it was made.
pub type NativeFn = fn(&mut Ctx, &Value, &[Value], &Value) -> Result<Value, super::interp::Ctrl>;

pub struct Closure {
    pub func: Rc<Func>,
    pub scope: Scope,
    /// Arrow functions keep the `this` of where they were made.
    pub this: Option<Value>,
    /// The object whose methods see `super` (class prototype or object
    /// literal).
    pub home: Option<Obj>,
    /// For class constructors: the class, whose fields are initialised
    /// on construction, and whether it extends another.
    pub class: Option<Rc<super::ast::Class>>,
    pub class_scope: Option<Scope>,
    pub derived: bool,
    /// `new.target` / the function object, for a named function
    /// expression's own binding.
    pub self_name: Option<JsStr>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PromiseState {
    Pending,
    Fulfilled,
    Rejected,
}

pub struct PromiseData {
    pub state: PromiseState,
    pub value: Value,
    /// Reactions waiting for settlement: (on fulfilled, on rejected,
    /// derived promise's resolve and reject functions).
    pub reactions: Vec<(Value, Value, Value, Value)>,
    pub handled: bool,
}

/// Iteration state of the built-in iterators.
pub enum IterState {
    /// Over a list of values computed at creation (Map, Set, keys,
    /// generator results...).
    List(Vec<Value>, usize),
    /// Over an array, live: elements pushed while iterating are seen.
    Array(Obj, usize, u8),
    /// Over a string's code points.
    Str(JsStr, usize),
    /// Over a Map or Set, live: (collection, position, 0 keys / 1 values
    /// / 2 entries).
    Map(Obj, usize, u8),
}

pub enum Kind {
    Ordinary,
    Array(Vec<Value>),
    Function(Rc<Closure>),
    Native(NativeFn, Value, JsStr),
    Bound(Obj, Value, Vec<Value>),
    Error,
    Boolean(bool),
    Number(Num),
    String(JsStr),
    Symbol(JsStr),
    Date(Num),
    RegExp(Rc<Regex>),
    Map(MapData),
    Set(MapData),
    Promise(PromiseData),
    Iterator(IterState),
    /// A DOM node wrapper.
    Node(usize),
    Arguments,
}

/// A Map or Set key: equal under SameValueZero exactly when the keys are
/// equal here (-0 is +0, every NaN is one NaN, objects by identity).
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum MapKey {
    Undefined,
    Null,
    Bool(bool),
    Num(u64),
    Str(JsStr),
    Sym(JsStr),
    Obj(usize),
}

impl MapKey {
    pub fn of(value: &Value) -> MapKey {
        match value {
            Value::Undefined => MapKey::Undefined,
            Value::Null => MapKey::Null,
            Value::Bool(b) => MapKey::Bool(*b),
            Value::Num(n) => {
                if n.is_nan() {
                    MapKey::Num(Num::NAN.0)
                } else if n.is_zero() {
                    MapKey::Num(0)
                } else {
                    MapKey::Num(n.0)
                }
            }
            Value::Str(s) => MapKey::Str(s.clone()),
            Value::Sym(s) => MapKey::Sym(s.clone()),
            Value::Obj(o) => MapKey::Obj(o.id()),
        }
    }
}

/// Entries in insertion order; deleted ones leave a hole so that live
/// iterators keep their place, and the holes are squeezed out when they
/// outnumber the entries.
#[derive(Default)]
pub struct MapData {
    pub entries: Vec<Option<(Value, Value)>>,
    index: alloc::collections::BTreeMap<MapKey, usize>,
    live: usize,
}

impl MapData {
    pub fn len(&self) -> usize {
        self.live
    }

    pub fn is_empty(&self) -> bool {
        self.live == 0
    }

    pub fn get(&self, key: &Value) -> Option<&Value> {
        let position = *self.index.get(&MapKey::of(key))?;
        self.entries[position].as_ref().map(|(_, v)| v)
    }

    pub fn has(&self, key: &Value) -> bool {
        self.index.contains_key(&MapKey::of(key))
    }

    pub fn set(&mut self, key: Value, value: Value) {
        let map_key = MapKey::of(&key);
        if let Some(position) = self.index.get(&map_key) {
            if let Some(entry) = &mut self.entries[*position] {
                entry.1 = value;
            }
            return;
        }
        // -0 is stored as +0.
        let key = match key {
            Value::Num(n) if n.is_zero() => Value::Num(Num::ZERO),
            other => other,
        };
        self.index.insert(map_key, self.entries.len());
        self.entries.push(Some((key, value)));
        self.live += 1;
    }

    pub fn delete(&mut self, key: &Value) -> bool {
        match self.index.remove(&MapKey::of(key)) {
            Some(position) => {
                self.entries[position] = None;
                self.live -= 1;
                true
            }
            None => false,
        }
    }

    pub fn clear(&mut self) {
        for entry in self.entries.iter_mut() {
            *entry = None;
        }
        self.index.clear();
        self.live = 0;
    }

    /// Squeezes the holes out, when no iterator is running (the caller
    /// knows).
    pub fn compact(&mut self) {
        if self.entries.len() < 32 || self.entries.len() < self.live * 2 {
            return;
        }
        self.entries.retain(|entry| entry.is_some());
        self.index.clear();
        for (position, entry) in self.entries.iter().enumerate() {
            if let Some((key, _)) = entry {
                self.index.insert(MapKey::of(key), position);
            }
        }
    }
}

pub struct Object {
    pub proto: Option<Obj>,
    pub props: Props,
    pub kind: Kind,
    pub extensible: bool,
    /// `Object.freeze`: writes are ignored.
    pub frozen: bool,
    /// The class tag for `Object.prototype.toString` and debugging.
    pub class: &'static str,
}

#[derive(Clone)]
pub struct Obj(pub Rc<RefCell<Object>>);

impl Obj {
    pub fn new(kind: Kind, proto: Option<Obj>, class: &'static str) -> Obj {
        Obj(Rc::new(RefCell::new(Object { proto, props: Props::default(), kind, extensible: true, frozen: false, class })))
    }

    pub fn ptr_eq(&self, other: &Obj) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }

    pub fn id(&self) -> usize {
        Rc::as_ptr(&self.0) as *const () as usize
    }

    pub fn downgrade(&self) -> Weak<RefCell<Object>> {
        Rc::downgrade(&self.0)
    }

    pub fn borrow(&self) -> core::cell::Ref<'_, Object> {
        self.0.borrow()
    }

    pub fn borrow_mut(&self) -> core::cell::RefMut<'_, Object> {
        self.0.borrow_mut()
    }

    pub fn is_callable(&self) -> bool {
        matches!(self.borrow().kind, Kind::Function(_) | Kind::Native(..) | Kind::Bound(..))
    }

    pub fn is_array(&self) -> bool {
        matches!(self.borrow().kind, Kind::Array(_))
    }

    pub fn class_name(&self) -> &'static str {
        self.borrow().class
    }

    pub fn proto(&self) -> Option<Obj> {
        self.borrow().proto.clone()
    }

    /// An own data property, set without setters or checks: for building
    /// objects.
    pub fn put(&self, key: &str, value: Value) {
        self.borrow_mut().props.set(JsStr::from(key), Slot::Data(value, DEFAULT));
    }

    pub fn put_key(&self, key: JsStr, value: Value, flags: u8) {
        self.borrow_mut().props.set(key, Slot::Data(value, flags));
    }

    pub fn put_hidden(&self, key: &str, value: Value) {
        self.borrow_mut().props.set(JsStr::from(key), Slot::Data(value, HIDDEN));
    }

    /// Own data value, if any (no getters, no prototype).
    pub fn own_value(&self, key: &str) -> Option<Value> {
        match self.borrow().props.get_str(key) {
            Some(Slot::Data(value, _)) => Some(value.clone()),
            _ => None,
        }
    }

    pub fn node(&self) -> Option<usize> {
        match self.borrow().kind {
            Kind::Node(id) => Some(id),
            _ => None,
        }
    }
}

/// Array index form of a key: a canonical decimal integer below 2^32 - 1.
pub fn array_index(key: &str) -> Option<usize> {
    let bytes = key.as_bytes();
    if bytes.is_empty() || bytes.len() > 10 || (bytes.len() > 1 && bytes[0] == b'0') {
        return None;
    }
    let mut value: u64 = 0;
    for b in bytes {
        if !b.is_ascii_digit() {
            return None;
        }
        value = value * 10 + (b - b'0') as u64;
    }
    if value >= u32::MAX as u64 {
        return None;
    }
    Some(value as usize)
}
