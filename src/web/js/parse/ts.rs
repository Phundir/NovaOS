//! TypeScript: the types the parser reads and drops, and the two
//! constructs that leave something at run time.
//!
//! ## Erasure, not checking
//!
//! A type is consumed by a small grammar of its own (`skip_type`), not by
//! counting brackets: `a < b && c > (d)` must stay a comparison, and only
//! a reading that knows what a type may contain can tell `f<T>(x)` from
//! it. Where the language itself is ambiguous (a `<` after a name, a `(`
//! that may open an arrow with a return type) the parser guesses, reads,
//! and backs up when the guess fails -- the tokens a type cut in two
//! (`>>` closing two lists) are put back with it.
//!
//! ## What has a value
//!
//! * `enum` becomes the object `tsc` emits: `E[E["A"] = 0] = "A"` for
//!   numbers (the reverse mapping), `E["S"] = "s"` for strings, members
//!   counting up from the last number.
//! * Parameter properties (`constructor(private x: T)`) become
//!   `this.x = x` at the start of the constructor, after `super(...)`.
//!
//! `namespace` and decorators are refused by name; JSX is not TypeScript
//! and is not read either.

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::vec::Vec;

use super::super::ast::*;
use super::super::lex::Tok;
use super::super::num::Num;
use super::super::value::JsStr;
use super::{PResult, Parser};

/// What a `>`-run token leaves once `count` of its `>` close a list.
fn after_closing(punct: &str, count: usize) -> Option<&'static str> {
    Some(match (punct, count) {
        (">>", 1) => ">",
        (">>>", 1) => ">>",
        (">>>", 2) => ">",
        (">=", 1) => "=",
        (">>=", 1) => ">=",
        (">>=", 2) => "=",
        (">>>=", 1) => ">>=",
        (">>>=", 2) => ">=",
        (">>>=", 3) => "=",
        _ => return None,
    })
}

/// How many `>` start a punctuator.
fn closing_count(punct: &str) -> usize {
    punct.bytes().take_while(|b| *b == b'>').count()
}

/// Words that start a type by themselves.
fn type_keyword(word: &str) -> bool {
    matches!(
        word,
        "any" | "unknown" | "never" | "void" | "null" | "undefined" | "string" | "number" | "boolean" | "bigint"
            | "symbol" | "object" | "true" | "false" | "this"
    )
}

impl<'a> Parser<'a> {
    // --------------------------------------------------------
    //  Guessing and backing up
    // --------------------------------------------------------

    /// A point to come back to.
    pub(super) fn mark(&self) -> (usize, usize, usize) {
        (self.pos, self.splits.len(), self.depth)
    }

    /// Back to `mark`, the cut tokens restored.
    pub(super) fn reset(&mut self, mark: (usize, usize, usize)) {
        while self.splits.len() > mark.1 {
            if let Some((index, tok)) = self.splits.pop() {
                self.tokens[index].tok = tok;
            }
        }
        self.pos = mark.0;
        self.depth = mark.2;
    }

    // --------------------------------------------------------
    //  Types
    // --------------------------------------------------------

    /// `: Type`, if one follows.
    pub(super) fn skip_type_annotation(&mut self) -> PResult<()> {
        if self.options.typescript && self.is(":") {
            self.advance();
            self.skip_type()?;
        }
        Ok(())
    }

    /// The return type after a parameter list, predicates included
    /// (`x is T`, `asserts x`, `asserts x is T`).
    pub(super) fn skip_return_type(&mut self) -> PResult<()> {
        if !(self.options.typescript && self.is(":")) {
            return Ok(());
        }
        self.advance();
        if self.is_word("asserts") && matches!(self.peek_at(1), Tok::Ident(_)) && !self.tokens[self.pos + 1].newline {
            self.advance();
            self.advance();
            if self.eat_word("is") {
                self.skip_type()?;
            }
            return Ok(());
        }
        if matches!(self.peek(), Tok::Ident(_)) && self.word_at(1, "is") && !self.tokens[self.pos + 1].newline {
            self.advance();
            self.advance();
        }
        self.skip_type()
    }

    /// `<T, U extends X = Y>` after a name, if there is one.
    pub(super) fn skip_type_parameters(&mut self) -> PResult<()> {
        if self.options.typescript && self.is("<") {
            self.skip_angles()?;
        }
        Ok(())
    }

    pub(super) fn skip_type(&mut self) -> PResult<()> {
        self.enter()?;
        let result = self.skip_type_inner();
        self.leave();
        result
    }

    fn skip_type_inner(&mut self) -> PResult<()> {
        // Function and constructor types.
        if self.is_word("abstract") && self.word_at(1, "new") {
            self.advance();
        }
        let constructor = self.is_word("new") && (self.punct_at(1, "(") || self.punct_at(1, "<"));
        if constructor {
            self.advance();
        }
        if self.is("<") {
            self.skip_angles()?;
            if !self.is("(") {
                return self.error("\"(\" attendu apres les parametres de type");
            }
        }
        if self.is("(") && self.paren_starts_function_type() {
            self.skip_balanced()?;
            self.expect("=>")?;
            return self.skip_type();
        }
        self.skip_union()?;
        // Conditional types.
        if self.is_word("extends") && !self.newline_before() {
            self.advance();
            self.skip_union()?;
            self.expect("?")?;
            self.skip_type()?;
            self.expect(":")?;
            self.skip_type()?;
        }
        Ok(())
    }

    /// From a `(` in a type: the parameters of a function type, or a
    /// parenthesized type? The token after the matching `)` decides.
    fn paren_starts_function_type(&self) -> bool {
        if self.punct_at(1, ")") || self.punct_at(1, "...") {
            return true;
        }
        let mut depth = 0usize;
        let mut index = self.pos;
        while index < self.tokens.len() {
            match &self.tokens[index].tok {
                Tok::Punct("(") | Tok::Punct("[") | Tok::Punct("{") => depth += 1,
                Tok::Punct(")") | Tok::Punct("]") | Tok::Punct("}") => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return matches!(self.tokens.get(index + 1).map(|t| &t.tok), Some(Tok::Punct("=>")));
                    }
                }
                Tok::Eof => return false,
                _ => {}
            }
            index += 1;
        }
        false
    }

    fn skip_union(&mut self) -> PResult<()> {
        self.eat("|");
        self.skip_intersection()?;
        while self.is("|") {
            self.advance();
            self.skip_intersection()?;
        }
        Ok(())
    }

    fn skip_intersection(&mut self) -> PResult<()> {
        self.eat("&");
        self.skip_operator_type()?;
        while self.is("&") {
            self.advance();
            self.skip_operator_type()?;
        }
        Ok(())
    }

    fn skip_operator_type(&mut self) -> PResult<()> {
        self.enter()?;
        let result = self.skip_operator_type_inner();
        self.leave();
        result
    }

    fn skip_operator_type_inner(&mut self) -> PResult<()> {
        if (self.is_word("keyof") || self.is_word("unique") || self.is_word("readonly"))
            && !matches!(self.peek_at(1), Tok::Punct(p) if matches!(*p, ")" | "]" | "}" | "," | ";" | "=" | ">" | "|" | "&"))
        {
            self.advance();
            return self.skip_operator_type();
        }
        if self.is_word("infer") && matches!(self.peek_at(1), Tok::Ident(_)) {
            self.advance();
            self.advance();
            return Ok(());
        }
        if self.is_word("typeof") {
            self.advance();
            if self.is_word("import") {
                self.advance();
                self.skip_balanced()?;
            } else {
                self.entity_name()?;
            }
            if self.is("<") && !self.newline_before() {
                self.skip_angles()?;
            }
        } else {
            self.skip_primary_type()?;
        }
        // `T[]`, `T[K]`.
        while self.is("[") && !self.newline_before() {
            self.advance();
            if !self.eat("]") {
                self.skip_type()?;
                self.expect("]")?;
            }
        }
        Ok(())
    }

    /// `a.b.c`
    fn entity_name(&mut self) -> PResult<()> {
        match self.advance() {
            Tok::Ident(_) => {}
            _ => return self.error("nom de type attendu"),
        }
        while self.is(".") && matches!(self.peek_at(1), Tok::Ident(_)) {
            self.advance();
            self.advance();
        }
        Ok(())
    }

    fn skip_primary_type(&mut self) -> PResult<()> {
        match self.peek().clone() {
            Tok::Punct("(") => {
                self.advance();
                self.skip_type()?;
                self.expect(")")
            }
            Tok::Punct("{") | Tok::Punct("[") => self.skip_balanced(),
            Tok::Punct("-") if matches!(self.peek_at(1), Tok::Num(_)) => {
                self.advance();
                self.advance();
                Ok(())
            }
            Tok::Str(_) | Tok::Num(_) | Tok::Template(..) => {
                self.advance();
                Ok(())
            }
            Tok::Ident(word) if type_keyword(&word) => {
                self.advance();
                Ok(())
            }
            Tok::Ident(word) if word.as_str() == "import" => {
                // `import("x").T`
                self.advance();
                self.skip_balanced()?;
                while self.eat(".") {
                    self.advance();
                }
                if self.is("<") {
                    self.skip_angles()?;
                }
                Ok(())
            }
            Tok::Ident(_) => {
                self.entity_name()?;
                if self.is("<") && !self.newline_before() {
                    self.skip_angles()?;
                }
                Ok(())
            }
            _ => self.error("type attendu"),
        }
    }

    /// From `(`, `[` or `{`, to just past the bracket that closes it.
    pub(super) fn skip_balanced(&mut self) -> PResult<()> {
        let mut depth = 0usize;
        loop {
            match self.advance() {
                Tok::Punct("(") | Tok::Punct("[") | Tok::Punct("{") => depth += 1,
                Tok::Punct(")") | Tok::Punct("]") | Tok::Punct("}") => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return Ok(());
                    }
                }
                Tok::Eof => return self.error("fin inattendue dans un type"),
                _ => {}
            }
            if depth == 0 {
                return self.error("\"(\", \"[\" ou \"{\" attendu");
            }
        }
    }

    /// From `<`, to just past the `>` that closes it, through nested
    /// lists; a `>>` that closes more than this list is cut in two.
    pub(super) fn skip_angles(&mut self) -> PResult<()> {
        self.expect("<")?;
        let mut depth = 1usize;
        loop {
            match self.peek().clone() {
                Tok::Punct("<") => {
                    depth += 1;
                    self.advance();
                }
                Tok::Punct(p) if p.starts_with('>') => {
                    let count = closing_count(p);
                    if count <= depth && count == p.len() {
                        depth -= count;
                        self.advance();
                    } else {
                        // Close what is open here, leave the rest.
                        let used = depth.min(count);
                        let Some(rest) = after_closing(p, used) else {
                            return self.error("\">\" attendu");
                        };
                        self.splits.push((self.pos, Tok::Punct(p)));
                        self.tokens[self.pos].tok = Tok::Punct(rest);
                        depth -= used;
                    }
                    if depth == 0 {
                        return Ok(());
                    }
                }
                Tok::Punct("(") | Tok::Punct("[") | Tok::Punct("{") => self.skip_balanced()?,
                Tok::Punct(p) if matches!(p, ";" | ")" | "]" | "}") => return self.error("\">\" attendu"),
                Tok::Eof => return self.error("\">\" attendu"),
                _ => {
                    self.advance();
                }
            }
        }
    }

    /// After a callee: `<T>(...)` is a call with type arguments when what
    /// is between the angles reads as types and a `(` or a template
    /// follows. Otherwise nothing is consumed.
    pub(super) fn try_type_arguments(&mut self) -> bool {
        if !self.options.typescript || !self.is("<") {
            return false;
        }
        let mark = self.mark();
        let ok = self.skip_type_argument_list().is_ok() && (self.is("(") || matches!(self.peek(), Tok::Template(..)));
        if !ok {
            self.reset(mark);
        }
        ok
    }

    fn skip_type_argument_list(&mut self) -> PResult<()> {
        self.expect("<")?;
        loop {
            self.skip_type()?;
            if self.eat(",") {
                continue;
            }
            if self.eat(">") {
                return Ok(());
            }
            return self.error("\">\" attendu");
        }
    }

    // --------------------------------------------------------
    //  Expressions and arrows
    // --------------------------------------------------------

    /// `x as T`, `x satisfies T`, `x as const`: the value stays.
    pub(super) fn ts_operator_ahead(&self) -> bool {
        self.options.typescript && (self.is_word("as") || self.is_word("satisfies")) && !self.newline_before()
    }

    pub(super) fn skip_ts_operator(&mut self) -> PResult<()> {
        self.advance();
        if self.is_word("const") {
            self.advance();
            return Ok(());
        }
        self.skip_type()
    }

    /// With a `(` at `open`: an arrow whose parameters have types and
    /// whose return type comes before `=>`.
    pub(super) fn typed_arrow_ahead(&mut self, open: usize) -> bool {
        if !self.options.typescript {
            return false;
        }
        let mut depth = 0usize;
        let mut index = open;
        while index < self.tokens.len() {
            match &self.tokens[index].tok {
                Tok::Punct("(") | Tok::Punct("[") | Tok::Punct("{") => depth += 1,
                Tok::Punct(")") | Tok::Punct("]") | Tok::Punct("}") => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        break;
                    }
                }
                Tok::Eof => return false,
                _ => {}
            }
            index += 1;
        }
        if !matches!(self.tokens.get(index + 1).map(|t| &t.tok), Some(Tok::Punct(":"))) {
            return false;
        }
        let mark = self.mark();
        self.pos = index + 1;
        let ok = self.skip_return_type().is_ok() && self.is("=>") && !self.newline_before();
        self.reset(mark);
        ok
    }

    /// `<T>(x: T) => x`: type parameters, then an arrow.
    pub(super) fn generic_arrow_ahead(&mut self) -> bool {
        if !self.options.typescript || !self.is("<") {
            return false;
        }
        let mark = self.mark();
        let ok = self.skip_angles().is_ok() && self.is("(") && {
            let open = self.pos;
            self.arrow_ahead(open) || self.typed_arrow_ahead(open)
        };
        self.reset(mark);
        ok
    }

    // --------------------------------------------------------
    //  Declarations
    // --------------------------------------------------------

    /// The function just parsed was a signature (no body): it declares
    /// nothing at run time.
    pub(super) fn take_signature(&mut self) -> bool {
        core::mem::replace(&mut self.signature, false)
    }

    /// A TypeScript declaration at statement level, if one starts here:
    /// `Some(statement)` (empty for a type), `None` when it is ordinary
    /// JavaScript after all.
    pub(super) fn ts_declaration(&mut self) -> PResult<Option<Stmt>> {
        if !self.options.typescript {
            return Ok(None);
        }
        let Tok::Ident(word) = self.peek().clone() else {
            return Ok(None);
        };
        let next_is_name = matches!(self.peek_at(1), Tok::Ident(_)) && !self.tokens[self.pos + 1].newline;
        match word.as_str() {
            "interface" if next_is_name => {
                self.advance();
                self.advance();
                self.skip_type_parameters()?;
                // `extends A, B<C>`
                while !self.is("{") {
                    if matches!(self.peek(), Tok::Eof) {
                        return self.error("\"{\" attendu");
                    }
                    self.advance();
                }
                self.skip_balanced()?;
                Ok(Some(Stmt::Empty))
            }
            "type" if next_is_name && (self.punct_at(2, "=") || self.punct_at(2, "<")) => {
                self.advance();
                self.advance();
                self.skip_type_parameters()?;
                self.expect("=")?;
                self.skip_type()?;
                self.semicolon()?;
                Ok(Some(Stmt::Empty))
            }
            "declare" if next_is_name => {
                self.advance();
                self.skip_ambient()?;
                Ok(Some(Stmt::Empty))
            }
            "abstract" if self.word_at(1, "class") => {
                self.advance();
                let class = self.class(true)?;
                let name = class.name.clone().unwrap_or_else(|| JsStr::from("default"));
                Ok(Some(Stmt::Class(name, class)))
            }
            "enum" if next_is_name => {
                self.advance();
                Ok(Some(self.enum_declaration()?))
            }
            "const" if self.word_at(1, "enum") => {
                self.advance();
                self.advance();
                Ok(Some(self.enum_declaration()?))
            }
            "namespace" | "module" if next_is_name && self.punct_at(2, "{") => {
                self.error("les namespace TypeScript ne sont pas pris en charge")
            }
            _ => Ok(None),
        }
    }

    /// After `declare`: a declaration with no value, skipped whole.
    fn skip_ambient(&mut self) -> PResult<()> {
        if self.is_word("const") || self.is_word("let") || self.is_word("var") {
            self.advance();
            loop {
                self.binding_target()?;
                self.skip_type_annotation()?;
                if !self.eat(",") {
                    break;
                }
            }
            return self.semicolon();
        }
        if self.is_word("function") {
            self.advance();
            self.identifier()?;
            self.skip_type_parameters()?;
            self.skip_balanced()?;
            self.skip_return_type()?;
            return self.semicolon();
        }
        // class, enum, module, namespace, global, interface, type...:
        // up to the end of their block, or of the statement.
        loop {
            match self.peek() {
                Tok::Punct("{") => {
                    self.skip_balanced()?;
                    return Ok(());
                }
                Tok::Punct(";") => {
                    self.advance();
                    return Ok(());
                }
                Tok::Eof => return Ok(()),
                _ => {
                    self.advance();
                }
            }
        }
    }

    /// `enum E { A, B = 5, C, S = "s" }`, from the name.
    fn enum_declaration(&mut self) -> PResult<Stmt> {
        let name = self.identifier()?;
        self.declare_var(&name);
        self.expect("{")?;
        let enum_ref = || Expr::Ident(name.clone());
        let mut body = alloc::vec![Stmt::Var(
            VarKind::Var,
            alloc::vec![(
                Pattern::Ident(name.clone()),
                // `E || {}`: a second declaration adds to the first.
                Some(Expr::Logic(LogicOp::Or, Box::new(enum_ref()), Box::new(Expr::Object(Vec::new())))),
            )],
        )];
        let mut members: Vec<JsStr> = Vec::new();
        // The value the next member takes without an initializer: a
        // known number, or the previous member plus one.
        let mut next: Result<Num, Option<JsStr>> = Ok(Num::from_i64(0));
        while !self.eat("}") {
            let member = match self.advance() {
                Tok::Ident(n) => self.intern(&n),
                Tok::Str(s) => s,
                _ => return self.error("membre d'enum attendu"),
            };
            let (value, numeric) = if self.eat("=") {
                let init = self.assignment()?;
                let init = enum_references(init, &members, &name);
                match &init {
                    Expr::Num(n) => {
                        next = Ok(n.add(Num::from_i64(1)));
                        (init, true)
                    }
                    Expr::Str(_) | Expr::Template(..) => {
                        next = Err(None);
                        (init, false)
                    }
                    _ => {
                        next = Err(Some(member.clone()));
                        (init, true)
                    }
                }
            } else {
                let value = match &next {
                    Ok(n) => Expr::Num(*n),
                    Err(Some(previous)) => Expr::Binary(
                        BinOp::Add,
                        Box::new(Expr::Member(Box::new(enum_ref()), previous.clone(), false)),
                        Box::new(Expr::Num(Num::from_i64(1))),
                    ),
                    Err(None) => return self.error("un membre d'enum apres une chaine demande une valeur"),
                };
                next = match next {
                    Ok(n) => Ok(n.add(Num::from_i64(1))),
                    Err(_) => Err(Some(member.clone())),
                };
                (value, true)
            };
            let slot = |key: Expr| Pattern::Target(Box::new(Expr::Index(Box::new(Expr::Ident(name.clone())), Box::new(key), false)));
            let assign = Expr::Assign(AssignOp::Assign, Box::new(slot(Expr::Str(member.clone()))), Box::new(value));
            let statement = if numeric {
                // E[E["A"] = 0] = "A"
                Expr::Assign(AssignOp::Assign, Box::new(slot(assign)), Box::new(Expr::Str(member.clone())))
            } else {
                assign
            };
            body.push(Stmt::Expr(statement));
            members.push(member);
            if !self.is("}") {
                self.expect(",")?;
            }
        }
        Ok(Stmt::Block(body))
    }

    // --------------------------------------------------------
    //  Classes and parameters
    // --------------------------------------------------------

    /// `public`, `private`, `protected`, `readonly`, `override` before a
    /// parameter: it is also a property. Returns whether one was read.
    pub(super) fn parameter_modifiers(&mut self) -> bool {
        if !self.options.typescript {
            return false;
        }
        let mut any = false;
        while matches!(self.peek(), Tok::Ident(w) if matches!(w.as_str(), "public" | "private" | "protected" | "readonly" | "override"))
            && matches!(self.peek_at(1), Tok::Ident(_) | Tok::Punct("{") | Tok::Punct("["))
        {
            self.advance();
            any = true;
        }
        any
    }

    /// Modifiers of a class member that mean nothing at run time.
    /// Returns whether the member is `abstract` or `declare` (no value).
    pub(super) fn member_modifiers(&mut self) -> bool {
        let mut ambient = false;
        while self.options.typescript
            && matches!(self.peek(), Tok::Ident(w) if matches!(w.as_str(), "public" | "private" | "protected" | "readonly" | "abstract" | "override" | "declare" | "accessor"))
            && self.modifier_applies()
        {
            if self.is_word("abstract") || self.is_word("declare") {
                ambient = true;
            }
            self.advance();
        }
        ambient
    }

    /// `[key: string]: T;` in a class body.
    pub(super) fn index_signature_ahead(&self) -> bool {
        self.options.typescript
            && self.is("[")
            && matches!(self.peek_at(1), Tok::Ident(_))
            && self.punct_at(2, ":")
    }

    /// `this.x = x` for each parameter property, after `super(...)` when
    /// the constructor calls it.
    pub(super) fn with_parameter_properties(&mut self, func: Rc<Func>) -> Rc<Func> {
        let properties = core::mem::take(&mut self.param_properties);
        if properties.is_empty() {
            return func;
        }
        let mut func = (*func).clone();
        let assignments: Vec<Stmt> = properties
            .into_iter()
            .map(|name| {
                let target = Expr::Member(Box::new(Expr::This), name.clone(), false);
                Stmt::Expr(Expr::Assign(AssignOp::Assign, Box::new(Pattern::Target(Box::new(target))), Box::new(Expr::Ident(name))))
            })
            .collect();
        let after_super = func
            .body
            .iter()
            .position(|s| matches!(s, Stmt::Expr(Expr::SuperCall(_))))
            .map_or(0, |i| i + 1);
        for (offset, statement) in assignments.into_iter().enumerate() {
            func.body.insert(after_super + offset, statement);
        }
        Rc::new(func)
    }
}

/// In an enum initializer, the earlier members are names: `B = A * 2`
/// reads `E.A`.
fn enum_references(expr: Expr, members: &[JsStr], name: &JsStr) -> Expr {
    let again = |e: Box<Expr>| Box::new(enum_references(*e, members, name));
    match expr {
        Expr::Ident(n) if members.iter().any(|m| *m == n) => Expr::Member(Box::new(Expr::Ident(name.clone())), n, false),
        Expr::Binary(op, a, b) => Expr::Binary(op, again(a), again(b)),
        Expr::Logic(op, a, b) => Expr::Logic(op, again(a), again(b)),
        Expr::Unary(op, a) => Expr::Unary(op, again(a)),
        Expr::Cond(a, b, c) => Expr::Cond(again(a), again(b), again(c)),
        other => other,
    }
}
