//! The parser: tokens to a typed tree (translation phase 7).
//!
//! Recursive descent over the grammar of C11 6.5 to 6.9, with the types
//! computed as the nodes are built (`expr.rs`). Declarations are the hard
//! part of C's grammar -- a declarator reads inside out, `int (*f)(int)`
//! -- and they live here; statements in `stmt.rs`, initializers and
//! constant expressions in `init.rs`.
//!
//! What the subset leaves out is refused **by name** when the parser
//! meets it (bit-fields, variable-length arrays, `long double`,
//! `_Complex`, `_Generic`, wide strings): a program outside the subset is
//! told which construct, not given "unexpected token".

mod expr;
mod init;
mod stmt;

use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

use super::CcError;
use super::ast::*;
use super::lex::{Kind as TokKind, Loc, Token};
use super::ty::{self, Kind, Member, Record, Type};

#[derive(Clone)]
enum Binding {
    Local(usize),
    /// An object or a function with linkage, by label.
    Global(Rc<str>, Rc<Type>),
    Typedef(Rc<Type>),
    Enum(i64),
}

#[derive(Default)]
struct Scope {
    names: BTreeMap<Rc<str>, Binding>,
    tags: BTreeMap<Rc<str>, Rc<Type>>,
}

/// Storage class and function specifiers of a declaration.
#[derive(Default, Clone, Copy)]
struct Storage {
    typedef: bool,
    is_static: bool,
    is_extern: bool,
}

/// A declarator's result: the type, the name, and -- for a function --
/// the names of its parameters.
struct Declarator {
    ty: Rc<Type>,
    name: Option<Token>,
    params: Vec<(Option<Token>, Rc<Type>)>,
}

/// The `switch` being parsed.
struct SwitchState {
    cases: Vec<(i64, u32)>,
    default: Option<u32>,
    ty: Rc<Type>,
}

pub struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    scopes: Vec<Scope>,
    pub program: Program,
    /// Index of each global label in `program.globals`.
    global_index: BTreeMap<Rc<str>, usize>,
    /// Labels of the functions defined so far.
    defined: Vec<Rc<str>>,
    /// Translation unit number: makes the labels of `static` names
    /// distinct between files.
    unit: u32,
    labels: u32,
    unique: u32,
    // The function being parsed.
    locals: Vec<Local>,
    references: Vec<Rc<str>>,
    goto_targets: BTreeMap<Rc<str>, u32>,
    goto_uses: Vec<(Rc<str>, Loc)>,
    goto_defined: Vec<Rc<str>>,
    exit: Option<u32>,
    next: Option<u32>,
    switch: Option<SwitchState>,
    returns: Option<Rc<Type>>,
    function_name: Rc<str>,
    return_buffer: Option<i64>,
    va_offset: i64,
    variadic: bool,
}

/// Label of a name with external linkage.
pub fn external_label(name: &str) -> Rc<str> {
    Rc::from(alloc::format!("_{}", name).as_str())
}

impl Parser {
    pub fn new() -> Parser {
        Parser {
            tokens: Vec::new(),
            pos: 0,
            scopes: Vec::new(),
            program: Program::default(),
            global_index: BTreeMap::new(),
            defined: Vec::new(),
            unit: 0,
            labels: 0,
            unique: 0,
            locals: Vec::new(),
            references: Vec::new(),
            goto_targets: BTreeMap::new(),
            goto_uses: Vec::new(),
            goto_defined: Vec::new(),
            exit: None,
            next: None,
            switch: None,
            returns: None,
            function_name: Rc::from(""),
            return_buffer: None,
            va_offset: 0,
            variadic: false,
        }
    }

    /// Parses one translation unit into `program`.
    pub fn unit(&mut self, tokens: Vec<Token>) -> Result<(), CcError> {
        self.tokens = tokens;
        self.pos = 0;
        self.unit += 1;
        self.scopes = alloc::vec![Scope::default()];
        while !self.at_end() {
            if self.eat(";") {
                continue;
            }
            if self.peek_is("_Static_assert") {
                self.static_assert()?;
                continue;
            }
            let mut storage = Storage::default();
            let base = self.declspec(Some(&mut storage))?;
            if storage.typedef {
                self.typedef(base)?;
                continue;
            }
            if self.eat(";") {
                continue;
            }
            let first = self.declarator(base.clone())?;
            if first.ty.kind == Kind::Func && (self.peek_is("{") || self.is_typename()) {
                if self.is_typename() {
                    return Err(self.error("les definitions de fonction a l'ancienne (K&R) ne font pas partie du C de Nova"));
                }
                self.function(first, storage)?;
                continue;
            }
            self.global_declaration(first, storage)?;
            while self.eat(",") {
                let next = self.declarator(base.clone())?;
                self.global_declaration(next, storage)?;
            }
            self.expect(";")?;
        }
        Ok(())
    }

    // ------------------------------------------------------------
    //  Tokens
    // ------------------------------------------------------------

    fn at_end(&self) -> bool {
        self.pos >= self.tokens.len()
    }

    fn token(&self) -> Token {
        self.tokens.get(self.pos).cloned().unwrap_or_else(|| self.end_token())
    }

    fn end_token(&self) -> Token {
        let mut end = self.tokens.last().cloned().unwrap_or(Token {
            kind: TokKind::Punct,
            text: Rc::from(""),
            loc: Loc::default(),
            bol: false,
            space: false,
            hide: Rc::new(Vec::new()),
        });
        end.text = Rc::from("<fin du fichier>");
        end.kind = TokKind::Punct;
        end
    }

    fn loc(&self) -> Loc {
        self.tokens.get(self.pos).or(self.tokens.last()).map(|t| t.loc).unwrap_or_default()
    }

    fn peek_is(&self, text: &str) -> bool {
        self.tokens.get(self.pos).is_some_and(|t| t.is(text))
    }

    fn peek_at(&self, offset: usize, text: &str) -> bool {
        self.tokens.get(self.pos + offset).is_some_and(|t| t.is(text))
    }

    fn eat(&mut self, text: &str) -> bool {
        if self.peek_is(text) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, text: &str) -> Result<(), CcError> {
        if self.eat(text) {
            Ok(())
        } else {
            Err(self.error(&alloc::format!("\"{}\" attendu, \"{}\" trouve", text, self.token().text)))
        }
    }

    fn error(&self, message: &str) -> CcError {
        CcError::at(self.loc(), message)
    }

    fn ident(&mut self) -> Result<Token, CcError> {
        let token = self.token();
        if token.kind != TokKind::Ident || is_keyword(&token.text) {
            return Err(self.error(&alloc::format!("identificateur attendu, \"{}\" trouve", token.text)));
        }
        self.pos += 1;
        Ok(token)
    }

    fn new_label(&mut self) -> u32 {
        self.labels += 1;
        self.labels
    }

    // ------------------------------------------------------------
    //  Scopes
    // ------------------------------------------------------------

    fn enter(&mut self) {
        self.scopes.push(Scope::default());
    }

    fn leave(&mut self) {
        self.scopes.pop();
    }

    fn lookup(&self, name: &str) -> Option<Binding> {
        self.scopes.iter().rev().find_map(|s| s.names.get(name).cloned())
    }

    fn lookup_tag(&self, name: &str) -> Option<Rc<Type>> {
        self.scopes.iter().rev().find_map(|s| s.tags.get(name).cloned())
    }

    fn bind(&mut self, name: &Rc<str>, binding: Binding) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.names.insert(name.clone(), binding);
        }
    }

    fn typedef_of(&self, token: &Token) -> Option<Rc<Type>> {
        if token.kind != TokKind::Ident {
            return None;
        }
        match self.lookup(&token.text)? {
            Binding::Typedef(ty) => Some(ty),
            _ => None,
        }
    }

    fn is_typename(&self) -> bool {
        self.is_typename_at(self.pos)
    }

    fn is_typename_at(&self, pos: usize) -> bool {
        let Some(token) = self.tokens.get(pos) else { return false };
        if token.kind != TokKind::Ident {
            return false;
        }
        const NAMES: &[&str] = &[
            "void", "_Bool", "char", "short", "int", "long", "float", "double", "signed", "unsigned", "struct",
            "union", "enum", "typedef", "static", "extern", "inline", "const", "volatile", "restrict", "auto",
            "register", "_Noreturn", "_Alignas", "__restrict", "__inline", "__attribute__", "__extension__",
            "_Thread_local", "__signed__", "_Complex",
        ];
        NAMES.contains(&&*token.text) || self.typedef_of(token).is_some()
    }

    fn new_local(&mut self, name: &Rc<str>, ty: Rc<Type>) -> usize {
        self.locals.push(Local { name: name.clone(), ty, offset: 0, param: false });
        let index = self.locals.len() - 1;
        self.bind(name, Binding::Local(index));
        index
    }

    /// An unnamed local (structure returned by a call, compound literal).
    fn temporary(&mut self, ty: Rc<Type>) -> usize {
        self.locals.push(Local { name: Rc::from(""), ty, offset: 0, param: false });
        self.locals.len() - 1
    }

    /// Records a global object or function, merging with an earlier
    /// declaration of the same label.
    fn declare_global(&mut self, label: &Rc<str>, ty: Rc<Type>, defined: bool) -> usize {
        if let Some(&index) = self.global_index.get(label) {
            let global = &mut self.program.globals[index];
            // `int a[];` then `int a[10];`: the complete type wins.
            if !global.ty.is_complete() && ty.is_complete() {
                global.ty = ty;
            }
            global.defined |= defined;
            return index;
        }
        self.program.globals.push(Global { label: label.clone(), ty, data: None, relocs: Vec::new(), defined });
        let index = self.program.globals.len() - 1;
        self.global_index.insert(label.clone(), index);
        index
    }

    /// An anonymous global holding `bytes` (a string literal).
    fn anonymous(&mut self, ty: Rc<Type>, bytes: Vec<u8>) -> Rc<str> {
        self.unique += 1;
        let label: Rc<str> = Rc::from(alloc::format!("__cc_str{}", self.unique).as_str());
        let index = self.declare_global(&label, ty, true);
        self.program.globals[index].data = Some(bytes);
        label
    }

    fn global_declaration(&mut self, declarator: Declarator, storage: Storage) -> Result<(), CcError> {
        let Some(name) = declarator.name else {
            return Err(self.error("declaration sans nom"));
        };
        let ty = declarator.ty;
        let label = if storage.is_static { self.static_label(&name.text) } else { external_label(&name.text) };
        if let Some(Binding::Global(existing, _)) = self.scopes[0].names.get(&name.text) {
            // `static int f(void);` then `int f(void) { }`: one object.
            let existing = existing.clone();
            return self.global_object(&existing, &name, ty, storage);
        }
        self.bind(&name.text, Binding::Global(label.clone(), ty.clone()));
        if ty.kind == Kind::Func {
            self.declare_global(&label, ty, false);
            return Ok(());
        }
        self.global_object(&label, &name, ty, storage)
    }

    fn global_object(&mut self, label: &Rc<str>, name: &Token, ty: Rc<Type>, storage: Storage) -> Result<(), CcError> {
        if ty.kind == Kind::Func {
            return Ok(());
        }
        if self.eat("=") {
            let index = self.declare_global(label, ty.clone(), true);
            let (ty, data, relocs) = self.global_initializer(ty)?;
            let global = &mut self.program.globals[index];
            if global.data.is_some() {
                return Err(CcError::at(name.loc, &alloc::format!("{} initialise deux fois", name.text)));
            }
            global.ty = ty;
            global.data = Some(data);
            global.relocs = relocs;
            self.rebind_type(&name.text, label);
            return Ok(());
        }
        // `extern int x;` declares; `int x;` is a tentative definition.
        self.declare_global(label, ty, !storage.is_extern);
        Ok(())
    }

    /// After `int a[] = {1, 2}` the type has its length: the binding
    /// must see it.
    fn rebind_type(&mut self, name: &Rc<str>, label: &Rc<str>) {
        if let Some(&index) = self.global_index.get(label) {
            let ty = self.program.globals[index].ty.clone();
            for scope in self.scopes.iter_mut().rev() {
                if let Some(Binding::Global(l, t)) = scope.names.get_mut(name) {
                    if l == label {
                        *t = ty;
                        return;
                    }
                }
            }
        }
    }

    fn static_label(&self, name: &str) -> Rc<str> {
        Rc::from(alloc::format!("_{}.{}", name, self.unit).as_str())
    }

    fn typedef(&mut self, base: Rc<Type>) -> Result<(), CcError> {
        loop {
            let declarator = self.declarator(base.clone())?;
            let Some(name) = declarator.name else {
                return Err(self.error("typedef sans nom"));
            };
            self.bind(&name.text, Binding::Typedef(declarator.ty));
            if !self.eat(",") {
                break;
            }
        }
        self.expect(";")
    }

    fn static_assert(&mut self) -> Result<(), CcError> {
        let loc = self.loc();
        self.expect("_Static_assert")?;
        self.expect("(")?;
        let condition = self.const_expr()?;
        let mut message = String::from("_Static_assert echoue");
        if self.eat(",") {
            let token = self.token();
            if token.kind == TokKind::Str {
                message = alloc::format!("_Static_assert echoue : {}", &token.text[1..token.text.len() - 1]);
                self.pos += 1;
            }
        }
        self.expect(")")?;
        self.expect(";")?;
        if condition == 0 {
            return Err(CcError::at(loc, &message));
        }
        Ok(())
    }

    // ------------------------------------------------------------
    //  Functions
    // ------------------------------------------------------------

    fn function(&mut self, declarator: Declarator, storage: Storage) -> Result<(), CcError> {
        let Some(name) = declarator.name.clone() else {
            return Err(self.error("fonction sans nom"));
        };
        let ty = declarator.ty.clone();
        let label = match self.scopes[0].names.get(&name.text) {
            Some(Binding::Global(label, _)) => label.clone(),
            _ if storage.is_static => self.static_label(&name.text),
            _ => external_label(&name.text),
        };
        if self.defined.contains(&label) {
            return Err(CcError::at(name.loc, &alloc::format!("fonction {} definie deux fois", name.text)));
        }
        self.defined.push(label.clone());
        self.bind(&name.text, Binding::Global(label.clone(), ty.clone()));
        self.declare_global(&label, ty.clone(), true);

        self.locals.clear();
        self.references.clear();
        self.goto_targets.clear();
        self.goto_uses.clear();
        self.goto_defined.clear();
        self.returns = Some(ty.ret());
        self.function_name = name.text.clone();
        self.variadic = ty.variadic;
        self.enter();

        // Arguments sit above the return address, one word each (a
        // structure takes its size rounded up to words): see `gen`.
        let mut offset = 16;
        self.return_buffer = None;
        if ty.ret().is_record() {
            self.return_buffer = Some(offset);
            offset += 8;
        }
        for (param_name, param_ty) in declarator.params.iter() {
            let local_name: Rc<str> = param_name.as_ref().map(|t| t.text.clone()).unwrap_or_else(|| Rc::from(""));
            let index = self.new_local(&local_name, param_ty.clone());
            self.locals[index].offset = offset;
            self.locals[index].param = true;
            offset += ty::align_to(param_ty.size().max(1), 8);
        }
        self.va_offset = offset;

        // `__func__`.
        let mut bytes = Vec::from(name.text.as_bytes());
        bytes.push(0);
        let func_ty = Type::array(Type::char(false), bytes.len() as i64);
        let func_label = self.anonymous(func_ty.clone(), bytes);
        self.bind(&Rc::from("__func__"), Binding::Global(func_label.clone(), func_ty.clone()));
        self.bind(&Rc::from("__FUNCTION__"), Binding::Global(func_label, func_ty));

        let body = self.compound()?;
        self.leave();

        for (target, loc) in self.goto_uses.iter() {
            if !self.goto_defined.contains(target) {
                return Err(CcError::at(*loc, &alloc::format!("etiquette {} absente de la fonction", target)));
            }
        }

        self.program.functions.push(Function {
            label,
            name: name.text.clone(),
            ty,
            locals: core::mem::take(&mut self.locals),
            body,
            return_buffer: self.return_buffer,
            va_offset: self.va_offset,
            references: core::mem::take(&mut self.references),
        });
        Ok(())
    }

    // ------------------------------------------------------------
    //  Declaration specifiers
    // ------------------------------------------------------------

    /// Skips `__attribute__((...))`, which Nova's C accepts and ignores.
    fn attributes(&mut self) -> Result<(), CcError> {
        while self.peek_is("__attribute__") || self.peek_is("__attribute") {
            self.pos += 1;
            self.expect("(")?;
            let mut depth = 1;
            while depth > 0 {
                if self.at_end() {
                    return Err(self.error("__attribute__ non ferme"));
                }
                if self.peek_is("(") {
                    depth += 1;
                } else if self.peek_is(")") {
                    depth -= 1;
                }
                self.pos += 1;
            }
        }
        Ok(())
    }

    fn declspec(&mut self, mut storage: Option<&mut Storage>) -> Result<Rc<Type>, CcError> {
        // Specifier counts, as C11 6.7.2 lists the valid combinations.
        let (mut void, mut boolean, mut char, mut short, mut int, mut long, mut float, mut double) =
            (0, 0, 0, 0, 0, 0, 0, 0);
        let (mut signed, mut unsigned) = (false, false);
        let mut other: Option<Rc<Type>> = None;
        let start = self.loc();

        loop {
            self.attributes()?;
            let token = self.token();
            if token.kind != TokKind::Ident {
                break;
            }
            match &*token.text {
                "typedef" | "static" | "extern" | "inline" | "__inline" | "_Noreturn" | "register" | "auto"
                | "_Thread_local" => {
                    let Some(storage) = storage.as_deref_mut() else {
                        return Err(self.error(&alloc::format!("{} interdit ici", token.text)));
                    };
                    match &*token.text {
                        "typedef" => storage.typedef = true,
                        "static" => storage.is_static = true,
                        "extern" => storage.is_extern = true,
                        _ => {}
                    }
                    self.pos += 1;
                    continue;
                }
                "const" | "volatile" | "restrict" | "__restrict" | "__restrict__" | "__extension__" | "__const" => {
                    self.pos += 1;
                    continue;
                }
                "_Alignas" => {
                    // Accepted and ignored: Nova lays out by natural
                    // alignment only.
                    self.pos += 1;
                    self.expect("(")?;
                    if self.is_typename() {
                        self.typename()?;
                    } else {
                        self.const_expr()?;
                    }
                    self.expect(")")?;
                    continue;
                }
                "_Complex" | "_Imaginary" => {
                    return Err(self.error("les nombres complexes ne font pas partie du C de Nova"));
                }
                "_Atomic" => return Err(self.error("_Atomic ne fait pas partie du C de Nova")),
                "void" => void += 1,
                "_Bool" => boolean += 1,
                "char" => char += 1,
                "short" => short += 1,
                "int" => int += 1,
                "long" => long += 1,
                "float" => float += 1,
                "double" => double += 1,
                "signed" | "__signed__" => signed = true,
                "unsigned" => unsigned = true,
                "struct" | "union" => {
                    if other.is_some() {
                        break;
                    }
                    self.pos += 1;
                    other = Some(self.record(&*token.text == "union")?);
                    continue;
                }
                "enum" => {
                    if other.is_some() {
                        break;
                    }
                    self.pos += 1;
                    other = Some(self.enumeration()?);
                    continue;
                }
                _ => {
                    let any = void + boolean + char + short + int + long + float + double;
                    if other.is_none() && any == 0 && !signed && !unsigned {
                        if let Some(ty) = self.typedef_of(&token) {
                            other = Some(ty);
                            self.pos += 1;
                            continue;
                        }
                    }
                    break;
                }
            }
            self.pos += 1;
        }

        let any = void + boolean + char + short + int + long + float + double;
        if let Some(ty) = other {
            if any > 0 || signed || unsigned {
                return Err(CcError::at(start, "combinaison de types invalide"));
            }
            return Ok(ty);
        }
        if long > 0 && double > 0 {
            return Err(CcError::at(start, "long double ne fait pas partie du C de Nova (double seulement)"));
        }
        let invalid = || CcError::at(start, "combinaison de types invalide");
        if signed && unsigned {
            return Err(invalid());
        }
        let ty = match (void, boolean, char, short, int, long, float, double) {
            (1, 0, 0, 0, 0, 0, 0, 0) if !signed && !unsigned => Type::void(),
            (0, 1, 0, 0, 0, 0, 0, 0) if !signed && !unsigned => Type::bool(),
            (0, 0, 1, 0, 0, 0, 0, 0) => Type::char(unsigned),
            (0, 0, 0, 1, 0..=1, 0, 0, 0) => Type::short(unsigned),
            (0, 0, 0, 0, 0..=1, 1..=2, 0, 0) => Type::integer(Kind::Long, unsigned),
            (0, 0, 0, 0, 1, 0, 0, 0) | (0, 0, 0, 0, 0, 0, 0, 0) => {
                if any == 0 && !signed && !unsigned {
                    return Err(CcError::at(start, &alloc::format!("type attendu, \"{}\" trouve", self.token().text)));
                }
                Type::integer(Kind::Int, unsigned)
            }
            (0, 0, 0, 0, 0, 0, 1, 0) if !signed && !unsigned => Type::float(),
            (0, 0, 0, 0, 0, 0, 0, 1) if !signed && !unsigned => Type::double(),
            _ => return Err(invalid()),
        };
        Ok(ty)
    }

    fn record(&mut self, union: bool) -> Result<Rc<Type>, CcError> {
        self.attributes()?;
        let kind = if union { Kind::Union } else { Kind::Struct };
        let tag = if self.token().kind == TokKind::Ident && !self.peek_is("{") { Some(self.ident()?) } else { None };

        if !self.peek_is("{") {
            let Some(tag) = tag else {
                return Err(self.error("struct sans nom ni corps"));
            };
            // `struct s;` alone declares a new incomplete type here.
            let alone = self.peek_is(";");
            if !alone {
                if let Some(ty) = self.lookup_tag(&tag.text) {
                    if ty.kind != kind {
                        return Err(CcError::at(tag.loc, "struct et union de meme nom"));
                    }
                    return Ok(ty);
                }
            }
            let ty = Type::record(kind, Rc::new(RefCell::new(Record { align: 1, ..Record::default() })));
            if let Some(scope) = self.scopes.last_mut() {
                scope.tags.insert(tag.text.clone(), ty.clone());
            }
            return Ok(ty);
        }

        // A definition: completes a declaration of this scope, or makes a
        // new type.
        self.expect("{")?;
        let ty = match &tag {
            Some(tag) => match self.scopes.last().and_then(|s| s.tags.get(&tag.text)).cloned() {
                Some(existing) if existing.kind == kind && !existing.is_complete() => existing,
                Some(existing) if existing.is_complete() => {
                    return Err(CcError::at(tag.loc, &alloc::format!("struct {} definie deux fois", tag.text)));
                }
                _ => Type::record(kind, Rc::new(RefCell::new(Record { align: 1, ..Record::default() }))),
            },
            None => Type::record(kind, Rc::new(RefCell::new(Record { align: 1, ..Record::default() }))),
        };
        if let Some(tag) = &tag {
            if let Some(scope) = self.scopes.last_mut() {
                scope.tags.insert(tag.text.clone(), ty.clone());
            }
        }

        let mut members: Vec<Member> = Vec::new();
        let mut flexible = false;
        while !self.eat("}") {
            if self.peek_is("_Static_assert") {
                self.static_assert()?;
                continue;
            }
            let base = self.declspec(None)?;
            // An anonymous struct or union member.
            if base.is_record() && self.eat(";") {
                members.push(Member { name: None, ty: base, offset: 0 });
                continue;
            }
            loop {
                let declarator = self.declarator(base.clone())?;
                if self.peek_is(":") {
                    return Err(self.error("les champs de bits ne font pas partie du C de Nova"));
                }
                if flexible {
                    return Err(self.error("un tableau flexible doit etre le dernier membre"));
                }
                let mut member_ty = declarator.ty;
                if member_ty.kind == Kind::Array && member_ty.len < 0 {
                    flexible = true;
                    member_ty = Type::array(member_ty.base(), 0);
                } else if !member_ty.is_complete() {
                    return Err(self.error("membre de type incomplet"));
                }
                members.push(Member { name: declarator.name.map(|t| t.text), ty: member_ty, offset: 0 });
                if !self.eat(",") {
                    break;
                }
            }
            self.expect(";")?;
        }
        self.attributes()?;

        let mut size = 0;
        let mut align = 1;
        for member in members.iter_mut() {
            let member_align = member.ty.align();
            align = align.max(member_align);
            if union {
                size = size.max(member.ty.size());
            } else {
                member.offset = ty::align_to(size, member_align);
                size = member.offset + member.ty.size();
            }
        }
        if let Some(record) = &ty.record {
            let mut record = record.borrow_mut();
            record.members = members;
            record.size = ty::align_to(size, align);
            record.align = align;
            record.complete = true;
            record.flexible = flexible;
        }
        Ok(ty)
    }

    fn enumeration(&mut self) -> Result<Rc<Type>, CcError> {
        self.attributes()?;
        let tag = if self.token().kind == TokKind::Ident && !self.peek_is("{") { Some(self.ident()?) } else { None };
        if !self.peek_is("{") {
            let Some(tag) = tag else {
                return Err(self.error("enum sans nom ni corps"));
            };
            return match self.lookup_tag(&tag.text) {
                Some(ty) => Ok(ty),
                None => Err(CcError::at(tag.loc, &alloc::format!("enum {} inconnue", tag.text))),
            };
        }
        self.expect("{")?;
        let mut value: i64 = 0;
        while !self.eat("}") {
            let name = self.ident()?;
            if self.eat("=") {
                value = self.const_expr()?;
            }
            self.bind(&name.text, Binding::Enum(value));
            value += 1;
            if !self.eat(",") {
                self.expect("}")?;
                break;
            }
        }
        let ty = Type::int();
        if let Some(tag) = tag {
            if let Some(scope) = self.scopes.last_mut() {
                scope.tags.insert(tag.text.clone(), ty.clone());
            }
        }
        Ok(ty)
    }

    // ------------------------------------------------------------
    //  Declarators
    // ------------------------------------------------------------

    fn pointers(&mut self, mut ty: Rc<Type>) -> Result<Rc<Type>, CcError> {
        while self.eat("*") {
            ty = Type::pointer(ty);
            loop {
                self.attributes()?;
                if !(self.eat("const") || self.eat("volatile") || self.eat("restrict") || self.eat("__restrict") || self.eat("__restrict__")) {
                    break;
                }
            }
        }
        Ok(ty)
    }

    /// `declarator = "*"* ("(" declarator ")" | ident)? suffix`
    ///
    /// The parenthesised inner declarator applies to the type *after*
    /// the suffix (`int (*p)[3]`: p is a pointer to an array of 3): it
    /// is skipped, the suffix read, then the inner part read again with
    /// the complete outer type.
    fn declarator(&mut self, ty: Rc<Type>) -> Result<Declarator, CcError> {
        let ty = self.pointers(ty)?;
        self.attributes()?;
        if self.peek_is("(") && !self.starts_parameters() {
            let inner = self.pos + 1;
            self.skip_parens()?;
            let outer = self.suffix(ty)?;
            let after = self.pos;
            self.pos = inner;
            let declarator = self.declarator(outer.ty)?;
            self.expect(")")?;
            self.pos = after;
            // The parameters belong to the innermost function.
            let params = if declarator.params.is_empty() { outer.params } else { declarator.params };
            return Ok(Declarator { params, ..declarator });
        }
        let name = if self.token().kind == TokKind::Ident && !is_keyword(&self.token().text) {
            let token = self.token();
            self.pos += 1;
            Some(token)
        } else {
            None
        };
        let mut declarator = self.suffix(ty)?;
        declarator.name = name;
        self.attributes()?;
        Ok(declarator)
    }

    /// Is the `(` here a parameter list rather than a nested declarator?
    /// `int (int)` in an abstract declarator, `void ()`.
    fn starts_parameters(&self) -> bool {
        match self.tokens.get(self.pos + 1) {
            Some(t) if t.is(")") || t.is("...") => true,
            Some(_) => self.is_typename_at(self.pos + 1),
            None => false,
        }
    }

    fn skip_parens(&mut self) -> Result<(), CcError> {
        let mut depth = 0;
        loop {
            if self.at_end() {
                return Err(self.error("parenthese non fermee"));
            }
            if self.peek_is("(") {
                depth += 1;
            } else if self.peek_is(")") {
                depth -= 1;
                if depth == 0 {
                    self.pos += 1;
                    return Ok(());
                }
            }
            self.pos += 1;
        }
    }

    fn suffix(&mut self, ty: Rc<Type>) -> Result<Declarator, CcError> {
        if self.eat("(") {
            return self.parameters(ty);
        }
        if self.eat("[") {
            while self.eat("static") || self.eat("const") || self.eat("restrict") || self.eat("volatile") {}
            let len = if self.peek_is("]") {
                -1
            } else {
                let loc = self.loc();
                let expr = self.conditional()?;
                match self.eval(&expr) {
                    Ok(n) if n >= 0 => n,
                    Ok(_) => return Err(CcError::at(loc, "taille de tableau negative")),
                    Err(_) => {
                        return Err(CcError::at(loc, "les tableaux de taille variable (VLA) ne font pas partie du C de Nova"));
                    }
                }
            };
            self.expect("]")?;
            let inner = self.suffix(ty)?;
            if inner.ty.kind == Kind::Func {
                return Err(self.error("tableau de fonctions"));
            }
            return Ok(Declarator { ty: Type::array(inner.ty, len), name: None, params: Vec::new() });
        }
        Ok(Declarator { ty, name: None, params: Vec::new() })
    }

    fn parameters(&mut self, ret: Rc<Type>) -> Result<Declarator, CcError> {
        if ret.kind == Kind::Array {
            return Err(self.error("une fonction ne peut pas rendre un tableau"));
        }
        if ret.kind == Kind::Func {
            return Err(self.error("une fonction ne peut pas rendre une fonction"));
        }
        if self.peek_is("void") && self.peek_at(1, ")") {
            self.pos += 2;
            return Ok(Declarator { ty: Type::function(ret, Vec::new(), false, true), name: None, params: Vec::new() });
        }
        if self.eat(")") {
            return Ok(Declarator { ty: Type::function(ret, Vec::new(), false, false), name: None, params: Vec::new() });
        }
        let mut params = Vec::new();
        let mut types = Vec::new();
        let mut variadic = false;
        loop {
            if self.eat("...") {
                variadic = true;
                self.expect(")")?;
                break;
            }
            let base = self.declspec(None)?;
            let declarator = self.declarator(base)?;
            // Arrays and functions as parameters are pointers
            // (C11 6.7.6.3p7-8).
            let ty = match declarator.ty.kind {
                Kind::Array => Type::pointer(declarator.ty.base()),
                Kind::Func => Type::pointer(declarator.ty),
                _ => declarator.ty,
            };
            if ty.kind == Kind::Void {
                return Err(self.error("parametre de type void"));
            }
            types.push(ty.clone());
            params.push((declarator.name, ty));
            if self.eat(")") {
                break;
            }
            self.expect(",")?;
        }
        Ok(Declarator { ty: Type::function(ret, types, variadic, true), name: None, params })
    }

    /// A type name, as in a cast or `sizeof(type)`.
    fn typename(&mut self) -> Result<Rc<Type>, CcError> {
        let base = self.declspec(None)?;
        let declarator = self.declarator(base)?;
        if let Some(name) = declarator.name {
            return Err(CcError::at(name.loc, "nom inattendu dans un nom de type"));
        }
        Ok(declarator.ty)
    }
}

/// Words that cannot name a variable.
pub fn is_keyword(word: &str) -> bool {
    const KEYWORDS: &[&str] = &[
        "auto", "break", "case", "char", "const", "continue", "default", "do", "double", "else", "enum", "extern",
        "float", "for", "goto", "if", "inline", "int", "long", "register", "restrict", "return", "short", "signed",
        "sizeof", "static", "struct", "switch", "typedef", "union", "unsigned", "void", "volatile", "while",
        "_Alignas", "_Alignof", "_Atomic", "_Bool", "_Complex", "_Generic", "_Imaginary", "_Noreturn",
        "_Static_assert", "_Thread_local",
    ];
    KEYWORDS.contains(&word)
}
