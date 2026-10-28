//! Functions, arrows and classes.

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::vec::Vec;

use super::Parser;
use super::super::ast::*;
use super::super::lex::Tok;
use super::{FunctionState, PResult};
use super::super::value::JsStr;

impl<'a> Parser<'a> {
    // --------------------------------------------------------
    //  Functions and classes
    // --------------------------------------------------------

    pub(super) fn parameters(&mut self) -> PResult<(Vec<(Pattern, Option<Expr>)>, Option<Pattern>)> {
        self.expect("(")?;
        let mut params = Vec::new();
        let mut rest = None;
        let mut properties = Vec::new();
        let saved = self.no_in;
        self.no_in = false;
        while !self.eat(")") {
            // TypeScript: `this: T` types `this` and is no parameter.
            if self.options.typescript && self.is_word("this") && (self.punct_at(1, ":") || self.punct_at(1, ",") || self.punct_at(1, ")")) {
                self.advance();
                self.skip_type_annotation()?;
                self.eat(",");
                continue;
            }
            if self.eat("...") {
                rest = Some(self.binding_target()?);
                self.skip_type_annotation()?;
                self.eat(",");
                self.expect(")")?;
                break;
            }
            let property = self.parameter_modifiers();
            let target = self.binding_target()?;
            if self.options.typescript {
                self.eat("?");
                self.skip_type_annotation()?;
            }
            if let (true, Pattern::Ident(name)) = (property, &target) {
                properties.push(name.clone());
            }
            let default = if self.eat("=") { Some(self.assignment()?) } else { None };
            params.push((target, default));
            if !self.is(")") {
                self.expect(",")?;
            }
        }
        self.no_in = saved;
        // A parameter list inside a default value has had its turn.
        self.param_properties = properties;
        Ok((params, rest))
    }

    /// TypeScript: a signature without a body (an overload, `declare`,
    /// `abstract`) ends here and makes nothing.
    fn signature_end(&mut self) -> PResult<Option<Rc<Func>>> {
        if !self.options.typescript || self.is("{") {
            return Ok(None);
        }
        self.functions.pop();
        self.semicolon()?;
        self.signature = true;
        Ok(Some(Rc::new(Func {
            name: None,
            params: Vec::new(),
            rest: None,
            body: Vec::new(),
            arrow: false,
            is_async: false,
            generator: false,
            expression: false,
            vars: Vec::new(),
            uses_arguments: false,
            simple_params: true,
            source: (0, 0),
            strict: true,
            is_ctor: false,
        })))
    }

    pub(super) fn function_body(
        &mut self,
        name: Option<JsStr>,
        params: Vec<(Pattern, Option<Expr>)>,
        rest: Option<Pattern>,
        start: usize,
        is_async: bool,
        generator: bool,
        is_ctor: bool,
    ) -> PResult<Rc<Func>> {
        self.expect("{")?;
        let strict = self.directive_strict();
        let body = self.block_body()?;
        let state = self.functions.pop().unwrap_or(FunctionState { vars: Vec::new(), uses_arguments: false, arrow: false, is_async, generator });
        let end = self.tokens[self.pos.saturating_sub(1)].at + 1;
        let simple_params = rest.is_none() && params.iter().all(|(p, d)| matches!(p, Pattern::Ident(_)) && d.is_none());
        Ok(Rc::new(Func {
            name,
            params,
            rest,
            body,
            arrow: false,
            is_async,
            generator,
            expression: false,
            vars: state.vars,
            uses_arguments: state.uses_arguments,
            simple_params,
            source: (start, end),
            strict,
            is_ctor,
        }))
    }

    /// `function name(...) {...}` from the `function` keyword.
    pub(super) fn function(&mut self, is_async: bool, declaration: bool) -> PResult<Rc<Func>> {
        let start = self.at();
        self.advance(); // function
        let generator = self.eat("*");
        let name = if matches!(self.peek(), Tok::Ident(_)) && !self.is("(") {
            Some(self.identifier()?)
        } else if declaration {
            return self.error("nom de fonction attendu");
        } else {
            None
        };
        self.skip_type_parameters()?;
        self.functions.push(FunctionState { vars: Vec::new(), uses_arguments: false, arrow: false, is_async, generator });
        let (params, rest) = match self.parameters().and_then(|parsed| self.skip_return_type().map(|_| parsed)) {
            Ok(parsed) => parsed,
            Err(error) => {
                self.functions.pop();
                return Err(error);
            }
        };
        if let Some(signature) = self.signature_end()? {
            return Ok(signature);
        }
        self.function_body(name, params, rest, start, is_async, generator, false)
    }

    pub(super) fn method_function(&mut self, key: &PropKey, is_async: bool, generator: bool) -> PResult<Rc<Func>> {
        let start = self.at();
        let name = match key {
            PropKey::Name(name) => Some(name.clone()),
            _ => None,
        };
        self.functions.push(FunctionState { vars: Vec::new(), uses_arguments: false, arrow: false, is_async, generator });
        let (params, rest) = match self.parameters().and_then(|parsed| self.skip_return_type().map(|_| parsed)) {
            Ok(parsed) => parsed,
            Err(error) => {
                self.functions.pop();
                return Err(error);
            }
        };
        self.function_body(name, params, rest, start, is_async, generator, false)
    }

    pub(super) fn arrow(&mut self, params: Vec<(Pattern, Option<Expr>)>, rest: Option<Pattern>, is_async: bool) -> PResult<Expr> {
        let start = self.at();
        self.expect("=>")?;
        self.functions.push(FunctionState { vars: Vec::new(), uses_arguments: false, arrow: true, is_async, generator: false });
        let simple_params = rest.is_none() && params.iter().all(|(p, d)| matches!(p, Pattern::Ident(_)) && d.is_none());
        let (body, expression) = if self.eat("{") {
            match self.block_body() {
                Ok(body) => (body, false),
                Err(error) => {
                    self.functions.pop();
                    return Err(error);
                }
            }
        } else {
            match self.assignment() {
                Ok(expr) => (alloc::vec![Stmt::Return(Some(expr))], true),
                Err(error) => {
                    self.functions.pop();
                    return Err(error);
                }
            }
        };
        let state = self.functions.pop().unwrap_or(FunctionState { vars: Vec::new(), uses_arguments: false, arrow: true, is_async, generator: false });
        if state.uses_arguments {
            if let Some(outer) = self.functions.last_mut() {
                outer.uses_arguments = true;
            }
        }
        let end = self.tokens[self.pos.saturating_sub(1)].at + 1;
        Ok(Expr::Function(Rc::new(Func {
            name: None,
            params,
            rest,
            body,
            arrow: true,
            is_async,
            generator: false,
            expression,
            vars: state.vars,
            uses_arguments: false,
            simple_params,
            source: (start, end),
            strict: false,
            is_ctor: false,
        })))
    }

    pub(super) fn class(&mut self, declaration: bool) -> PResult<Rc<Class>> {
        self.advance(); // class
        let name = if matches!(self.peek(), Tok::Ident(w) if w.as_str() != "extends" && w.as_str() != "implements") {
            Some(self.identifier()?)
        } else if declaration {
            return self.error("nom de classe attendu");
        } else {
            None
        };
        self.skip_type_parameters()?;
        let extends = if self.eat_word("extends") {
            let parent = self.call_member(false)?;
            self.skip_type_parameters()?;
            Some(Box::new(parent))
        } else {
            None
        };
        if self.options.typescript && self.eat_word("implements") {
            loop {
                self.skip_type()?;
                if !self.eat(",") {
                    break;
                }
            }
        }
        self.expect("{")?;
        let mut constructor = None;
        let mut members = Vec::new();
        let mut static_blocks = Vec::new();
        while !self.eat("}") {
            if self.eat(";") {
                continue;
            }
            if self.is("@") {
                return self.error("les decorateurs ne sont pas pris en charge");
            }
            if self.index_signature_ahead() {
                self.skip_balanced()?;
                self.skip_type_annotation()?;
                self.semicolon()?;
                continue;
            }
            let mut ambient = self.member_modifiers();
            let mut is_static = false;
            if self.is_word("static") && self.modifier_applies() {
                self.advance();
                is_static = true;
                ambient |= self.member_modifiers();
                if self.is("{") {
                    let start = self.at();
                    self.functions.push(FunctionState { vars: Vec::new(), uses_arguments: false, arrow: false, is_async: false, generator: false });
                    let func = self.function_body(None, Vec::new(), None, start, false, false, false)?;
                    static_blocks.push(func);
                    continue;
                }
            }
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
            if self.options.typescript {
                // `x?: T`, `x!: T`, `m?()`, `m<T>()`.
                if !self.eat("?") {
                    self.eat("!");
                }
                if self.is("<") {
                    self.skip_angles()?;
                }
            }
            if self.is("(") {
                let is_constructor = !is_static && matches!(&key, PropKey::Name(n) if n.as_str() == "constructor");
                let start = self.at();
                let name = match &key {
                    PropKey::Name(name) => Some(name.clone()),
                    _ => None,
                };
                self.functions.push(FunctionState { vars: Vec::new(), uses_arguments: false, arrow: false, is_async, generator });
                let (params, rest) = match self.parameters().and_then(|parsed| self.skip_return_type().map(|_| parsed)) {
                    Ok(parsed) => parsed,
                    Err(error) => {
                        self.functions.pop();
                        return Err(error);
                    }
                };
                if self.signature_end()?.is_some() {
                    self.signature = false;
                    continue;
                }
                let properties = core::mem::take(&mut self.param_properties);
                let func = self.function_body(name, params, rest, start, is_async, generator, is_constructor)?;
                if is_constructor {
                    self.param_properties = properties;
                    constructor = Some(self.with_parameter_properties(func));
                    continue;
                }
                let kind = match accessor {
                    Some(true) => PropKind::Get,
                    Some(false) => PropKind::Set,
                    None => PropKind::Method,
                };
                members.push(ClassMember { key, kind, is_static, value: Some(Expr::Function(func)), field: false });
            } else {
                // A field, with its initializer evaluated per instance as
                // the body of a method (so `this` is the instance).
                self.skip_type_annotation()?;
                if ambient && !self.is("=") {
                    // `declare x: T;`, `abstract x: T;`: no property.
                    self.semicolon()?;
                    continue;
                }
                let value = if self.eat("=") {
                    let start = self.at();
                    self.functions.push(FunctionState { vars: Vec::new(), uses_arguments: false, arrow: true, is_async: false, generator: false });
                    let init = self.assignment();
                    let state = self.functions.pop();
                    let init = init?;
                    let end = self.tokens[self.pos.saturating_sub(1)].at + 1;
                    Some(Expr::Function(Rc::new(Func {
                        name: None,
                        params: Vec::new(),
                        rest: None,
                        body: alloc::vec![Stmt::Return(Some(init))],
                        arrow: false,
                        is_async: false,
                        generator: false,
                        expression: true,
                        vars: state.map(|s| s.vars).unwrap_or_default(),
                        uses_arguments: false,
                        simple_params: true,
                        source: (start, end),
                        strict: true,
                        is_ctor: false,
                    })))
                } else {
                    None
                };
                self.semicolon()?;
                members.push(ClassMember { key, kind: PropKind::Value, is_static, value, field: true });
            }
        }
        Ok(Rc::new(Class { name, extends, constructor, members, static_blocks }))
    }
}
