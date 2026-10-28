//! Initializers (C11 6.7.9) and constant expressions (6.6).
//!
//! An initializer is first read into a tree shaped like its type (`Init`)
//! -- that is where braces may be elided and designators (`.x =`,
//! `[3] =`) jump around -- then lowered: for a local, into a clearing of
//! the object followed by one assignment per initialized scalar; for a
//! global, into the bytes of `.data`. A pointer in those bytes (`char *s
//! = "x";`, `int *p = &v;`) becomes a relocation the startup code fills,
//! Nova binaries having no relocation table.

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::vec::Vec;

use super::super::CcError;
use super::super::ast::*;
use super::super::lex::{Kind as TokKind, Loc};
use super::super::ty::{Kind, Type};
use super::Parser;
use super::expr::round_to_float;
use super::stmt::convert_constant;
use crate::web::js::num::Num;

/// The initializer of one object, shaped like its type.
struct Init {
    ty: Rc<Type>,
    expr: Option<Node>,
    children: Vec<Init>,
    /// Union: the member initialized.
    member: Option<usize>,
}

impl Init {
    fn new(ty: Rc<Type>) -> Init {
        let mut children = Vec::new();
        match ty.kind {
            Kind::Array if ty.len > 0 => {
                for _ in 0..ty.len {
                    children.push(Init::new(ty.base()));
                }
            }
            Kind::Struct | Kind::Union => {
                if let Some(record) = &ty.record {
                    for member in record.borrow().members.iter() {
                        children.push(Init::new(member.ty.clone()));
                    }
                }
            }
            _ => {}
        }
        Init { ty, expr: None, children, member: None }
    }

    /// An array of unknown length grows to hold element `index`.
    fn reach(&mut self, index: usize) {
        while self.children.len() <= index {
            self.children.push(Init::new(self.ty.base()));
        }
    }
}

fn boxed(node: Node) -> Box<Node> {
    Box::new(node)
}

impl Parser {
    /// `const-expr`: a conditional expression with an integer value.
    pub(super) fn const_expr(&mut self) -> Result<i64, CcError> {
        let node = self.conditional()?;
        self.eval(&node)
    }

    /// Reads the initializer of an object of type `ty`; gives back the
    /// tree and the type made complete (`int a[] = {1, 2}` has 2).
    fn initializer(&mut self, ty: Rc<Type>) -> Result<(Init, Rc<Type>), CcError> {
        let unknown = ty.kind == Kind::Array && ty.len < 0;
        let mut init = Init::new(if unknown { Type::array(ty.base(), 0) } else { ty.clone() });
        if unknown {
            init.ty = ty.clone();
        }
        self.initializer_into(&mut init, unknown)?;
        let ty = if unknown { Type::array(ty.base(), init.children.len() as i64) } else { ty };
        init.ty = ty.clone();
        Ok((init, ty))
    }

    fn initializer_into(&mut self, init: &mut Init, growable: bool) -> Result<(), CcError> {
        let ty = init.ty.clone();

        // `char s[] = "abc"`, with or without braces around the string.
        if ty.kind == Kind::Array && ty.base().kind == Kind::Char {
            let braced = self.peek_is("{") && self.tokens.get(self.pos + 1).is_some_and(|t| t.kind == TokKind::Str);
            if braced || self.token().kind == TokKind::Str {
                if braced {
                    self.pos += 1;
                }
                let loc = self.loc();
                let node = self.assign()?;
                let NodeKind::Global(label) = &node.kind else {
                    return Err(CcError::at(loc, "chaine attendue"));
                };
                let bytes = self.string_bytes(label);
                let count = if growable { bytes.len() } else { bytes.len().min(ty.len.max(0) as usize) };
                if growable {
                    init.reach(count.saturating_sub(1));
                }
                for (index, byte) in bytes.iter().take(count).enumerate() {
                    init.children[index].expr = Some(Node::new(NodeKind::Num(*byte as i8 as i64), Type::char(false), loc));
                }
                if braced {
                    self.eat(",");
                    self.expect("}")?;
                }
                return Ok(());
            }
        }

        match ty.kind {
            Kind::Array => {
                if self.eat("{") {
                    self.array_items(init, growable, true)
                } else {
                    self.array_items(init, growable, false)
                }
            }
            Kind::Struct | Kind::Union => {
                if self.eat("{") {
                    return self.record_items(init, true);
                }
                // A whole structure, or the first members with braces
                // elided.
                let start = self.pos;
                let node = self.assign()?;
                if node.ty.is_record() {
                    if !node.ty.same(&ty) {
                        return Err(CcError::at(node.loc, "initialisation d'une struct par une struct d'un autre type"));
                    }
                    init.expr = Some(node);
                    return Ok(());
                }
                self.pos = start;
                self.record_items(init, false)
            }
            _ => {
                if self.eat("{") {
                    let node = self.assign()?;
                    self.eat(",");
                    self.expect("}")?;
                    init.expr = Some(node);
                    return Ok(());
                }
                init.expr = Some(self.assign()?);
                Ok(())
            }
        }
    }

    /// The bytes of the string literal held by an anonymous global.
    fn string_bytes(&self, label: &Rc<str>) -> Vec<u8> {
        self.global_index
            .get(label)
            .and_then(|i| self.program.globals[*i].data.clone())
            .unwrap_or_default()
    }

    fn array_items(&mut self, init: &mut Init, growable: bool, braced: bool) -> Result<(), CcError> {
        let mut index: usize = 0;
        let limit = if growable { usize::MAX } else { init.ty.len.max(0) as usize };
        let mut first = true;
        loop {
            if braced && self.eat("}") {
                return Ok(());
            }
            if !first {
                if braced {
                    self.expect(",")?;
                    if self.eat("}") {
                        return Ok(());
                    }
                } else {
                    // Without braces the array takes what fits, and stops
                    // at a `,` it cannot use.
                    if index >= limit || !self.peek_is(",") || self.peek_at(1, "}") || self.peek_at(1, ".") {
                        return Ok(());
                    }
                    if self.peek_at(1, "[") && braced {
                        return Ok(());
                    }
                    self.pos += 1;
                }
            }
            first = false;
            if braced && self.peek_is("[") {
                self.pos += 1;
                let loc = self.loc();
                let at = self.const_expr()?;
                self.expect("]")?;
                if at < 0 || (at as usize) >= limit {
                    return Err(CcError::at(loc, "indice de designateur hors du tableau"));
                }
                index = at as usize;
                if growable {
                    init.reach(index);
                }
                self.designated(&mut init.children[index])?;
                index += 1;
                continue;
            }
            if index >= limit {
                if braced {
                    // Excess elements: GCC warns and drops them.
                    self.skip_initializer()?;
                    continue;
                }
                return Ok(());
            }
            if growable {
                init.reach(index);
            }
            self.initializer_into(&mut init.children[index], false)?;
            index += 1;
        }
    }

    fn record_items(&mut self, init: &mut Init, braced: bool) -> Result<(), CcError> {
        let ty = init.ty.clone();
        let union = ty.kind == Kind::Union;
        let members: Vec<(Option<Rc<str>>, Rc<Type>)> = ty
            .record
            .as_ref()
            .map(|r| r.borrow().members.iter().map(|m| (m.name.clone(), m.ty.clone())).collect())
            .unwrap_or_default();
        let mut index = 0;
        let mut first = true;
        loop {
            if braced && self.eat("}") {
                return Ok(());
            }
            if !first {
                if braced {
                    self.expect(",")?;
                    if self.eat("}") {
                        return Ok(());
                    }
                } else {
                    if index >= members.len() || union || !self.peek_is(",") || self.peek_at(1, "}") || self.peek_at(1, ".") {
                        return Ok(());
                    }
                    self.pos += 1;
                }
            }
            first = false;
            if braced && self.peek_is(".") {
                self.pos += 1;
                let name = self.ident()?;
                let Some(position) = members.iter().position(|(n, _)| n.as_deref() == Some(&*name.text)) else {
                    return Err(CcError::at(name.loc, &alloc::format!("aucun membre {}", name.text)));
                };
                index = position;
                if union {
                    init.member = Some(index);
                }
                self.designated(&mut init.children[index])?;
                index += 1;
                continue;
            }
            if index >= members.len() || (union && init.member.is_some()) {
                if braced {
                    self.skip_initializer()?;
                    continue;
                }
                return Ok(());
            }
            if union {
                init.member = Some(index);
            }
            self.initializer_into(&mut init.children[index], false)?;
            index += 1;
        }
    }

    /// After a designator: more designators, or `=` and the value.
    fn designated(&mut self, init: &mut Init) -> Result<(), CcError> {
        if self.peek_is("[") && init.ty.kind == Kind::Array {
            self.pos += 1;
            let loc = self.loc();
            let at = self.const_expr()?;
            self.expect("]")?;
            if at < 0 || (init.ty.len >= 0 && at >= init.ty.len) {
                return Err(CcError::at(loc, "indice de designateur hors du tableau"));
            }
            return self.designated(&mut init.children[at as usize]);
        }
        if self.peek_is(".") && init.ty.is_record() {
            self.pos += 1;
            let name = self.ident()?;
            let names: Vec<Option<Rc<str>>> = init
                .ty
                .record
                .as_ref()
                .map(|r| r.borrow().members.iter().map(|m| m.name.clone()).collect())
                .unwrap_or_default();
            let Some(position) = names.iter().position(|n| n.as_deref() == Some(&*name.text)) else {
                return Err(CcError::at(name.loc, &alloc::format!("aucun membre {}", name.text)));
            };
            if init.ty.kind == Kind::Union {
                init.member = Some(position);
            }
            return self.designated(&mut init.children[position]);
        }
        self.expect("=")?;
        self.initializer_into(init, false)
    }

    fn skip_initializer(&mut self) -> Result<(), CcError> {
        if self.eat("{") {
            let mut depth = 1;
            while depth > 0 {
                if self.at_end() {
                    return Err(self.error("initialiseur non ferme"));
                }
                if self.peek_is("{") {
                    depth += 1;
                } else if self.peek_is("}") {
                    depth -= 1;
                }
                self.pos += 1;
            }
            return Ok(());
        }
        self.assign().map(|_| ())
    }

    // ------------------------------------------------------------
    //  Locals: assignments
    // ------------------------------------------------------------

    /// `= initializer` of the local `index`, as an expression.
    pub(super) fn local_initializer(&mut self, index: usize, loc: Loc) -> Result<Node, CcError> {
        let ty = self.locals[index].ty.clone();
        let (init, ty) = self.initializer(ty)?;
        self.locals[index].ty = ty.clone();
        let mut node = Node::new(NodeKind::Zero(index), Type::void(), loc);
        let target = Node::new(NodeKind::Local(index), ty, loc);
        let mut assignments = Vec::new();
        self.lower_local(&init, target, &mut assignments)?;
        for assignment in assignments {
            node = Node::new(NodeKind::Comma(boxed(node), boxed(assignment)), Type::void(), loc);
        }
        Ok(node)
    }

    fn lower_local(&mut self, init: &Init, target: Node, out: &mut Vec<Node>) -> Result<(), CcError> {
        let loc = target.loc;
        if let Some(expr) = &init.expr {
            out.push(self.assign_node(target, expr.clone(), loc)?);
            return Ok(());
        }
        match init.ty.kind {
            Kind::Array => {
                for (index, child) in init.children.iter().enumerate() {
                    if !child.has_value() {
                        continue;
                    }
                    let offset = Node::new(NodeKind::Num(index as i64), Type::long(), loc);
                    let element = self.add(target.clone(), offset, loc)?;
                    let element = self.deref(element, loc)?;
                    self.lower_local(child, element, out)?;
                }
            }
            Kind::Struct | Kind::Union => {
                let members = init.ty.record.as_ref().map(|r| r.borrow().members.clone()).unwrap_or_default();
                for (index, child) in init.children.iter().enumerate() {
                    if !child.has_value() || init.member.is_some_and(|m| m != index) {
                        continue;
                    }
                    let member = &members[index];
                    let field = Node::new(NodeKind::Member(boxed(target.clone()), member.offset), member.ty.clone(), loc);
                    self.lower_local(child, field, out)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    // ------------------------------------------------------------
    //  Globals: bytes
    // ------------------------------------------------------------

    /// `= initializer` of a global: its complete type, its bytes, the
    /// pointers to fill at startup.
    pub(super) fn global_initializer(&mut self, ty: Rc<Type>) -> Result<(Rc<Type>, Vec<u8>, Vec<Reloc>), CcError> {
        let (init, ty) = self.initializer(ty)?;
        let mut data = alloc::vec![0u8; ty.size().max(0) as usize];
        let mut relocs = Vec::new();
        self.lower_global(&init, &ty, &mut data, 0, &mut relocs)?;
        Ok((ty, data, relocs))
    }

    fn lower_global(&mut self, init: &Init, ty: &Rc<Type>, data: &mut [u8], offset: usize, relocs: &mut Vec<Reloc>) -> Result<(), CcError> {
        match ty.kind {
            Kind::Array => {
                let size = ty.base().size().max(0) as usize;
                for (index, child) in init.children.iter().enumerate() {
                    if (index + 1) * size > data.len().saturating_sub(offset) && size > 0 {
                        break;
                    }
                    self.lower_global(child, &ty.base(), data, offset + index * size, relocs)?;
                }
                return Ok(());
            }
            Kind::Struct | Kind::Union if init.expr.is_none() => {
                let members = ty.record.as_ref().map(|r| r.borrow().members.clone()).unwrap_or_default();
                for (index, child) in init.children.iter().enumerate() {
                    if init.member.is_some_and(|m| m != index) || (ty.kind == Kind::Union && init.member.is_none() && index > 0) {
                        continue;
                    }
                    let member = &members[index];
                    self.lower_global(child, &member.ty, data, offset + member.offset as usize, relocs)?;
                }
                return Ok(());
            }
            _ => {}
        }
        let Some(expr) = &init.expr else { return Ok(()) };
        if ty.is_record() {
            return Err(CcError::at(expr.loc, "initialisation d'une struct globale par une expression non constante"));
        }
        let size = ty.size() as usize;
        if ty.is_flonum() {
            let value = self.eval_float(expr)?;
            let bits = if ty.kind == Kind::Float { float_bits(round_to_float(value.0)) as u64 } else { value.0 };
            data[offset..offset + size].copy_from_slice(&bits.to_le_bytes()[..size]);
            return Ok(());
        }
        let (value, label) = self.eval_reloc(expr)?;
        match label {
            Some(label) => {
                if size != 8 {
                    return Err(CcError::at(expr.loc, "adresse dans un objet trop petit pour un pointeur"));
                }
                relocs.push(Reloc { offset: offset as i64, label, addend: value });
            }
            None => {
                let value = if ty.kind == Kind::Bool { (value != 0) as i64 } else { value };
                data[offset..offset + size].copy_from_slice(&value.to_le_bytes()[..size]);
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------
    //  Constant expressions
    // ------------------------------------------------------------

    pub(super) fn eval(&self, node: &Node) -> Result<i64, CcError> {
        match self.eval_reloc(node)? {
            (value, None) => Ok(value),
            (_, Some(_)) => Err(CcError::at(node.loc, "expression constante entiere attendue")),
        }
    }

    /// An integer or address constant: a value, plus the label it is an
    /// offset from, if any.
    fn eval_reloc(&self, node: &Node) -> Result<(i64, Option<Rc<str>>), CcError> {
        let not_constant = || CcError::at(node.loc, "expression non constante");
        let wrap = |value: i64| if node.ty.is_integer() { convert_constant(value, &node.ty) } else { value };
        let plain = |n: &Node| -> Result<i64, CcError> { self.eval(n) };
        Ok(match &node.kind {
            NodeKind::Num(value) => (wrap(*value), None),
            NodeKind::Binary(op, left, right) => {
                if matches!(op, Op::Add | Op::Sub) && node.ty.kind == Kind::Ptr || matches!(op, Op::Add | Op::Sub) && node.ty.kind == Kind::Long {
                    let (a, label) = self.eval_reloc(left)?;
                    let (b, right_label) = self.eval_reloc(right)?;
                    if right_label.is_some() {
                        if *op == Op::Add && label.is_none() {
                            return Ok((a.wrapping_add(b), right_label));
                        }
                        return Err(not_constant());
                    }
                    let value = if *op == Op::Add { a.wrapping_add(b) } else { a.wrapping_sub(b) };
                    return Ok((wrap(value), label));
                }
                if left.ty.is_flonum() {
                    let a = self.eval_float(left)?;
                    let b = self.eval_float(right)?;
                    let value = match op {
                        Op::Eq => a.equals(b),
                        Op::Ne => !a.equals(b),
                        Op::Lt => a.lt(b),
                        Op::Le => a.lt(b) || a.equals(b),
                        _ => return Err(not_constant()),
                    };
                    return Ok((value as i64, None));
                }
                let a = plain(left)?;
                let b = plain(right)?;
                let unsigned = left.ty.unsigned || left.ty.kind == Kind::Ptr;
                let value = match op {
                    Op::Add => a.wrapping_add(b),
                    Op::Sub => a.wrapping_sub(b),
                    Op::Mul => a.wrapping_mul(b),
                    Op::Div | Op::Mod => {
                        if b == 0 {
                            return Err(CcError::at(node.loc, "division par zero dans une expression constante"));
                        }
                        match (op, unsigned) {
                            (Op::Div, true) => ((a as u64) / (b as u64)) as i64,
                            (Op::Div, false) => a.wrapping_div(b),
                            (_, true) => ((a as u64) % (b as u64)) as i64,
                            _ => a.wrapping_rem(b),
                        }
                    }
                    Op::BitAnd => a & b,
                    Op::BitOr => a | b,
                    Op::BitXor => a ^ b,
                    Op::Shl => a.wrapping_shl(b as u32),
                    Op::Shr => {
                        if unsigned {
                            let width = (left.ty.size() * 8) as u32;
                            let masked = if width < 64 { (a as u64) & ((1u64 << width) - 1) } else { a as u64 };
                            (masked >> (b as u32 & 63)) as i64
                        } else {
                            a.wrapping_shr(b as u32)
                        }
                    }
                    Op::Eq => (a == b) as i64,
                    Op::Ne => (a != b) as i64,
                    Op::Lt => (if unsigned { (a as u64) < (b as u64) } else { a < b }) as i64,
                    Op::Le => (if unsigned { (a as u64) <= (b as u64) } else { a <= b }) as i64,
                };
                (wrap(value), None)
            }
            NodeKind::Neg(inner) => (wrap(plain(inner)?.wrapping_neg()), None),
            NodeKind::Not(inner) => {
                if inner.ty.is_flonum() {
                    return Ok((self.eval_float(inner)?.is_zero() as i64, None));
                }
                ((plain(inner)? == 0) as i64, None)
            }
            NodeKind::BitNot(inner) => (wrap(!plain(inner)?), None),
            NodeKind::LogAnd(a, b) => ((self.truth(a)? && self.truth(b)?) as i64, None),
            NodeKind::LogOr(a, b) => ((self.truth(a)? || self.truth(b)?) as i64, None),
            NodeKind::Cond(c, a, b) => {
                if self.truth(c)? {
                    self.eval_reloc(a)?
                } else {
                    self.eval_reloc(b)?
                }
            }
            NodeKind::Comma(_, b) => self.eval_reloc(b)?,
            NodeKind::Cast(inner) => {
                if inner.ty.is_flonum() {
                    let value = self.eval_float(inner)?;
                    if node.ty.kind == Kind::Bool {
                        return Ok(((!value.is_zero()) as i64, None));
                    }
                    let value = if node.ty.unsigned && node.ty.size() == 8 && !value.lt(Num::from_u64(1 << 63)) {
                        (value.sub(Num::from_u64(1 << 63)).to_i64_saturating() as u64 | (1 << 63)) as i64
                    } else {
                        value.trunc().to_i64_saturating()
                    };
                    return Ok((wrap(value), None));
                }
                let (value, label) = self.eval_reloc(inner)?;
                if label.is_some() {
                    if node.ty.size() != 8 {
                        return Err(not_constant());
                    }
                    return Ok((value, label));
                }
                if node.ty.kind == Kind::Bool {
                    return Ok(((value != 0) as i64, None));
                }
                (wrap(value), None)
            }
            NodeKind::Addr(inner) => self.eval_addr(inner)?,
            NodeKind::Global(label) if matches!(node.ty.kind, Kind::Array | Kind::Func) => (0, Some(label.clone())),
            NodeKind::Member(..) | NodeKind::Deref(_) if matches!(node.ty.kind, Kind::Array | Kind::Func) => {
                self.eval_addr(node)?
            }
            _ => return Err(not_constant()),
        })
    }

    fn truth(&self, node: &Node) -> Result<bool, CcError> {
        if node.ty.is_flonum() {
            return Ok(!self.eval_float(node)?.is_zero());
        }
        let (value, label) = self.eval_reloc(node)?;
        Ok(value != 0 || label.is_some())
    }

    /// The address of an lvalue, as a constant.
    fn eval_addr(&self, node: &Node) -> Result<(i64, Option<Rc<str>>), CcError> {
        match &node.kind {
            NodeKind::Global(label) => Ok((0, Some(label.clone()))),
            NodeKind::Member(inner, offset) => {
                let (value, label) = self.eval_addr(inner)?;
                Ok((value + offset, label))
            }
            NodeKind::Deref(inner) => self.eval_reloc(inner),
            _ => Err(CcError::at(node.loc, "adresse non constante")),
        }
    }

    pub(super) fn eval_float(&self, node: &Node) -> Result<Num, CcError> {
        let round = |value: Num| if node.ty.kind == Kind::Float { Num(round_to_float(value.0)) } else { value };
        Ok(match &node.kind {
            NodeKind::Float(bits) => Num(*bits),
            NodeKind::Num(value) => {
                if node.ty.unsigned && node.ty.size() == 8 {
                    Num::from_u64(*value as u64)
                } else {
                    Num::from_i64(*value)
                }
            }
            NodeKind::Cast(inner) => {
                if inner.ty.is_flonum() {
                    round(self.eval_float(inner)?)
                } else {
                    let value = self.eval(inner)?;
                    let number = if inner.ty.unsigned && inner.ty.size() == 8 { Num::from_u64(value as u64) } else { Num::from_i64(value) };
                    round(number)
                }
            }
            NodeKind::Binary(op, left, right) => {
                let a = self.eval_float(left)?;
                let b = self.eval_float(right)?;
                round(match op {
                    Op::Add => a.add(b),
                    Op::Sub => a.sub(b),
                    Op::Mul => a.mul(b),
                    Op::Div => a.div(b),
                    _ => return Err(CcError::at(node.loc, "expression flottante non constante")),
                })
            }
            NodeKind::Neg(inner) => self.eval_float(inner)?.neg(),
            NodeKind::Cond(c, a, b) => {
                if self.truth(c)? {
                    self.eval_float(a)?
                } else {
                    self.eval_float(b)?
                }
            }
            NodeKind::Comma(_, b) => self.eval_float(b)?,
            _ => return Err(CcError::at(node.loc, "expression flottante non constante")),
        })
    }
}

impl Init {
    /// Does this subtree initialize anything?
    fn has_value(&self) -> bool {
        self.expr.is_some() || self.children.iter().any(Init::has_value)
    }
}

/// The IEEE single-precision bits of a `double` that holds a `float`
/// value exactly.
pub fn float_bits(bits: u64) -> u32 {
    let sign = ((bits >> 63) as u32) << 31;
    let exponent = ((bits >> 52) & 0x7FF) as i32;
    let fraction = bits & ((1 << 52) - 1);
    if exponent == 0x7FF {
        return sign | 0x7F80_0000 | if fraction != 0 { 0x40_0000 } else { 0 };
    }
    if exponent == 0 && fraction == 0 {
        return sign;
    }
    let e = exponent - 1023;
    if e < -126 {
        // A subnormal float: 2^-149 units.
        let mantissa = fraction | (1 << 52);
        let shift = 52 - (e + 149);
        return sign | if (0..64).contains(&shift) { (mantissa >> shift) as u32 } else { 0 };
    }
    sign | (((e + 127) as u32) << 23) | ((fraction >> 29) as u32)
}
