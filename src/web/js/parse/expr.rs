//! Expressions: assignment down to primary, object literals, templates.

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::vec::Vec;

use super::Parser;
use super::super::ast::*;
use alloc::string::String;

use super::super::lex::{Lexer, Tok};
use super::{assign_op, binary_precedence, is_reserved, PResult, ParseError};
use super::super::value::JsStr;

impl<'a> Parser<'a> {
    // --------------------------------------------------------
    //  Expressions
    // --------------------------------------------------------

    pub fn expression(&mut self) -> PResult<Expr> {
        let first = self.assignment()?;
        if !self.is(",") {
            return Ok(first);
        }
        let mut list = alloc::vec![first];
        while self.eat(",") {
            list.push(self.assignment()?);
        }
        Ok(Expr::Seq(list))
    }

    pub(super) fn assignment(&mut self) -> PResult<Expr> {
        self.enter()?;
        let result = self.assignment_inner();
        self.leave();
        result
    }

    /// Is the token at `self.pos` the `(` of an arrow's parameters?
    pub(super) fn arrow_ahead(&self, open: usize) -> bool {
        let mut depth = 0usize;
        let mut index = open;
        while index < self.tokens.len() {
            match &self.tokens[index].tok {
                Tok::Punct("(") | Tok::Punct("[") | Tok::Punct("{") => depth += 1,
                Tok::Punct(")") | Tok::Punct("]") | Tok::Punct("}") => {
                    depth -= 1;
                    if depth == 0 {
                        return index + 1 < self.tokens.len()
                            && matches!(self.tokens[index + 1].tok, Tok::Punct("=>"))
                            && !self.tokens[index + 1].newline;
                    }
                }
                Tok::Eof => return false,
                _ => {}
            }
            index += 1;
        }
        false
    }

    pub(super) fn assignment_inner(&mut self) -> PResult<Expr> {
        // Arrow functions.
        match self.peek().clone() {
            Tok::Ident(word) if word.as_str() == "async" && !self.tokens[self.pos + 1].newline => {
                if let Tok::Ident(param) = self.peek_at(1).clone() {
                    if self.punct_at(2, "=>") && !is_reserved(&param) {
                        self.advance();
                        let name = self.identifier()?;
                        return self.arrow(alloc::vec![(Pattern::Ident(name), None)], None, true);
                    }
                }
                if self.punct_at(1, "(") && (self.arrow_ahead(self.pos + 1) || self.typed_arrow_ahead(self.pos + 1)) {
                    self.advance();
                    let (params, rest) = self.parameters()?;
                    self.skip_return_type()?;
                    return self.arrow(params, rest, true);
                }
                if self.punct_at(1, "<") {
                    let mark = self.mark();
                    self.advance();
                    if self.generic_arrow_ahead() {
                        self.skip_angles()?;
                        let (params, rest) = self.parameters()?;
                        self.skip_return_type()?;
                        return self.arrow(params, rest, true);
                    }
                    self.reset(mark);
                }
            }
            Tok::Ident(word) if !is_reserved(&word) && self.punct_at(1, "=>") && !self.tokens[self.pos + 1].newline => {
                let name = self.identifier()?;
                return self.arrow(alloc::vec![(Pattern::Ident(name), None)], None, false);
            }
            Tok::Punct("(") if self.arrow_ahead(self.pos) || self.typed_arrow_ahead(self.pos) => {
                let (params, rest) = self.parameters()?;
                self.skip_return_type()?;
                return self.arrow(params, rest, false);
            }
            Tok::Punct("<") if self.generic_arrow_ahead() => {
                self.skip_angles()?;
                let (params, rest) = self.parameters()?;
                self.skip_return_type()?;
                return self.arrow(params, rest, false);
            }
            Tok::Ident(word) if word.as_str() == "yield" && self.in_generator() => {
                self.advance();
                let delegate = self.eat("*");
                let value = if self.newline_before()
                    || matches!(self.peek(), Tok::Punct(")") | Tok::Punct("]") | Tok::Punct("}") | Tok::Punct(",") | Tok::Punct(";") | Tok::Punct(":") | Tok::Eof)
                {
                    None
                } else {
                    Some(Box::new(self.assignment()?))
                };
                return Ok(Expr::Yield(value, delegate));
            }
            _ => {}
        }
        let left = self.conditional()?;
        if let Tok::Punct(punct) = self.peek().clone() {
            if let Some(op) = assign_op(punct) {
                self.advance();
                let target = if op == AssignOp::Assign {
                    self.to_pattern(left)?
                } else {
                    match left {
                        Expr::Ident(_) | Expr::Member(..) | Expr::Index(..) | Expr::PrivateMember(..) | Expr::SuperMember(..) => {
                            self.to_pattern(left)?
                        }
                        _ => return self.error("cible d'affectation invalide"),
                    }
                };
                let value = self.assignment()?;
                return Ok(Expr::Assign(op, Box::new(target), Box::new(value)));
            }
        }
        Ok(left)
    }

    pub(super) fn conditional(&mut self) -> PResult<Expr> {
        let test = self.binary(0)?;
        if !self.eat("?") {
            return Ok(test);
        }
        let saved = self.no_in;
        self.no_in = false;
        let then = self.assignment()?;
        self.no_in = saved;
        self.expect(":")?;
        let otherwise = self.assignment()?;
        Ok(Expr::Cond(Box::new(test), Box::new(then), Box::new(otherwise)))
    }

    pub(super) fn binary(&mut self, min: u8) -> PResult<Expr> {
        self.enter()?;
        let result = self.binary_inner(min);
        self.leave();
        result
    }

    pub(super) fn binary_inner(&mut self, min: u8) -> PResult<Expr> {
        // `#x in obj`
        let mut left = if let Tok::Private(name) = self.peek().clone() {
            if self.word_at(1, "in") {
                self.advance();
                self.advance();
                let object = self.binary(9)?;
                Expr::PrivateIn(name, Box::new(object))
            } else {
                return self.error("nom prive inattendu");
            }
        } else {
            self.unary()?
        };
        loop {
            // TypeScript: `x as T`, at the level of the comparisons.
            if self.ts_operator_ahead() {
                if 8 <= min {
                    return Ok(left);
                }
                self.skip_ts_operator()?;
                continue;
            }
            let operator = match self.peek() {
                Tok::Punct(p) => binary_precedence(p, self.no_in),
                Tok::Ident(w) if w.as_str() == "instanceof" || w.as_str() == "in" => binary_precedence(w, self.no_in),
                _ => None,
            };
            let Some((precedence, binary, logic)) = operator else {
                return Ok(left);
            };
            if precedence <= min && !(precedence == 12 && min == 12) {
                return Ok(left);
            }
            self.advance();
            // `**` is right-associative.
            let right = if precedence == 12 { self.binary(11)? } else { self.binary(precedence)? };
            left = match (binary, logic) {
                (Some(op), _) => Expr::Binary(op, Box::new(left), Box::new(right)),
                (None, Some(op)) => Expr::Logic(op, Box::new(left), Box::new(right)),
                _ => return self.error("operateur inconnu"),
            };
        }
    }

    pub(super) fn unary(&mut self) -> PResult<Expr> {
        self.enter()?;
        let result = self.unary_inner();
        self.leave();
        result
    }

    pub(super) fn unary_inner(&mut self) -> PResult<Expr> {
        // TypeScript: `<T>value`, the older form of `value as T`.
        if self.options.typescript && self.is("<") {
            self.skip_angles()?;
            return self.unary();
        }
        let op = match self.peek() {
            Tok::Punct("!") => Some(UnaryOp::Not),
            Tok::Punct("-") => Some(UnaryOp::Neg),
            Tok::Punct("+") => Some(UnaryOp::Plus),
            Tok::Punct("~") => Some(UnaryOp::BitNot),
            Tok::Ident(w) if w.as_str() == "typeof" => Some(UnaryOp::TypeOf),
            Tok::Ident(w) if w.as_str() == "void" => Some(UnaryOp::Void),
            Tok::Ident(w) if w.as_str() == "delete" => Some(UnaryOp::Delete),
            _ => None,
        };
        if let Some(op) = op {
            self.advance();
            let operand = self.unary()?;
            // Constant folding of negative literals keeps `-1` a number.
            if let (UnaryOp::Neg, Expr::Num(n)) = (op, &operand) {
                return Ok(Expr::Num(n.neg()));
            }
            return Ok(Expr::Unary(op, Box::new(operand)));
        }
        if self.is("++") || self.is("--") {
            let increment = self.is("++");
            self.advance();
            let target = self.unary()?;
            return Ok(Expr::Update(increment, true, Box::new(target)));
        }
        if self.is_word("await") && (self.in_async() || self.functions.len() <= 1) {
            // Top-level await in a classic script is an identifier; treat
            // it as await only when followed by something to await.
            if self.in_async() || !matches!(self.peek_at(1), Tok::Punct(_) | Tok::Eof) || self.punct_at(1, "(") || self.punct_at(1, "[") {
                if self.in_async() {
                    self.advance();
                    let operand = self.unary()?;
                    return Ok(Expr::Await(Box::new(operand)));
                }
            }
        }
        let expr = self.postfix()?;
        Ok(expr)
    }

    pub(super) fn postfix(&mut self) -> PResult<Expr> {
        let expr = self.call_member(false)?;
        if (self.is("++") || self.is("--")) && !self.newline_before() {
            let increment = self.is("++");
            self.advance();
            return Ok(Expr::Update(increment, false, Box::new(expr)));
        }
        Ok(expr)
    }

    pub(super) fn arguments(&mut self) -> PResult<Vec<Arg>> {
        let mut out = Vec::new();
        let saved = self.no_in;
        self.no_in = false;
        while !self.eat(")") {
            if self.eat("...") {
                out.push(Arg::Spread(self.assignment()?));
            } else {
                out.push(Arg::Plain(self.assignment()?));
            }
            if !self.is(")") {
                self.expect(",")?;
            }
        }
        self.no_in = saved;
        Ok(out)
    }

    /// Member accesses, calls and `new`, with optional chains.
    pub(super) fn call_member(&mut self, in_new: bool) -> PResult<Expr> {
        let mut expr = if self.is_word("new") {
            self.advance();
            if self.eat(".") {
                if !self.eat_word("target") {
                    return self.error("\"new.target\" attendu");
                }
                Expr::NewTarget
            } else {
                self.enter()?;
                let callee = self.call_member(true);
                self.leave();
                let callee = callee?;
                let args = if self.eat("(") { self.arguments()? } else { Vec::new() };
                Expr::New(Box::new(callee), args)
            }
        } else {
            self.primary()?
        };
        let mut chained = false;
        loop {
            // TypeScript: `f<T>(x)`, and `x!`.
            if self.is("<") && self.try_type_arguments() {
                continue;
            }
            if self.options.typescript && self.is("!") && !self.newline_before() {
                self.advance();
                continue;
            }
            if self.eat(".") {
                match self.advance() {
                    Tok::Ident(name) => {
                        let name = self.intern(&name);
                        expr = Expr::Member(Box::new(expr), name, false);
                    }
                    Tok::Private(name) => expr = Expr::PrivateMember(Box::new(expr), name),
                    _ => return self.error("nom de propriete attendu"),
                }
            } else if self.is("?.") {
                if in_new {
                    return self.error("chaine optionnelle dans new");
                }
                self.advance();
                chained = true;
                if self.eat("(") {
                    let args = self.arguments()?;
                    expr = Expr::Call(Box::new(expr), args, true);
                } else if self.eat("[") {
                    let index = self.expression()?;
                    self.expect("]")?;
                    expr = Expr::Index(Box::new(expr), Box::new(index), true);
                } else {
                    match self.advance() {
                        Tok::Ident(name) => {
                            let name = self.intern(&name);
                            expr = Expr::Member(Box::new(expr), name, true);
                        }
                        Tok::Private(name) => expr = Expr::PrivateMember(Box::new(expr), name),
                        _ => return self.error("nom de propriete attendu"),
                    }
                }
            } else if self.is("[") {
                self.advance();
                let saved = self.no_in;
                self.no_in = false;
                let index = self.expression()?;
                self.no_in = saved;
                self.expect("]")?;
                expr = Expr::Index(Box::new(expr), Box::new(index), false);
            } else if self.is("(") && !in_new {
                self.advance();
                let args = self.arguments()?;
                expr = Expr::Call(Box::new(expr), args, false);
            } else if let Tok::Template(cooked, raw, spans) = self.peek().clone() {
                if chained {
                    return self.error("gabarit dans une chaine optionnelle");
                }
                self.advance();
                let substitutions = self.substitutions(&spans)?;
                expr = Expr::Tagged(Box::new(expr), Rc::new((cooked, raw)), substitutions);
            } else {
                break;
            }
        }
        Ok(if chained { Expr::Chain(Box::new(expr)) } else { expr })
    }

    pub(super) fn substitutions(&mut self, spans: &[(usize, usize)]) -> PResult<Vec<Expr>> {
        let mut out = Vec::new();
        for (start, end) in spans {
            let tokens = Lexer::new(&self.source[*start..*end], *start)
                .tokenize()
                .map_err(|(message, at)| ParseError { message, at })?;
            let saved_tokens = core::mem::replace(&mut self.tokens, tokens);
            let saved_pos = core::mem::replace(&mut self.pos, 0);
            let saved_in = core::mem::replace(&mut self.no_in, false);
            let result = self.expression();
            let finished = matches!(self.peek(), Tok::Eof);
            self.tokens = saved_tokens;
            self.pos = saved_pos;
            self.no_in = saved_in;
            let expr = result?;
            if !finished {
                return Err(ParseError { message: String::from("\"}\" attendu dans le gabarit"), at: *start });
            }
            out.push(expr);
        }
        Ok(out)
    }

    pub(super) fn primary(&mut self) -> PResult<Expr> {
        let at = self.at();
        match self.advance() {
            Tok::Num(n) => Ok(Expr::Num(n)),
            Tok::Str(s) => Ok(Expr::Str(s)),
            Tok::Regex(pattern, flags) => Ok(Expr::Regex(pattern, flags)),
            Tok::Template(cooked, _, spans) => {
                let substitutions = self.substitutions(&spans)?;
                let parts = cooked.into_iter().map(|part| part.unwrap_or_else(|| JsStr::from(""))).collect();
                Ok(Expr::Template(parts, substitutions))
            }
            Tok::Punct("(") => {
                let saved = self.no_in;
                self.no_in = false;
                let expr = self.expression()?;
                self.no_in = saved;
                self.expect(")")?;
                Ok(expr)
            }
            Tok::Punct("[") => {
                let mut elements = Vec::new();
                let saved = self.no_in;
                self.no_in = false;
                loop {
                    if self.eat("]") {
                        break;
                    }
                    if self.eat(",") {
                        elements.push(None);
                        continue;
                    }
                    if self.eat("...") {
                        elements.push(Some(Arg::Spread(self.assignment()?)));
                    } else {
                        elements.push(Some(Arg::Plain(self.assignment()?)));
                    }
                    if !self.is("]") {
                        self.expect(",")?;
                    }
                }
                self.no_in = saved;
                Ok(Expr::Array(elements))
            }
            Tok::Punct("{") => self.object_literal(),
            Tok::Ident(word) => match word.as_str() {
                "this" => Ok(Expr::This),
                "null" => Ok(Expr::Null),
                "true" => Ok(Expr::Bool(true)),
                "false" => Ok(Expr::Bool(false)),
                "function" => {
                    self.pos -= 1;
                    Ok(Expr::Function(self.function(false, false)?))
                }
                "async" if self.is_word("function") && !self.newline_before() => {
                    Ok(Expr::Function(self.function(true, false)?))
                }
                "class" => {
                    self.pos -= 1;
                    Ok(Expr::Class(self.class(false)?))
                }
                "super" => {
                    if self.eat("(") {
                        let args = self.arguments()?;
                        Ok(Expr::SuperCall(args))
                    } else if self.eat(".") {
                        match self.advance() {
                            Tok::Ident(name) => Ok(Expr::SuperMember(Box::new(Expr::Str(name)))),
                            _ => self.error("nom de propriete attendu"),
                        }
                    } else if self.eat("[") {
                        let key = self.expression()?;
                        self.expect("]")?;
                        Ok(Expr::SuperMember(Box::new(key)))
                    } else {
                        self.error("\"super\" inattendu")
                    }
                }
                "import" => {
                    if self.eat(".") {
                        if !self.eat_word("meta") {
                            return self.error("\"import.meta\" attendu");
                        }
                        return Ok(Expr::ImportMeta);
                    }
                    self.expect("(")?;
                    let specifier = self.assignment()?;
                    self.eat(",");
                    if !self.is(")") {
                        self.assignment()?;
                    }
                    self.expect(")")?;
                    Ok(Expr::Import(Box::new(specifier)))
                }
                "new" => {
                    self.pos -= 1;
                    self.call_member(false)
                }
                _ if is_reserved(&word) => Err(ParseError { message: alloc::format!("\"{}\" inattendu", word.as_str()), at }),
                _ => {
                    if word.as_str() == "arguments" {
                        for state in self.functions.iter_mut().rev() {
                            state.uses_arguments = true;
                            if !state.arrow {
                                break;
                            }
                        }
                    }
                    if word.as_str() == "undefined" {
                        return Ok(Expr::Ident(self.intern("undefined")));
                    }
                    Ok(Expr::Ident(self.intern(&word)))
                }
            },
            Tok::Eof => Err(ParseError { message: String::from("fin inattendue du script"), at }),
            _ => Err(ParseError { message: String::from("expression attendue"), at }),
        }
    }

    pub(super) fn property_key(&mut self) -> PResult<PropKey> {
        match self.advance() {
            Tok::Ident(name) => Ok(PropKey::Name(self.intern(&name))),
            Tok::Str(s) => Ok(PropKey::Name(s)),
            Tok::Num(n) => Ok(PropKey::Name(JsStr::from(n.to_js_string()))),
            Tok::Private(name) => Ok(PropKey::Private(name)),
            Tok::Punct("[") => {
                let saved = self.no_in;
                self.no_in = false;
                let key = self.assignment()?;
                self.no_in = saved;
                self.expect("]")?;
                Ok(PropKey::Computed(Box::new(key)))
            }
            _ => self.error("nom de propriete attendu"),
        }
    }

    /// After `get`/`set`/`async`/`static`, is the next token a property
    /// key (so the word is a modifier) rather than the end of a member?
    pub(super) fn modifier_applies(&self) -> bool {
        !matches!(self.peek_at(1), Tok::Punct("(") | Tok::Punct(",") | Tok::Punct(":") | Tok::Punct("}") | Tok::Punct("=") | Tok::Punct(";"))
            && !self.tokens[self.pos + 1].newline
    }

    pub(super) fn object_literal(&mut self) -> PResult<Expr> {
        let mut properties = Vec::new();
        let saved = self.no_in;
        self.no_in = false;
        loop {
            if self.eat("}") {
                break;
            }
            if self.eat("...") {
                let value = self.assignment()?;
                properties.push(PropDef { key: PropKey::Name(JsStr::from("")), kind: PropKind::Spread, value });
            } else {
                let mut is_async = false;
                let mut generator = false;
                let mut accessor = None;
                if self.is_word("async") && self.modifier_applies() {
                    self.advance();
                    is_async = true;
                }
                if self.eat("*") {
                    generator = true;
                }
                if (self.is_word("get") || self.is_word("set")) && self.modifier_applies() && !is_async && !generator {
                    accessor = Some(self.is_word("get"));
                    self.advance();
                }
                let key = self.property_key()?;
                if self.options.typescript && self.is("<") {
                    self.skip_angles()?;
                }
                if self.is("(") {
                    let func = self.method_function(&key, is_async, generator)?;
                    let kind = match accessor {
                        Some(true) => PropKind::Get,
                        Some(false) => PropKind::Set,
                        None => PropKind::Method,
                    };
                    properties.push(PropDef { key, kind, value: Expr::Function(func) });
                } else if self.eat(":") {
                    let value = self.assignment()?;
                    properties.push(PropDef { key, kind: PropKind::Value, value });
                } else {
                    // Shorthand `{a}` or, in a pattern, `{a = 1}`.
                    let PropKey::Name(name) = &key else {
                        return self.error("\":\" attendu");
                    };
                    let name = self.intern(name);
                    let value = if self.eat("=") {
                        let default = self.assignment()?;
                        Expr::Assign(AssignOp::Assign, Box::new(Pattern::Ident(name.clone())), Box::new(default))
                    } else {
                        Expr::Ident(name)
                    };
                    properties.push(PropDef { key, kind: PropKind::Value, value });
                }
            }
            if !self.is("}") {
                self.expect(",")?;
            }
        }
        self.no_in = saved;
        Ok(Expr::Object(properties))
    }
}
