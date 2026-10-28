//! Code generation: the typed tree becomes assembly text for `sdk::asm`.
//!
//! ## A stack machine, as in `sdk::rustc`
//!
//! Every expression leaves its value in `rax`; a binary operation
//! evaluates its right side, pushes it, evaluates its left side, pops the
//! right into `rdi`. The reasons are those of `sdk::rustc::codegen`: it is
//! obviously correct, a `syscall` in the middle of an expression cannot
//! corrupt anything, and nested calls need no save list.
//!
//! ## Values in a register
//!
//! A value of an integer type is kept **extended to 64 bits** according
//! to its type: an `int` sign-extended, an `unsigned` zero-extended, and
//! so on. An operation computes on the full register and extends its
//! result again (`movsxd rax, eax` after an `int` addition): that is
//! exactly C's arithmetic modulo 2^32 for unsigned and what x86 does for
//! signed overflow. A comparison can then always compare 64 bits.
//!
//! A `double` travels in `rax` as its bit pattern and visits `xmm0` and
//! `xmm1` only to be computed on. A `float` is held **as the double of
//! the same value**: every `float` operation is done in double and
//! rounded back to float, which gives the same result as a single
//! precision operation for `+ - * /` (a double has more than twice the
//! bits of a float, so the double rounding is harmless). Memory holds a
//! `float` in its 4 bytes.
//!
//! ## Calling convention
//!
//! Nova's own, not System V's: **every argument goes on the stack**, the
//! last pushed first, one 8-byte word per scalar (a `float` parameter as
//! its 4 bytes, the rest of the word zero) and a structure copied whole,
//! rounded up to words. The caller removes them. The callee finds its
//! first argument at `[rbp + 16]`. A structure is returned through a
//! hidden pointer pushed after the arguments (so at `[rbp + 16]`, the
//! arguments following), the callee copying into it and returning it in
//! `rax`. Everything else returns in `rax`.
//!
//! Since every binary is compiled whole here -- program and C library --
//! nothing needs System V's register allocation of arguments, and `...`
//! becomes trivial: `va_start` is the address of the word after the last
//! named argument, `va_arg` reads a word and advances (`<stdarg.h>`).
//!
//! ## What is emitted
//!
//! Only what `main` and the startup code can reach: a program that never
//! calls `printf` does not carry its formatting code. A pointer stored in
//! initialised data is filled by `__cc_init` before `main`, since a Nova
//! binary has no relocation table.

mod expr;

use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use super::CcError;
use super::ast::*;
use super::lex::Loc;
use super::ty::{self, Kind, Type};

/// Label of the C library's entry point (`__cc_start` in `libc.c`).
pub const START: &str = "___cc_start";

pub struct Generator<'a> {
    program: &'a Program,
    out: String,
    function: Option<&'a Function>,
    locals: Vec<Local>,
    return_label: String,
    labels: u32,
}

/// Generates the assembly of the whole program.
pub fn generate(program: &Program) -> Result<String, CcError> {
    let functions: BTreeMap<&str, &Function> = program.functions.iter().map(|f| (&*f.label, f)).collect();
    let globals: BTreeMap<&str, &Global> = program.globals.iter().map(|g| (&*g.label, g)).collect();

    // What the startup code reaches, functions and data together.
    let mut reached: Vec<Rc<str>> = Vec::new();
    let mut pending: Vec<Rc<str>> = alloc::vec![Rc::from(START)];
    while let Some(label) = pending.pop() {
        if reached.contains(&label) {
            continue;
        }
        if let Some(function) = functions.get(&*label) {
            pending.extend(function.references.iter().cloned());
        } else if let Some(global) = globals.get(&*label) {
            let defined = global.defined && global.ty.kind != Kind::Func;
            if !defined {
                let name = label.strip_prefix('_').unwrap_or(&label);
                let name = name.split('.').next().unwrap_or(name);
                let what = if global.ty.kind == Kind::Func { "fonction" } else { "variable" };
                return Err(CcError::plain(&alloc::format!("{} {} declaree mais jamais definie", what, name)));
            }
            pending.extend(global.relocs.iter().map(|r| r.label.clone()));
        } else if &*label == START {
            return Err(CcError::plain("point d'entree __cc_start absent (bibliotheque C manquante)"));
        } else {
            return Err(CcError::plain(&alloc::format!("reference indefinie a {}", label)));
        }
        reached.push(label);
    }
    if !functions.contains_key("_main") {
        return Err(CcError::plain("aucune fonction main"));
    }

    let mut generator = Generator {
        program,
        out: String::new(),
        function: None,
        locals: Vec::new(),
        return_label: String::new(),
        labels: 0,
    };
    let g = &mut generator;
    g.out.push_str("; Nova C - produit par sdk::cc, ne pas editer\n.text\n_start:\n");
    g.line("call __cc_init");
    g.line(&alloc::format!("call {}", START));
    g.line("hlt");

    // Pointers in initialised data.
    g.out.push_str("__cc_init:\n");
    for global in program.globals.iter().filter(|g| reached.contains(&g.label)) {
        for reloc in global.relocs.iter() {
            g.line(&alloc::format!("lea rax, [rip + {}]", reloc.label));
            if reloc.addend != 0 {
                g.line(&alloc::format!("mov rcx, {}", reloc.addend));
                g.line("add rax, rcx");
            }
            g.line(&alloc::format!("lea rdi, [rip + {}]", global.label));
            g.line(&alloc::format!("mov [rdi + {}], rax", reloc.offset));
        }
    }
    g.line("ret");

    for function in program.functions.iter() {
        if reached.contains(&function.label) {
            g.function(function)?;
        }
    }

    let mut data = String::new();
    let mut bss = String::new();
    for global in program.globals.iter() {
        if !reached.contains(&global.label) || global.ty.kind == Kind::Func || !global.defined {
            continue;
        }
        let align = global.ty.align().max(1);
        match &global.data {
            Some(bytes) => {
                data.push_str(&alloc::format!("align {}\n{}:\n", align, global.label));
                for chunk in bytes.chunks(16) {
                    let list: Vec<String> = chunk.iter().map(|b| alloc::format!("{}", b)).collect();
                    data.push_str(&alloc::format!("    db {}\n", list.join(", ")));
                }
            }
            None => {
                bss.push_str(&alloc::format!("align {}\n{}:\n    resb {}\n", align, global.label, global.ty.size().max(0)));
            }
        }
    }
    if !data.is_empty() {
        g.out.push_str(".data\n");
        g.out.push_str(&data);
    }
    if !bss.is_empty() {
        g.out.push_str(".bss\n");
        g.out.push_str(&bss);
    }
    Ok(generator.out)
}

impl<'a> Generator<'a> {
    fn line(&mut self, text: &str) {
        self.out.push_str("    ");
        self.out.push_str(text);
        self.out.push('\n');
    }

    fn label(&mut self, name: &str) {
        self.out.push_str(name);
        self.out.push_str(":\n");
    }

    fn fresh(&mut self) -> String {
        self.labels += 1;
        alloc::format!("__G{}", self.labels)
    }

    fn function(&mut self, function: &'a Function) -> Result<(), CcError> {
        self.function = Some(function);
        self.locals = function.locals.clone();
        // Locals below rbp, each on its own alignment.
        let mut offset: i64 = 0;
        for local in self.locals.iter_mut().filter(|l| !l.param) {
            offset = ty::align_to(offset + local.ty.size().max(1), local.ty.align().max(1));
            local.offset = -offset;
        }
        let frame = ty::align_to(offset, 16);
        self.return_label = self.fresh();

        self.label(&function.label);
        self.line("push rbp");
        self.line("mov rbp, rsp");
        if frame > 0 {
            self.line(&alloc::format!("sub rsp, {}", frame));
        }
        self.statement(&function.body)?;
        // Falling off the end of `main` returns 0 (C11 5.1.2.2.3); of any
        // other function, whatever `rax` holds.
        if &*function.name == "main" {
            self.line("mov rax, 0");
        }
        let label = self.return_label.clone();
        self.label(&label);
        self.line("mov rsp, rbp");
        self.line("pop rbp");
        self.line("ret");
        self.function = None;
        Ok(())
    }

    fn statement(&mut self, node: &Node) -> Result<(), CcError> {
        match &node.kind {
            NodeKind::Null => {}
            NodeKind::Block(items) => {
                for item in items {
                    self.statement(item)?;
                }
            }
            NodeKind::Expr(expr) => self.expr(expr)?,
            NodeKind::Return(value) => {
                if let Some(value) = value {
                    self.expr(value)?;
                    if value.ty.is_record() {
                        let buffer = self.function.and_then(|f| f.return_buffer).unwrap_or(16);
                        self.line("mov rsi, rax");
                        self.line(&alloc::format!("mov rdi, [rbp + {}]", buffer));
                        self.line(&alloc::format!("mov rcx, {}", value.ty.size()));
                        self.line("rep movsb");
                        self.line(&alloc::format!("mov rax, [rbp + {}]", buffer));
                    }
                }
                let label = self.return_label.clone();
                self.line(&alloc::format!("jmp {}", label));
            }
            NodeKind::If(condition, then, other) => {
                let (other_label, end) = (self.fresh(), self.fresh());
                self.truth(condition)?;
                self.line(&alloc::format!("je {}", other_label));
                self.statement(then)?;
                self.line(&alloc::format!("jmp {}", end));
                self.label(&other_label);
                if let Some(other) = other {
                    self.statement(other)?;
                }
                self.label(&end);
            }
            NodeKind::Loop { init, cond, step, body, exit, next } => {
                if let Some(init) = init {
                    self.statement(init)?;
                }
                let begin = self.fresh();
                self.label(&begin);
                if let Some(cond) = cond {
                    self.truth(cond)?;
                    self.line(&alloc::format!("je __L{}", exit));
                }
                self.statement(body)?;
                self.label(&alloc::format!("__L{}", next));
                if let Some(step) = step {
                    self.expr(step)?;
                }
                self.line(&alloc::format!("jmp {}", begin));
                self.label(&alloc::format!("__L{}", exit));
            }
            NodeKind::DoWhile { body, cond, exit, next } => {
                let begin = self.fresh();
                self.label(&begin);
                self.statement(body)?;
                self.label(&alloc::format!("__L{}", next));
                self.truth(cond)?;
                self.line(&alloc::format!("jne {}", begin));
                self.label(&alloc::format!("__L{}", exit));
            }
            NodeKind::Switch { cond, body, cases, default, exit } => {
                self.expr(cond)?;
                for (value, label) in cases {
                    if i32::try_from(*value).is_ok() {
                        self.line(&alloc::format!("cmp rax, {}", value));
                    } else {
                        self.line(&alloc::format!("mov rdi, {}", value));
                        self.line("cmp rax, rdi");
                    }
                    self.line(&alloc::format!("je __L{}", label));
                }
                match default {
                    Some(label) => self.line(&alloc::format!("jmp __L{}", label)),
                    None => self.line(&alloc::format!("jmp __L{}", exit)),
                }
                self.statement(body)?;
                self.label(&alloc::format!("__L{}", exit));
            }
            NodeKind::Goto(label) => self.line(&alloc::format!("jmp __L{}", label)),
            NodeKind::Label(label, body) => {
                self.label(&alloc::format!("__L{}", label));
                self.statement(body)?;
            }
            _ => self.expr(node)?,
        }
        Ok(())
    }

    /// Evaluates a condition and sets ZF when it is false.
    fn truth(&mut self, node: &Node) -> Result<(), CcError> {
        self.expr(node)?;
        if node.ty.is_flonum() {
            // Zero whatever its sign; a NaN is true.
            self.line("shl rax, 1");
        }
        self.line("cmp rax, 0");
        Ok(())
    }

    fn local_offset(&self, index: usize) -> i64 {
        self.locals[index].offset
    }

    fn error(&self, loc: Loc, message: &str) -> CcError {
        CcError::at(loc, message)
    }

    fn type_of_local(&self, index: usize) -> Rc<Type> {
        self.locals[index].ty.clone()
    }

    fn program(&self) -> &Program {
        self.program
    }
}
