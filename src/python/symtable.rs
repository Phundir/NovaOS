//! Scopes: where each name of a program lives.
//!
//! Python decides at compile time whether a name is a local of its
//! function (a slot), a cell shared with an inner function, a free
//! variable received from an enclosing one, or a global. The rules
//! (`global`, `nonlocal`, assignment anywhere in the body making a name
//! local, class bodies being invisible to the functions inside them,
//! comprehensions being functions whose walrus binds outside) are
//! CPython's `symtable.c`, applied in two passes: gather what each scope
//! binds and uses, then resolve from the outside in.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use super::ast::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeKind {
    Module,
    Class,
    Function,
    /// A comprehension or a generator expression.
    Comprehension,
}

/// Where a name lives, seen from one scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    /// A local slot (functions).
    Local,
    /// A local shared with an inner function.
    Cell,
    /// From an enclosing function.
    Free,
    /// `global` declared, or found nowhere around (functions).
    Global,
    /// The namespace dict (module and class bodies).
    Name,
}

#[derive(Debug)]
pub struct Scope {
    pub kind: ScopeKind,
    pub name: Rc<str>,
    pub params: Vec<Rc<str>>,
    pub bound: BTreeSet<Rc<str>>,
    pub used: BTreeSet<Rc<str>>,
    pub globals: BTreeSet<Rc<str>>,
    pub nonlocals: BTreeSet<Rc<str>>,
    pub children: Vec<usize>,
    pub parent: Option<usize>,
    /// Resolved places, filled by the second pass.
    pub places: BTreeMap<Rc<str>, Place>,
    /// Free names it must receive (in order of first need).
    pub frees: Vec<Rc<str>>,
    /// Its locals inner scopes capture.
    pub cells: Vec<Rc<str>>,
    pub generator: bool,
    pub coroutine: bool,
    /// Uses `super` or `__class__`: the enclosing class gets a
    /// `__class__` cell.
    pub needs_class_cell: bool,
}

pub struct Table {
    pub scopes: Vec<Scope>,
    /// AST node address to scope index.
    pub by_node: BTreeMap<usize, usize>,
}

pub struct SymError {
    pub message: String,
    pub line: u32,
}

struct Builder {
    table: Table,
    current: usize,
}

fn node_key<T>(node: &T) -> usize {
    node as *const T as usize
}

pub fn function_key(def: &Rc<FunctionDef>) -> usize {
    Rc::as_ptr(def) as usize
}

pub fn class_key(def: &Rc<ClassDef>) -> usize {
    Rc::as_ptr(def) as usize
}

pub fn expr_key(expr: &Expr) -> usize {
    node_key(expr)
}

impl Builder {
    fn scope(&mut self) -> &mut Scope {
        &mut self.table.scopes[self.current]
    }

    fn push(&mut self, kind: ScopeKind, name: &str, key: usize) -> usize {
        let index = self.table.scopes.len();
        self.table.scopes.push(Scope {
            kind,
            name: Rc::from(name),
            params: Vec::new(),
            bound: BTreeSet::new(),
            used: BTreeSet::new(),
            globals: BTreeSet::new(),
            nonlocals: BTreeSet::new(),
            children: Vec::new(),
            parent: Some(self.current),
            places: BTreeMap::new(),
            frees: Vec::new(),
            cells: Vec::new(),
            generator: false,
            coroutine: false,
            needs_class_cell: false,
        });
        self.table.scopes[self.current].children.push(index);
        self.table.by_node.insert(key, index);
        index
    }

    fn bind(&mut self, name: &Rc<str>) {
        let scope = self.scope();
        scope.bound.insert(name.clone());
    }

    fn use_name(&mut self, name: &Rc<str>) {
        let scope = self.scope();
        scope.used.insert(name.clone());
    }

    fn body(&mut self, body: &[Stmt]) -> Result<(), SymError> {
        for statement in body {
            self.stmt(statement)?;
        }
        Ok(())
    }

    fn params(&mut self, params: &Params) -> Result<(), SymError> {
        // Defaults belong to the enclosing scope: visited by the caller.
        let names: Vec<Rc<str>> = params
            .posonly
            .iter()
            .chain(params.args.iter())
            .map(|p| p.name.clone())
            .chain(params.vararg.iter().cloned())
            .chain(params.kwonly.iter().map(|p| p.name.clone()))
            .chain(params.kwarg.iter().cloned())
            .collect();
        for name in &names {
            if self.scope().params.contains(name) {
                return Err(SymError { message: alloc::format!("parametre '{}' en double", name), line: 0 });
            }
            self.scope().params.push(name.clone());
            self.bind(name);
        }
        Ok(())
    }

    fn defaults(&mut self, params: &Params) -> Result<(), SymError> {
        for param in params.posonly.iter().chain(params.args.iter()).chain(params.kwonly.iter()) {
            if let Some(default) = &param.default {
                self.expr(default)?;
            }
        }
        Ok(())
    }

    fn target(&mut self, target: &Expr) -> Result<(), SymError> {
        match &target.kind {
            ExprKind::Name(name) => {
                self.bind(name);
                Ok(())
            }
            ExprKind::Tuple(items) | ExprKind::List(items) => {
                for item in items {
                    self.target(item)?;
                }
                Ok(())
            }
            ExprKind::Starred(inner) => self.target(inner),
            _ => self.expr(target),
        }
    }

    fn stmt(&mut self, statement: &Stmt) -> Result<(), SymError> {
        match &statement.kind {
            StmtKind::Expr(e) => self.expr(e),
            StmtKind::Assign(targets, value) => {
                self.expr(value)?;
                for target in targets {
                    self.target(target)?;
                }
                Ok(())
            }
            StmtKind::AugAssign(target, _, value) => {
                self.expr(value)?;
                if let ExprKind::Name(name) = &target.kind {
                    self.use_name(name);
                }
                self.target(target)
            }
            StmtKind::AnnAssign(target, annotation, value) => {
                // Annotations of names are evaluated in modules and class
                // bodies (stored in `__annotations__`), not in functions.
                if self.scope().kind != ScopeKind::Function && matches!(target.kind, ExprKind::Name(_)) {
                    self.expr(annotation)?;
                }
                if let Some(value) = value {
                    self.expr(value)?;
                    self.target(target)?;
                } else if !matches!(target.kind, ExprKind::Name(_)) {
                    self.expr(target)?;
                } else if let ExprKind::Name(name) = &target.kind {
                    // `x: int` alone binds nothing, but makes x local.
                    if self.scope().kind == ScopeKind::Function {
                        self.bind(name);
                    }
                }
                Ok(())
            }
            StmtKind::If(test, body, orelse) | StmtKind::While(test, body, orelse) => {
                self.expr(test)?;
                self.body(body)?;
                self.body(orelse)
            }
            StmtKind::For(target, iter, body, orelse) => {
                self.expr(iter)?;
                self.target(target)?;
                self.body(body)?;
                self.body(orelse)
            }
            StmtKind::Break | StmtKind::Continue | StmtKind::Pass => Ok(()),
            StmtKind::Return(value) => value.as_ref().map_or(Ok(()), |v| self.expr(v)),
            StmtKind::FunctionDef(def) => {
                for decorator in &def.decorators {
                    self.expr(decorator)?;
                }
                self.defaults(&def.params)?;
                self.bind(&def.name);
                let saved = self.current;
                self.current = self.push(ScopeKind::Function, &def.name, function_key(def));
                self.scope().coroutine = def.is_async;
                self.params(&def.params).map_err(|mut e| {
                    e.line = def.line;
                    e
                })?;
                self.body(&def.body)?;
                self.current = saved;
                Ok(())
            }
            StmtKind::ClassDef(def) => {
                for decorator in &def.decorators {
                    self.expr(decorator)?;
                }
                for base in &def.bases {
                    self.expr(base)?;
                }
                for (_, value) in &def.keywords {
                    self.expr(value)?;
                }
                self.bind(&def.name);
                let saved = self.current;
                self.current = self.push(ScopeKind::Class, &def.name, class_key(def));
                self.body(&def.body)?;
                self.current = saved;
                Ok(())
            }
            StmtKind::Try { body, handlers, orelse, finalbody } => {
                self.body(body)?;
                for handler in handlers {
                    if let Some(kind) = &handler.kind {
                        self.expr(kind)?;
                    }
                    if let Some(name) = &handler.name {
                        self.bind(name);
                    }
                    self.body(&handler.body)?;
                }
                self.body(orelse)?;
                self.body(finalbody)
            }
            StmtKind::Raise(exc, cause) => {
                if let Some(e) = exc {
                    self.expr(e)?;
                }
                if let Some(c) = cause {
                    self.expr(c)?;
                }
                Ok(())
            }
            StmtKind::With(items, body) => {
                for (context, target) in items {
                    self.expr(context)?;
                    if let Some(t) = target {
                        self.target(t)?;
                    }
                }
                self.body(body)
            }
            StmtKind::Import(names) => {
                for (dotted, alias) in names {
                    let bound: Rc<str> = match alias {
                        Some(a) => a.clone(),
                        None => Rc::from(dotted.split('.').next().unwrap_or(dotted)),
                    };
                    self.bind(&bound);
                }
                Ok(())
            }
            StmtKind::ImportFrom(_, _, names) => {
                for (name, alias) in names {
                    if &**name == "*" {
                        if self.scope().kind != ScopeKind::Module {
                            return Err(SymError { message: String::from("import * n'est permis qu'au niveau du module"), line: statement.line });
                        }
                        continue;
                    }
                    self.bind(alias.as_ref().unwrap_or(name));
                }
                Ok(())
            }
            StmtKind::Global(names) => {
                for name in names {
                    if self.scope().bound.contains(name) || self.scope().params.contains(name) {
                        // CPython: "name 'x' is assigned to before global
                        // declaration" -- only for parameters is it fatal.
                        if self.scope().params.contains(name) {
                            return Err(SymError { message: alloc::format!("le nom '{}' est un parametre et global", name), line: statement.line });
                        }
                    }
                    self.scope().globals.insert(name.clone());
                }
                Ok(())
            }
            StmtKind::Nonlocal(names) => {
                if matches!(self.scope().kind, ScopeKind::Module | ScopeKind::Class) {
                    return Err(SymError { message: String::from("nonlocal hors d'une fonction"), line: statement.line });
                }
                for name in names {
                    self.scope().nonlocals.insert(name.clone());
                }
                Ok(())
            }
            StmtKind::Delete(targets) => {
                for target in targets {
                    self.target(target)?;
                }
                Ok(())
            }
            StmtKind::Assert(test, message) => {
                self.expr(test)?;
                message.as_ref().map_or(Ok(()), |m| self.expr(m))
            }
            StmtKind::Match(subject, cases) => {
                self.expr(subject)?;
                for case in cases {
                    self.pattern(&case.pattern)?;
                    if let Some(guard) = &case.guard {
                        self.expr(guard)?;
                    }
                    self.body(&case.body)?;
                }
                Ok(())
            }
        }
    }

    fn pattern(&mut self, pattern: &Pattern) -> Result<(), SymError> {
        match pattern {
            Pattern::Wildcard | Pattern::Singleton(_) | Pattern::Star(None) => Ok(()),
            Pattern::Capture(name) | Pattern::Star(Some(name)) => {
                self.bind(name);
                Ok(())
            }
            Pattern::Value(expr) => self.expr(expr),
            Pattern::Sequence(items) | Pattern::Or(items) => {
                for item in items {
                    self.pattern(item)?;
                }
                Ok(())
            }
            Pattern::Mapping(items, rest) => {
                for (key, value) in items {
                    self.expr(key)?;
                    self.pattern(value)?;
                }
                if let Some(rest) = rest {
                    self.bind(rest);
                }
                Ok(())
            }
            Pattern::Class(class, args, kwargs) => {
                self.expr(class)?;
                for arg in args {
                    self.pattern(arg)?;
                }
                for (_, value) in kwargs {
                    self.pattern(value)?;
                }
                Ok(())
            }
            Pattern::As(inner, name) => {
                self.pattern(inner)?;
                self.bind(name);
                Ok(())
            }
        }
    }

    fn comprehension(&mut self, node: &Expr, elements: &[&Expr], generators: &[Comprehension], generator: bool) -> Result<(), SymError> {
        // The first iterable is evaluated outside.
        if let Some(first) = generators.first() {
            self.expr(&first.iter)?;
        }
        let saved = self.current;
        self.current = self.push(ScopeKind::Comprehension, "<listcomp>", expr_key(node));
        let dot: Rc<str> = Rc::from(".0");
        self.scope().params.push(dot.clone());
        self.bind(&dot);
        self.scope().generator = generator;
        if generators.iter().any(|c| c.is_async) {
            self.scope().coroutine = true;
        }
        for (index, clause) in generators.iter().enumerate() {
            if index > 0 {
                self.expr(&clause.iter)?;
            }
            self.target(&clause.target)?;
            for condition in &clause.ifs {
                self.expr(condition)?;
            }
        }
        for element in elements {
            self.expr(element)?;
        }
        self.current = saved;
        Ok(())
    }

    fn expr(&mut self, expr: &Expr) -> Result<(), SymError> {
        match &expr.kind {
            ExprKind::Name(name) => {
                if &**name == "super" || &**name == "__class__" {
                    self.scope().needs_class_cell = true;
                }
                self.use_name(name);
                Ok(())
            }
            ExprKind::Const(_) => Ok(()),
            ExprKind::FString(parts) => self.fparts(parts),
            ExprKind::BinOp(_, a, b) => {
                self.expr(a)?;
                self.expr(b)
            }
            ExprKind::Unary(_, a) | ExprKind::Starred(a) | ExprKind::Attribute(a, _) => self.expr(a),
            ExprKind::BoolOp(_, values) | ExprKind::List(values) | ExprKind::Tuple(values) | ExprKind::Set(values) => {
                for v in values {
                    self.expr(v)?;
                }
                Ok(())
            }
            ExprKind::Compare(first, rest) => {
                self.expr(first)?;
                for (_, e) in rest {
                    self.expr(e)?;
                }
                Ok(())
            }
            ExprKind::Call(function, args) => {
                self.expr(function)?;
                for arg in args {
                    match arg {
                        Arg::Positional(e) | Arg::Starred(e) | Arg::Keyword(_, e) | Arg::DoubleStarred(e) => self.expr(e)?,
                    }
                }
                Ok(())
            }
            ExprKind::Subscript(a, b) => {
                self.expr(a)?;
                self.expr(b)
            }
            ExprKind::Slice(a, b, c) => {
                for part in [a, b, c].into_iter().flatten() {
                    self.expr(part)?;
                }
                Ok(())
            }
            ExprKind::Dict(pairs) => {
                for (k, v) in pairs {
                    if let Some(k) = k {
                        self.expr(k)?;
                    }
                    self.expr(v)?;
                }
                Ok(())
            }
            ExprKind::ListComp(element, generators) | ExprKind::SetComp(element, generators) => {
                self.comprehension(expr, &[element], generators, false)
            }
            ExprKind::Generator(element, generators) => self.comprehension(expr, &[element], generators, true),
            ExprKind::DictComp(key, value, generators) => self.comprehension(expr, &[key, value], generators, false),
            ExprKind::Lambda(params, body) => {
                self.defaults(params)?;
                let saved = self.current;
                self.current = self.push(ScopeKind::Function, "<lambda>", expr_key(expr));
                self.params(params).map_err(|mut e| {
                    e.line = expr.line;
                    e
                })?;
                self.expr(body)?;
                self.current = saved;
                Ok(())
            }
            ExprKind::IfExp(a, b, c) => {
                self.expr(a)?;
                self.expr(b)?;
                self.expr(c)
            }
            ExprKind::Named(name, value) => {
                self.expr(value)?;
                // In a comprehension, the walrus binds in the first
                // enclosing scope that is not one.
                let mut index = self.current;
                while self.table.scopes[index].kind == ScopeKind::Comprehension {
                    let scope = &mut self.table.scopes[index];
                    scope.nonlocals.insert(name.clone());
                    index = scope.parent.unwrap_or(0);
                }
                let target = &mut self.table.scopes[index];
                if target.kind == ScopeKind::Class && index != self.current {
                    return Err(SymError { message: String::from(":= dans une comprehension au niveau d'une classe"), line: expr.line });
                }
                target.bound.insert(name.clone());
                if index != self.current && target.kind == ScopeKind::Module {
                    // Bound at module level: the comprehension reads it
                    // as a global.
                    let mut i = self.current;
                    while self.table.scopes[i].kind == ScopeKind::Comprehension {
                        self.table.scopes[i].nonlocals.remove(name);
                        self.table.scopes[i].globals.insert(name.clone());
                        i = self.table.scopes[i].parent.unwrap_or(0);
                    }
                }
                Ok(())
            }
            ExprKind::Yield(value) => {
                self.scope().generator = true;
                value.as_ref().map_or(Ok(()), |v| self.expr(v))
            }
            ExprKind::Await(value) => {
                // An `await` makes a comprehension asynchronous.
                if self.scope().kind == ScopeKind::Comprehension {
                    self.scope().coroutine = true;
                }
                self.expr(value)
            }
            ExprKind::YieldFrom(value) => {
                self.scope().generator = true;
                self.expr(value)
            }
        }
    }

    fn fparts(&mut self, parts: &[FPart]) -> Result<(), SymError> {
        for part in parts {
            if let FPart::Field { value, spec, .. } = part {
                self.expr(value)?;
                if let Some(spec) = spec {
                    self.fparts(spec)?;
                }
            }
        }
        Ok(())
    }
}

/// Second pass: from the module down, each scope's names placed.
fn resolve(table: &mut Table, index: usize, enclosing: &BTreeSet<Rc<str>>) -> Result<(), SymError> {
    let kind = table.scopes[index].kind;
    // What the children may see from here (class bodies are skipped).
    let mut visible: BTreeSet<Rc<str>> = enclosing.clone();
    if matches!(kind, ScopeKind::Function | ScopeKind::Comprehension) {
        for name in table.scopes[index].bound.iter() {
            if !table.scopes[index].globals.contains(name) && !table.scopes[index].nonlocals.contains(name) {
                visible.insert(name.clone());
            }
        }
        for name in table.scopes[index].nonlocals.iter() {
            visible.insert(name.clone());
        }
    }
    if kind == ScopeKind::Class {
        // `__class__` is a cell of the class body, for the methods'
        // `super()`.
        visible.insert(Rc::from("__class__"));
    }
    if kind == ScopeKind::Module {
        visible.clear();
    }
    let children = table.scopes[index].children.clone();
    for child in &children {
        resolve(table, *child, &visible)?;
    }

    // Names the children need from here or above.
    let mut wanted: Vec<Rc<str>> = Vec::new();
    for child in &children {
        for name in table.scopes[*child].frees.clone() {
            if !wanted.contains(&name) {
                wanted.push(name);
            }
        }
    }

    let scope = &mut table.scopes[index];
    for name in scope.nonlocals.iter() {
        if !enclosing.contains(name) {
            return Err(SymError { message: alloc::format!("pas de liaison pour le nonlocal '{}'", name), line: 0 });
        }
    }
    let names: BTreeSet<Rc<str>> = scope.bound.iter().chain(scope.used.iter()).chain(scope.globals.iter()).cloned().collect();
    for name in names {
        let place = match kind {
            ScopeKind::Module => Place::Name,
            ScopeKind::Class => {
                if scope.globals.contains(&name) {
                    Place::Global
                } else if !scope.bound.contains(&name) && enclosing.contains(&name) && &*name != "__class__" {
                    Place::Free
                } else {
                    Place::Name
                }
            }
            ScopeKind::Function | ScopeKind::Comprehension => {
                if scope.globals.contains(&name) {
                    Place::Global
                } else if scope.nonlocals.contains(&name) {
                    Place::Free
                } else if scope.bound.contains(&name) {
                    Place::Local
                } else if enclosing.contains(&name) {
                    Place::Free
                } else {
                    Place::Global
                }
            }
        };
        scope.places.insert(name, place);
    }
    // Children's frees: cells here if bound here, frees passed through
    // otherwise.
    for name in wanted {
        match (kind, scope.places.get(&name).copied()) {
            (ScopeKind::Class, _) if &*name == "__class__" => {
                if !scope.cells.contains(&name) {
                    scope.cells.push(name.clone());
                }
            }
            (ScopeKind::Class, _) => {
                // A class body passes its enclosing function's variables
                // through to its methods.
                scope.places.entry(name.clone()).or_insert(Place::Free);
                if !scope.frees.contains(&name) {
                    scope.frees.push(name);
                }
            }
            (_, Some(Place::Local)) | (_, Some(Place::Cell)) => {
                scope.places.insert(name.clone(), Place::Cell);
                if !scope.cells.contains(&name) {
                    scope.cells.push(name);
                }
            }
            (ScopeKind::Module, _) => {}
            _ => {
                scope.places.insert(name.clone(), Place::Free);
                if !scope.frees.contains(&name) {
                    scope.frees.push(name);
                }
            }
        }
    }
    // A function's own frees.
    let own: Vec<Rc<str>> = scope.places.iter().filter(|(_, p)| **p == Place::Free).map(|(n, _)| n.clone()).collect();
    for name in own {
        if !scope.frees.contains(&name) {
            scope.frees.push(name);
        }
    }
    // `super()` in a method: `__class__` comes from the class body.
    if kind == ScopeKind::Function && scope.needs_class_cell {
        let class_cell: Rc<str> = Rc::from("__class__");
        if enclosing.contains(&class_cell) && !scope.frees.contains(&class_cell) {
            scope.frees.push(class_cell.clone());
            scope.places.insert(class_cell, Place::Free);
        }
    }
    Ok(())
}

/// Builds the table of a module.
pub fn build(body: &[Stmt]) -> Result<Table, SymError> {
    let mut builder = Builder {
        table: Table {
            scopes: alloc::vec![Scope {
                kind: ScopeKind::Module,
                name: Rc::from("<module>"),
                params: Vec::new(),
                bound: BTreeSet::new(),
                used: BTreeSet::new(),
                globals: BTreeSet::new(),
                nonlocals: BTreeSet::new(),
                children: Vec::new(),
                parent: None,
                places: BTreeMap::new(),
                frees: Vec::new(),
                cells: Vec::new(),
                generator: false,
                coroutine: false,
                needs_class_cell: false,
            }],
            by_node: BTreeMap::new(),
        },
        current: 0,
    };
    builder.body(body)?;
    let mut table = builder.table;
    resolve(&mut table, 0, &BTreeSet::new())?;
    Ok(table)
}
