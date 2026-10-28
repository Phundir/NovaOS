//! Statements and binding patterns.

use alloc::boxed::Box;
use alloc::vec::Vec;

use super::Parser;
use super::super::ast::*;
use super::super::lex::Tok;
use super::{is_reserved, PResult};
use super::super::value::JsStr;

impl<'a> Parser<'a> {
    // --------------------------------------------------------
    //  Statements
    // --------------------------------------------------------

    pub(super) fn statement(&mut self) -> PResult<Stmt> {
        self.enter()?;
        let result = self.statement_inner();
        self.leave();
        result
    }

    pub(super) fn statement_inner(&mut self) -> PResult<Stmt> {
        if let Some(statement) = self.ts_declaration()? {
            return Ok(statement);
        }
        match self.peek().clone() {
            Tok::Punct("@") => self.error("les decorateurs ne sont pas pris en charge"),
            Tok::Punct("{") => {
                self.advance();
                Ok(Stmt::Block(self.block_body()?))
            }
            Tok::Punct(";") => {
                self.advance();
                Ok(Stmt::Empty)
            }
            Tok::Ident(word) => match word.as_str() {
                "var" => {
                    self.advance();
                    let declarations = self.declarations(VarKind::Var)?;
                    self.semicolon()?;
                    Ok(Stmt::Var(VarKind::Var, declarations))
                }
                "const" => {
                    self.advance();
                    let declarations = self.declarations(VarKind::Const)?;
                    self.semicolon()?;
                    Ok(Stmt::Var(VarKind::Const, declarations))
                }
                "let" if matches!(self.peek_at(1), Tok::Ident(_) | Tok::Punct("[") | Tok::Punct("{")) => {
                    self.advance();
                    let declarations = self.declarations(VarKind::Let)?;
                    self.semicolon()?;
                    Ok(Stmt::Var(VarKind::Let, declarations))
                }
                "function" => {
                    let func = self.function(false, true)?;
                    if self.take_signature() {
                        return Ok(Stmt::Empty);
                    }
                    if let Some(name) = &func.name {
                        let name = name.clone();
                        self.declare_var(&name);
                    }
                    Ok(Stmt::Function(func))
                }
                "async" if self.word_at(1, "function") && !self.tokens[self.pos + 1].newline => {
                    self.advance();
                    let func = self.function(true, true)?;
                    if self.take_signature() {
                        return Ok(Stmt::Empty);
                    }
                    if let Some(name) = &func.name {
                        let name = name.clone();
                        self.declare_var(&name);
                    }
                    Ok(Stmt::Function(func))
                }
                "class" => {
                    let class = self.class(true)?;
                    let name = class.name.clone().unwrap_or_else(|| JsStr::from("default"));
                    Ok(Stmt::Class(name, class))
                }
                "if" => {
                    self.advance();
                    self.expect("(")?;
                    let test = self.expression()?;
                    self.expect(")")?;
                    let then = Box::new(self.statement()?);
                    let otherwise = if self.eat_word("else") { Some(Box::new(self.statement()?)) } else { None };
                    Ok(Stmt::If(test, then, otherwise))
                }
                "for" => self.for_statement(),
                "while" => {
                    self.advance();
                    self.expect("(")?;
                    let test = self.expression()?;
                    self.expect(")")?;
                    Ok(Stmt::While(test, Box::new(self.statement()?)))
                }
                "do" => {
                    self.advance();
                    let body = Box::new(self.statement()?);
                    if !self.eat_word("while") {
                        return self.error("\"while\" attendu");
                    }
                    self.expect("(")?;
                    let test = self.expression()?;
                    self.expect(")")?;
                    self.eat(";");
                    Ok(Stmt::DoWhile(body, test))
                }
                "return" => {
                    self.advance();
                    let value = if self.is(";") || self.is("}") || self.newline_before() || matches!(self.peek(), Tok::Eof) {
                        None
                    } else {
                        Some(self.expression()?)
                    };
                    self.semicolon()?;
                    Ok(Stmt::Return(value))
                }
                "break" | "continue" => {
                    self.advance();
                    let label = match self.peek().clone() {
                        Tok::Ident(name) if !self.newline_before() && !is_reserved(&name) => {
                            self.advance();
                            Some(self.intern(&name))
                        }
                        _ => None,
                    };
                    self.semicolon()?;
                    Ok(if word.as_str() == "break" { Stmt::Break(label) } else { Stmt::Continue(label) })
                }
                "throw" => {
                    self.advance();
                    if self.newline_before() {
                        return self.error("retour a la ligne apres throw");
                    }
                    let value = self.expression()?;
                    self.semicolon()?;
                    Ok(Stmt::Throw(value))
                }
                "try" => self.try_statement(),
                "switch" => self.switch_statement(),
                "debugger" => {
                    self.advance();
                    self.semicolon()?;
                    Ok(Stmt::Empty)
                }
                "with" => self.error("\"with\" n'est pas pris en charge"),
                "import" if !self.punct_at(1, "(") && !self.punct_at(1, ".") => {
                    self.error("\"import\" hors d'un module")
                }
                "export" => self.error("\"export\" hors d'un module"),
                _ if self.punct_at(1, ":") && !is_reserved(&word) => {
                    self.advance();
                    self.advance();
                    let label = self.intern(&word);
                    // A function declaration may be labeled (Annex B).
                    Ok(Stmt::Labeled(label, Box::new(self.statement()?)))
                }
                _ => self.expression_statement(),
            },
            _ => self.expression_statement(),
        }
    }

    pub(super) fn expression_statement(&mut self) -> PResult<Stmt> {
        let expr = self.expression()?;
        self.semicolon()?;
        Ok(Stmt::Expr(expr))
    }

    pub(super) fn block_body(&mut self) -> PResult<Vec<Stmt>> {
        let mut body = Vec::new();
        while !self.eat("}") {
            if matches!(self.peek(), Tok::Eof) {
                return self.error("\"}\" attendu");
            }
            body.push(self.statement()?);
        }
        Ok(body)
    }

    pub(super) fn declarations(&mut self, kind: VarKind) -> PResult<Vec<(Pattern, Option<Expr>)>> {
        let mut out = Vec::new();
        loop {
            let target = self.binding_target()?;
            if kind == VarKind::Var {
                self.declare_pattern_vars(&target);
            }
            if self.options.typescript {
                self.eat("!");
                self.skip_type_annotation()?;
            }
            let init = if self.eat("=") { Some(self.assignment()?) } else { None };
            out.push((target, init));
            if !self.eat(",") {
                return Ok(out);
            }
        }
    }

    pub(super) fn for_statement(&mut self) -> PResult<Stmt> {
        self.advance();
        self.eat_word("await");
        self.expect("(")?;
        let kind = if self.is_word("var") {
            Some(VarKind::Var)
        } else if self.is_word("const") {
            Some(VarKind::Const)
        } else if self.is_word("let") && matches!(self.peek_at(1), Tok::Ident(_) | Tok::Punct("[") | Tok::Punct("{")) {
            Some(VarKind::Let)
        } else {
            None
        };
        let init = if self.is(";") {
            None
        } else if let Some(kind) = kind {
            self.advance();
            let target = self.binding_target()?;
            if kind == VarKind::Var {
                self.declare_pattern_vars(&target);
            }
            self.skip_type_annotation()?;
            if self.eat_word("of") {
                let object = self.assignment()?;
                self.expect(")")?;
                return Ok(Stmt::ForOf(Some(kind), target, object, Box::new(self.statement()?)));
            }
            if self.eat_word("in") {
                let object = self.expression()?;
                self.expect(")")?;
                return Ok(Stmt::ForIn(Some(kind), target, object, Box::new(self.statement()?)));
            }
            let saved = self.no_in;
            self.no_in = true;
            let first_init = if self.eat("=") { Some(self.assignment()?) } else { None };
            let mut declarations = alloc::vec![(target, first_init)];
            while self.eat(",") {
                let target = self.binding_target()?;
                if kind == VarKind::Var {
                    self.declare_pattern_vars(&target);
                }
                self.skip_type_annotation()?;
                let init = if self.eat("=") { Some(self.assignment()?) } else { None };
                declarations.push((target, init));
            }
            self.no_in = saved;
            Some(ForInit::Var(kind, declarations))
        } else {
            let saved = self.no_in;
            self.no_in = true;
            let expr = self.left_hand_or_expression()?;
            self.no_in = saved;
            if self.eat_word("of") {
                let target = self.to_pattern(expr)?;
                let object = self.assignment()?;
                self.expect(")")?;
                return Ok(Stmt::ForOf(None, target, object, Box::new(self.statement()?)));
            }
            if self.eat_word("in") {
                let target = self.to_pattern(expr)?;
                let object = self.expression()?;
                self.expect(")")?;
                return Ok(Stmt::ForIn(None, target, object, Box::new(self.statement()?)));
            }
            Some(ForInit::Expr(expr))
        };
        self.expect(";")?;
        let test = if self.is(";") { None } else { Some(self.expression()?) };
        self.expect(";")?;
        let update = if self.is(")") { None } else { Some(self.expression()?) };
        self.expect(")")?;
        Ok(Stmt::For(init, test, update, Box::new(self.statement()?)))
    }

    /// In a `for` head without declaration: an expression, parsed so that
    /// `in` is not taken as the operator.
    pub(super) fn left_hand_or_expression(&mut self) -> PResult<Expr> {
        self.expression()
    }

    pub(super) fn try_statement(&mut self) -> PResult<Stmt> {
        self.advance();
        self.expect("{")?;
        let block = self.block_body()?;
        let catch = if self.eat_word("catch") {
            let binding = if self.eat("(") {
                let target = self.binding_target()?;
                self.skip_type_annotation()?;
                self.expect(")")?;
                Some(target)
            } else {
                None
            };
            self.expect("{")?;
            Some((binding, self.block_body()?))
        } else {
            None
        };
        let finally = if self.eat_word("finally") {
            self.expect("{")?;
            Some(self.block_body()?)
        } else {
            None
        };
        if catch.is_none() && finally.is_none() {
            return self.error("\"catch\" ou \"finally\" attendu");
        }
        Ok(Stmt::Try(block, catch, finally))
    }

    pub(super) fn switch_statement(&mut self) -> PResult<Stmt> {
        self.advance();
        self.expect("(")?;
        let discriminant = self.expression()?;
        self.expect(")")?;
        self.expect("{")?;
        let mut cases = Vec::new();
        while !self.eat("}") {
            let test = if self.eat_word("case") {
                Some(self.expression()?)
            } else if self.eat_word("default") {
                None
            } else {
                return self.error("\"case\" attendu");
            };
            self.expect(":")?;
            let mut body = Vec::new();
            while !self.is_word("case") && !self.is_word("default") && !self.is("}") {
                if matches!(self.peek(), Tok::Eof) {
                    return self.error("\"}\" attendu");
                }
                body.push(self.statement()?);
            }
            cases.push((test, body));
        }
        Ok(Stmt::Switch(discriminant, cases))
    }

    // --------------------------------------------------------
    //  Patterns
    // --------------------------------------------------------

    pub(super) fn binding_target(&mut self) -> PResult<Pattern> {
        self.enter()?;
        let result = self.binding_target_inner();
        self.leave();
        result
    }

    pub(super) fn binding_target_inner(&mut self) -> PResult<Pattern> {
        if self.eat("[") {
            let mut elements = Vec::new();
            let mut rest = None;
            loop {
                if self.eat("]") {
                    break;
                }
                if self.eat(",") {
                    elements.push(None);
                    continue;
                }
                if self.eat("...") {
                    rest = Some(Box::new(self.binding_target()?));
                    self.eat(",");
                    self.expect("]")?;
                    break;
                }
                let target = self.binding_target()?;
                let default = if self.eat("=") { Some(self.assignment()?) } else { None };
                elements.push(Some((target, default)));
                if !self.is("]") {
                    self.expect(",")?;
                }
            }
            return Ok(Pattern::Array(elements, rest));
        }
        if self.eat("{") {
            let mut properties = Vec::new();
            let mut rest = None;
            loop {
                if self.eat("}") {
                    break;
                }
                if self.eat("...") {
                    rest = Some(Box::new(self.binding_target()?));
                    self.eat(",");
                    self.expect("}")?;
                    break;
                }
                let key = self.property_key()?;
                let target = if self.eat(":") {
                    self.binding_target()?
                } else {
                    match &key {
                        PropKey::Name(name) => Pattern::Ident(self.intern(name)),
                        _ => return self.error("\":\" attendu"),
                    }
                };
                let default = if self.eat("=") { Some(self.assignment()?) } else { None };
                properties.push((key, target, default));
                if !self.is("}") {
                    self.expect(",")?;
                }
            }
            return Ok(Pattern::Object(properties, rest));
        }
        Ok(Pattern::Ident(self.identifier()?))
    }

    /// An expression parsed before knowing it was a target (`[a, b] =
    /// ...`, `for (x.y of ...)`) becomes a pattern.
    pub(super) fn to_pattern(&self, expr: Expr) -> PResult<Pattern> {
        Ok(match expr {
            Expr::Ident(name) => Pattern::Ident(name),
            Expr::Member(..) | Expr::Index(..) | Expr::PrivateMember(..) | Expr::SuperMember(..) => {
                Pattern::Target(Box::new(expr))
            }
            Expr::Array(elements) => {
                let mut out = Vec::new();
                let mut rest = None;
                let count = elements.len();
                for (index, element) in elements.into_iter().enumerate() {
                    match element {
                        None => out.push(None),
                        Some(Arg::Spread(inner)) if index + 1 == count => rest = Some(Box::new(self.to_pattern(inner)?)),
                        Some(Arg::Spread(_)) => return self.error("\"...\" doit etre le dernier element"),
                        Some(Arg::Plain(Expr::Assign(AssignOp::Assign, target, default))) => {
                            out.push(Some((*target, Some(*default))))
                        }
                        Some(Arg::Plain(inner)) => out.push(Some((self.to_pattern(inner)?, None))),
                    }
                }
                Pattern::Array(out, rest)
            }
            Expr::Object(properties) => {
                let mut out = Vec::new();
                let mut rest = None;
                for property in properties {
                    match property.kind {
                        PropKind::Spread => rest = Some(Box::new(self.to_pattern(property.value)?)),
                        PropKind::Value => match property.value {
                            Expr::Assign(AssignOp::Assign, target, default) => out.push((property.key, *target, Some(*default))),
                            value => out.push((property.key, self.to_pattern(value)?, None)),
                        },
                        _ => return self.error("cible d'affectation invalide"),
                    }
                }
                Pattern::Object(out, rest)
            }
            Expr::Assign(AssignOp::Assign, target, _) => *target,
            Expr::Chain(inner) => return self.to_pattern(*inner),
            _ => return self.error("cible d'affectation invalide"),
        })
    }
}
