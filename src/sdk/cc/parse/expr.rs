//! Expressions (C11 6.5), and the typing of each node as it is built.
//!
//! The constructors at the end (`cast`, `arith`, `add`, `sub`, `compare`,
//! `assign_node`) are where C's implicit rules are applied: the integer
//! promotions, the usual arithmetic conversions, pointer arithmetic
//! scaled by the size of the pointed-to type, the conversion of an
//! argument to its parameter. A compound assignment `a op= b` becomes
//! `tmp = &a, *tmp = *tmp op b` so that `a` is evaluated once, and
//! `a++` becomes `(a += 1) - 1` in the type of `a`.

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::vec::Vec;

use super::super::CcError;
use super::super::ast::*;
use super::super::lex::{self, Kind as TokKind, Loc};
use super::super::pp;
use super::super::ty::{self, Kind, Type};
use super::{Binding, Parser};
use crate::web::js::num::Num;

fn boxed(node: Node) -> Box<Node> {
    Box::new(node)
}

impl Parser {
    /// `expr = assign ("," assign)*`
    pub(super) fn expr(&mut self) -> Result<Node, CcError> {
        let mut node = self.assign()?;
        while self.peek_is(",") {
            let loc = self.loc();
            self.pos += 1;
            let right = self.assign()?;
            let ty = right.ty.clone();
            node = Node::new(NodeKind::Comma(boxed(node), boxed(right)), ty, loc);
        }
        Ok(node)
    }

    pub(super) fn assign(&mut self) -> Result<Node, CcError> {
        let left = self.conditional()?;
        let loc = self.loc();
        const COMPOUND: &[(&str, Op)] = &[
            ("+=", Op::Add),
            ("-=", Op::Sub),
            ("*=", Op::Mul),
            ("/=", Op::Div),
            ("%=", Op::Mod),
            ("&=", Op::BitAnd),
            ("|=", Op::BitOr),
            ("^=", Op::BitXor),
            ("<<=", Op::Shl),
            (">>=", Op::Shr),
        ];
        if self.eat("=") {
            let right = self.assign()?;
            return self.assign_node(left, right, loc);
        }
        for (text, op) in COMPOUND {
            if self.eat(text) {
                let right = self.assign()?;
                return self.compound_assign(*op, left, right, loc);
            }
        }
        Ok(left)
    }

    /// `a op= b` as `tmp = &a, *tmp = *tmp op b`.
    fn compound_assign(&mut self, op: Op, left: Node, right: Node, loc: Loc) -> Result<Node, CcError> {
        self.check_lvalue(&left)?;
        let pointer = Type::pointer(left.ty.clone());
        let tmp = self.temporary(pointer.clone());
        let tmp_node = || Node::new(NodeKind::Local(tmp), pointer.clone(), loc);
        let address = self.addr(left, loc)?;
        let first = Node::new(NodeKind::Assign(boxed(tmp_node()), boxed(address)), pointer.clone(), loc);
        let target = self.deref(tmp_node(), loc)?;
        let current = self.deref(tmp_node(), loc)?;
        let value = self.binary_op(op, current, right, loc)?;
        let second = self.assign_node(target, value, loc)?;
        let ty = second.ty.clone();
        Ok(Node::new(NodeKind::Comma(boxed(first), boxed(second)), ty, loc))
    }

    pub(super) fn conditional(&mut self) -> Result<Node, CcError> {
        let condition = self.binary(0)?;
        if !self.peek_is("?") {
            return Ok(condition);
        }
        let loc = self.loc();
        self.pos += 1;
        // `a ?: b` (GNU) is not accepted: it is not C.
        let then = self.expr()?;
        self.expect(":")?;
        let other = self.conditional()?;
        self.check_scalar(&condition)?;
        let (then, other, ty) = self.unify(then, other, loc)?;
        Ok(Node::new(NodeKind::Cond(boxed(condition), boxed(then), boxed(other)), ty, loc))
    }

    /// The type of `c ? a : b`, and the branches converted to it.
    fn unify(&mut self, a: Node, b: Node, loc: Loc) -> Result<(Node, Node, Rc<Type>), CcError> {
        if a.ty.kind == Kind::Void || b.ty.kind == Kind::Void {
            return Ok((a, b, Type::void()));
        }
        if a.ty.is_numeric() && b.ty.is_numeric() {
            let ty = ty::common(&a.ty, &b.ty);
            return Ok((self.cast(a, ty.clone()), self.cast(b, ty.clone()), ty));
        }
        if a.ty.is_record() && b.ty.is_record() {
            let ty = a.ty.clone();
            return Ok((a, b, ty));
        }
        let pointer_of = |n: &Node| match n.ty.kind {
            Kind::Ptr => Some(n.ty.clone()),
            Kind::Array => Some(Type::pointer(n.ty.base())),
            Kind::Func => Some(Type::pointer(n.ty.clone())),
            _ => None,
        };
        let ty = match (pointer_of(&a), pointer_of(&b)) {
            (Some(p), Some(q)) => {
                if p.base().kind == Kind::Void { p } else if q.base().kind == Kind::Void { q } else { p }
            }
            (Some(p), None) if b.ty.is_integer() => p,
            (None, Some(q)) if a.ty.is_integer() => q,
            _ => return Err(CcError::at(loc, "branches de ?: de types incompatibles")),
        };
        Ok((self.cast(a, ty.clone()), self.cast(b, ty.clone()), ty))
    }

    /// Binary operators by precedence climbing, `||` (1) to `*` (10).
    fn binary(&mut self, min: u8) -> Result<Node, CcError> {
        let mut left = self.cast_expr()?;
        loop {
            let token = self.token();
            if token.kind != TokKind::Punct {
                break;
            }
            let precedence = match &*token.text {
                "||" => 1,
                "&&" => 2,
                "|" => 3,
                "^" => 4,
                "&" => 5,
                "==" | "!=" => 6,
                "<" | ">" | "<=" | ">=" => 7,
                "<<" | ">>" => 8,
                "+" | "-" => 9,
                "*" | "/" | "%" => 10,
                _ => break,
            };
            if precedence <= min {
                break;
            }
            let loc = token.loc;
            self.pos += 1;
            let right = self.binary(precedence)?;
            left = match &*token.text {
                "||" | "&&" => {
                    self.check_scalar(&left)?;
                    self.check_scalar(&right)?;
                    let kind = if &*token.text == "||" {
                        NodeKind::LogOr(boxed(left), boxed(right))
                    } else {
                        NodeKind::LogAnd(boxed(left), boxed(right))
                    };
                    Node::new(kind, Type::int(), loc)
                }
                "|" => self.binary_op(Op::BitOr, left, right, loc)?,
                "^" => self.binary_op(Op::BitXor, left, right, loc)?,
                "&" => self.binary_op(Op::BitAnd, left, right, loc)?,
                "==" => self.compare(Op::Eq, left, right, loc)?,
                "!=" => self.compare(Op::Ne, left, right, loc)?,
                "<" => self.compare(Op::Lt, left, right, loc)?,
                "<=" => self.compare(Op::Le, left, right, loc)?,
                ">" => self.compare(Op::Lt, right, left, loc)?,
                ">=" => self.compare(Op::Le, right, left, loc)?,
                "<<" => self.binary_op(Op::Shl, left, right, loc)?,
                ">>" => self.binary_op(Op::Shr, left, right, loc)?,
                "+" => self.binary_op(Op::Add, left, right, loc)?,
                "-" => self.binary_op(Op::Sub, left, right, loc)?,
                "*" => self.binary_op(Op::Mul, left, right, loc)?,
                "/" => self.binary_op(Op::Div, left, right, loc)?,
                _ => self.binary_op(Op::Mod, left, right, loc)?,
            };
        }
        Ok(left)
    }

    /// `cast = "(" typename ")" cast | unary`
    fn cast_expr(&mut self) -> Result<Node, CcError> {
        if self.peek_is("(") && self.is_typename_at(self.pos + 1) {
            let loc = self.loc();
            let start = self.pos;
            self.pos += 1;
            let ty = self.typename()?;
            self.expect(")")?;
            if self.peek_is("{") {
                // A compound literal: back to the postfix grammar.
                self.pos = start;
                return self.unary();
            }
            let operand = self.cast_expr()?;
            if ty.kind != Kind::Void && !ty.is_scalar() {
                return Err(CcError::at(loc, "conversion vers un type non scalaire"));
            }
            if ty.kind != Kind::Void {
                self.check_scalar(&operand)?;
            }
            return Ok(Node { loc, ..self.cast(operand, ty) });
        }
        self.unary()
    }

    fn unary(&mut self) -> Result<Node, CcError> {
        let loc = self.loc();
        if self.eat("+") {
            let operand = self.cast_expr()?;
            self.check_numeric(&operand)?;
            let ty = operand.ty.promoted();
            return Ok(self.cast(operand, ty));
        }
        if self.eat("-") {
            let operand = self.cast_expr()?;
            self.check_numeric(&operand)?;
            let ty = operand.ty.promoted();
            let operand = self.cast(operand, ty.clone());
            return Ok(Node::new(NodeKind::Neg(boxed(operand)), ty, loc));
        }
        if self.eat("!") {
            let operand = self.cast_expr()?;
            self.check_scalar(&operand)?;
            return Ok(Node::new(NodeKind::Not(boxed(operand)), Type::int(), loc));
        }
        if self.eat("~") {
            let operand = self.cast_expr()?;
            if !operand.ty.is_integer() {
                return Err(CcError::at(loc, "~ sur un non-entier"));
            }
            let ty = operand.ty.promoted();
            let operand = self.cast(operand, ty.clone());
            return Ok(Node::new(NodeKind::BitNot(boxed(operand)), ty, loc));
        }
        if self.eat("&") {
            let operand = self.cast_expr()?;
            return self.addr(operand, loc);
        }
        if self.eat("*") {
            let operand = self.cast_expr()?;
            return self.deref(operand, loc);
        }
        if self.eat("++") {
            let operand = self.unary()?;
            let one = self.num(1, loc);
            return self.compound_assign(Op::Add, operand, one, loc);
        }
        if self.eat("--") {
            let operand = self.unary()?;
            let one = self.num(1, loc);
            return self.compound_assign(Op::Sub, operand, one, loc);
        }
        if self.eat("sizeof") {
            let ty = if self.peek_is("(") && self.is_typename_at(self.pos + 1) {
                self.pos += 1;
                let ty = self.typename()?;
                self.expect(")")?;
                if self.peek_is("{") {
                    // `sizeof (T){...}`: the size of a compound literal.
                    self.compound_literal(ty, loc)?.ty
                } else {
                    ty
                }
            } else {
                self.unary()?.ty
            };
            if !ty.is_complete() && !(ty.kind == Kind::Array && ty.len == 0) {
                return Err(CcError::at(loc, "sizeof d'un type incomplet"));
            }
            return Ok(Node::new(NodeKind::Num(ty.size()), Type::ulong(), loc));
        }
        if self.eat("_Alignof") || self.eat("__alignof__") {
            self.expect("(")?;
            let ty = self.typename()?;
            self.expect(")")?;
            return Ok(Node::new(NodeKind::Num(ty.align()), Type::ulong(), loc));
        }
        self.postfix()
    }

    fn postfix(&mut self) -> Result<Node, CcError> {
        // `(type){ ... }`: a compound literal, an unnamed object.
        if self.peek_is("(") && self.is_typename_at(self.pos + 1) {
            let loc = self.loc();
            self.pos += 1;
            let ty = self.typename()?;
            self.expect(")")?;
            let node = self.compound_literal(ty, loc)?;
            return self.postfix_rest(node);
        }
        let node = self.primary()?;
        self.postfix_rest(node)
    }

    fn postfix_rest(&mut self, mut node: Node) -> Result<Node, CcError> {
        loop {
            let loc = self.loc();
            if self.eat("(") {
                node = self.call(node, loc)?;
                continue;
            }
            if self.eat("[") {
                let index = self.expr()?;
                self.expect("]")?;
                let sum = self.add(node, index, loc)?;
                node = self.deref(sum, loc)?;
                continue;
            }
            if self.eat(".") {
                node = self.member(node, loc)?;
                continue;
            }
            if self.eat("->") {
                let target = self.deref(node, loc)?;
                node = self.member(target, loc)?;
                continue;
            }
            if self.eat("++") {
                node = self.post_step(node, Op::Add, loc)?;
                continue;
            }
            if self.eat("--") {
                node = self.post_step(node, Op::Sub, loc)?;
                continue;
            }
            return Ok(node);
        }
    }

    /// `a++` as `(typeof a)((a += 1) - 1)`.
    fn post_step(&mut self, node: Node, op: Op, loc: Loc) -> Result<Node, CcError> {
        let ty = node.ty.clone();
        self.check_scalar(&node)?;
        let one = self.num(1, loc);
        let stepped = self.compound_assign(op, node, one, loc)?;
        let one = self.num(1, loc);
        let back = if op == Op::Add { self.sub(stepped, one, loc)? } else { self.add(stepped, one, loc)? };
        Ok(self.cast(back, ty))
    }

    fn member(&mut self, node: Node, loc: Loc) -> Result<Node, CcError> {
        let name = self.ident()?;
        if !node.ty.is_record() {
            return Err(CcError::at(loc, &alloc::format!("acces au membre {} d'un non-struct", name.text)));
        }
        if !node.ty.is_complete() {
            return Err(CcError::at(loc, "acces a un membre d'un struct incomplet"));
        }
        let Some(member) = node.ty.member(&name.text) else {
            return Err(CcError::at(name.loc, &alloc::format!("aucun membre {}", name.text)));
        };
        Ok(Node::new(NodeKind::Member(boxed(node), member.offset), member.ty, loc))
    }

    fn call(&mut self, callee: Node, loc: Loc) -> Result<Node, CcError> {
        let function = match callee.ty.kind {
            Kind::Func => callee.ty.clone(),
            Kind::Ptr if callee.ty.base().kind == Kind::Func => callee.ty.base(),
            _ => return Err(CcError::at(loc, "appel d'une expression qui n'est pas une fonction")),
        };
        let mut args = Vec::new();
        if !self.eat(")") {
            loop {
                args.push(self.assign()?);
                if self.eat(")") {
                    break;
                }
                self.expect(",")?;
            }
        }
        let params = function.params.clone();
        if function.prototyped {
            if args.len() < params.len() || (args.len() > params.len() && !function.variadic) {
                return Err(CcError::at(
                    loc,
                    &alloc::format!("{} argument(s) pour {} parametre(s)", args.len(), params.len()),
                ));
            }
        }
        let mut converted = Vec::with_capacity(args.len());
        for (index, arg) in args.into_iter().enumerate() {
            if let Some(param) = params.get(index) {
                if param.is_record() {
                    if !arg.ty.same(param) {
                        return Err(CcError::at(arg.loc, "argument de struct de type incompatible"));
                    }
                    converted.push(arg);
                } else {
                    self.check_assignable(param, &arg)?;
                    converted.push(self.cast(arg, param.clone()));
                }
                continue;
            }
            // `...` and unprototyped calls: the default argument
            // promotions (C11 6.5.2.2p6).
            let promoted = match arg.ty.kind {
                Kind::Float => Type::double(),
                Kind::Bool | Kind::Char | Kind::Short => Type::int(),
                _ => arg.ty.clone(),
            };
            converted.push(self.cast(arg, promoted));
        }
        let ret = function.ret();
        let buffer = if ret.is_record() { Some(self.temporary(ret.clone())) } else { None };
        Ok(Node::new(NodeKind::Call { callee: boxed(callee), args: converted, function, buffer }, ret, loc))
    }

    fn primary(&mut self) -> Result<Node, CcError> {
        let token = self.token();
        let loc = token.loc;
        if self.eat("(") {
            if self.peek_is("{") {
                return Err(self.error("les expressions-instructions ({ ... }) ne font pas partie du C de Nova"));
            }
            let node = self.expr()?;
            self.expect(")")?;
            return Ok(node);
        }
        match token.kind {
            TokKind::Number => {
                self.pos += 1;
                self.number(&token.text, loc)
            }
            TokKind::Char => {
                self.pos += 1;
                Ok(Node::new(NodeKind::Num(pp::char_literal(&token)?), Type::int(), loc))
            }
            TokKind::Str => self.string(),
            TokKind::Ident => {
                if &*token.text == "_Generic" {
                    return Err(self.error("_Generic ne fait pas partie du C de Nova"));
                }
                if &*token.text == "__builtin_syscall" {
                    self.pos += 1;
                    return self.syscall(loc);
                }
                if &*token.text == "__builtin_sqrt" {
                    self.pos += 1;
                    self.expect("(")?;
                    let arg = self.assign()?;
                    self.expect(")")?;
                    self.check_numeric(&arg)?;
                    let arg = self.cast(arg, Type::double());
                    return Ok(Node::new(NodeKind::Sqrt(boxed(arg)), Type::double(), loc));
                }
                if &*token.text == "__builtin_va_start" {
                    self.pos += 1;
                    self.expect("(")?;
                    self.expect(")")?;
                    if !self.variadic {
                        return Err(CcError::at(loc, "va_start hors d'une fonction variadique"));
                    }
                    return Ok(Node::new(NodeKind::VaStart, Type::pointer(Type::char(false)), loc));
                }
                if super::is_keyword(&token.text) {
                    return Err(CcError::at(loc, &alloc::format!("expression attendue, \"{}\" trouve", token.text)));
                }
                self.pos += 1;
                match self.lookup(&token.text) {
                    Some(Binding::Local(index)) => {
                        let ty = self.locals[index].ty.clone();
                        Ok(Node::new(NodeKind::Local(index), ty, loc))
                    }
                    Some(Binding::Global(label, ty)) => {
                        if !self.references.contains(&label) {
                            self.references.push(label.clone());
                        }
                        Ok(Node::new(NodeKind::Global(label), ty, loc))
                    }
                    Some(Binding::Enum(value)) => Ok(Node::new(NodeKind::Num(value), Type::int(), loc)),
                    Some(Binding::Typedef(_)) => Err(CcError::at(loc, &alloc::format!("{} est un type", token.text))),
                    None if self.peek_is("(") => {
                        Err(CcError::at(loc, &alloc::format!("fonction {} non declaree (un #include manque ?)", token.text)))
                    }
                    None => Err(CcError::at(loc, &alloc::format!("{} non declare", token.text))),
                }
            }
            _ => Err(CcError::at(loc, &alloc::format!("expression attendue, \"{}\" trouve", token.text))),
        }
    }

    fn syscall(&mut self, loc: Loc) -> Result<Node, CcError> {
        self.expect("(")?;
        let mut args = Vec::new();
        loop {
            let arg = self.assign()?;
            self.check_scalar(&arg)?;
            let arg = if arg.ty.is_flonum() { return Err(CcError::at(loc, "argument flottant a un appel systeme")) } else { arg };
            args.push(arg);
            if self.eat(")") {
                break;
            }
            self.expect(",")?;
        }
        if args.len() > 7 {
            return Err(CcError::at(loc, "un appel systeme prend au plus six arguments"));
        }
        Ok(Node::new(NodeKind::Syscall(args), Type::long(), loc))
    }

    /// Adjacent string literals, concatenated, as an anonymous array.
    fn string(&mut self) -> Result<Node, CcError> {
        let loc = self.loc();
        let mut bytes = Vec::new();
        while self.token().kind == TokKind::Str && !self.at_end() {
            let token = self.token();
            let text = &token.text;
            let quote = text.find('"').unwrap_or(0);
            let prefix = &text[..quote];
            if !prefix.is_empty() && prefix != "u8" {
                return Err(CcError::at(token.loc, "les chaines larges (L\"\", u\"\", U\"\") ne font pas partie du C de Nova"));
            }
            bytes.extend(lex::unescape(&text[quote + 1..text.len() - 1], token.loc)?);
            self.pos += 1;
        }
        bytes.push(0);
        let ty = Type::array(Type::char(false), bytes.len() as i64);
        let label = self.anonymous(ty.clone(), bytes);
        if !self.references.contains(&label) {
            self.references.push(label.clone());
        }
        Ok(Node::new(NodeKind::Global(label), ty, loc))
    }

    /// An integer or floating constant, with the type C gives it.
    fn number(&mut self, text: &str, loc: Loc) -> Result<Node, CcError> {
        let lower = text.to_ascii_lowercase();
        let hex = lower.starts_with("0x");
        let floating = lower.contains('.') || (!hex && lower.contains('e')) || (hex && lower.contains('p'));
        if !floating {
            let Some(literal) = pp::integer_literal(text) else {
                return Err(CcError::at(loc, &alloc::format!("constante invalide : {}", text)));
            };
            let v = literal.value;
            let fits_int = v <= i32::MAX as u64;
            let fits_uint = v <= u32::MAX as u64;
            let fits_long = v <= i64::MAX as u64;
            // C11 6.4.4.1: the first type of the list that holds the value.
            let ty = match (literal.unsigned, literal.longs, literal.decimal) {
                (false, 0, true) if fits_int => Type::int(),
                (false, 0, false) if fits_int => Type::int(),
                (false, 0, false) if fits_uint => Type::uint(),
                (true, 0, _) if fits_uint => Type::uint(),
                (false, _, _) if fits_long => Type::long(),
                _ => Type::ulong(),
            };
            return Ok(Node::new(NodeKind::Num(v as i64), ty, loc));
        }
        let (body, is_float) = match lower.as_bytes().last() {
            Some(b'f') if !hex || lower.contains('p') => (&lower[..lower.len() - 1], true),
            Some(b'l') => return Err(CcError::at(loc, "long double ne fait pas partie du C de Nova")),
            _ => (lower.as_str(), false),
        };
        let value = if hex { hex_float(body) } else { decimal_float(body) };
        let Some(value) = value else {
            return Err(CcError::at(loc, &alloc::format!("constante flottante invalide : {}", text)));
        };
        if is_float {
            return Ok(Node::new(NodeKind::Float(round_to_float(value.0)), Type::float(), loc));
        }
        Ok(Node::new(NodeKind::Float(value.0), Type::double(), loc))
    }

    fn compound_literal(&mut self, ty: Rc<Type>, loc: Loc) -> Result<Node, CcError> {
        if self.scopes.len() == 1 {
            let (ty, data, relocs) = self.global_initializer(ty)?;
            self.unique += 1;
            let label: Rc<str> = Rc::from(alloc::format!("__cc_lit{}", self.unique).as_str());
            let index = self.declare_global(&label, ty.clone(), true);
            self.program.globals[index].data = Some(data);
            self.program.globals[index].relocs = relocs;
            return Ok(Node::new(NodeKind::Global(label), ty, loc));
        }
        let index = self.temporary(ty.clone());
        let init = self.local_initializer(index, loc)?;
        let ty = self.locals[index].ty.clone();
        let var = Node::new(NodeKind::Local(index), ty.clone(), loc);
        Ok(Node::new(NodeKind::Comma(boxed(init), boxed(var)), ty, loc))
    }

    // ------------------------------------------------------------
    //  Typed constructors
    // ------------------------------------------------------------

    pub(super) fn num(&self, value: i64, loc: Loc) -> Node {
        Node::new(NodeKind::Num(value), Type::int(), loc)
    }

    pub(super) fn cast(&self, node: Node, ty: Rc<Type>) -> Node {
        let loc = node.loc;
        Node::new(NodeKind::Cast(boxed(node)), ty, loc)
    }

    fn check_scalar(&self, node: &Node) -> Result<(), CcError> {
        if node.ty.is_scalar() || matches!(node.ty.kind, Kind::Array | Kind::Func) {
            Ok(())
        } else {
            Err(CcError::at(node.loc, "valeur scalaire attendue"))
        }
    }

    fn check_numeric(&self, node: &Node) -> Result<(), CcError> {
        if node.ty.is_numeric() {
            Ok(())
        } else {
            Err(CcError::at(node.loc, "valeur numerique attendue"))
        }
    }

    pub(super) fn check_lvalue(&self, node: &Node) -> Result<(), CcError> {
        match node.kind {
            NodeKind::Local(_) | NodeKind::Global(_) | NodeKind::Deref(_) | NodeKind::Member(..) => Ok(()),
            _ => Err(CcError::at(node.loc, "cette expression n'est pas affectable")),
        }
    }

    /// Can `value` be assigned to an object of type `to`? (C11 6.5.16.1,
    /// leniently: pointer mismatches are accepted, as GCC warns.)
    fn check_assignable(&self, to: &Rc<Type>, value: &Node) -> Result<(), CcError> {
        let from = &value.ty;
        let ok = match to.kind {
            Kind::Bool => from.is_scalar() || matches!(from.kind, Kind::Array | Kind::Func),
            _ if to.is_numeric() => from.is_numeric() || (to.is_integer() && from.kind == Kind::Ptr),
            Kind::Ptr => from.is_integer() || matches!(from.kind, Kind::Ptr | Kind::Array | Kind::Func),
            Kind::Struct | Kind::Union => from.same(to),
            _ => false,
        };
        if ok { Ok(()) } else { Err(CcError::at(value.loc, "affectation entre types incompatibles")) }
    }

    pub(super) fn assign_node(&mut self, left: Node, right: Node, loc: Loc) -> Result<Node, CcError> {
        self.check_lvalue(&left)?;
        if left.ty.kind == Kind::Array {
            return Err(CcError::at(loc, "affectation a un tableau"));
        }
        self.check_assignable(&left.ty, &right)?;
        let ty = left.ty.clone();
        let right = if ty.is_record() { right } else { self.cast(right, ty.clone()) };
        Ok(Node::new(NodeKind::Assign(boxed(left), boxed(right)), ty, loc))
    }

    pub(super) fn deref(&self, node: Node, loc: Loc) -> Result<Node, CcError> {
        let ty = match node.ty.kind {
            Kind::Ptr | Kind::Array => node.ty.base(),
            Kind::Func => return Ok(node),
            _ => return Err(CcError::at(loc, "dereferencement d'un non-pointeur")),
        };
        if ty.kind == Kind::Void {
            return Err(CcError::at(loc, "dereferencement d'un void *"));
        }
        Ok(Node::new(NodeKind::Deref(boxed(node)), ty, loc))
    }

    pub(super) fn addr(&self, node: Node, loc: Loc) -> Result<Node, CcError> {
        match node.kind {
            NodeKind::Local(_) | NodeKind::Global(_) | NodeKind::Member(..) => {}
            // `&*p` is `p`, typed as a pointer.
            NodeKind::Deref(inner) => {
                let ty = Type::pointer(node.ty.clone());
                return Ok(Node::new(NodeKind::Cast(inner), ty, loc));
            }
            NodeKind::Comma(..) if node.ty.is_record() || node.ty.kind == Kind::Array => {}
            _ => return Err(CcError::at(loc, "adresse d'une expression qui n'est pas un objet")),
        }
        let ty = Type::pointer(node.ty.clone());
        Ok(Node::new(NodeKind::Addr(boxed(node)), ty, loc))
    }

    pub(super) fn binary_op(&mut self, op: Op, left: Node, right: Node, loc: Loc) -> Result<Node, CcError> {
        match op {
            Op::Add => self.add(left, right, loc),
            Op::Sub => self.sub(left, right, loc),
            Op::Shl | Op::Shr => {
                if !left.ty.is_integer() || !right.ty.is_integer() {
                    return Err(CcError::at(loc, "decalage d'un non-entier"));
                }
                let ty = left.ty.promoted();
                let left = self.cast(left, ty.clone());
                let right_ty = right.ty.promoted();
                let right = self.cast(right, right_ty);
                Ok(Node::new(NodeKind::Binary(op, boxed(left), boxed(right)), ty, loc))
            }
            Op::Mod | Op::BitAnd | Op::BitOr | Op::BitXor => {
                if !left.ty.is_integer() || !right.ty.is_integer() {
                    return Err(CcError::at(loc, "operation entiere sur un non-entier"));
                }
                self.arith(op, left, right, loc)
            }
            _ => {
                self.check_numeric(&left)?;
                self.check_numeric(&right)?;
                self.arith(op, left, right, loc)
            }
        }
    }

    fn arith(&mut self, op: Op, left: Node, right: Node, loc: Loc) -> Result<Node, CcError> {
        let ty = ty::common(&left.ty, &right.ty);
        let left = self.cast(left, ty.clone());
        let right = self.cast(right, ty.clone());
        Ok(Node::new(NodeKind::Binary(op, boxed(left), boxed(right)), ty, loc))
    }

    pub(super) fn compare(&mut self, op: Op, left: Node, right: Node, loc: Loc) -> Result<Node, CcError> {
        if left.ty.is_numeric() && right.ty.is_numeric() {
            let ty = ty::common(&left.ty, &right.ty);
            let left = self.cast(left, ty.clone());
            let right = self.cast(right, ty);
            return Ok(Node::new(NodeKind::Binary(op, boxed(left), boxed(right)), Type::int(), loc));
        }
        let pointerish = |n: &Node| matches!(n.ty.kind, Kind::Ptr | Kind::Array | Kind::Func);
        if !(pointerish(&left) || left.ty.is_integer()) || !(pointerish(&right) || right.ty.is_integer()) {
            return Err(CcError::at(loc, "comparaison entre types incompatibles"));
        }
        let ty = Type::pointer(Type::void());
        let left = self.cast(left, ty.clone());
        let right = self.cast(right, ty);
        Ok(Node::new(NodeKind::Binary(op, boxed(left), boxed(right)), Type::int(), loc))
    }

    pub(super) fn add(&mut self, left: Node, right: Node, loc: Loc) -> Result<Node, CcError> {
        if left.ty.is_numeric() && right.ty.is_numeric() {
            return self.arith(Op::Add, left, right, loc);
        }
        let (pointer, index) = if left.ty.is_pointer_like() {
            (left, right)
        } else if right.ty.is_pointer_like() {
            (right, left)
        } else {
            return Err(CcError::at(loc, "addition invalide"));
        };
        if !index.ty.is_integer() {
            return Err(CcError::at(loc, "addition de deux pointeurs"));
        }
        let base = pointer.ty.base();
        if !base.is_complete() && base.kind != Kind::Void {
            return Err(CcError::at(loc, "arithmetique sur un pointeur vers un type incomplet"));
        }
        let ty = Type::pointer(base.clone());
        let scaled = self.scale(index, base.size(), loc);
        Ok(Node::new(NodeKind::Binary(Op::Add, boxed(pointer), boxed(scaled)), ty, loc))
    }

    pub(super) fn sub(&mut self, left: Node, right: Node, loc: Loc) -> Result<Node, CcError> {
        if left.ty.is_numeric() && right.ty.is_numeric() {
            return self.arith(Op::Sub, left, right, loc);
        }
        if left.ty.is_pointer_like() && right.ty.is_integer() {
            let base = left.ty.base();
            let ty = Type::pointer(base.clone());
            let scaled = self.scale(right, base.size(), loc);
            return Ok(Node::new(NodeKind::Binary(Op::Sub, boxed(left), boxed(scaled)), ty, loc));
        }
        if left.ty.is_pointer_like() && right.ty.is_pointer_like() {
            // `p - q`: the distance in elements, a `long` (ptrdiff_t).
            let size = left.ty.base().size().max(1);
            let difference = Node::new(NodeKind::Binary(Op::Sub, boxed(left), boxed(right)), Type::long(), loc);
            let size = Node::new(NodeKind::Num(size), Type::long(), loc);
            return Ok(Node::new(NodeKind::Binary(Op::Div, boxed(difference), boxed(size)), Type::long(), loc));
        }
        Err(CcError::at(loc, "soustraction invalide"))
    }

    /// `index * size`, as a `long`.
    fn scale(&self, index: Node, size: i64, loc: Loc) -> Node {
        let index = self.cast(index, Type::long());
        if size == 1 {
            return index;
        }
        let size = Node::new(NodeKind::Num(size), Type::long(), loc);
        Node::new(NodeKind::Binary(Op::Mul, boxed(index), boxed(size)), Type::long(), loc)
    }
}

/// A decimal floating constant, correctly rounded.
fn decimal_float(text: &str) -> Option<Num> {
    if !text.bytes().all(|b| b.is_ascii_digit() || matches!(b, b'.' | b'e' | b'+' | b'-')) {
        return None;
    }
    let (mantissa, exponent) = match text.find('e') {
        Some(index) => (&text[..index], text[index + 1..].parse::<i32>().ok()?),
        None => (text, 0),
    };
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    if whole.is_empty() && fraction.is_empty() {
        return None;
    }
    let digits: Vec<u8> = whole.bytes().chain(fraction.bytes()).collect();
    let exp = exponent.checked_sub(fraction.len() as i32)?;
    Some(Num::from_decimal(&digits, exp))
}

/// A hexadecimal floating constant (`0x1.8p3`), exact up to rounding.
fn hex_float(text: &str) -> Option<Num> {
    let body = text.strip_prefix("0x")?;
    let (mantissa, exponent) = body.split_once('p')?;
    let exponent: i32 = exponent.parse().ok()?;
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let mut value: u128 = 0;
    let mut shift = 0i32;
    for c in whole.chars().chain(fraction.chars()) {
        let digit = c.to_digit(16)? as u128;
        if value >> 120 == 0 {
            value = value * 16 + digit;
        } else {
            // Beyond 120 bits a digit only matters as a sticky bit.
            shift += 4;
            value |= u128::from(digit != 0);
        }
    }
    let e = exponent - 4 * fraction.len() as i32 + shift;
    if value == 0 {
        return Some(Num::ZERO);
    }
    Some(Num::from_parts(false, e, value))
}

/// Rounds a `double` (bits) to the nearest `float`, ties to even, and
/// gives it back as a `double`: the representation of `float` values in
/// registers (see `gen`).
pub fn round_to_float(bits: u64) -> u64 {
    let sign = bits & (1 << 63);
    let exponent = ((bits >> 52) & 0x7FF) as i32;
    let fraction = bits & ((1 << 52) - 1);
    if exponent == 0x7FF || (exponent == 0 && fraction == 0) {
        return bits;
    }
    // value = mantissa * 2^(e), mantissa with its implicit bit.
    let (mantissa, e) = if exponent == 0 { (fraction, -1074) } else { (fraction | (1 << 52), exponent - 1075) };
    // Bits a float keeps: 24, fewer below 2^-126.
    let top = 63 - mantissa.leading_zeros() as i32 + e; // exponent of the leading bit
    let keep = if top >= -126 { 24 } else { 24 - (-126 - top) };
    if keep <= 0 {
        // Below half the smallest subnormal float: rounds to zero, or
        // up to it when above half.
        if keep == 0 && (mantissa << mantissa.leading_zeros()) > (1 << 63) {
            return sign | (Num::from_parts(false, -149, 1).0);
        }
        return sign;
    }
    let length = 64 - mantissa.leading_zeros() as i32;
    let drop = length - keep;
    let rounded = if drop > 0 {
        let half = 1u64 << (drop - 1);
        let rest = mantissa & ((1u64 << drop) - 1);
        let mut kept = mantissa >> drop;
        if rest > half || (rest == half && kept & 1 == 1) {
            kept += 1;
        }
        Num::from_parts(false, e + drop, kept as u128).0
    } else {
        bits & !(1 << 63)
    };
    // Beyond the largest float: infinity.
    if rounded >= 0x47EF_FFFF_F000_0000 + 0x1000_0000 {
        return sign | 0x7FF0_0000_0000_0000;
    }
    sign | rounded
}
