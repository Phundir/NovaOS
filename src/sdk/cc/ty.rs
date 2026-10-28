//! C types, with the sizes of x86_64 System V (LP64).
//!
//! `char` is signed, `int` 32 bits, `long` and pointers 64, `long long`
//! is `long`, an `enum` is an `int`. A structure's members, size and
//! alignment live in a shared `Record`, so that `struct s;` named before
//! its definition becomes complete everywhere at once when the
//! definition arrives.

use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::RefCell;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Void,
    Bool,
    Char,
    Short,
    Int,
    Long,
    Float,
    Double,
    Ptr,
    Func,
    Array,
    Struct,
    Union,
}

#[derive(Debug, Clone)]
pub struct Member {
    pub name: Option<alloc::rc::Rc<str>>,
    pub ty: Rc<Type>,
    pub offset: i64,
}

#[derive(Debug, Default)]
pub struct Record {
    pub members: Vec<Member>,
    pub size: i64,
    pub align: i64,
    pub complete: bool,
    /// Ends with `T name[]`.
    pub flexible: bool,
}

#[derive(Debug, Clone)]
pub struct Type {
    pub kind: Kind,
    pub unsigned: bool,
    /// Pointed-to type, or element type.
    pub base: Option<Rc<Type>>,
    /// Array length; -1 while unknown (`int a[]`).
    pub len: i64,
    pub record: Option<Rc<RefCell<Record>>>,
    /// Function: return type, parameter types, `...`.
    pub ret: Option<Rc<Type>>,
    pub params: Vec<Rc<Type>>,
    pub variadic: bool,
    /// Declared with a parameter list (`f(void)`, `f(int)`), not `f()`.
    pub prototyped: bool,
}

impl Type {
    fn simple(kind: Kind, unsigned: bool) -> Rc<Type> {
        Rc::new(Type {
            kind,
            unsigned,
            base: None,
            len: 0,
            record: None,
            ret: None,
            params: Vec::new(),
            variadic: false,
            prototyped: false,
        })
    }

    pub fn void() -> Rc<Type> {
        Type::simple(Kind::Void, false)
    }
    pub fn bool() -> Rc<Type> {
        Type::simple(Kind::Bool, true)
    }
    pub fn char(unsigned: bool) -> Rc<Type> {
        Type::simple(Kind::Char, unsigned)
    }
    pub fn short(unsigned: bool) -> Rc<Type> {
        Type::simple(Kind::Short, unsigned)
    }
    pub fn int() -> Rc<Type> {
        Type::simple(Kind::Int, false)
    }
    pub fn uint() -> Rc<Type> {
        Type::simple(Kind::Int, true)
    }
    pub fn long() -> Rc<Type> {
        Type::simple(Kind::Long, false)
    }
    pub fn ulong() -> Rc<Type> {
        Type::simple(Kind::Long, true)
    }
    pub fn integer(kind: Kind, unsigned: bool) -> Rc<Type> {
        Type::simple(kind, unsigned)
    }
    pub fn float() -> Rc<Type> {
        Type::simple(Kind::Float, false)
    }
    pub fn double() -> Rc<Type> {
        Type::simple(Kind::Double, false)
    }

    pub fn pointer(base: Rc<Type>) -> Rc<Type> {
        Rc::new(Type { base: Some(base), unsigned: true, ..(*Type::simple(Kind::Ptr, true)).clone() })
    }

    pub fn array(base: Rc<Type>, len: i64) -> Rc<Type> {
        Rc::new(Type { base: Some(base), len, ..(*Type::simple(Kind::Array, false)).clone() })
    }

    pub fn function(ret: Rc<Type>, params: Vec<Rc<Type>>, variadic: bool, prototyped: bool) -> Rc<Type> {
        Rc::new(Type { ret: Some(ret), params, variadic, prototyped, ..(*Type::simple(Kind::Func, false)).clone() })
    }

    pub fn record(kind: Kind, record: Rc<RefCell<Record>>) -> Rc<Type> {
        Rc::new(Type { record: Some(record), ..(*Type::simple(kind, false)).clone() })
    }

    pub fn size(&self) -> i64 {
        match self.kind {
            Kind::Void => 1,
            Kind::Bool | Kind::Char => 1,
            Kind::Short => 2,
            Kind::Int | Kind::Float => 4,
            Kind::Long | Kind::Double | Kind::Ptr => 8,
            Kind::Func => 1,
            Kind::Array => self.base().size() * self.len.max(0),
            Kind::Struct | Kind::Union => self.record.as_ref().map(|r| r.borrow().size).unwrap_or(0),
        }
    }

    pub fn align(&self) -> i64 {
        match self.kind {
            Kind::Array => self.base().align(),
            Kind::Struct | Kind::Union => self.record.as_ref().map(|r| r.borrow().align).unwrap_or(1),
            Kind::Func | Kind::Void => 1,
            _ => self.size(),
        }
    }

    pub fn base(&self) -> Rc<Type> {
        self.base.clone().unwrap_or_else(Type::void)
    }

    pub fn ret(&self) -> Rc<Type> {
        self.ret.clone().unwrap_or_else(Type::void)
    }

    pub fn is_integer(&self) -> bool {
        matches!(self.kind, Kind::Bool | Kind::Char | Kind::Short | Kind::Int | Kind::Long)
    }

    pub fn is_flonum(&self) -> bool {
        matches!(self.kind, Kind::Float | Kind::Double)
    }

    pub fn is_numeric(&self) -> bool {
        self.is_integer() || self.is_flonum()
    }

    pub fn is_pointer_like(&self) -> bool {
        matches!(self.kind, Kind::Ptr | Kind::Array)
    }

    pub fn is_scalar(&self) -> bool {
        self.is_numeric() || self.kind == Kind::Ptr
    }

    pub fn is_record(&self) -> bool {
        matches!(self.kind, Kind::Struct | Kind::Union)
    }

    pub fn is_complete(&self) -> bool {
        match self.kind {
            Kind::Void => false,
            Kind::Array => self.len >= 0 && self.base().is_complete(),
            Kind::Struct | Kind::Union => self.record.as_ref().is_some_and(|r| r.borrow().complete),
            _ => true,
        }
    }

    /// A member by name, looking into anonymous members.
    pub fn member(&self, name: &str) -> Option<Member> {
        let record = self.record.as_ref()?;
        for member in record.borrow().members.iter() {
            match &member.name {
                Some(n) if &**n == name => return Some(member.clone()),
                None if member.ty.is_record() => {
                    if let Some(mut inner) = member.ty.member(name) {
                        inner.offset += member.offset;
                        return Some(inner);
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// Compatible enough for `?:`, assignment between pointers and
    /// redeclarations: same shape, ignoring qualifiers.
    pub fn same(&self, other: &Type) -> bool {
        if self.kind != other.kind {
            return false;
        }
        match self.kind {
            Kind::Ptr => self.base().same(&other.base()) || self.base().kind == Kind::Void || other.base().kind == Kind::Void,
            Kind::Array => self.base().same(&other.base()),
            Kind::Struct | Kind::Union => match (&self.record, &other.record) {
                (Some(a), Some(b)) => Rc::ptr_eq(a, b),
                _ => false,
            },
            Kind::Func => self.ret().same(&other.ret()),
            _ => self.unsigned == other.unsigned,
        }
    }

    /// The type after the integer promotions (C11 6.3.1.1).
    pub fn promoted(self: &Rc<Type>) -> Rc<Type> {
        match self.kind {
            Kind::Bool | Kind::Char | Kind::Short => Type::int(),
            _ => self.clone(),
        }
    }
}

/// The usual arithmetic conversions (C11 6.3.1.8): the type both
/// operands of `a + b` are converted to.
pub fn common(a: &Rc<Type>, b: &Rc<Type>) -> Rc<Type> {
    if a.kind == Kind::Double || b.kind == Kind::Double {
        return Type::double();
    }
    if a.kind == Kind::Float || b.kind == Kind::Float {
        return Type::float();
    }
    let a = a.promoted();
    let b = b.promoted();
    if a.size() != b.size() {
        return if a.size() > b.size() { a } else { b };
    }
    if b.unsigned { b } else { a }
}

/// Rounds `n` up to a multiple of `align` (a power of two).
pub fn align_to(n: i64, align: i64) -> i64 {
    (n + align - 1) / align * align
}
