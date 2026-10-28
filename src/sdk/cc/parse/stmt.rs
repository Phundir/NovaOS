//! Statements (C11 6.8) and the declarations inside a block.
//!
//! Control flow is reduced to labels and jumps the generator numbers:
//! `break` and `continue` are jumps to the labels of the enclosing loop
//! or `switch`, a `case` is a label the `switch` jumps to, a named
//! `goto` target gets a number the first time it is seen, defined or not.

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::vec::Vec;

use super::super::CcError;
use super::super::ast::*;
use super::super::lex::Kind as TokKind;
use super::super::ty::{Kind, Type};
use super::{Binding, Parser, Storage, SwitchState, external_label};

fn boxed(node: Node) -> Box<Node> {
    Box::new(node)
}

/// `value` converted to the integer type `ty`, as a `case` label is to
/// the controlling expression's type.
pub fn convert_constant(value: i64, ty: &Type) -> i64 {
    match (ty.size(), ty.unsigned) {
        (1, false) => value as i8 as i64,
        (1, true) => value as u8 as i64,
        (2, false) => value as i16 as i64,
        (2, true) => value as u16 as i64,
        (4, false) => value as i32 as i64,
        (4, true) => value as u32 as i64,
        _ => value,
    }
}

impl Parser {
    /// `{ block-item* }`, the `{` not yet read.
    pub(super) fn compound(&mut self) -> Result<Node, CcError> {
        let loc = self.loc();
        self.expect("{")?;
        self.enter();
        let mut items = Vec::new();
        while !self.eat("}") {
            if self.at_end() {
                self.leave();
                return Err(CcError::at(loc, "bloc { jamais ferme"));
            }
            if self.peek_is("_Static_assert") {
                self.static_assert()?;
                continue;
            }
            // `x: ...` is a label even when x names a type.
            if self.is_typename() && !self.peek_at(1, ":") {
                items.extend(self.local_declaration()?);
            } else {
                items.push(self.statement()?);
            }
        }
        self.leave();
        Ok(Node::statement(NodeKind::Block(items), loc))
    }

    fn local_declaration(&mut self) -> Result<Vec<Node>, CcError> {
        let mut storage = Storage::default();
        let base = self.declspec(Some(&mut storage))?;
        if storage.typedef {
            self.typedef(base)?;
            return Ok(Vec::new());
        }
        let mut nodes = Vec::new();
        if self.eat(";") {
            return Ok(nodes);
        }
        loop {
            let declarator = self.declarator(base.clone())?;
            let Some(name) = declarator.name else {
                return Err(self.error("declaration sans nom"));
            };
            let ty = declarator.ty;
            if ty.kind == Kind::Void {
                return Err(CcError::at(name.loc, "variable de type void"));
            }
            if ty.kind == Kind::Func {
                // A function declared inside a block.
                let label = external_label(&name.text);
                self.bind(&name.text, Binding::Global(label.clone(), ty.clone()));
                self.declare_global(&label, ty, false);
            } else if storage.is_extern {
                let label = match self.scopes[0].names.get(&name.text) {
                    Some(Binding::Global(label, _)) => label.clone(),
                    _ => external_label(&name.text),
                };
                self.bind(&name.text, Binding::Global(label.clone(), ty.clone()));
                self.declare_global(&label, ty, false);
            } else if storage.is_static {
                // A static local is a global with a label of its own.
                self.unique += 1;
                let label: Rc<str> = Rc::from(alloc::format!("__cc_s{}_{}", self.unique, name.text).as_str());
                self.bind(&name.text, Binding::Global(label.clone(), ty.clone()));
                let index = self.declare_global(&label, ty.clone(), true);
                if self.eat("=") {
                    let (ty, data, relocs) = self.global_initializer(ty)?;
                    let global = &mut self.program.globals[index];
                    global.ty = ty.clone();
                    global.data = Some(data);
                    global.relocs = relocs;
                    self.bind(&name.text, Binding::Global(label.clone(), ty));
                } else if !ty.is_complete() {
                    return Err(CcError::at(name.loc, "variable de type incomplet"));
                }
                if !self.references.contains(&label) {
                    self.references.push(label);
                }
            } else {
                let index = self.new_local(&name.text, ty.clone());
                if self.eat("=") {
                    let init = self.local_initializer(index, name.loc)?;
                    nodes.push(Node::statement(NodeKind::Expr(boxed(init)), name.loc));
                } else if !ty.is_complete() {
                    return Err(CcError::at(name.loc, "variable de type incomplet"));
                }
            }
            if !self.eat(",") {
                break;
            }
        }
        self.expect(";")?;
        Ok(nodes)
    }

    fn statement(&mut self) -> Result<Node, CcError> {
        let loc = self.loc();
        let token = self.token();

        if token.kind == TokKind::Ident && self.peek_at(1, ":") && !super::is_keyword(&token.text) {
            self.pos += 2;
            if self.goto_defined.contains(&token.text) {
                return Err(CcError::at(loc, &alloc::format!("etiquette {} definie deux fois", token.text)));
            }
            self.goto_defined.push(token.text.clone());
            let id = self.goto_label(&token.text);
            let body = if self.peek_is("}") { Node::statement(NodeKind::Null, loc) } else { self.statement()? };
            return Ok(Node::statement(NodeKind::Label(id, boxed(body)), loc));
        }

        match &*token.text {
            "return" if token.kind == TokKind::Ident => {
                self.pos += 1;
                if self.eat(";") {
                    return Ok(Node::statement(NodeKind::Return(None), loc));
                }
                let value = self.expr()?;
                self.expect(";")?;
                let ret = self.returns.clone().unwrap_or_else(Type::void);
                if ret.kind == Kind::Void {
                    return Ok(Node::statement(NodeKind::Block(alloc::vec![
                        Node::statement(NodeKind::Expr(boxed(value)), loc),
                        Node::statement(NodeKind::Return(None), loc),
                    ]), loc));
                }
                let value = if ret.is_record() {
                    if !value.ty.same(&ret) {
                        return Err(CcError::at(loc, "return d'une struct de type incompatible"));
                    }
                    value
                } else {
                    if !value.ty.is_scalar() && !matches!(value.ty.kind, Kind::Array | Kind::Func) {
                        return Err(CcError::at(loc, "valeur de retour incompatible"));
                    }
                    self.cast(value, ret)
                };
                Ok(Node::statement(NodeKind::Return(Some(boxed(value))), loc))
            }
            "if" => {
                self.pos += 1;
                self.expect("(")?;
                let condition = self.expr()?;
                self.expect(")")?;
                let then = self.statement()?;
                let other = if self.eat("else") { Some(boxed(self.statement()?)) } else { None };
                Ok(Node::statement(NodeKind::If(boxed(condition), boxed(then), other), loc))
            }
            "while" => {
                self.pos += 1;
                self.expect("(")?;
                let condition = self.expr()?;
                self.expect(")")?;
                let (exit, next) = (self.new_label(), self.new_label());
                let body = self.loop_body(exit, next)?;
                Ok(Node::statement(
                    NodeKind::Loop { init: None, cond: Some(boxed(condition)), step: None, body: boxed(body), exit, next },
                    loc,
                ))
            }
            "do" => {
                self.pos += 1;
                let (exit, next) = (self.new_label(), self.new_label());
                let body = self.loop_body(exit, next)?;
                self.expect("while")?;
                self.expect("(")?;
                let condition = self.expr()?;
                self.expect(")")?;
                self.expect(";")?;
                Ok(Node::statement(NodeKind::DoWhile { body: boxed(body), cond: boxed(condition), exit, next }, loc))
            }
            "for" => {
                self.pos += 1;
                self.expect("(")?;
                self.enter();
                let init = if self.eat(";") {
                    None
                } else if self.is_typename() {
                    let nodes = self.local_declaration()?;
                    Some(boxed(Node::statement(NodeKind::Block(nodes), loc)))
                } else {
                    let expr = self.expr()?;
                    self.expect(";")?;
                    Some(boxed(Node::statement(NodeKind::Expr(boxed(expr)), loc)))
                };
                let condition = if self.peek_is(";") { None } else { Some(boxed(self.expr()?)) };
                self.expect(";")?;
                let step = if self.peek_is(")") { None } else { Some(boxed(self.expr()?)) };
                self.expect(")")?;
                let (exit, next) = (self.new_label(), self.new_label());
                let body = self.loop_body(exit, next)?;
                self.leave();
                Ok(Node::statement(NodeKind::Loop { init, cond: condition, step, body: boxed(body), exit, next }, loc))
            }
            "switch" => {
                self.pos += 1;
                self.expect("(")?;
                let condition = self.expr()?;
                self.expect(")")?;
                if !condition.ty.is_integer() {
                    return Err(CcError::at(loc, "switch sur une valeur non entiere"));
                }
                let ty = condition.ty.promoted();
                let condition = self.cast(condition, ty.clone());
                let exit = self.new_label();
                let outer_switch = self.switch.replace(SwitchState { cases: Vec::new(), default: None, ty });
                let outer_exit = self.exit.replace(exit);
                let body = self.statement();
                self.exit = outer_exit;
                let state = core::mem::replace(&mut self.switch, outer_switch);
                let body = body?;
                let state = state.unwrap_or_else(|| unreachable!());
                Ok(Node::statement(
                    NodeKind::Switch { cond: boxed(condition), body: boxed(body), cases: state.cases, default: state.default, exit },
                    loc,
                ))
            }
            "case" => {
                self.pos += 1;
                let value = self.const_expr()?;
                if self.eat("...") {
                    return Err(self.error("les intervalles de case (a ... b) ne font pas partie du C de Nova"));
                }
                self.expect(":")?;
                let label = self.new_label();
                let Some(state) = self.switch.as_mut() else {
                    return Err(CcError::at(loc, "case hors d'un switch"));
                };
                let value = convert_constant(value, &state.ty.clone());
                if state.cases.iter().any(|(v, _)| *v == value) {
                    return Err(CcError::at(loc, &alloc::format!("case {} en double", value)));
                }
                state.cases.push((value, label));
                let body = if self.peek_is("}") { Node::statement(NodeKind::Null, loc) } else { self.statement()? };
                Ok(Node::statement(NodeKind::Label(label, boxed(body)), loc))
            }
            "default" => {
                self.pos += 1;
                self.expect(":")?;
                let label = self.new_label();
                let Some(state) = self.switch.as_mut() else {
                    return Err(CcError::at(loc, "default hors d'un switch"));
                };
                if state.default.is_some() {
                    return Err(CcError::at(loc, "default en double"));
                }
                state.default = Some(label);
                let body = if self.peek_is("}") { Node::statement(NodeKind::Null, loc) } else { self.statement()? };
                Ok(Node::statement(NodeKind::Label(label, boxed(body)), loc))
            }
            "break" => {
                self.pos += 1;
                self.expect(";")?;
                match self.exit {
                    Some(label) => Ok(Node::statement(NodeKind::Goto(label), loc)),
                    None => Err(CcError::at(loc, "break hors d'une boucle ou d'un switch")),
                }
            }
            "continue" => {
                self.pos += 1;
                self.expect(";")?;
                match self.next {
                    Some(label) => Ok(Node::statement(NodeKind::Goto(label), loc)),
                    None => Err(CcError::at(loc, "continue hors d'une boucle")),
                }
            }
            "goto" => {
                self.pos += 1;
                if self.peek_is("*") {
                    return Err(self.error("goto calcule (goto *p) ne fait pas partie du C de Nova"));
                }
                let name = self.ident()?;
                self.expect(";")?;
                let id = self.goto_label(&name.text);
                self.goto_uses.push((name.text.clone(), name.loc));
                Ok(Node::statement(NodeKind::Goto(id), loc))
            }
            "asm" | "__asm__" | "__asm" => Err(self.error("l'assembleur en ligne ne fait pas partie du C de Nova")),
            "{" => self.compound(),
            ";" => {
                self.pos += 1;
                Ok(Node::statement(NodeKind::Null, loc))
            }
            _ => {
                let expr = self.expr()?;
                self.expect(";")?;
                Ok(Node::statement(NodeKind::Expr(boxed(expr)), loc))
            }
        }
    }

    /// The body of a loop, with `break` and `continue` pointing at it.
    fn loop_body(&mut self, exit: u32, next: u32) -> Result<Node, CcError> {
        let outer_exit = self.exit.replace(exit);
        let outer_next = self.next.replace(next);
        let body = self.statement();
        self.exit = outer_exit;
        self.next = outer_next;
        body
    }

    /// The number of a named label of the function, made on first sight.
    fn goto_label(&mut self, name: &Rc<str>) -> u32 {
        if let Some(id) = self.goto_targets.get(name) {
            return *id;
        }
        let id = self.new_label();
        self.goto_targets.insert(name.clone(), id);
        id
    }
}
