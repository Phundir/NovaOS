//! 16.6 -- What a C compiler needs of the assembler, and nothing more.
//!
//! The Rust subset of 9.5.1 has one type of value, the 64-bit word, and
//! the instructions of `mod.rs` were chosen for it. C has eight integer
//! widths, signed and unsigned, and floating point. Compiling it through
//! the same assembler (the reason is in `mod.rs`: one encoder, not two)
//! asks for:
//!
//! * **sized memory accesses**: `movsx`/`movzx` to read a `char` or a
//!   `short` and extend it, `movsxd` and `mov r32` for an `int` or an
//!   `unsigned`, stores of 8, 16 and 32 bits -- and the `byte`, `word`,
//!   `dword`, `qword` prefixes that say which;
//! * **`lea` on a local**, to take the address of a variable;
//! * **`setcc`**, to turn a comparison into the 0 or 1 C defines;
//! * **`test`**, **`call`/`jmp` through a register** (function pointers),
//!   **`rep movsb`/`rep stosb`** (copying and clearing a structure);
//! * **scalar SSE2**: `movq`/`movd` between general and XMM registers,
//!   `addsd` and its siblings, `ucomisd`, the conversions. A `double`
//!   travels in a general register as its bit pattern, like any other
//!   word, and visits an XMM register only to be computed on;
//! * **`align`**, so that a `double` or a pointer in `.data` sits on its
//!   natural boundary.
//!
//! Each encoding was checked against GNU `as` on the host
//! (`tools/cchost --asm`), byte for byte: a wrong prefix order here would
//! not fail to assemble, it would compute something else.
//!
//! `emit` answers only the forms it knows and returns `None` for the
//! rest, which `mod.rs` encodes as before: nothing that assembled
//! yesterday changes its bytes.

use alloc::string::String;

use super::{AsmError, Assembler, Memory, memory_operand, modrm, register, register8};

/// Width of an operand, in bytes.
type Width = u8;

const REGISTERS32: [&str; 16] = [
    "eax", "ecx", "edx", "ebx", "esp", "ebp", "esi", "edi", "r8d", "r9d", "r10d", "r11d", "r12d",
    "r13d", "r14d", "r15d",
];

const REGISTERS16: [&str; 16] = [
    "ax", "cx", "dx", "bx", "sp", "bp", "si", "di", "r8w", "r9w", "r10w", "r11w", "r12w", "r13w",
    "r14w", "r15w",
];

/// An operand as the extension reads it.
#[derive(Debug, Clone, Copy)]
enum Operand {
    /// A general register and its width.
    Reg(u8, Width),
    Xmm(u8),
    /// `[base + disp]`, with the width its prefix states, if any.
    Mem(Memory, Option<Width>),
}

fn find(table: &[&str; 16], name: &str) -> Option<u8> {
    table.iter().position(|candidate| *candidate == name).map(|index| index as u8)
}

fn operand(text: &str) -> Option<Operand> {
    let text = text.trim();
    if let Some(code) = register(text) {
        return Some(Operand::Reg(code, 8));
    }
    if let Some(code) = find(&REGISTERS32, text) {
        return Some(Operand::Reg(code, 4));
    }
    if let Some(code) = find(&REGISTERS16, text) {
        return Some(Operand::Reg(code, 2));
    }
    if let Some(code) = register8(text) {
        return Some(Operand::Reg(code, 1));
    }
    if let Some(number) = text.strip_prefix("xmm") {
        let code: u8 = number.parse().ok()?;
        return (code < 16).then_some(Operand::Xmm(code));
    }
    let (width, rest) = if let Some(rest) = text.strip_prefix("byte") {
        (Some(1), rest)
    } else if let Some(rest) = text.strip_prefix("dword") {
        (Some(4), rest)
    } else if let Some(rest) = text.strip_prefix("qword") {
        (Some(8), rest)
    } else if let Some(rest) = text.strip_prefix("word") {
        (Some(2), rest)
    } else {
        (None, text)
    };
    let rest = rest.trim_start();
    let rest = if width.is_some() { rest.strip_prefix("ptr").map(str::trim_start).unwrap_or(rest) } else { rest };
    memory_operand(rest).map(|memory| Operand::Mem(memory, width))
}

/// Does the operand say its width?
fn sized(operand: &Operand) -> bool {
    match operand {
        Operand::Reg(_, width) => *width != 8,
        Operand::Mem(_, width) => width.is_some(),
        Operand::Xmm(_) => true,
    }
}

impl Assembler {
    /// Emits the optional REX prefix. `byte_reg` names a byte register
    /// that needs the prefix merely to exist (`spl`..`dil`).
    fn rex(&mut self, wide: bool, reg: u8, base: u8, byte_reg: bool) {
        let value = 0x40 | (u8::from(wide) << 3) | ((reg >> 3) << 2) | (base >> 3);
        if value != 0x40 || byte_reg {
            self.push(value);
        }
    }

    /// ModRM for a register or memory second operand.
    fn rm(&mut self, reg: u8, target: &Operand) {
        match *target {
            Operand::Reg(code, _) | Operand::Xmm(code) => self.push(modrm(3, reg, code)),
            Operand::Mem(memory, _) => self.memory(reg, memory),
        }
    }
}

/// The base register of a register-or-memory operand, for REX.B.
fn base_of(target: &Operand) -> u8 {
    match *target {
        Operand::Reg(code, _) | Operand::Xmm(code) => code,
        Operand::Mem(memory, _) => memory.base,
    }
}

/// Is this a byte register that needs REX to be named?
fn low_byte(target: &Operand) -> bool {
    matches!(*target, Operand::Reg(code, 1) if (4..8).contains(&code))
}

const SETCC: &[(&str, u8)] = &[
    ("seto", 0x90),
    ("setb", 0x92),
    ("setae", 0x93),
    ("sete", 0x94),
    ("setz", 0x94),
    ("setne", 0x95),
    ("setnz", 0x95),
    ("setbe", 0x96),
    ("seta", 0x97),
    ("sets", 0x98),
    ("setp", 0x9A),
    ("setnp", 0x9B),
    ("setl", 0x9C),
    ("setge", 0x9D),
    ("setle", 0x9E),
    ("setg", 0x9F),
];

/// Scalar SSE operations on two XMM registers: (name, prefix, opcode).
const SSE: &[(&str, u8, u8)] = &[
    ("addsd", 0xF2, 0x58),
    ("mulsd", 0xF2, 0x59),
    ("subsd", 0xF2, 0x5C),
    ("divsd", 0xF2, 0x5E),
    ("sqrtsd", 0xF2, 0x51),
    ("minsd", 0xF2, 0x5D),
    ("maxsd", 0xF2, 0x5F),
    ("cvtsd2ss", 0xF2, 0x5A),
    ("cvtss2sd", 0xF3, 0x5A),
    ("ucomisd", 0x66, 0x2E),
];

/// Encodes the forms of the extension; `None` leaves the line to the
/// encoders of `mod.rs`.
pub(super) fn emit(state: &mut Assembler, number: usize, mnemonic: &str, operands: &[String]) -> Option<Result<(), AsmError>> {
    let bad = |text: &str| Some(Err(AsmError::BadOperand(number, String::from(text))));
    let count = || Some(Err(AsmError::BadOperandCount(number, String::from(mnemonic))));

    match mnemonic {
        "movsx" | "movzx" | "movsxd" => {
            let [destination, source] = operands else { return count() };
            let Some(Operand::Reg(reg, 8)) = operand(destination) else { return bad(destination) };
            let Some(source_operand) = operand(source) else { return bad(source) };
            let width = match source_operand {
                Operand::Reg(_, width) => width,
                Operand::Mem(_, Some(width)) => width,
                _ => return bad(source),
            };
            let opcode: &[u8] = match (mnemonic, width) {
                ("movsx", 1) => &[0x0F, 0xBE],
                ("movsx", 2) => &[0x0F, 0xBF],
                ("movzx", 1) => &[0x0F, 0xB6],
                ("movzx", 2) => &[0x0F, 0xB7],
                ("movsxd", 4) => &[0x63],
                _ => return bad(source),
            };
            state.instructions += 1;
            state.rex(true, reg, base_of(&source_operand), low_byte(&source_operand));
            state.push_slice(opcode);
            state.rm(reg, &source_operand);
            Some(Ok(()))
        }

        "mov" => {
            let [destination, source] = operands else { return None };
            let (Some(to), Some(from)) = (operand(destination), operand(source)) else { return None };
            if !sized(&to) && !sized(&from) {
                return None;
            }
            // The width is the register's; a memory prefix must agree.
            let (reg, target, store, width) = match (to, from) {
                (Operand::Reg(reg, width), target @ Operand::Mem(_, prefix)) => {
                    if prefix.is_some_and(|p| p != width) {
                        return bad(source);
                    }
                    (reg, target, false, width)
                }
                (target @ Operand::Mem(_, prefix), Operand::Reg(reg, width)) => {
                    if prefix.is_some_and(|p| p != width) {
                        return bad(destination);
                    }
                    (reg, target, true, width)
                }
                (target @ Operand::Reg(_, w1), Operand::Reg(reg, w2)) if w1 == w2 => (reg, target, true, w1),
                _ => return bad(source),
            };
            state.instructions += 1;
            if width == 2 {
                state.push(0x66);
            }
            state.rex(width == 8, reg, base_of(&target), low_byte(&target) || (width == 1 && (4..8).contains(&reg)));
            let opcode = match (width, store) {
                (1, true) => 0x88,
                (1, false) => 0x8A,
                (_, true) => 0x89,
                (_, false) => 0x8B,
            };
            state.push(opcode);
            state.rm(reg, &target);
            Some(Ok(()))
        }

        "lea" => {
            let [destination, source] = operands else { return None };
            let Some(memory) = memory_operand(source) else { return None };
            let Some(reg) = register(destination) else { return bad(destination) };
            state.instructions += 1;
            state.rex(true, reg, memory.base, false);
            state.push(0x8D);
            state.memory(reg, memory);
            Some(Ok(()))
        }

        "test" => {
            let [first, second] = operands else { return count() };
            let (Some(target @ Operand::Reg(_, 8)), Some(Operand::Reg(reg, 8))) = (operand(first), operand(second)) else {
                return bad(first);
            };
            state.instructions += 1;
            state.rex(true, reg, base_of(&target), false);
            state.push(0x85);
            state.rm(reg, &target);
            Some(Ok(()))
        }

        "call" | "jmp" => {
            let [target] = operands else { return None };
            let Some(Operand::Reg(code, 8)) = operand(target) else { return None };
            state.instructions += 1;
            state.rex(false, 0, code, false);
            state.push(0xFF);
            state.push(modrm(3, if mnemonic == "call" { 2 } else { 4 }, code));
            Some(Ok(()))
        }

        "rep" => {
            let [what] = operands else { return count() };
            let opcode = match what.as_str() {
                "movsb" => 0xA4,
                "stosb" => 0xAA,
                _ => return bad(what),
            };
            state.instructions += 1;
            state.push(0xF3);
            state.push(opcode);
            Some(Ok(()))
        }

        "movq" | "movd" => {
            let [destination, source] = operands else { return count() };
            let wide = mnemonic == "movq";
            let expected = if wide { 8 } else { 4 };
            let (xmm, gpr, opcode) = match (operand(destination), operand(source)) {
                (Some(Operand::Xmm(x)), Some(Operand::Reg(r, w))) if w == expected => (x, r, 0x6E),
                (Some(Operand::Reg(r, w)), Some(Operand::Xmm(x))) if w == expected => (x, r, 0x7E),
                _ => return bad(source),
            };
            state.instructions += 1;
            state.push(0x66);
            state.rex(wide, xmm, gpr, false);
            state.push(0x0F);
            state.push(opcode);
            state.push(modrm(3, xmm, gpr));
            Some(Ok(()))
        }

        "cvtsi2sd" => {
            let [destination, source] = operands else { return count() };
            let (Some(Operand::Xmm(xmm)), Some(Operand::Reg(gpr, 8))) = (operand(destination), operand(source)) else {
                return bad(source);
            };
            state.instructions += 1;
            state.push(0xF2);
            state.rex(true, xmm, gpr, false);
            state.push_slice(&[0x0F, 0x2A]);
            state.push(modrm(3, xmm, gpr));
            Some(Ok(()))
        }

        "cvttsd2si" => {
            let [destination, source] = operands else { return count() };
            let (Some(Operand::Reg(gpr, 8)), Some(Operand::Xmm(xmm))) = (operand(destination), operand(source)) else {
                return bad(source);
            };
            state.instructions += 1;
            state.push(0xF2);
            state.rex(true, gpr, xmm, false);
            state.push_slice(&[0x0F, 0x2C]);
            state.push(modrm(3, gpr, xmm));
            Some(Ok(()))
        }

        _ => {
            if let Some((_, opcode)) = SETCC.iter().find(|(name, _)| *name == mnemonic) {
                let [target] = operands else { return count() };
                let Some(target_operand @ Operand::Reg(_, 1)) = operand(target) else { return bad(target) };
                state.instructions += 1;
                state.rex(false, 0, base_of(&target_operand), low_byte(&target_operand));
                state.push_slice(&[0x0F, *opcode]);
                state.rm(0, &target_operand);
                return Some(Ok(()));
            }
            if let Some((_, prefix, opcode)) = SSE.iter().find(|(name, _, _)| *name == mnemonic) {
                let [destination, source] = operands else { return count() };
                let (Some(Operand::Xmm(to)), Some(Operand::Xmm(from))) = (operand(destination), operand(source)) else {
                    return bad(source);
                };
                state.instructions += 1;
                state.push(*prefix);
                state.rex(false, to, from, false);
                state.push_slice(&[0x0F, *opcode]);
                state.push(modrm(3, to, from));
                return Some(Ok(()));
            }
            None
        }
    }
}

/// `align n`: pads the current section to a multiple of `n` (a power of
/// two), with `nop` in `.text` and zeros elsewhere.
pub(super) fn align(state: &mut Assembler, number: usize, operands: &[String]) -> Result<(), AsmError> {
    let [value] = operands else {
        return Err(AsmError::BadOperandCount(number, String::from("align")));
    };
    let Some(boundary) = super::integer(value).filter(|n| *n > 0 && (*n as u64).is_power_of_two() && *n <= 4096) else {
        return Err(AsmError::BadOperand(number, value.clone()));
    };
    let filler = if state.section == super::Section::Text { 0x90 } else { 0 };
    while state.here() % boundary as u64 != 0 {
        state.push(filler);
    }
    Ok(())
}
