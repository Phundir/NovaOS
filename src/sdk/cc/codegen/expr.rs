//! Expressions: addresses, loads and stores by type, conversions,
//! operators, calls. The conventions are in `mod.rs`.

use alloc::string::String;

use super::super::CcError;
use super::super::ast::*;
use super::super::ty::{self, Kind, Type};
use super::Generator;

/// 2^63 as a `double`: the boundary of the conversions between `double`
/// and `unsigned long`.
const TWO_63: u64 = 0x43E0_0000_0000_0000;
const SIGN: u64 = 0x8000_0000_0000_0000;

impl Generator<'_> {
    /// Leaves the address of an lvalue in `rax`.
    fn addr(&mut self, node: &Node) -> Result<(), CcError> {
        match &node.kind {
            NodeKind::Local(index) => {
                let offset = self.local_offset(*index);
                self.line(&memory_lea("rbp", offset));
            }
            NodeKind::Global(label) => self.line(&alloc::format!("lea rax, [rip + {}]", label)),
            NodeKind::Deref(inner) => self.expr(inner)?,
            NodeKind::Member(inner, offset) => {
                self.addr(inner)?;
                if *offset != 0 {
                    self.line(&alloc::format!("add rax, {}", offset));
                }
            }
            NodeKind::Comma(first, second) => {
                self.expr(first)?;
                self.addr(second)?;
            }
            // A structure-valued expression already yields an address.
            NodeKind::Call { .. } | NodeKind::Cond(..) | NodeKind::Assign(..) if node.ty.is_record() => self.expr(node)?,
            _ => return Err(self.error(node.loc, "adresse d'une expression qui n'est pas un objet")),
        }
        Ok(())
    }

    /// Replaces the address in `rax` by the value of type `ty` stored
    /// there.
    fn load(&mut self, ty: &Type) {
        match ty.kind {
            Kind::Array | Kind::Struct | Kind::Union | Kind::Func => {}
            Kind::Bool => self.line("movzx rax, byte [rax]"),
            Kind::Char if ty.unsigned => self.line("movzx rax, byte [rax]"),
            Kind::Char => self.line("movsx rax, byte [rax]"),
            Kind::Short if ty.unsigned => self.line("movzx rax, word [rax]"),
            Kind::Short => self.line("movsx rax, word [rax]"),
            Kind::Int if ty.unsigned => self.line("mov eax, dword [rax]"),
            Kind::Int => self.line("movsxd rax, dword [rax]"),
            Kind::Float => {
                self.line("mov eax, dword [rax]");
                self.line("movd xmm0, eax");
                self.line("cvtss2sd xmm0, xmm0");
                self.line("movq rax, xmm0");
            }
            _ => self.line("mov rax, [rax]"),
        }
    }

    /// Stores `rax` (a value of type `ty`) at the address on top of the
    /// stack, which is popped. `rax` keeps the value.
    fn store(&mut self, ty: &Type) {
        self.line("pop rdi");
        match ty.kind {
            Kind::Struct | Kind::Union => {
                self.line("mov rsi, rax");
                self.line(&alloc::format!("mov rcx, {}", ty.size()));
                self.line("rep movsb");
            }
            Kind::Bool | Kind::Char => self.line("mov byte [rdi], al"),
            Kind::Short => self.line("mov word [rdi], ax"),
            Kind::Int => self.line("mov dword [rdi], eax"),
            Kind::Float => {
                self.line("movq xmm0, rax");
                self.line("cvtsd2ss xmm0, xmm0");
                self.line("movd edx, xmm0");
                self.line("mov dword [rdi], edx");
            }
            _ => self.line("mov [rdi], rax"),
        }
    }

    /// Brings `rax` back to the canonical form of the integer type `ty`.
    fn normalize(&mut self, ty: &Type) {
        match (ty.kind, ty.unsigned) {
            (Kind::Bool, _) | (Kind::Char, true) => self.line("movzx rax, al"),
            (Kind::Char, false) => self.line("movsx rax, al"),
            (Kind::Short, true) => self.line("movzx rax, ax"),
            (Kind::Short, false) => self.line("movsx rax, ax"),
            (Kind::Int, true) => self.line("mov eax, eax"),
            (Kind::Int, false) => self.line("movsxd rax, eax"),
            _ => {}
        }
    }

    /// `rax` rounded to single precision (still as a double).
    fn round_float(&mut self) {
        self.line("movq xmm0, rax");
        self.line("cvtsd2ss xmm0, xmm0");
        self.line("cvtss2sd xmm0, xmm0");
        self.line("movq rax, xmm0");
    }

    fn cast(&mut self, from: &Type, to: &Type) {
        if to.kind == Kind::Void || matches!(from.kind, Kind::Array | Kind::Func | Kind::Struct | Kind::Union) {
            return;
        }
        if to.kind == Kind::Bool {
            if from.is_flonum() {
                self.line("shl rax, 1");
            }
            self.line("cmp rax, 0");
            self.line("setne al");
            self.line("movzx rax, al");
            return;
        }
        let from_int = !from.is_flonum();
        let to_int = !to.is_flonum();
        if from_int && to_int {
            if to.size() < 8 {
                self.normalize(to);
            }
            return;
        }
        if from_int {
            if from.size() == 8 && from.unsigned {
                // An unsigned 64-bit value above 2^63: halve it (keeping
                // the low bit, for rounding), convert, double.
                let (high, done) = (self.fresh(), self.fresh());
                self.line("test rax, rax");
                self.line(&alloc::format!("js {}", high));
                self.line("cvtsi2sd xmm0, rax");
                self.line(&alloc::format!("jmp {}", done));
                self.label(&high);
                self.line("mov rdi, rax");
                self.line("shr rdi, 1");
                self.line("and rax, 1");
                self.line("or rdi, rax");
                self.line("cvtsi2sd xmm0, rdi");
                self.line("addsd xmm0, xmm0");
                self.label(&done);
            } else {
                self.line("cvtsi2sd xmm0, rax");
            }
            if to.kind == Kind::Float {
                self.line("cvtsd2ss xmm0, xmm0");
                self.line("cvtss2sd xmm0, xmm0");
            }
            self.line("movq rax, xmm0");
            return;
        }
        if to_int {
            self.line("movq xmm0, rax");
            if to.size() == 8 && to.unsigned {
                let (high, done) = (self.fresh(), self.fresh());
                self.line(&alloc::format!("mov rdi, {}", TWO_63 as i64));
                self.line("movq xmm1, rdi");
                self.line("ucomisd xmm0, xmm1");
                self.line(&alloc::format!("jae {}", high));
                self.line("cvttsd2si rax, xmm0");
                self.line(&alloc::format!("jmp {}", done));
                self.label(&high);
                self.line("subsd xmm0, xmm1");
                self.line("cvttsd2si rax, xmm0");
                self.line(&alloc::format!("mov rdi, {}", SIGN as i64));
                self.line("xor rax, rdi");
                self.label(&done);
            } else {
                self.line("cvttsd2si rax, xmm0");
                if to.size() < 8 {
                    self.normalize(to);
                }
            }
            return;
        }
        if to.kind == Kind::Float && from.kind == Kind::Double {
            self.round_float();
        }
    }

    pub(super) fn expr(&mut self, node: &Node) -> Result<(), CcError> {
        match &node.kind {
            NodeKind::Null => {}
            NodeKind::Num(value) => self.line(&alloc::format!("mov rax, {}", value)),
            NodeKind::Float(bits) => self.line(&alloc::format!("mov rax, {}", *bits as i64)),
            NodeKind::Local(_) | NodeKind::Global(_) | NodeKind::Member(..) | NodeKind::Deref(_) => {
                self.addr(node)?;
                self.load(&node.ty);
            }
            NodeKind::Addr(inner) => self.addr(inner)?,
            NodeKind::Neg(inner) => {
                self.expr(inner)?;
                if node.ty.is_flonum() {
                    self.line(&alloc::format!("mov rdi, {}", SIGN as i64));
                    self.line("xor rax, rdi");
                } else {
                    self.line("neg rax");
                    self.normalize(&node.ty);
                }
            }
            NodeKind::Not(inner) => {
                self.expr(inner)?;
                if inner.ty.is_flonum() {
                    self.line("shl rax, 1");
                }
                self.line("cmp rax, 0");
                self.line("sete al");
                self.line("movzx rax, al");
            }
            NodeKind::BitNot(inner) => {
                self.expr(inner)?;
                self.line("not rax");
                self.normalize(&node.ty);
            }
            NodeKind::Cast(inner) => {
                self.expr(inner)?;
                self.cast(&inner.ty, &node.ty);
            }
            NodeKind::Binary(op, left, right) => self.binary(*op, left, right, &node.ty)?,
            NodeKind::LogAnd(left, right) | NodeKind::LogOr(left, right) => {
                let and = matches!(node.kind, NodeKind::LogAnd(..));
                let (short, end) = (self.fresh(), self.fresh());
                self.truth(left)?;
                self.line(&alloc::format!("{} {}", if and { "je" } else { "jne" }, short));
                self.truth(right)?;
                self.line(&alloc::format!("{} {}", if and { "je" } else { "jne" }, short));
                self.line(&alloc::format!("mov rax, {}", if and { 1 } else { 0 }));
                self.line(&alloc::format!("jmp {}", end));
                self.label(&short);
                self.line(&alloc::format!("mov rax, {}", if and { 0 } else { 1 }));
                self.label(&end);
            }
            NodeKind::Cond(condition, then, other) => {
                let (other_label, end) = (self.fresh(), self.fresh());
                self.truth(condition)?;
                self.line(&alloc::format!("je {}", other_label));
                self.expr(then)?;
                self.line(&alloc::format!("jmp {}", end));
                self.label(&other_label);
                self.expr(other)?;
                self.label(&end);
            }
            NodeKind::Comma(first, second) => {
                self.expr(first)?;
                self.expr(second)?;
            }
            NodeKind::Assign(target, value) => {
                self.addr(target)?;
                self.line("push rax");
                self.expr(value)?;
                self.store(&target.ty);
            }
            NodeKind::Call { callee, args, function, buffer } => self.call(callee, args, function, *buffer)?,
            NodeKind::Zero(index) => {
                let offset = self.local_offset(*index);
                let size = self.type_of_local(*index).size();
                self.line(&memory_lea("rbp", offset).replacen("rax", "rdi", 1));
                self.line(&alloc::format!("mov rcx, {}", size));
                self.line("mov rax, 0");
                self.line("rep stosb");
            }
            NodeKind::Syscall(args) => {
                for arg in args {
                    self.expr(arg)?;
                    self.line("push rax");
                }
                const REGISTERS: [&str; 7] = ["rax", "rdi", "rsi", "rdx", "r10", "r8", "r9"];
                for register in REGISTERS[..args.len()].iter().rev() {
                    self.line(&alloc::format!("pop {}", register));
                }
                self.line("syscall");
            }
            NodeKind::Sqrt(inner) => {
                self.expr(inner)?;
                self.line("movq xmm0, rax");
                self.line("sqrtsd xmm0, xmm0");
                self.line("movq rax, xmm0");
            }
            NodeKind::VaStart => {
                let offset = self.function.map(|f| f.va_offset).unwrap_or(16);
                self.line(&memory_lea("rbp", offset));
            }
            _ => return Err(self.error(node.loc, "instruction a la place d'une expression")),
        }
        Ok(())
    }

    fn binary(&mut self, op: Op, left: &Node, right: &Node, ty: &Type) -> Result<(), CcError> {
        self.expr(right)?;
        self.line("push rax");
        self.expr(left)?;
        self.line("pop rdi");

        if left.ty.is_flonum() {
            self.line("movq xmm0, rax");
            self.line("movq xmm1, rdi");
            let instruction = match op {
                Op::Add => "addsd xmm0, xmm1",
                Op::Sub => "subsd xmm0, xmm1",
                Op::Mul => "mulsd xmm0, xmm1",
                Op::Div => "divsd xmm0, xmm1",
                Op::Eq | Op::Ne => {
                    self.line("ucomisd xmm0, xmm1");
                    if op == Op::Eq {
                        self.line("sete al");
                        self.line("setnp cl");
                    } else {
                        self.line("setne al");
                        self.line("setp cl");
                    }
                    self.line("movzx rax, al");
                    self.line("movzx rcx, cl");
                    self.line(if op == Op::Eq { "and rax, rcx" } else { "or rax, rcx" });
                    return Ok(());
                }
                Op::Lt | Op::Le => {
                    // a < b as b > a: `seta` is false on an unordered
                    // result, as C wants for a NaN.
                    self.line("ucomisd xmm1, xmm0");
                    self.line(if op == Op::Lt { "seta al" } else { "setae al" });
                    self.line("movzx rax, al");
                    return Ok(());
                }
                _ => return Err(self.error(left.loc, "operation entiere sur des flottants")),
            };
            self.line(instruction);
            if ty.kind == Kind::Float {
                self.line("cvtsd2ss xmm0, xmm0");
                self.line("cvtss2sd xmm0, xmm0");
            }
            self.line("movq rax, xmm0");
            return Ok(());
        }

        let unsigned = left.ty.unsigned || left.ty.kind == Kind::Ptr;
        match op {
            Op::Add => self.line("add rax, rdi"),
            Op::Sub => self.line("sub rax, rdi"),
            Op::Mul => self.line("imul rax, rdi"),
            Op::Div | Op::Mod => {
                if unsigned {
                    self.line("mov rdx, 0");
                    self.line("div rdi");
                } else {
                    self.line("cqo");
                    self.line("idiv rdi");
                }
                if op == Op::Mod {
                    self.line("mov rax, rdx");
                }
            }
            Op::BitAnd => self.line("and rax, rdi"),
            Op::BitOr => self.line("or rax, rdi"),
            Op::BitXor => self.line("xor rax, rdi"),
            Op::Shl => {
                self.line("mov rcx, rdi");
                self.line("shl rax, cl");
            }
            Op::Shr => {
                self.line("mov rcx, rdi");
                self.line(if unsigned { "shr rax, cl" } else { "sar rax, cl" });
            }
            Op::Eq | Op::Ne | Op::Lt | Op::Le => {
                self.line("cmp rax, rdi");
                let set = match (op, unsigned) {
                    (Op::Eq, _) => "sete al",
                    (Op::Ne, _) => "setne al",
                    (Op::Lt, true) => "setb al",
                    (Op::Lt, false) => "setl al",
                    (Op::Le, true) => "setbe al",
                    _ => "setle al",
                };
                self.line(set);
                self.line("movzx rax, al");
                return Ok(());
            }
        }
        if ty.is_integer() && ty.size() < 8 {
            self.normalize(ty);
        }
        Ok(())
    }

    fn call(&mut self, callee: &Node, args: &[Node], function: &Type, buffer: Option<usize>) -> Result<(), CcError> {
        let mut pushed: i64 = 0;
        for (index, arg) in args.iter().enumerate().rev() {
            self.expr(arg)?;
            if arg.ty.is_record() {
                let size = arg.ty.size();
                let words = ty::align_to(size.max(1), 8);
                self.line(&alloc::format!("sub rsp, {}", words));
                self.line("mov rsi, rax");
                self.line("mov rdi, rsp");
                self.line(&alloc::format!("mov rcx, {}", size));
                self.line("rep movsb");
                pushed += words;
                continue;
            }
            // A `float` parameter receives its 4 bytes.
            if function.params.get(index).is_some_and(|p| p.kind == Kind::Float) {
                self.line("movq xmm0, rax");
                self.line("cvtsd2ss xmm0, xmm0");
                self.line("movd eax, xmm0");
            }
            self.line("push rax");
            pushed += 8;
        }
        if let Some(buffer) = buffer {
            let offset = self.local_offset(buffer);
            self.line(&memory_lea("rbp", offset));
            self.line("push rax");
            pushed += 8;
        }
        match &callee.kind {
            NodeKind::Global(label) if callee.ty.kind == Kind::Func => {
                self.line(&alloc::format!("call {}", label));
            }
            _ => {
                self.expr(callee)?;
                self.line("call rax");
            }
        }
        if pushed > 0 {
            self.line(&alloc::format!("add rsp, {}", pushed));
        }
        Ok(())
    }
}

/// `lea rax, [base +/- offset]`.
fn memory_lea(base: &str, offset: i64) -> String {
    if offset < 0 {
        alloc::format!("lea rax, [{} - {}]", base, -offset)
    } else {
        alloc::format!("lea rax, [{} + {}]", base, offset)
    }
}
