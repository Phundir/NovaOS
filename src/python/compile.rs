//! Syntax tree to bytecode.
//!
//! One `Code` per function, class body, comprehension and module, in the
//! manner of CPython 3.8: the places of names come from `symtable`, the
//! exceptions from a block stack (`SetupFinally` ... `PopBlock`), and a
//! `return`, `break` or `continue` that leaves a `try`, a `with` or a
//! loop first emits what leaving each of them takes -- popping the block,
//! running the `finally` body (compiled again at that point), calling
//! `__exit__`, popping the loop's iterator.

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use super::ast::*;
use super::code::{Code, Op};
use super::int::Int;
use super::symtable::{self, Place, ScopeKind, Table};
use super::value::Value;

pub struct CompileError {
    pub message: String,
    pub line: u32,
}

type CResult<T> = Result<T, CompileError>;

/// What `return`, `break` and `continue` must undo on their way out.
#[derive(Clone)]
enum Block {
    While { top: usize, breaks: Vec<usize> },
    For { top: usize, breaks: Vec<usize> },
    /// The body of a `try` with handlers.
    TryExcept,
    /// The body of a `try` with a `finally`, and that body.
    FinallyTry(Rc<Vec<Stmt>>),
    /// The `finally` body on the exception path: the exception under.
    FinallyEnd,
    /// An `except` clause's body (and the name it bound).
    Handler(Option<Rc<str>>),
    /// The body of a `with`: `__exit__` under.
    With,
}

struct Unit {
    code: Code,
    scope: usize,
    blocks: Vec<Block>,
}

struct Compiler<'a> {
    table: &'a Table,
    units: Vec<Unit>,
    filename: Rc<str>,
    line: u32,
}

fn err<T>(message: &str, line: u32) -> CResult<T> {
    Err(CompileError { message: String::from(message), line })
}

impl<'a> Compiler<'a> {
    fn unit(&mut self) -> &mut Unit {
        let last = self.units.len() - 1;
        &mut self.units[last]
    }

    fn scope_kind(&self) -> ScopeKind {
        self.table.scopes[self.units[self.units.len() - 1].scope].kind
    }

    fn emit(&mut self, op: Op) -> usize {
        let line = self.line;
        let unit = self.unit();
        unit.code.ops.push(op);
        unit.code.lines.push(line);
        unit.code.ops.len() - 1
    }

    fn here(&mut self) -> usize {
        self.unit().code.ops.len()
    }

    /// Points the jump at `at` to `target`.
    fn patch(&mut self, at: usize, target: usize) {
        let target = target as u32;
        let op = &mut self.unit().code.ops[at];
        *op = match *op {
            Op::Jump(_) => Op::Jump(target),
            Op::PopJumpIfFalse(_) => Op::PopJumpIfFalse(target),
            Op::PopJumpIfTrue(_) => Op::PopJumpIfTrue(target),
            Op::JumpIfFalseOrPop(_) => Op::JumpIfFalseOrPop(target),
            Op::JumpIfTrueOrPop(_) => Op::JumpIfTrueOrPop(target),
            Op::ForIter(_) => Op::ForIter(target),
            Op::SetupFinally(_) => Op::SetupFinally(target),
            Op::SetupWith(_) => Op::SetupWith(target),
            Op::JumpIfNotExcMatch(_) => Op::JumpIfNotExcMatch(target),
            other => other,
        };
    }

    fn patch_here(&mut self, at: usize) {
        let here = self.here();
        self.patch(at, here);
    }

    fn constant(&mut self, value: Value) -> u32 {
        let consts = &mut self.unit().code.consts;
        // Strings and small ints are shared.
        for (i, existing) in consts.iter().enumerate() {
            let same = match (existing, &value) {
                (Value::Str(a), Value::Str(b)) => a == b,
                (Value::Int(Int::Small(a)), Value::Int(Int::Small(b))) => a == b,
                (Value::None, Value::None) => true,
                (Value::Bool(a), Value::Bool(b)) => a == b,
                _ => false,
            };
            if same {
                return i as u32;
            }
        }
        consts.push(value);
        (consts.len() - 1) as u32
    }

    fn load_const(&mut self, value: Value) {
        let index = self.constant(value);
        self.emit(Op::LoadConst(index));
    }

    fn name_index(&mut self, name: &str) -> u32 {
        let names = &mut self.unit().code.names;
        if let Some(i) = names.iter().position(|n| &**n == name) {
            return i as u32;
        }
        names.push(Rc::from(name));
        (names.len() - 1) as u32
    }

    fn varname_index(&mut self, name: &str) -> u32 {
        let vars = &mut self.unit().code.varnames;
        if let Some(i) = vars.iter().position(|n| &**n == name) {
            return i as u32;
        }
        vars.push(Rc::from(name));
        (vars.len() - 1) as u32
    }

    /// Index among cells then frees.
    fn deref_index(&mut self, name: &str) -> u32 {
        let code = &self.unit().code;
        if let Some(i) = code.cellvars.iter().position(|n| &**n == name) {
            return i as u32;
        }
        let cells = code.cellvars.len();
        if let Some(i) = code.freevars.iter().position(|n| &**n == name) {
            return (cells + i) as u32;
        }
        0
    }

    fn place(&self, name: &str) -> Place {
        let scope = &self.table.scopes[self.units[self.units.len() - 1].scope];
        match scope.places.get(name) {
            Some(place) => *place,
            None => match scope.kind {
                ScopeKind::Module | ScopeKind::Class => Place::Name,
                _ => Place::Global,
            },
        }
    }

    fn load_name(&mut self, name: &str) {
        match self.place(name) {
            Place::Local => {
                let i = self.varname_index(name);
                self.emit(Op::LoadFast(i));
            }
            Place::Cell => {
                let i = self.deref_index(name);
                self.emit(Op::LoadDeref(i));
            }
            Place::Free => {
                let i = self.deref_index(name);
                if self.scope_kind() == ScopeKind::Class {
                    self.emit(Op::LoadClassDeref(i));
                } else {
                    self.emit(Op::LoadDeref(i));
                }
            }
            Place::Global => {
                let i = self.name_index(name);
                self.emit(Op::LoadGlobal(i));
            }
            Place::Name => {
                let i = self.name_index(name);
                self.emit(Op::LoadName(i));
            }
        }
    }

    fn store_name(&mut self, name: &str) {
        match self.place(name) {
            Place::Local => {
                let i = self.varname_index(name);
                self.emit(Op::StoreFast(i));
            }
            Place::Cell | Place::Free => {
                let i = self.deref_index(name);
                self.emit(Op::StoreDeref(i));
            }
            Place::Global => {
                let i = self.name_index(name);
                self.emit(Op::StoreGlobal(i));
            }
            Place::Name => {
                let i = self.name_index(name);
                self.emit(Op::StoreName(i));
            }
        }
    }

    fn delete_name(&mut self, name: &str) {
        match self.place(name) {
            Place::Local => {
                let i = self.varname_index(name);
                self.emit(Op::DeleteFast(i));
            }
            Place::Cell | Place::Free => {
                let i = self.deref_index(name);
                self.emit(Op::DeleteDeref(i));
            }
            Place::Global => {
                let i = self.name_index(name);
                self.emit(Op::DeleteGlobal(i));
            }
            Place::Name => {
                let i = self.name_index(name);
                self.emit(Op::DeleteName(i));
            }
        }
    }

    // --------------------------------------------------------
    //  Units
    // --------------------------------------------------------

    fn new_unit(&mut self, scope: usize, name: &str, qualname: &str, params: Option<&Params>, line: u32) {
        let info = &self.table.scopes[scope];
        let mut code = Code {
            name: Rc::from(name),
            qualname: Rc::from(qualname),
            filename: self.filename.clone(),
            ops: Vec::new(),
            lines: Vec::new(),
            consts: Vec::new(),
            names: Vec::new(),
            varnames: Vec::new(),
            cellvars: info.cells.clone(),
            freevars: info.frees.clone(),
            cell_params: Vec::new(),
            argcount: 0,
            posonly: 0,
            kwonly: 0,
            varargs: false,
            varkw: false,
            generator: info.generator,
            uses_namespace: matches!(info.kind, ScopeKind::Module | ScopeKind::Class),
            first_line: line,
            doc: None,
        };
        if let Some(params) = params {
            for p in params.posonly.iter().chain(params.args.iter()) {
                code.varnames.push(p.name.clone());
            }
            code.posonly = params.posonly.len() as u32;
            code.argcount = (params.posonly.len() + params.args.len()) as u32;
            for p in params.kwonly.iter() {
                code.varnames.push(p.name.clone());
            }
            code.kwonly = params.kwonly.len() as u32;
            if let Some(v) = &params.vararg {
                code.varnames.push(v.clone());
                code.varargs = true;
            }
            if let Some(k) = &params.kwarg {
                code.varnames.push(k.clone());
                code.varkw = true;
            }
        } else if info.kind == ScopeKind::Comprehension {
            code.varnames.push(Rc::from(".0"));
            code.argcount = 1;
        }
        // Parameters that are cells: copied into their cell at entry.
        for (cell_index, cell) in code.cellvars.iter().enumerate() {
            if let Some(slot) = code.varnames.iter().position(|v| v == cell) {
                code.cell_params.push((slot as u32, cell_index as u32));
            }
        }
        self.units.push(Unit { code, scope, blocks: Vec::new() });
    }

    fn finish_unit(&mut self) -> Rc<Code> {
        let unit = self.units.pop().expect("unit");
        Rc::new(unit.code)
    }

    fn qualname(&self, name: &str) -> String {
        // The enclosing functions and classes, CPython's way.
        let mut parts: Vec<String> = Vec::new();
        for unit in self.units.iter().skip(1) {
            let scope = &self.table.scopes[unit.scope];
            match scope.kind {
                ScopeKind::Function => {
                    parts.push(String::from(&*unit.code.qualname));
                    parts = alloc::vec![alloc::format!("{}.<locals>", parts.last().cloned().unwrap_or_default())];
                }
                ScopeKind::Class => parts = alloc::vec![String::from(&*unit.code.qualname)],
                _ => {}
            }
        }
        match parts.last() {
            Some(prefix) => alloc::format!("{}.{}", prefix, name),
            None => String::from(name),
        }
    }

    /// Makes a function object from `code` on the stack: closure cells,
    /// then the code and its qualified name.
    fn make_closure(&mut self, code: Rc<Code>, flags: u8) {
        let mut flags = flags;
        if !code.freevars.is_empty() {
            for name in code.freevars.clone().iter() {
                // The cell of that name here: our own cell or our free.
                let i = self.deref_index(name);
                self.emit(Op::LoadClosure(i));
            }
            self.emit(Op::BuildTuple(code.freevars.len() as u32));
            flags |= 8;
        }
        let qualname = code.qualname.clone();
        let index = self.constant(Value::Code(code));
        self.emit(Op::LoadConst(index));
        self.load_const(Value::Str(qualname));
        self.emit(Op::MakeFunction(index, flags));
    }

    // --------------------------------------------------------
    //  Statements
    // --------------------------------------------------------

    fn body(&mut self, body: &[Stmt]) -> CResult<()> {
        for statement in body {
            self.stmt(statement)?;
        }
        Ok(())
    }

    fn stmt(&mut self, statement: &Stmt) -> CResult<()> {
        self.line = statement.line;
        match &statement.kind {
            StmtKind::Expr(e) => {
                self.expr(e)?;
                self.emit(Op::Pop);
            }
            StmtKind::Assign(targets, value) => {
                self.expr(value)?;
                for (i, target) in targets.iter().enumerate() {
                    if i + 1 < targets.len() {
                        self.emit(Op::Dup);
                    }
                    self.store_target(target)?;
                }
            }
            StmtKind::AugAssign(target, op, value) => self.aug_assign(target, *op, value)?,
            StmtKind::AnnAssign(target, annotation, value) => {
                if let Some(value) = value {
                    self.expr(value)?;
                    self.store_target(target)?;
                }
                if let ExprKind::Name(name) = &target.kind {
                    if self.scope_kind() == ScopeKind::Class || self.scope_kind() == ScopeKind::Module {
                        // __annotations__[name] = annotation, evaluated now
                        // as CPython 3.13 does.
                        self.expr(annotation)?;
                        let i = self.name_index("__annotations__");
                        self.emit(Op::LoadName(i));
                        self.load_const(Value::Str(name.clone()));
                        self.emit(Op::StoreSubscr);
                    }
                }
            }
            StmtKind::If(test, body, orelse) => {
                self.expr(test)?;
                let to_else = self.emit(Op::PopJumpIfFalse(0));
                self.body(body)?;
                if orelse.is_empty() {
                    self.patch_here(to_else);
                } else {
                    let to_end = self.emit(Op::Jump(0));
                    self.patch_here(to_else);
                    self.body(orelse)?;
                    self.patch_here(to_end);
                }
            }
            StmtKind::While(test, body, orelse) => {
                let top = self.here();
                self.expr(test)?;
                let exit = self.emit(Op::PopJumpIfFalse(0));
                self.unit().blocks.push(Block::While { top, breaks: Vec::new() });
                self.body(body)?;
                self.emit(Op::Jump(top as u32));
                let block = self.unit().blocks.pop();
                self.patch_here(exit);
                self.body(orelse)?;
                if let Some(Block::While { breaks, .. }) = block {
                    for b in breaks {
                        self.patch_here(b);
                    }
                }
            }
            StmtKind::For(target, iter, body, orelse) => {
                self.expr(iter)?;
                self.emit(Op::GetIter);
                let top = self.here();
                let exit = self.emit(Op::ForIter(0));
                self.store_target(target)?;
                self.unit().blocks.push(Block::For { top, breaks: Vec::new() });
                self.body(body)?;
                self.emit(Op::Jump(top as u32));
                let block = self.unit().blocks.pop();
                self.patch_here(exit);
                self.body(orelse)?;
                if let Some(Block::For { breaks, .. }) = block {
                    for b in breaks {
                        self.patch_here(b);
                    }
                }
            }
            StmtKind::Break => self.break_or_continue(true, statement.line)?,
            StmtKind::Continue => self.break_or_continue(false, statement.line)?,
            StmtKind::Pass => {}
            StmtKind::Return(value) => {
                if self.scope_kind() != ScopeKind::Function {
                    return err("'return' hors d'une fonction", statement.line);
                }
                match value {
                    Some(v) => self.expr(v)?,
                    None => self.load_const(Value::None),
                }
                let blocks = self.unit().blocks.clone();
                for block in blocks.iter().rev() {
                    self.unwind(block, true)?;
                }
                self.emit(Op::Return);
            }
            StmtKind::FunctionDef(def) => self.function_def(def)?,
            StmtKind::ClassDef(def) => self.class_def(def)?,
            StmtKind::Try { body, handlers, orelse, finalbody } => {
                if finalbody.is_empty() {
                    self.try_except(body, handlers, orelse)?;
                } else {
                    self.try_finally(body, handlers, orelse, finalbody)?;
                }
            }
            StmtKind::Raise(exc, cause) => {
                let mut count = 0;
                if let Some(e) = exc {
                    self.expr(e)?;
                    count = 1;
                    if let Some(c) = cause {
                        self.expr(c)?;
                        count = 2;
                    }
                }
                self.emit(Op::Raise(count));
            }
            StmtKind::With(items, body) => self.with(items, body)?,
            StmtKind::Import(names) => {
                for (dotted, alias) in names {
                    self.load_const(Value::None);
                    let i = self.name_index(dotted);
                    self.emit(Op::ImportName(i, 0));
                    match alias {
                        Some(alias) => {
                            // `import a.b as c` binds a.b itself.
                            for part in dotted.split('.').skip(1) {
                                let i = self.name_index(part);
                                self.emit(Op::LoadAttr(i));
                            }
                            self.store_name(alias);
                        }
                        None => {
                            let first = dotted.split('.').next().unwrap_or(dotted);
                            self.store_name(first);
                        }
                    }
                }
            }
            StmtKind::ImportFrom(module, level, names) => {
                let fromlist: Vec<Value> = names.iter().map(|(n, _)| Value::Str(n.clone())).collect();
                self.load_const(Value::tuple(fromlist));
                let module_name: &str = module.as_deref().unwrap_or("");
                let i = self.name_index(module_name);
                self.emit(Op::ImportName(i, *level as u32));
                if names.len() == 1 && &*names[0].0 == "*" {
                    self.emit(Op::ImportStar);
                } else {
                    for (name, alias) in names {
                        let i = self.name_index(name);
                        self.emit(Op::ImportFrom(i));
                        self.store_name(alias.as_ref().unwrap_or(name));
                    }
                    self.emit(Op::Pop);
                }
            }
            StmtKind::Global(_) | StmtKind::Nonlocal(_) => {}
            StmtKind::Delete(targets) => {
                for target in targets {
                    self.delete_target(target)?;
                }
            }
            StmtKind::Assert(test, message) => {
                self.expr(test)?;
                let ok = self.emit(Op::PopJumpIfTrue(0));
                self.emit(Op::LoadAssertionError);
                if let Some(m) = message {
                    self.expr(m)?;
                    self.emit(Op::Call(1));
                }
                self.emit(Op::Raise(1));
                self.patch_here(ok);
            }
        }
        Ok(())
    }

    /// What leaving `block` takes; `keep` says a return value sits on the
    /// stack above whatever the block left there.
    fn unwind(&mut self, block: &Block, keep: bool) -> CResult<()> {
        match block {
            Block::While { .. } => {}
            Block::For { .. } => {
                if keep {
                    self.emit(Op::Rot2);
                }
                self.emit(Op::Pop);
            }
            Block::TryExcept => {
                self.emit(Op::PopBlock);
            }
            Block::FinallyTry(finalbody) => {
                self.emit(Op::PopBlock);
                // The finally body runs here, outside its own block: a
                // second leave inside it must not run it again.
                let saved = self.unit().blocks.clone();
                let position = saved.iter().rposition(|b| matches!(b, Block::FinallyTry(f) if Rc::ptr_eq(f, finalbody)));
                if let Some(p) = position {
                    self.unit().blocks.truncate(p);
                }
                self.body(finalbody)?;
                self.unit().blocks = saved;
            }
            Block::FinallyEnd => {
                if keep {
                    self.emit(Op::Rot2);
                }
                self.emit(Op::Pop);
                self.emit(Op::PopExcept);
            }
            Block::Handler(name) => {
                self.emit(Op::PopExcept);
                if let Some(name) = name {
                    self.load_const(Value::None);
                    self.store_name(name);
                    self.delete_name(name);
                }
            }
            Block::With => {
                self.emit(Op::PopBlock);
                if keep {
                    self.emit(Op::Rot2);
                }
                self.load_const(Value::None);
                self.emit(Op::Dup);
                self.emit(Op::Dup);
                self.emit(Op::Call(3));
                self.emit(Op::Pop);
            }
        }
        Ok(())
    }

    fn break_or_continue(&mut self, is_break: bool, line: u32) -> CResult<()> {
        let blocks = self.unit().blocks.clone();
        let Some(loop_index) = blocks.iter().rposition(|b| matches!(b, Block::While { .. } | Block::For { .. })) else {
            return err(if is_break { "'break' hors d'une boucle" } else { "'continue' hors d'une boucle" }, line);
        };
        for block in blocks[loop_index + 1..].iter().rev() {
            self.unwind(block, false)?;
        }
        if is_break {
            // Leaving a for: its iterator goes.
            if matches!(blocks[loop_index], Block::For { .. }) {
                self.emit(Op::Pop);
            }
            let jump = self.emit(Op::Jump(0));
            match &mut self.unit().blocks[loop_index] {
                Block::While { breaks, .. } | Block::For { breaks, .. } => breaks.push(jump),
                _ => {}
            }
        } else {
            let top = match &blocks[loop_index] {
                Block::While { top, .. } | Block::For { top, .. } => *top,
                _ => 0,
            };
            self.emit(Op::Jump(top as u32));
        }
        Ok(())
    }

    fn try_except(&mut self, body: &[Stmt], handlers: &[ExceptHandler], orelse: &[Stmt]) -> CResult<()> {
        let setup = self.emit(Op::SetupFinally(0));
        self.unit().blocks.push(Block::TryExcept);
        self.body(body)?;
        self.unit().blocks.pop();
        self.emit(Op::PopBlock);
        self.body(orelse)?;
        let mut to_end = alloc::vec![self.emit(Op::Jump(0))];
        self.patch_here(setup);
        // The exception is on the stack; an except-handler block is set.
        for (index, handler) in handlers.iter().enumerate() {
            self.line = handler.line;
            let mut next = None;
            match &handler.kind {
                Some(kind) => {
                    self.emit(Op::Dup);
                    self.expr(kind)?;
                    next = Some(self.emit(Op::JumpIfNotExcMatch(0)));
                }
                None => {
                    if index + 1 != handlers.len() {
                        return err("un 'except:' nu doit etre le dernier", handler.line);
                    }
                }
            }
            match &handler.name {
                Some(name) => self.store_name(name),
                None => {
                    self.emit(Op::Pop);
                }
            }
            self.unit().blocks.push(Block::Handler(handler.name.clone()));
            self.body(&handler.body)?;
            self.unit().blocks.pop();
            self.emit(Op::PopExcept);
            if let Some(name) = &handler.name {
                self.load_const(Value::None);
                self.store_name(name);
                self.delete_name(name);
            }
            to_end.push(self.emit(Op::Jump(0)));
            if let Some(n) = next {
                self.patch_here(n);
            }
        }
        self.emit(Op::Reraise);
        for jump in to_end {
            self.patch_here(jump);
        }
        Ok(())
    }

    fn try_finally(&mut self, body: &[Stmt], handlers: &[ExceptHandler], orelse: &[Stmt], finalbody: &[Stmt]) -> CResult<()> {
        let finalbody = Rc::new(finalbody.to_vec());
        let setup = self.emit(Op::SetupFinally(0));
        self.unit().blocks.push(Block::FinallyTry(finalbody.clone()));
        if handlers.is_empty() {
            self.body(body)?;
        } else {
            self.try_except(body, handlers, orelse)?;
        }
        self.unit().blocks.pop();
        self.emit(Op::PopBlock);
        self.body(&finalbody)?;
        let to_end = self.emit(Op::Jump(0));
        self.patch_here(setup);
        self.unit().blocks.push(Block::FinallyEnd);
        self.body(&finalbody)?;
        self.unit().blocks.pop();
        self.emit(Op::Reraise);
        self.patch_here(to_end);
        Ok(())
    }

    fn with(&mut self, items: &[(Expr, Option<Expr>)], body: &[Stmt]) -> CResult<()> {
        let Some(((context, target), rest)) = items.split_first() else {
            return self.body(body);
        };
        self.expr(context)?;
        let setup = self.emit(Op::SetupWith(0));
        match target {
            Some(t) => self.store_target(t)?,
            None => {
                self.emit(Op::Pop);
            }
        }
        self.unit().blocks.push(Block::With);
        self.with(rest, body)?;
        self.unit().blocks.pop();
        self.emit(Op::PopBlock);
        self.load_const(Value::None);
        self.emit(Op::Dup);
        self.emit(Op::Dup);
        self.emit(Op::Call(3));
        self.emit(Op::Pop);
        let to_end = self.emit(Op::Jump(0));
        self.patch_here(setup);
        // [exit, exc]
        self.emit(Op::WithExceptStart);
        let suppress = self.emit(Op::PopJumpIfTrue(0));
        self.emit(Op::Reraise);
        self.patch_here(suppress);
        self.emit(Op::Pop);
        self.emit(Op::PopExcept);
        self.emit(Op::Pop);
        self.patch_here(to_end);
        Ok(())
    }

    fn defaults(&mut self, params: &Params) -> CResult<u8> {
        let mut flags = 0;
        let positional: Vec<&Param> = params.posonly.iter().chain(params.args.iter()).filter(|p| p.default.is_some()).collect();
        if !positional.is_empty() {
            for p in &positional {
                if let Some(d) = &p.default {
                    self.expr(d)?;
                }
            }
            self.emit(Op::BuildTuple(positional.len() as u32));
            flags |= 1;
        }
        let keyword: Vec<&Param> = params.kwonly.iter().filter(|p| p.default.is_some()).collect();
        if !keyword.is_empty() {
            for p in &keyword {
                self.load_const(Value::Str(p.name.clone()));
                if let Some(d) = &p.default {
                    self.expr(d)?;
                }
            }
            self.emit(Op::BuildMap(keyword.len() as u32));
            flags |= 2;
        }
        Ok(flags)
    }

    fn docstring(body: &[Stmt]) -> Option<Rc<str>> {
        match body.first().map(|s| &s.kind) {
            Some(StmtKind::Expr(Expr { kind: ExprKind::Const(Const::Str(s)), .. })) => Some(s.clone()),
            _ => None,
        }
    }

    fn function_def(&mut self, def: &Rc<FunctionDef>) -> CResult<()> {
        for decorator in &def.decorators {
            self.expr(decorator)?;
        }
        let flags = self.defaults(&def.params)?;
        let scope = *self.table.by_node.get(&symtable::function_key(def)).ok_or(CompileError { message: String::from("portee introuvable"), line: def.line })?;
        let qualname = self.qualname(&def.name);
        self.new_unit(scope, &def.name, &qualname, Some(&def.params), def.line);
        self.unit().code.doc = Self::docstring(&def.body);
        self.body(&def.body)?;
        self.load_const(Value::None);
        self.emit(Op::Return);
        let code = self.finish_unit();
        self.line = def.line;
        self.make_closure(code, flags);
        for _ in &def.decorators {
            self.emit(Op::Call(1));
        }
        self.store_name(&def.name);
        Ok(())
    }

    fn class_def(&mut self, def: &Rc<ClassDef>) -> CResult<()> {
        for decorator in &def.decorators {
            self.expr(decorator)?;
        }
        let scope = *self.table.by_node.get(&symtable::class_key(def)).ok_or(CompileError { message: String::from("portee introuvable"), line: def.line })?;
        let qualname = self.qualname(&def.name);
        self.new_unit(scope, &def.name, &qualname, None, def.line);
        // __module__, __qualname__, __doc__, annotations.
        let i = self.name_index("__name__");
        self.emit(Op::LoadName(i));
        let i = self.name_index("__module__");
        self.emit(Op::StoreName(i));
        self.load_const(Value::Str(Rc::from(qualname.as_str())));
        let i = self.name_index("__qualname__");
        self.emit(Op::StoreName(i));
        if let Some(doc) = Self::docstring(&def.body) {
            self.load_const(Value::Str(doc));
            let i = self.name_index("__doc__");
            self.emit(Op::StoreName(i));
        }
        if def.body.iter().any(|s| matches!(s.kind, StmtKind::AnnAssign(..))) {
            self.emit(Op::SetupAnnotations);
        }
        self.body(&def.body)?;
        let has_class_cell = self.unit().code.cellvars.iter().any(|c| &**c == "__class__");
        if has_class_cell {
            let i = self.deref_index("__class__");
            self.emit(Op::LoadClosure(i));
        } else {
            self.load_const(Value::None);
        }
        self.emit(Op::Return);
        let code = self.finish_unit();
        self.line = def.line;
        self.make_closure(code, 0);
        self.load_const(Value::Str(def.name.clone()));
        for base in &def.bases {
            self.expr(base)?;
        }
        for (name, value) in &def.keywords {
            self.load_const(Value::Str(name.clone()));
            self.expr(value)?;
        }
        self.emit(Op::BuildClass(def.bases.len() as u32, def.keywords.len() as u32));
        for _ in &def.decorators {
            self.emit(Op::Call(1));
        }
        self.store_name(&def.name);
        Ok(())
    }

    // --------------------------------------------------------
    //  Targets
    // --------------------------------------------------------

    fn store_target(&mut self, target: &Expr) -> CResult<()> {
        match &target.kind {
            ExprKind::Name(name) => self.store_name(name),
            ExprKind::Attribute(object, name) => {
                self.expr(object)?;
                let i = self.name_index(name);
                self.emit(Op::StoreAttr(i));
            }
            ExprKind::Subscript(object, index) => {
                self.expr(object)?;
                self.expr(index)?;
                self.emit(Op::StoreSubscr);
            }
            ExprKind::Tuple(items) | ExprKind::List(items) => {
                let star = items.iter().position(|i| matches!(i.kind, ExprKind::Starred(_)));
                match star {
                    Some(s) => {
                        self.emit(Op::UnpackEx(s as u16, (items.len() - s - 1) as u16));
                    }
                    None => {
                        self.emit(Op::UnpackSequence(items.len() as u32));
                    }
                }
                for item in items {
                    match &item.kind {
                        ExprKind::Starred(inner) => self.store_target(inner)?,
                        _ => self.store_target(item)?,
                    }
                }
            }
            ExprKind::Starred(_) => return err("une expression etoilee doit etre dans une liste ou un tuple", target.line),
            _ => return err("cible d'affectation invalide", target.line),
        }
        Ok(())
    }

    fn delete_target(&mut self, target: &Expr) -> CResult<()> {
        match &target.kind {
            ExprKind::Name(name) => self.delete_name(name),
            ExprKind::Attribute(object, name) => {
                self.expr(object)?;
                let i = self.name_index(name);
                self.emit(Op::DeleteAttr(i));
            }
            ExprKind::Subscript(object, index) => {
                self.expr(object)?;
                self.expr(index)?;
                self.emit(Op::DeleteSubscr);
            }
            ExprKind::Tuple(items) | ExprKind::List(items) => {
                for item in items {
                    self.delete_target(item)?;
                }
            }
            _ => return err("impossible de supprimer cette expression", target.line),
        }
        Ok(())
    }

    fn aug_assign(&mut self, target: &Expr, op: BinOp, value: &Expr) -> CResult<()> {
        match &target.kind {
            ExprKind::Name(name) => {
                self.load_name(name);
                self.expr(value)?;
                self.emit(Op::Inplace(op));
                self.store_name(name);
            }
            ExprKind::Attribute(object, name) => {
                self.expr(object)?;
                self.emit(Op::Dup);
                let i = self.name_index(name);
                self.emit(Op::LoadAttr(i));
                self.expr(value)?;
                self.emit(Op::Inplace(op));
                self.emit(Op::Rot2);
                self.emit(Op::StoreAttr(i));
            }
            ExprKind::Subscript(object, index) => {
                self.expr(object)?;
                self.expr(index)?;
                self.emit(Op::DupTwo);
                self.emit(Op::Subscr);
                self.expr(value)?;
                self.emit(Op::Inplace(op));
                self.emit(Op::Rot3);
                self.emit(Op::StoreSubscr);
            }
            _ => return err("cible invalide pour une affectation augmentee", target.line),
        }
        Ok(())
    }

    // --------------------------------------------------------
    //  Expressions
    // --------------------------------------------------------

    fn expr(&mut self, expr: &Expr) -> CResult<()> {
        let saved = self.line;
        self.line = expr.line;
        let result = self.expr_inner(expr);
        self.line = saved;
        result
    }

    fn expr_inner(&mut self, expr: &Expr) -> CResult<()> {
        match &expr.kind {
            ExprKind::Name(name) => self.load_name(name),
            ExprKind::Const(c) => {
                let value = match c {
                    Const::None => Value::None,
                    Const::True => Value::Bool(true),
                    Const::False => Value::Bool(false),
                    Const::Ellipsis => Value::Ellipsis,
                    Const::Int(i) => Value::Int(i.clone()),
                    Const::Float(f) => Value::Float(*f),
                    Const::Str(s) => Value::Str(s.clone()),
                    Const::Bytes(b) => Value::Bytes(b.clone()),
                };
                self.load_const(value);
            }
            ExprKind::FString(parts) => {
                let count = self.fparts(parts)?;
                if count != 1 {
                    self.emit(Op::BuildString(count));
                }
            }
            ExprKind::BinOp(op, a, b) => {
                self.expr(a)?;
                self.expr(b)?;
                self.emit(Op::Binary(*op));
            }
            ExprKind::Unary(op, a) => {
                self.expr(a)?;
                self.emit(Op::Unary(*op));
            }
            ExprKind::BoolOp(is_and, values) => {
                let mut jumps = Vec::new();
                for (i, v) in values.iter().enumerate() {
                    self.expr(v)?;
                    if i + 1 < values.len() {
                        jumps.push(self.emit(if *is_and { Op::JumpIfFalseOrPop(0) } else { Op::JumpIfTrueOrPop(0) }));
                    }
                }
                for j in jumps {
                    self.patch_here(j);
                }
            }
            ExprKind::Compare(first, rest) => {
                self.expr(first)?;
                if rest.len() == 1 {
                    self.expr(&rest[0].1)?;
                    self.compare_op(rest[0].0);
                } else {
                    let mut cleanups = Vec::new();
                    for (i, (op, operand)) in rest.iter().enumerate() {
                        self.expr(operand)?;
                        if i + 1 < rest.len() {
                            self.emit(Op::Dup);
                            self.emit(Op::Rot3);
                            self.compare_op(*op);
                            cleanups.push(self.emit(Op::JumpIfFalseOrPop(0)));
                        } else {
                            self.compare_op(*op);
                        }
                    }
                    let to_end = self.emit(Op::Jump(0));
                    for c in cleanups {
                        self.patch_here(c);
                    }
                    self.emit(Op::Rot2);
                    self.emit(Op::Pop);
                    self.patch_here(to_end);
                }
            }
            ExprKind::Call(function, args) => self.call(function, args)?,
            ExprKind::Attribute(object, name) => {
                self.expr(object)?;
                let i = self.name_index(name);
                self.emit(Op::LoadAttr(i));
            }
            ExprKind::Subscript(object, index) => {
                self.expr(object)?;
                self.expr(index)?;
                self.emit(Op::Subscr);
            }
            ExprKind::Slice(a, b, c) => {
                for part in [a, b] {
                    match part {
                        Some(e) => self.expr(e)?,
                        None => self.load_const(Value::None),
                    }
                }
                match c {
                    Some(e) => {
                        self.expr(e)?;
                        self.emit(Op::BuildSlice(3));
                    }
                    None => {
                        self.emit(Op::BuildSlice(2));
                    }
                }
            }
            ExprKind::List(items) => self.sequence(items, 0)?,
            ExprKind::Tuple(items) => self.sequence(items, 1)?,
            ExprKind::Set(items) => self.sequence(items, 2)?,
            ExprKind::Dict(pairs) => {
                let plain = pairs.iter().all(|(k, _)| k.is_some());
                if plain {
                    for (k, v) in pairs {
                        if let Some(k) = k {
                            self.expr(k)?;
                        }
                        self.expr(v)?;
                    }
                    self.emit(Op::BuildMap(pairs.len() as u32));
                } else {
                    self.emit(Op::BuildMap(0));
                    for (k, v) in pairs {
                        match k {
                            Some(k) => {
                                self.expr(k)?;
                                self.expr(v)?;
                                self.emit(Op::BuildMap(1));
                                self.emit(Op::DictUpdate(1));
                            }
                            None => {
                                self.expr(v)?;
                                self.emit(Op::DictUpdate(1));
                            }
                        }
                    }
                }
            }
            ExprKind::ListComp(element, generators) => self.comprehension(expr, 0, element, None, generators)?,
            ExprKind::SetComp(element, generators) => self.comprehension(expr, 1, element, None, generators)?,
            ExprKind::DictComp(key, value, generators) => self.comprehension(expr, 2, key, Some(value), generators)?,
            ExprKind::Generator(element, generators) => self.comprehension(expr, 3, element, None, generators)?,
            ExprKind::Lambda(params, body) => {
                let flags = self.defaults(params)?;
                let scope = *self.table.by_node.get(&symtable::expr_key(expr)).ok_or(CompileError { message: String::from("portee introuvable"), line: expr.line })?;
                let qualname = self.qualname("<lambda>");
                self.new_unit(scope, "<lambda>", &qualname, Some(params), expr.line);
                self.expr(body)?;
                self.emit(Op::Return);
                let code = self.finish_unit();
                self.make_closure(code, flags);
            }
            ExprKind::IfExp(test, body, orelse) => {
                self.expr(test)?;
                let to_else = self.emit(Op::PopJumpIfFalse(0));
                self.expr(body)?;
                let to_end = self.emit(Op::Jump(0));
                self.patch_here(to_else);
                self.expr(orelse)?;
                self.patch_here(to_end);
            }
            ExprKind::Starred(_) => return err("expression etoilee hors d'un contexte qui l'accepte", expr.line),
            ExprKind::Named(name, value) => {
                self.expr(value)?;
                self.emit(Op::Dup);
                self.store_name(name);
            }
            ExprKind::Yield(value) => {
                match value {
                    Some(v) => self.expr(v)?,
                    None => self.load_const(Value::None),
                }
                self.emit(Op::Yield);
            }
            ExprKind::YieldFrom(value) => {
                self.expr(value)?;
                self.emit(Op::GetYieldFromIter);
                self.load_const(Value::None);
                self.emit(Op::YieldFrom);
            }
        }
        Ok(())
    }

    fn compare_op(&mut self, op: CmpOp) {
        match op {
            CmpOp::Is => self.emit(Op::Is(false)),
            CmpOp::IsNot => self.emit(Op::Is(true)),
            CmpOp::In => self.emit(Op::Contains(false)),
            CmpOp::NotIn => self.emit(Op::Contains(true)),
            other => self.emit(Op::Compare(other)),
        };
    }

    /// Pushes the parts of an f-string; returns how many.
    fn fparts(&mut self, parts: &[FPart]) -> CResult<u32> {
        let mut count = 0;
        for part in parts {
            match part {
                FPart::Text(t) => {
                    self.load_const(Value::Str(t.clone()));
                    count += 1;
                }
                FPart::Field { value, conversion, spec, debug } => {
                    if let Some(d) = debug {
                        self.load_const(Value::Str(d.clone()));
                        count += 1;
                    }
                    self.expr(value)?;
                    let conv = match conversion {
                        Some('s') => b's',
                        Some('r') => b'r',
                        Some('a') => b'a',
                        _ => 0,
                    };
                    let has_spec = match spec {
                        Some(spec) => {
                            let n = self.fparts(spec)?;
                            if n != 1 {
                                self.emit(Op::BuildString(n));
                            }
                            true
                        }
                        None => false,
                    };
                    self.emit(Op::FormatValue(conv, has_spec));
                    count += 1;
                }
            }
        }
        if count == 0 {
            self.load_const(Value::str(""));
            count = 1;
        }
        Ok(count)
    }

    /// A list (0), tuple (1) or set (2) display, starred items expanded.
    fn sequence(&mut self, items: &[Expr], kind: u8) -> CResult<()> {
        let starred = items.iter().any(|i| matches!(i.kind, ExprKind::Starred(_)));
        if !starred {
            for item in items {
                self.expr(item)?;
            }
            let n = items.len() as u32;
            let constant = |e: &Expr| match &e.kind {
                ExprKind::Const(_) => true,
                ExprKind::Unary(UnaryOp::Neg | UnaryOp::Pos, inner) => matches!(inner.kind, ExprKind::Const(Const::Int(_) | Const::Float(_))),
                _ => false,
            };
            let all_constant = items.iter().all(constant);
            self.emit(match kind {
                0 => Op::BuildList(n),
                1 => Op::BuildTuple(n),
                _ if n > 2 && all_constant => Op::BuildConstSet(n),
                _ => Op::BuildSet(n),
            });
            return Ok(());
        }
        self.emit(if kind == 2 { Op::BuildSet(0) } else { Op::BuildList(0) });
        for item in items {
            match &item.kind {
                ExprKind::Starred(inner) => {
                    self.expr(inner)?;
                    self.emit(if kind == 2 { Op::SetUpdate(1) } else { Op::ListExtend(1) });
                }
                _ => {
                    self.expr(item)?;
                    self.emit(if kind == 2 { Op::SetAdd(1) } else { Op::ListAppend(1) });
                }
            }
        }
        if kind == 1 {
            self.emit(Op::ListToTuple);
        }
        Ok(())
    }

    fn call(&mut self, function: &Expr, args: &[Arg]) -> CResult<()> {
        // `super()` without arguments: `super(__class__, first argument)`.
        if let ExprKind::Name(name) = &function.kind {
            if &**name == "super" && args.is_empty() && self.scope_kind() == ScopeKind::Function {
                let has_class = self.unit().code.freevars.iter().any(|f| &**f == "__class__");
                if has_class {
                    self.load_name("super");
                    let i = self.deref_index("__class__");
                    self.emit(Op::LoadDeref(i));
                    let first = self.unit().code.varnames.first().cloned();
                    let Some(first) = first.filter(|_| self.unit().code.argcount > 0) else {
                        return err("super() : pas de premier argument", function.line);
                    };
                    self.load_name(&first);
                    self.emit(Op::Call(2));
                    return Ok(());
                }
            }
        }
        self.expr(function)?;
        let complex = args.iter().any(|a| matches!(a, Arg::Starred(_) | Arg::DoubleStarred(_)));
        if !complex {
            let mut names = Vec::new();
            let mut count = 0;
            for arg in args {
                match arg {
                    Arg::Positional(e) => {
                        if !names.is_empty() {
                            return err("argument positionnel apres un argument nomme", e.line);
                        }
                        self.expr(e)?;
                    }
                    Arg::Keyword(name, e) => {
                        if names.iter().any(|n: &Value| matches!(n, Value::Str(s) if s == name)) {
                            return err(&alloc::format!("argument nomme '{}' repete", name), e.line);
                        }
                        self.expr(e)?;
                        names.push(Value::Str(name.clone()));
                    }
                    _ => {}
                }
                count += 1;
            }
            if names.is_empty() {
                self.emit(Op::Call(count));
            } else {
                self.load_const(Value::tuple(names));
                self.emit(Op::CallKw(count));
            }
            return Ok(());
        }
        // The general form: a tuple and a dict.
        self.emit(Op::BuildList(0));
        for arg in args {
            match arg {
                Arg::Positional(e) => {
                    self.expr(e)?;
                    self.emit(Op::ListAppend(1));
                }
                Arg::Starred(e) => {
                    self.expr(e)?;
                    self.emit(Op::ListExtend(1));
                }
                _ => {}
            }
        }
        self.emit(Op::ListToTuple);
        let has_keywords = args.iter().any(|a| matches!(a, Arg::Keyword(..) | Arg::DoubleStarred(_)));
        if has_keywords {
            self.emit(Op::BuildMap(0));
            for arg in args {
                match arg {
                    Arg::Keyword(name, e) => {
                        self.load_const(Value::Str(name.clone()));
                        self.expr(e)?;
                        self.emit(Op::BuildMap(1));
                        self.emit(Op::DictMerge(1));
                    }
                    Arg::DoubleStarred(e) => {
                        self.expr(e)?;
                        self.emit(Op::DictMerge(1));
                    }
                    _ => {}
                }
            }
        }
        self.emit(Op::CallEx(has_keywords));
        Ok(())
    }

    /// A comprehension: a function of its first iterator, called at once.
    /// Kind: 0 list, 1 set, 2 dict, 3 generator.
    fn comprehension(&mut self, node: &Expr, kind: u8, element: &Expr, value: Option<&Expr>, generators: &[Comprehension]) -> CResult<()> {
        let scope = *self.table.by_node.get(&symtable::expr_key(node)).ok_or(CompileError { message: String::from("portee introuvable"), line: node.line })?;
        let name = match kind {
            0 => "<listcomp>",
            1 => "<setcomp>",
            2 => "<dictcomp>",
            _ => "<genexpr>",
        };
        let qualname = self.qualname(name);
        self.new_unit(scope, name, &qualname, None, node.line);
        match kind {
            0 => {
                self.emit(Op::BuildList(0));
            }
            1 => {
                self.emit(Op::BuildSet(0));
            }
            2 => {
                self.emit(Op::BuildMap(0));
            }
            _ => {}
        }
        self.comprehension_clause(kind, element, value, generators, 0)?;
        if kind == 3 {
            self.load_const(Value::None);
        }
        self.emit(Op::Return);
        let code = self.finish_unit();
        self.make_closure(code, 0);
        self.expr(&generators[0].iter)?;
        self.emit(Op::GetIter);
        self.emit(Op::Call(1));
        Ok(())
    }

    fn comprehension_clause(&mut self, kind: u8, element: &Expr, value: Option<&Expr>, generators: &[Comprehension], index: usize) -> CResult<()> {
        let clause = &generators[index];
        if index == 0 {
            let i = self.varname_index(".0");
            self.emit(Op::LoadFast(i));
        } else {
            self.expr(&clause.iter)?;
            self.emit(Op::GetIter);
        }
        let top = self.here();
        let exit = self.emit(Op::ForIter(0));
        self.store_target(&clause.target)?;
        let mut skips = Vec::new();
        for condition in &clause.ifs {
            self.expr(condition)?;
            skips.push(self.emit(Op::PopJumpIfFalse(0)));
        }
        if index + 1 < generators.len() {
            self.comprehension_clause(kind, element, value, generators, index + 1)?;
        } else {
            // The accumulator sits under the iterators: one per clause.
            let depth = (generators.len() + 1) as u32;
            match kind {
                0 => {
                    self.expr(element)?;
                    self.emit(Op::ListAppend(depth));
                }
                1 => {
                    self.expr(element)?;
                    self.emit(Op::SetAdd(depth));
                }
                2 => {
                    self.expr(element)?;
                    if let Some(v) = value {
                        self.expr(v)?;
                    }
                    self.emit(Op::MapAdd(depth));
                }
                _ => {
                    self.expr(element)?;
                    self.emit(Op::Yield);
                    self.emit(Op::Pop);
                }
            }
        }
        for s in skips {
            self.patch(s, top);
        }
        self.emit(Op::Jump(top as u32));
        self.patch_here(exit);
        Ok(())
    }
}

/// Compiles a module.
pub fn compile_module(body: &[Stmt], filename: &str) -> Result<Rc<Code>, CompileError> {
    let table = symtable::build(body).map_err(|e| CompileError { message: e.message, line: e.line })?;
    let mut compiler = Compiler { table: &table, units: Vec::new(), filename: Rc::from(filename), line: 1 };
    compiler.new_unit(0, "<module>", "<module>", None, 1);
    compiler.unit().code.doc = Compiler::docstring(body);
    if body.iter().any(|s| matches!(s.kind, StmtKind::AnnAssign(..))) {
        compiler.emit(Op::SetupAnnotations);
    }
    compiler.body(body)?;
    compiler.load_const(Value::None);
    compiler.emit(Op::Return);
    Ok(compiler.finish_unit())
}

/// Compiles one expression into a code that returns it (`eval`).
pub fn compile_eval(expr: &Expr, filename: &str) -> Result<Rc<Code>, CompileError> {
    let statement = Stmt { kind: StmtKind::Expr(expr.clone()), line: expr.line };
    let body = alloc::vec![statement];
    let table = symtable::build(&body).map_err(|e| CompileError { message: e.message, line: e.line })?;
    let mut compiler = Compiler { table: &table, units: Vec::new(), filename: Rc::from(filename), line: 1 };
    compiler.new_unit(0, "<module>", "<module>", None, 1);
    // The expression the table was built on (scopes are keyed by node).
    let StmtKind::Expr(expr) = &body[0].kind else {
        return Err(CompileError { message: String::from("expression attendue"), line: 1 });
    };
    compiler.expr(expr)?;
    compiler.emit(Op::Return);
    Ok(compiler.finish_unit())
}
