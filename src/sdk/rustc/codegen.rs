//! 9.5.1 -- Code generation: the tree becomes assembly text.
//!
//! ## Why text, and not bytes
//!
//! `sdk::asm` already knows how to encode x86_64, resolve labels and
//! leave the RIP fixups the linker fills. Emitting bytes here would mean
//! a **second** encoder in the tree, and two encoders drift: the drift
//! does not show as a build failure, it shows as a program that runs and
//! does something else. So this file writes assembly and hands it over,
//! which is what every compiler with a separate assembler does.
//!
//! The side effect is worth having on its own: the intermediate text is
//! readable, so a wrong instruction can be seen rather than deduced.
//!
//! ## A stack machine, deliberately
//!
//! Every expression leaves its value in `rax`. A binary operation pushes
//! the left side on the machine stack, evaluates the right, pops. That is
//! slower than a register allocator by a wide margin and it is the right
//! choice here for three reasons:
//!
//! * **It is obviously correct.** A register allocator that spills wrong
//!   produces a program that works until it does not.
//! * **`syscall` destroys `rcx` and `r11`.** With every live value on the
//!   stack, a call to `print!` in the middle of an expression cannot
//!   corrupt anything.
//! * **Nested calls need no save list.** A callee's prologue and epilogue
//!   restore `rsp`, so whatever the caller pushed is still there.
//!
//! ## Frame
//!
//! ```text
//!   [rbp + 8]   return address
//!   [rbp]       saved rbp
//!   [rbp - 8]   slot 0     <- parameters first, in declaration order,
//!   [rbp - 16]  slot 1        then locals in the order they are declared
//!   ...
//! ```
//!
//! Parameters are copied out of their registers into slots on entry.
//! Keeping them in registers would be faster and would mean every call
//! had to save them.
//!
//! ## Registers
//!
//! Generated user code touches `rax`, `rcx`, `rdx` and the six argument
//! registers, and nothing else. `r12` to `r15` therefore belong to the
//! runtime helpers, which can use them across a call to another helper
//! without saving them. That convention is what keeps the helpers short.
//!
//! ## `&str` at run time
//!
//! One word, pointing at a block in `.data`:
//!
//! ```text
//!   +0  u64   length
//!   +8  ...   the bytes
//! ```
//!
//! One word per value everywhere means the stack machine never has to
//! know what it is carrying, and that is what lets `&str` pass through a
//! parameter, a `const` and a `println!` hole with no special case.

use alloc::string::String;
use alloc::vec::Vec;

use super::ast::*;
use super::check::{Checked, ConstValue, signature_of};
use super::{BUILTINS, Builtin};

/// Argument registers, in the order the convention assigns them.
const ARGUMENT_REGISTERS: [&str; 6] = ["rdi", "rsi", "rdx", "rcx", "r8", "r9"];

/// Size of the line buffer, in bytes.
///
/// 512 is `domain_core::PRINT_MAX`: a longer buffer would be truncated by
/// the system call, and the program would have no way to know.
const BUFFER_SIZE: usize = 512;

/// Exit code of a panic. Rust's own, so that a Nova OS process that
/// divides by zero reports what a Rust programmer expects to see.
const PANIC_EXIT: u64 = 101;

const SYS_DEBUG_PRINT: u64 = 0x0001;
const SYS_PROC_YIELD: u64 = 0x0208;
const SYS_PROC_SELF: u64 = 0x020A;
const SYS_PROC_EXIT: u64 = 0x020B;
const SYS_TIME_SLEEP_MS: u64 = 0x0706;

struct Emitter<'a> {
    program: &'a Program,
    checked: &'a Checked,
    text: String,
    /// String literals seen, in order. The index is the label suffix.
    strings: Vec<String>,
    /// Slots of the function being emitted.
    slots: &'a [(String, usize, Type)],
    labels: usize,
    /// Stack of (break label, continue label) for the enclosing loops.
    loops: Vec<(String, String)>,
    /// Does the program print? Decides whether the buffer helpers are
    /// emitted at all -- a program that never prints should not carry
    /// two hundred bytes of decimal conversion.
    uses_output: bool,
    /// Does it compare strings, or divide?
    uses_str_eq: bool,
    uses_panic: bool,
}

/// Compiles a checked program to assembly text.
pub fn emit(program: &Program, checked: &Checked) -> String {
    let mut emitter = Emitter {
        program,
        checked,
        text: String::new(),
        strings: Vec::new(),
        slots: &[],
        labels: 0,
        loops: Vec::new(),
        uses_output: false,
        uses_str_eq: false,
        uses_panic: false,
    };

    // The body is emitted first and the prologue afterwards: what the
    // runtime must carry is only known once the whole program has been
    // walked. Deciding it up front would mean a second walk whose only
    // job is to answer "does this program print".
    let mut body = String::new();
    for (index, function) in program.functions.iter().enumerate() {
        emitter.slots = &checked.slots[index];
        emitter.function(function, checked.frames[index]);
        body.push_str(&core::mem::take(&mut emitter.text));
    }

    let mut out = String::new();
    out.push_str("; Nova Rust - produit par sdk::rustc, ne pas editer\n");
    out.push_str(".text\n");
    out.push_str("_start:\n");
    out.push_str("    call main\n");
    out.push_str("    mov rdi, 0\n");
    out.push_str(&alloc::format!("    mov rax, {}\n", SYS_PROC_EXIT));
    out.push_str("    syscall\n");
    out.push_str("    hlt\n\n");
    out.push_str(&body);
    out.push_str(&emitter.runtime());
    out.push_str(&emitter.data());
    out
}

impl Emitter<'_> {
    fn line(&mut self, text: &str) {
        self.text.push_str("    ");
        self.text.push_str(text);
        self.text.push('\n');
    }

    fn label(&mut self, name: &str) {
        self.text.push_str(name);
        self.text.push_str(":\n");
    }

    fn fresh(&mut self, hint: &str) -> String {
        self.labels += 1;
        alloc::format!("__L{}_{}", hint, self.labels)
    }

    /// Registers a string literal and returns the label of its block.
    ///
    /// Identical literals share one block. Not to save space -- a program
    /// has few strings -- but so that `s == "ok"` compared against a
    /// literal used elsewhere does not depend on which copy it got.
    fn string(&mut self, text: &str) -> String {
        if let Some(index) = self.strings.iter().position(|other| other == text) {
            return alloc::format!("__nova_str_{}", index);
        }
        self.strings.push(String::from(text));
        alloc::format!("__nova_str_{}", self.strings.len() - 1)
    }

    fn slot(&self, name: &str) -> Option<usize> {
        self.slots
            .iter()
            .find(|(other, _, _)| other == name)
            .map(|(_, slot, _)| *slot)
    }

    /// Type of a name in the function being emitted.
    fn local_type(&self, name: &str) -> Option<Type> {
        self.slots
            .iter()
            .find(|(other, _, _)| other == name)
            .map(|(_, _, ty)| *ty)
    }

    /// Address of a slot, as the assembler writes it.
    fn slot_address(slot: usize) -> String {
        alloc::format!("[rbp - {}]", (slot + 1) * 8)
    }

    // --------------------------------------------------------
    //  Functions
    // --------------------------------------------------------

    fn function(&mut self, function: &Function, frame_slots: usize) {
        // Rounded up to sixteen bytes. Nothing here calls an external
        // ABI that would demand it, but a frame that is a multiple of the
        // push size keeps a disassembly readable.
        let frame = (frame_slots * 8).div_ceil(16) * 16;

        self.text.push('\n');
        self.label(&function.name);
        self.line("push rbp");
        self.line("mov rbp, rsp");
        if frame != 0 {
            self.line(&alloc::format!("sub rsp, {}", frame));
        }

        // Parameters out of their registers and into their slots.
        for (index, param) in function.params.iter().enumerate() {
            let slot = self
                .slot(&param.name)
                .expect("le verificateur a attribue un emplacement a chaque parametre");
            self.line(&alloc::format!(
                "mov {}, {}",
                Self::slot_address(slot),
                ARGUMENT_REGISTERS[index]
            ));
        }

        self.block(&function.body);

        // Falling off the end, and **only** when there is nothing in
        // `rax` worth keeping.
        //
        // A body with a tail expression has just left its value there:
        // `fn somme(n: i64) -> i64 { if n == 0 { 0 } else { n + somme(n - 1) } }`
        // is exactly that shape, and zeroing `rax` afterwards made every
        // such function return zero. The program still ran, still
        // printed, and printed `0` -- which is why this had to be found
        // by running it rather than by reading it.
        if function.body.tail.is_none() {
            self.line("mov rax, 0");
        }
        self.epilogue(frame);
    }

    /// `mov rsp, rbp` unconditionally, even for a frame of zero.
    ///
    /// It costs three bytes and it makes the epilogue independent of what
    /// the body left on the stack. A `break` out of a loop, an early
    /// return inside an expression -- both are balanced by construction,
    /// and neither should have to be for the return address to be found.
    fn epilogue(&mut self, _frame: usize) {
        self.line("mov rsp, rbp");
        self.line("pop rbp");
        self.line("ret");
    }

    fn block(&mut self, block: &Block) {
        for statement in block.statements.iter() {
            self.statement(statement);
        }
        if let Some(tail) = block.tail.as_ref() {
            self.expr(tail);
        }
    }

    // --------------------------------------------------------
    //  Statements
    // --------------------------------------------------------

    fn statement(&mut self, statement: &Stmt) {
        match statement {
            Stmt::Let { name, value, .. } => {
                self.expr(value);
                if let Some(slot) = self.slot(name) {
                    self.line(&alloc::format!("mov {}, rax", Self::slot_address(slot)));
                }
            }

            Stmt::Assign {
                name, op, value, ..
            } => {
                let Some(slot) = self.slot(name) else {
                    return;
                };
                match op {
                    None => {
                        self.expr(value);
                        self.line(&alloc::format!("mov {}, rax", Self::slot_address(slot)));
                    }
                    Some(operator) => {
                        // `x op= e` is `x = x op e`, and the operand types
                        // are the ones the checker already agreed on.
                        let ty = self.type_of_local(name);
                        self.line(&alloc::format!("mov rax, {}", Self::slot_address(slot)));
                        self.line("push rax");
                        self.expr(value);
                        self.line("mov rcx, rax");
                        self.line("pop rax");
                        self.binary_op(*operator, ty);
                        self.line(&alloc::format!("mov {}, rax", Self::slot_address(slot)));
                    }
                }
            }

            Stmt::If(condition, then_block, else_block, _) => {
                let end = self.fresh("endif");
                match else_block {
                    None => {
                        self.expr(condition);
                        self.line("cmp rax, 0");
                        self.line(&alloc::format!("je {}", end));
                        self.block(then_block);
                        self.label(&end);
                    }
                    Some(other) => {
                        let else_label = self.fresh("else");
                        self.expr(condition);
                        self.line("cmp rax, 0");
                        self.line(&alloc::format!("je {}", else_label));
                        self.block(then_block);
                        self.line(&alloc::format!("jmp {}", end));
                        self.label(&else_label);
                        self.block(other);
                        self.label(&end);
                    }
                }
            }

            Stmt::While(condition, body, _) => {
                let top = self.fresh("while");
                let end = self.fresh("endwhile");
                self.label(&top);
                self.expr(condition);
                self.line("cmp rax, 0");
                self.line(&alloc::format!("je {}", end));
                self.loops.push((end.clone(), top.clone()));
                self.block(body);
                self.loops.pop();
                self.line(&alloc::format!("jmp {}", top));
                self.label(&end);
            }

            Stmt::Loop(body, _) => {
                let top = self.fresh("loop");
                let end = self.fresh("endloop");
                self.label(&top);
                self.loops.push((end.clone(), top.clone()));
                self.block(body);
                self.loops.pop();
                self.line(&alloc::format!("jmp {}", top));
                self.label(&end);
            }

            Stmt::Break(_) => {
                if let Some((end, _)) = self.loops.last().cloned() {
                    self.line(&alloc::format!("jmp {}", end));
                }
            }

            Stmt::Continue(_) => {
                if let Some((_, top)) = self.loops.last().cloned() {
                    self.line(&alloc::format!("jmp {}", top));
                }
            }

            Stmt::Return(value, _) => {
                match value {
                    Some(expr) => self.expr(expr),
                    None => self.line("mov rax, 0"),
                }
                // `mov rsp, rbp` unconditionally: a `return` may sit
                // inside an expression that has pushed operands, and
                // leaving them on the stack would return to a wrong
                // address. Restoring from rbp discards them all.
                self.line("mov rsp, rbp");
                self.line("pop rbp");
                self.line("ret");
            }

            Stmt::Expr(expr, _) => self.expr(expr),

            Stmt::Print {
                parts,
                args,
                newline,
                ..
            } => self.print(parts, args, *newline),
        }
    }

    /// Type of a local of the function being emitted.
    ///
    /// It comes from the checker's table rather than from a second walk
    /// of the tree: the checker decided it, and two answers to one
    /// question is how `idiv` ends up where `div` belongs.
    fn type_of_local(&self, name: &str) -> Type {
        self.local_type(name).unwrap_or(Type::I64)
    }

    // --------------------------------------------------------
    //  Expressions -- result in rax
    // --------------------------------------------------------

    fn expr(&mut self, expr: &Expr) {
        match expr {
            Expr::Int(value, _) => {
                self.line(&alloc::format!("mov rax, {}", *value as i64));
            }
            Expr::Bool(value) => {
                self.line(&alloc::format!("mov rax, {}", u8::from(*value)));
            }
            Expr::Unit => self.line("mov rax, 0"),
            Expr::Str(text) => {
                let label = self.string(text);
                self.line(&alloc::format!("lea rax, [rip + {}]", label));
            }

            Expr::Path(name, _) => {
                if let Some(slot) = self.slot(name) {
                    self.line(&alloc::format!("mov rax, {}", Self::slot_address(slot)));
                    return;
                }
                self.constant(name);
            }

            Expr::Unary(operator, inner, _) => {
                self.expr(inner);
                match operator {
                    UnOp::Neg => self.line("neg rax"),
                    UnOp::Not => {
                        // `!` is logical on a bool and bitwise on an
                        // integer. The distinction is the type's, and
                        // `not rax` on a bool would turn 0 into -1, which
                        // is true and is not what `!false` means.
                        if self.static_type(inner) == Type::Bool {
                            self.line("xor rax, 1");
                        } else {
                            self.line("not rax");
                        }
                    }
                }
            }

            Expr::Binary(BinOp::And, left, right, _) => {
                // Short circuit: the right side must not run when the
                // left is false. Evaluating both and masking would call
                // functions the program said not to call.
                let end = self.fresh("andend");
                self.expr(left);
                self.line("cmp rax, 0");
                self.line(&alloc::format!("je {}", end));
                self.expr(right);
                self.label(&end);
            }

            Expr::Binary(BinOp::Or, left, right, _) => {
                let end = self.fresh("orend");
                self.expr(left);
                self.line("cmp rax, 0");
                self.line(&alloc::format!("jne {}", end));
                self.expr(right);
                self.label(&end);
            }

            Expr::Binary(operator, left, right, _) => {
                let operand_type = self.static_type(left);
                if operand_type == Type::Str {
                    self.string_compare(*operator, left, right);
                    return;
                }
                self.expr(left);
                self.line("push rax");
                self.expr(right);
                self.line("mov rcx, rax");
                self.line("pop rax");
                self.binary_op(*operator, operand_type);
            }

            Expr::Cast(inner, target, _) => {
                self.expr(inner);
                // Every value is a 64-bit word, so i64 <-> u64 is a
                // reinterpretation and costs nothing. `bool as i64` is
                // already 0 or 1. The cast is real all the same: it is
                // the checker that gave it a meaning.
                let _ = target;
            }

            Expr::Call(name, args, _) => self.call(name, args),

            Expr::If(condition, then_block, else_block, _) => {
                let end = self.fresh("endifv");
                match else_block {
                    None => {
                        self.expr(condition);
                        self.line("cmp rax, 0");
                        self.line(&alloc::format!("je {}", end));
                        self.block(then_block);
                        self.label(&end);
                    }
                    Some(other) => {
                        let else_label = self.fresh("elsev");
                        self.expr(condition);
                        self.line("cmp rax, 0");
                        self.line(&alloc::format!("je {}", else_label));
                        self.block(then_block);
                        self.line(&alloc::format!("jmp {}", end));
                        self.label(&else_label);
                        self.block(other);
                        self.label(&end);
                    }
                }
            }

            Expr::Block(block) => self.block(block),
        }
    }

    /// Loads a constant into `rax`.
    fn constant(&mut self, name: &str) {
        let Some((_, _, value)) = self
            .checked
            .constants
            .iter()
            .find(|(other, _, _)| other == name)
            .cloned()
        else {
            self.line("mov rax, 0");
            return;
        };
        match value {
            ConstValue::Int(value) => {
                self.line(&alloc::format!("mov rax, {}", value as i64));
            }
            ConstValue::Bool(value) => {
                self.line(&alloc::format!("mov rax, {}", u8::from(value)));
            }
            ConstValue::Str(text) => {
                let label = self.string(&text);
                self.line(&alloc::format!("lea rax, [rip + {}]", label));
            }
        }
    }

    /// `rax op rcx`, result in `rax`.
    fn binary_op(&mut self, operator: BinOp, operand_type: Type) {
        let signed = operand_type.is_signed();
        match operator {
            BinOp::Add => self.line("add rax, rcx"),
            BinOp::Sub => self.line("sub rax, rcx"),
            BinOp::Mul => self.line("imul rax, rcx"),
            BinOp::BitAnd => self.line("and rax, rcx"),
            BinOp::BitOr => self.line("or rax, rcx"),
            BinOp::BitXor => self.line("xor rax, rcx"),

            BinOp::Shl | BinOp::Shr => {
                // The count must be in `cl`, and it already is: `rcx`
                // holds the right operand.
                if operator == BinOp::Shl {
                    self.line("shl rax, cl");
                } else if signed {
                    // An arithmetic shift on a signed value, a logical
                    // one otherwise. Using `shr` on an i64 turns every
                    // negative number positive.
                    self.line("sar rax, cl");
                } else {
                    self.line("shr rax, cl");
                }
            }

            BinOp::Div | BinOp::Rem => {
                self.uses_panic = true;
                let alive = self.fresh("divok");
                self.line("cmp rcx, 0");
                self.line(&alloc::format!("jne {}", alive));
                self.line("call __nova_panic_div0");
                self.label(&alive);
                if signed {
                    // `cqo` sign-extends rax into rdx. Without it the
                    // dividend is rax with whatever rdx held, and the
                    // quotient is arbitrary.
                    self.line("cqo");
                    self.line("idiv rcx");
                } else {
                    self.line("mov rdx, 0");
                    self.line("div rcx");
                }
                if operator == BinOp::Rem {
                    self.line("mov rax, rdx");
                }
            }

            _ => {
                // The six comparisons. A `setcc` would be shorter and
                // needs an 8-bit destination; two jumps need nothing the
                // assembler does not already have.
                let condition = match (operator, signed) {
                    (BinOp::Eq, _) => "jne",
                    (BinOp::Ne, _) => "je",
                    (BinOp::Lt, true) => "jge",
                    (BinOp::Lt, false) => "jae",
                    (BinOp::Le, true) => "jg",
                    (BinOp::Le, false) => "ja",
                    (BinOp::Gt, true) => "jle",
                    (BinOp::Gt, false) => "jbe",
                    (BinOp::Ge, true) => "jl",
                    (BinOp::Ge, false) => "jb",
                    _ => "jne",
                };
                let end = self.fresh("cmp");
                self.line("cmp rax, rcx");
                // `mov` does not touch the flags, so the comparison
                // survives it. Zeroing with `xor` would not.
                self.line("mov rax, 0");
                self.line(&alloc::format!("{} {}", condition, end));
                self.line("mov rax, 1");
                self.label(&end);
            }
        }
    }

    /// `==` and `!=` on two `&str`.
    fn string_compare(&mut self, operator: BinOp, left: &Expr, right: &Expr) {
        self.uses_str_eq = true;
        self.expr(left);
        self.line("push rax");
        self.expr(right);
        self.line("mov rsi, rax");
        self.line("pop rdi");
        self.line("call __nova_str_eq");
        if operator == BinOp::Ne {
            self.line("xor rax, 1");
        }
    }

    fn call(&mut self, name: &str, args: &[Expr]) {
        // Arguments are evaluated left to right, as Rust does, and each
        // is pushed. Evaluating straight into the argument registers
        // would let the second argument's own call destroy the first.
        for argument in args.iter() {
            self.expr(argument);
            self.line("push rax");
        }
        for index in (0..args.len()).rev() {
            self.line(&alloc::format!("pop {}", ARGUMENT_REGISTERS[index]));
        }

        if let Some(builtin) = BUILTINS.iter().find(|entry| entry.name == name) {
            self.builtin(builtin.kind);
            return;
        }
        self.line(&alloc::format!("call {}", name));
    }

    fn builtin(&mut self, kind: Builtin) {
        match kind {
            Builtin::Exit => {
                self.line(&alloc::format!("mov rax, {}", SYS_PROC_EXIT));
                self.line("syscall");
                // A task that comes back from EXIT is a task the kernel
                // failed to stop. Halting is the only honest answer.
                self.line("hlt");
            }
            Builtin::SleepMs => {
                self.line(&alloc::format!("mov rax, {}", SYS_TIME_SLEEP_MS));
                self.line("syscall");
                self.line("mov rax, 0");
            }
            Builtin::Yield => {
                self.line(&alloc::format!("mov rax, {}", SYS_PROC_YIELD));
                self.line("syscall");
                self.line("mov rax, 0");
            }
            Builtin::Pid => {
                self.line(&alloc::format!("mov rax, {}", SYS_PROC_SELF));
                self.line("syscall");
            }
        }
    }

    // --------------------------------------------------------
    //  print! / println!
    // --------------------------------------------------------

    fn print(&mut self, parts: &[FormatPart], args: &[Expr], newline: bool) {
        self.uses_output = true;

        for part in parts.iter() {
            match part {
                FormatPart::Literal(text) => {
                    let label = self.string(text);
                    self.line(&alloc::format!("lea rdi, [rip + {}]", label));
                    self.line("call __nova_buf_str");
                }
                FormatPart::Hole(index) => {
                    let Some(argument) = args.get(*index) else {
                        continue;
                    };
                    let ty = self.static_type(argument);
                    self.expr(argument);
                    self.line("mov rdi, rax");
                    let helper = match ty {
                        Type::I64 => "__nova_buf_i64",
                        Type::U64 => "__nova_buf_u64",
                        Type::Bool => "__nova_buf_bool",
                        _ => "__nova_buf_str",
                    };
                    self.line(&alloc::format!("call {}", helper));
                }
            }
        }

        // `print!` leaves the line open; `println!` sends it. The console
        // is line-oriented -- one `SYS_DEBUG_PRINT` is one line -- so a
        // `print!` that flushed would break a line into pieces, which is
        // the opposite of what it means in Rust.
        if newline {
            self.line("call __nova_buf_flush");
        }
        self.line("mov rax, 0");
    }

    /// The type of an expression, recomputed for the code generator.
    ///
    /// The checker established it; re-deriving it here rather than
    /// threading it through the tree keeps the tree the one thing the
    /// interpreter and the compiler both read unchanged. It is exact for
    /// what it is asked -- an operand's type, an argument's type -- and
    /// falls back to `i64` nowhere the answer matters.
    fn static_type(&self, expr: &Expr) -> Type {
        match expr {
            Expr::Int(_, Some(ty)) => *ty,
            Expr::Int(_, None) => Type::I64,
            Expr::Bool(_) => Type::Bool,
            Expr::Str(_) => Type::Str,
            Expr::Unit => Type::Unit,
            Expr::Path(name, _) => self.name_type(name),
            Expr::Unary(UnOp::Neg, _, _) => Type::I64,
            Expr::Unary(UnOp::Not, inner, _) => self.static_type(inner),
            Expr::Binary(operator, left, _, _) => {
                if operator.is_comparison() || matches!(operator, BinOp::And | BinOp::Or) {
                    Type::Bool
                } else {
                    self.static_type(left)
                }
            }
            Expr::Cast(_, ty, _) => *ty,
            Expr::Call(name, _, _) => signature_of(self.program, name)
                .map(|signature| signature.result)
                .unwrap_or(Type::Unit),
            Expr::If(_, then_block, _, _) => then_block
                .tail
                .as_ref()
                .map(|tail| self.static_type(tail))
                .unwrap_or(Type::Unit),
            Expr::Block(block) => block
                .tail
                .as_ref()
                .map(|tail| self.static_type(tail))
                .unwrap_or(Type::Unit),
        }
    }

    /// Type of a name: local of the current function first, then
    /// constant. That order is the scoping rule -- a local of the same
    /// name as a constant is refused by the checker, so in practice only
    /// one of the two ever answers.
    fn name_type(&self, name: &str) -> Type {
        if let Some(ty) = self.local_type(name) {
            return ty;
        }
        self.checked
            .constants
            .iter()
            .find(|(other, _, _)| other == name)
            .map(|(_, ty, _)| *ty)
            .unwrap_or(Type::I64)
    }

    // --------------------------------------------------------
    //  Runtime
    // --------------------------------------------------------

    /// The helper routines, emitted only when something calls them.
    fn runtime(&self) -> String {
        let mut out = String::new();

        if self.uses_panic {
            out.push_str(
                "\n__nova_panic_div0:\n\
                 \x20   lea rdi, [rip + __nova_msg_div0]\n\
                 \x20   mov rsi, [rdi + 0]\n\
                 \x20   add rdi, 8\n\
                 \x20   mov rax, 1\n\
                 \x20   syscall\n",
            );
            out.push_str(&alloc::format!(
                "\x20   mov rdi, {}\n\x20   mov rax, {}\n\x20   syscall\n\x20   hlt\n",
                PANIC_EXIT,
                SYS_PROC_EXIT
            ));
        }

        if self.uses_str_eq {
            // rdi and rsi are two descriptors; rax comes back 0 or 1.
            out.push_str(
                "\n__nova_str_eq:\n\
                 \x20   mov r12, [rdi + 0]\n\
                 \x20   mov r13, [rsi + 0]\n\
                 \x20   cmp r12, r13\n\
                 \x20   jne __Lsq_no\n\
                 \x20   mov r14, 0\n\
                 __Lsq_loop:\n\
                 \x20   cmp r14, r12\n\
                 \x20   jae __Lsq_yes\n\
                 \x20   mov r15, rdi\n\
                 \x20   add r15, r14\n\
                 \x20   mov rax, 0\n\
                 \x20   mov al, [r15 + 8]\n\
                 \x20   mov r15, rsi\n\
                 \x20   add r15, r14\n\
                 \x20   mov rcx, 0\n\
                 \x20   mov cl, [r15 + 8]\n\
                 \x20   cmp rax, rcx\n\
                 \x20   jne __Lsq_no\n\
                 \x20   inc r14\n\
                 \x20   jmp __Lsq_loop\n\
                 __Lsq_yes:\n\
                 \x20   mov rax, 1\n\
                 \x20   ret\n\
                 __Lsq_no:\n\
                 \x20   mov rax, 0\n\
                 \x20   ret\n",
            );
        }

        if !self.uses_output {
            return out;
        }

        // One byte into the buffer. Silently dropped once full: the
        // alternative is to fail a `println!`, and a diagnostic that
        // cannot be printed because it is too long is worse than a
        // diagnostic that is cut.
        out.push_str(&alloc::format!(
            "\n__nova_buf_push:\n\
             \x20   mov rax, [rip + __nova_buf_len]\n\
             \x20   cmp rax, {}\n\
             \x20   jae __Lbp_full\n\
             \x20   lea rcx, [rip + __nova_buf]\n\
             \x20   add rcx, rax\n\
             \x20   mov [rcx + 0], dil\n\
             \x20   inc rax\n\
             \x20   mov [rip + __nova_buf_len], rax\n\
             __Lbp_full:\n\
             \x20   ret\n",
            BUFFER_SIZE
        ));

        out.push_str(
            "\n__nova_buf_str:\n\
             \x20   mov r12, rdi\n\
             \x20   mov r13, [r12 + 0]\n\
             \x20   mov r14, 0\n\
             __Lbs_loop:\n\
             \x20   cmp r14, r13\n\
             \x20   jae __Lbs_done\n\
             \x20   mov r15, r12\n\
             \x20   add r15, r14\n\
             \x20   mov rdi, 0\n\
             \x20   mov dil, [r15 + 8]\n\
             \x20   call __nova_buf_push\n\
             \x20   inc r14\n\
             \x20   jmp __Lbs_loop\n\
             __Lbs_done:\n\
             \x20   ret\n",
        );

        // Digits come out least significant first, so they go on the
        // machine stack and come back off it in the right order. Division
        // is **unsigned** even for an i64: the caller has already turned
        // the value into its magnitude, and the magnitude of i64::MIN
        // does not fit in an i64.
        out.push_str(
            "\n__nova_buf_u64:\n\
             \x20   mov r12, 0\n\
             \x20   mov rax, rdi\n\
             \x20   cmp rax, 0\n\
             \x20   jne __Lbu_conv\n\
             \x20   mov rdi, 48\n\
             \x20   call __nova_buf_push\n\
             \x20   ret\n\
             __Lbu_conv:\n\
             \x20   mov rdx, 0\n\
             \x20   mov rcx, 10\n\
             \x20   div rcx\n\
             \x20   push rdx\n\
             \x20   inc r12\n\
             \x20   cmp rax, 0\n\
             \x20   jne __Lbu_conv\n\
             __Lbu_emit:\n\
             \x20   cmp r12, 0\n\
             \x20   je __Lbu_done\n\
             \x20   pop rdi\n\
             \x20   add rdi, 48\n\
             \x20   call __nova_buf_push\n\
             \x20   dec r12\n\
             \x20   jmp __Lbu_emit\n\
             __Lbu_done:\n\
             \x20   ret\n",
        );

        // `neg` on i64::MIN gives i64::MIN back, whose **unsigned** value
        // is exactly the magnitude wanted. That is why the sign is peeled
        // off here and the digits are produced unsigned.
        out.push_str(
            "\n__nova_buf_i64:\n\
             \x20   cmp rdi, 0\n\
             \x20   jge __Lbi_pos\n\
             \x20   mov r13, rdi\n\
             \x20   mov rdi, 45\n\
             \x20   call __nova_buf_push\n\
             \x20   mov rdi, r13\n\
             \x20   neg rdi\n\
             __Lbi_pos:\n\
             \x20   jmp __nova_buf_u64\n",
        );

        out.push_str(
            "\n__nova_buf_bool:\n\
             \x20   cmp rdi, 0\n\
             \x20   je __Lbb_false\n\
             \x20   lea rdi, [rip + __nova_true]\n\
             \x20   jmp __nova_buf_str\n\
             __Lbb_false:\n\
             \x20   lea rdi, [rip + __nova_false]\n\
             \x20   jmp __nova_buf_str\n",
        );

        out.push_str(&alloc::format!(
            "\n__nova_buf_flush:\n\
             \x20   lea rdi, [rip + __nova_buf]\n\
             \x20   mov rsi, [rip + __nova_buf_len]\n\
             \x20   mov rax, {}\n\
             \x20   syscall\n\
             \x20   mov rax, 0\n\
             \x20   mov [rip + __nova_buf_len], rax\n\
             \x20   ret\n",
            SYS_DEBUG_PRINT
        ));

        out
    }

    /// `.data` and `.bss`.
    fn data(&self) -> String {
        let mut out = String::from("\n.data\n");

        for (index, text) in self.strings.iter().enumerate() {
            out.push_str(&alloc::format!("__nova_str_{}:\n", index));
            out.push_str(&alloc::format!("    dq {}\n", text.len()));
            if !text.is_empty() {
                out.push_str(&alloc::format!("    db {}\n", quote(text)));
            }
            // Padded to eight bytes so the next descriptor's length word
            // stays aligned. x86 tolerates an unaligned load; a reader of
            // the disassembly does not.
            let padding = (8 - (text.len() % 8)) % 8;
            if padding != 0 {
                out.push_str(&alloc::format!("    resb {}\n", padding));
            }
        }

        if self.uses_panic {
            let message = "panique : division par zero";
            out.push_str("__nova_msg_div0:\n");
            out.push_str(&alloc::format!("    dq {}\n", message.len()));
            out.push_str(&alloc::format!("    db {}\n", quote(message)));
        }

        if self.uses_output {
            out.push_str("__nova_true:\n    dq 4\n    db \"true\"\n    resb 4\n");
            out.push_str("__nova_false:\n    dq 5\n    db \"false\"\n    resb 3\n");
            out.push_str("\n.bss\n");
            out.push_str("__nova_buf_len:\n    resb 8\n");
            out.push_str(&alloc::format!("__nova_buf:\n    resb {}\n", BUFFER_SIZE));
        }

        out
    }
}

/// Renders a string for a `db` directive.
///
/// A newline cannot go inside the quotes -- the assembler cuts its lines
/// on it -- so the text is broken into runs and the control characters
/// come out as numbers. That is also why `db` accepts a list.
fn quote(text: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut run = String::new();

    for byte in text.bytes() {
        if (0x20..=0x7E).contains(&byte) && byte != b'"' {
            run.push(byte as char);
            continue;
        }
        if !run.is_empty() {
            parts.push(alloc::format!("\"{}\"", core::mem::take(&mut run)));
        }
        parts.push(alloc::format!("{}", byte));
    }
    if !run.is_empty() {
        parts.push(alloc::format!("\"{}\"", run));
    }
    if parts.is_empty() {
        parts.push(String::from("0"));
    }
    parts.join(", ")
}
