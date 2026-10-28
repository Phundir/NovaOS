//! 9.5.7 -- x86_64 assembler (spec 13.2, "direct assembly" column).
//!
//! ## Why this is the only line of 13.2 that could be written here
//!
//! The seven other languages in the table assume LLVM, Mono, a JVM or V8:
//! hundreds of megabytes of external dependency, ruled out by the "no
//! dependency" rule. Assembly, on the other hand, asks for nothing beyond
//! an instruction encoder and a label table. It fits in one file, it can
//! be audited line by line, and it produces code that really runs in ring
//! 3 -- which makes `dev build` something other than a ceremony around a
//! run of `nop`.
//!
//! ## Position-independent, and nothing else
//!
//! The assembler **emits no absolute relocation**. Every reference to a
//! datum goes through `lea rax, [rip + label]`, every jump through a
//! relative displacement. The reason is the one that made the phase 2.8
//! demonstration program RIP-relative: the loader arena places images
//! where it pleases, and a binary with nothing to fix up loads at any
//! base without any relocation table that could be forged. What the
//! format can do -- `RelocKind::Abs64` -- stays available for a future
//! linker; this path does not need it.
//!
//! ## Two passes, and fixed-size jumps
//!
//! Pass 1: walk the lines, measure each instruction, record the address
//! of each label. Pass 2: emit.
//!
//! Jumps are **always** encoded as `rel32`, never `rel8`, even when the
//! target is three bytes away. An assembler optimising size would see its
//! labels move, so it would have to start again until a fixed point -- an
//! algorithm that is correct but may fail to converge if programmed
//! badly. Five extra bytes per jump are worth more than a relaxation loop
//! inside a kernel.
//!
//! ## The RIP displacement is computed at link time, not here
//!
//! `[rip + label]` measures the distance between the end of the
//! instruction and the datum aimed at. But `.data` has no address yet
//! when `.text` is assembled: the binary emitter is what will decide the
//! layout. So the assembler leaves a four-byte hole and records a
//! `Fixup`. `sdk::emit` resolves them once the layout is known. That is
//! exactly the assembler / linker boundary, and respecting it avoids
//! having to know the layout in two places.
//!
//! ## What 9.5.1 added, and why here rather than beside
//!
//! The Rust compiler of `sdk::rustc` emits **assembly text** and hands it
//! to this file, the way rustc hands its output to an assembler. That
//! choice is what stops a second instruction encoder existing in the
//! tree -- two encoders diverge, and the divergence shows up as a program
//! that runs wrong rather than as a build that fails.
//!
//! But a compiler needs what a hand-written program can do without: a
//! stack frame. So four things were added, and nothing else:
//!
//! * **`[reg + disp]` memory operands.** Locals live at `[rbp - 8n]`.
//!   Without them the only storage is the sixteen registers, which caps
//!   a function at sixteen variables and forbids recursion.
//! * **Byte-sized `mov` to and from memory.** A decimal conversion
//!   writes its digits one byte at a time; the alternative was to build
//!   eight digits inside a register and store them in one go, which is a
//!   trick, not a fix.
//! * **`imul` and `idiv`.** `mul` and `div` are unsigned: using them for
//!   an `i64` gives a plausible and wrong answer for every negative
//!   operand, which is the worst kind of wrong.
//! * **`shl`, `shr`, `sar`**, by an immediate or by `cl`.
//!
//! Every addition is an instruction the manual describes, and none
//! changes what was already there.

use alloc::string::String;
use alloc::vec::Vec;

mod ext;

// ============================================================
//  Registers
// ============================================================

/// The sixteen general registers, in encoding order.
const REGISTERS: [&str; 16] = [
    "rax", "rcx", "rdx", "rbx", "rsp", "rbp", "rsi", "rdi", "r8", "r9", "r10", "r11", "r12", "r13",
    "r14", "r15",
];

fn register(name: &str) -> Option<u8> {
    REGISTERS
        .iter()
        .position(|candidate| *candidate == name)
        .map(|index| index as u8)
}

/// The sixteen low byte registers, in the same encoding order.
///
/// `spl`, `bpl`, `sil` and `dil` exist only **with** a REX prefix: with
/// no prefix those four encodings name `ah`, `ch`, `dh` and `bh`
/// instead. A byte `mov` on `sil` emitted without REX would therefore
/// write into the high half of `rax` -- a wrong program that assembles
/// perfectly. `byte_needs_rex` is what stops it.
const REGISTERS8: [&str; 16] = [
    "al", "cl", "dl", "bl", "spl", "bpl", "sil", "dil", "r8b", "r9b", "r10b", "r11b", "r12b",
    "r13b", "r14b", "r15b",
];

fn register8(name: &str) -> Option<u8> {
    REGISTERS8
        .iter()
        .position(|candidate| *candidate == name)
        .map(|index| index as u8)
}

/// Does naming this byte register require a REX prefix?
fn byte_needs_rex(code: u8) -> bool {
    code >= 4
}

// ============================================================
//  Errors
// ============================================================

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AsmError {
    /// Unknown instruction or directive.
    UnknownMnemonic(usize, String),
    /// The operand count does not match the instruction.
    BadOperandCount(usize, String),
    /// An operand is neither a register, nor an immediate, nor a label.
    BadOperand(usize, String),
    /// Label referenced but never defined.
    UndefinedLabel(String),
    /// Two definitions for the same label.
    DuplicateLabel(usize, String),
    /// Immediate outside the range the encoding accepts.
    ImmediateOutOfRange(usize),
    /// Unterminated string literal, or byte outside 0x20..0x7E.
    BadString(usize),
    /// A shift count outside 0..63.
    BadShiftCount(usize),
    /// No `_start` label in `.text`.
    NoEntryPoint,
    /// The program emits no code byte at all.
    EmptyText,
}

impl AsmError {
    pub fn message(&self) -> String {
        match self {
            AsmError::UnknownMnemonic(line, word) => {
                alloc::format!("ligne {} : mnemonique inconnu \"{}\"", line, word)
            }
            AsmError::BadOperandCount(line, word) => {
                alloc::format!("ligne {} : nombre d'operandes invalide pour \"{}\"", line, word)
            }
            AsmError::BadOperand(line, word) => {
                alloc::format!("ligne {} : operande invalide \"{}\"", line, word)
            }
            AsmError::UndefinedLabel(name) => {
                alloc::format!("etiquette \"{}\" jamais definie", name)
            }
            AsmError::DuplicateLabel(line, name) => {
                alloc::format!("ligne {} : etiquette \"{}\" definie deux fois", line, name)
            }
            AsmError::ImmediateOutOfRange(line) => {
                alloc::format!("ligne {} : immediat hors plage", line)
            }
            AsmError::BadString(line) => alloc::format!("ligne {} : chaine invalide", line),
            AsmError::BadShiftCount(line) => {
                alloc::format!("ligne {} : decalage hors de 0..63", line)
            }
            AsmError::NoEntryPoint => String::from("aucune etiquette _start dans .text"),
            AsmError::EmptyText => String::from("section .text vide"),
        }
    }
}

// ============================================================
//  Object produced
// ============================================================

/// Host section of a label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Text,
    Data,
    Bss,
}

impl Section {
    pub fn as_str(&self) -> &'static str {
        match self {
            Section::Text => ".text",
            Section::Data => ".data",
            Section::Bss => ".bss",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Symbol {
    pub name: String,
    pub section: Section,
    /// Offset from the start of its section.
    pub offset: u64,
}

/// A four-byte hole to fill once the layout is known.
#[derive(Debug, Clone)]
pub struct Fixup {
    /// Position of the 32-bit field inside `.text`.
    pub at: usize,
    /// Address, relative to `.text`, of the end of the instruction: that
    /// is where the processor measures a RIP-relative displacement from.
    pub next: usize,
    pub symbol: String,
}

/// Result of a successful assembly.
#[derive(Debug, Clone)]
pub struct Object {
    pub text: Vec<u8>,
    pub data: Vec<u8>,
    pub bss_size: u64,
    pub symbols: Vec<Symbol>,
    pub fixups: Vec<Fixup>,
    /// Offset of `_start` inside `.text`.
    pub entry: u64,
    /// Number of instructions emitted -- used by the `dev build` report.
    pub instructions: usize,
}

impl Object {
    pub fn symbol(&self, name: &str) -> Option<&Symbol> {
        self.symbols.iter().find(|symbol| symbol.name == name)
    }
}

// ============================================================
//  Splitting a line
// ============================================================

/// Strips the comment and the whitespace of a line.
///
/// The semicolon does not cut inside a string: `db ";"` is a one-character
/// string, not an empty comment.
fn strip_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut in_string = false;
    for (index, byte) in bytes.iter().enumerate() {
        match byte {
            b'"' => in_string = !in_string,
            b';' | b'#' if !in_string => return line[..index].trim(),
            _ => {}
        }
    }
    line.trim()
}

/// Separates the mnemonic from its operands.
///
/// Operands are cut on the comma, outside strings: `db "a,b", 0` gives
/// two operands and not three.
fn split_operands(rest: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut in_string = false;

    for character in rest.chars() {
        match character {
            '"' => {
                in_string = !in_string;
                current.push(character);
            }
            ',' if !in_string => {
                let trimmed = current.trim();
                if !trimmed.is_empty() {
                    parts.push(String::from(trimmed));
                }
                current.clear();
            }
            _ => current.push(character),
        }
    }
    let trimmed = current.trim();
    if !trimmed.is_empty() {
        parts.push(String::from(trimmed));
    }
    parts
}

/// Decodes an integer: decimal, hexadecimal `0x`, binary `0b`, or the
/// character form `'a'`.
fn integer(text: &str) -> Option<i64> {
    let text = text.trim();
    if let Some(rest) = text.strip_prefix("0x") {
        return i64::from_str_radix(rest, 16).ok();
    }
    if let Some(rest) = text.strip_prefix("-0x") {
        return i64::from_str_radix(rest, 16).ok().map(|value| -value);
    }
    if let Some(rest) = text.strip_prefix("0b") {
        return i64::from_str_radix(rest, 2).ok();
    }
    if text.len() == 3 && text.starts_with('\'') && text.ends_with('\'') {
        return Some(text.as_bytes()[1] as i64);
    }
    text.parse::<i64>().ok()
}

/// Decodes an immediate: integer literal, or difference of two labels of
/// the same section.
///
/// The difference serves to measure a string without counting its bytes
/// by hand: `dq end - start` or `mov rsi, end - start`. It is bounded to
/// one section because the gap between `.text` and `.data` depends on the
/// layout, which the assembler does not know -- the linker fixes it.
fn resolve_immediate(state: &Assembler, text: &str) -> Option<i64> {
    if let Some(value) = integer(text) {
        return Some(value);
    }
    let (left, right) = text.split_once('-')?;
    let (left, right) = (left.trim(), right.trim());
    if !is_label(left) || !is_label(right) {
        return None;
    }
    // In pass 1 not every label is known. Returning zero falsifies
    // nothing: the value does not enter into the instruction size, which
    // is the only thing pass 1 measures.
    if !state.emitting {
        return Some(0);
    }
    let (left_section, left_offset) = state.label_offset(left)?;
    let (right_section, right_offset) = state.label_offset(right)?;
    if left_section != right_section {
        return None;
    }
    Some(left_offset as i64 - right_offset as i64)
}

/// Decodes `[rip + label]` and returns the label.
fn rip_target(text: &str) -> Option<&str> {
    let inner = text.trim().strip_prefix('[')?.strip_suffix(']')?;
    let mut parts = inner.split('+');
    if parts.next()?.trim() != "rip" {
        return None;
    }
    let label = parts.next()?.trim();
    if parts.next().is_some() || label.is_empty() {
        return None;
    }
    Some(label)
}

/// A memory operand of the form `[base]`, `[base + n]` or `[base - n]`.
///
/// `base` is one of the sixteen 64-bit registers -- `rip` is handled by
/// `rip_target`, which is a different addressing mode and a different
/// fixup.
#[derive(Debug, Clone, Copy)]
struct Memory {
    base: u8,
    displacement: i32,
}

/// Decodes a memory operand. Returns `None` for anything else, so that
/// the caller can fall back on a register or an immediate.
fn memory_operand(text: &str) -> Option<Memory> {
    let inner = text.trim().strip_prefix('[')?.strip_suffix(']')?.trim();

    // `[rax]` -- no displacement.
    if let Some(base) = register(inner) {
        return Some(Memory {
            base,
            displacement: 0,
        });
    }

    // `[rbp - 8]` / `[rbp + 16]`. The sign is read from the operator, not
    // from the number: `[rbp + -8]` is not a form worth accepting, and
    // accepting it would make two spellings of one address.
    let (base_text, sign, offset_text) = match inner.find(['+', '-']) {
        Some(position) => (
            inner[..position].trim(),
            inner.as_bytes()[position],
            inner[position + 1..].trim(),
        ),
        None => return None,
    };
    let base = register(base_text)?;
    let magnitude = integer(offset_text)?;
    if magnitude < 0 {
        return None;
    }
    let signed = if sign == b'-' { -magnitude } else { magnitude };
    if signed > i32::MAX as i64 || signed < i32::MIN as i64 {
        return None;
    }
    Some(Memory {
        base,
        displacement: signed as i32,
    })
}

/// A valid label name: letter or `_`, then alphanumeric, `_`, `.`. The
/// dot allows local labels such as `boucle.fin`.
fn is_label(text: &str) -> bool {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
        _ => return false,
    }
    chars.all(|character| character.is_ascii_alphanumeric() || character == '_' || character == '.')
}

// ============================================================
//  Encoding
// ============================================================

/// REX.W prefix, plus the register extension bits.
fn rex_w(reg: u8, rm: u8) -> u8 {
    0x48 | ((reg >> 3) << 2) | (rm >> 3)
}

fn modrm(mode: u8, reg: u8, rm: u8) -> u8 {
    (mode << 6) | ((reg & 7) << 3) | (rm & 7)
}

/// Instructions with no operand, and their encoding.
const NULLARY: &[(&str, &[u8])] = &[
    ("nop", &[0x90]),
    ("ret", &[0xC3]),
    ("syscall", &[0x0F, 0x05]),
    ("hlt", &[0xF4]),
    ("int3", &[0xCC]),
    ("leave", &[0xC9]),
    ("cdq", &[0x99]),
    ("cqo", &[0x48, 0x99]),
];

/// Arithmetic operations on two registers, and their "rm64, r64" opcode.
/// The `/n` field serves the immediate form, opcode 0x81.
const ARITH: &[(&str, u8, u8)] = &[
    ("add", 0x01, 0),
    ("or", 0x09, 1),
    ("and", 0x21, 4),
    ("sub", 0x29, 5),
    ("xor", 0x31, 6),
    ("cmp", 0x39, 7),
];

/// Conditional jumps: suffix and second byte of the 0x0F opcode.
const CONDITIONS: &[(&str, u8)] = &[
    ("je", 0x84),
    ("jz", 0x84),
    ("jne", 0x85),
    ("jnz", 0x85),
    ("jb", 0x82),
    ("jae", 0x83),
    ("jbe", 0x86),
    ("ja", 0x87),
    ("jl", 0x8C),
    ("jge", 0x8D),
    ("jle", 0x8E),
    ("jg", 0x8F),
    ("js", 0x88),
    ("jns", 0x89),
    ("jp", 0x8A),
    ("jnp", 0x8B),
];

/// State of one pass.
struct Assembler {
    text: Vec<u8>,
    data: Vec<u8>,
    bss_size: u64,
    symbols: Vec<Symbol>,
    fixups: Vec<Fixup>,
    section: Section,
    instructions: usize,
    /// Pass 1: measure without resolving. Pass 2: emit.
    emitting: bool,
}

impl Assembler {
    fn new() -> Self {
        Assembler {
            text: Vec::new(),
            data: Vec::new(),
            bss_size: 0,
            symbols: Vec::new(),
            fixups: Vec::new(),
            section: Section::Text,
            instructions: 0,
            emitting: false,
        }
    }

    fn here(&self) -> u64 {
        match self.section {
            Section::Text => self.text.len() as u64,
            Section::Data => self.data.len() as u64,
            Section::Bss => self.bss_size,
        }
    }

    fn push(&mut self, byte: u8) {
        match self.section {
            Section::Text => self.text.push(byte),
            Section::Data => self.data.push(byte),
            // An uninitialised section has no bytes: only its size is
            // counted. A `db` in `.bss` is refused higher up.
            Section::Bss => self.bss_size += 1,
        }
    }

    fn push_slice(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.push(*byte);
        }
    }

    fn push_u32(&mut self, value: u32) {
        self.push_slice(&value.to_le_bytes());
    }

    fn label_offset(&self, name: &str) -> Option<(Section, u64)> {
        self.symbols
            .iter()
            .find(|symbol| symbol.name == name)
            .map(|symbol| (symbol.section, symbol.offset))
    }

    /// Emits a 32-bit displacement towards a label of `.text`.
    ///
    /// In pass 1 not every label is known: four zeros are written, the
    /// size is the same. In pass 2 the table is complete and the
    /// computation is exact.
    fn branch(&mut self, target: &str) -> Result<(), AsmError> {
        let next = self.text.len() + 4;
        if !self.emitting {
            self.push_u32(0);
            return Ok(());
        }
        let Some((section, offset)) = self.label_offset(target) else {
            return Err(AsmError::UndefinedLabel(String::from(target)));
        };
        if section != Section::Text {
            return Err(AsmError::UndefinedLabel(String::from(target)));
        }
        let displacement = offset as i64 - next as i64;
        self.push_u32(displacement as i32 as u32);
        Ok(())
    }

    /// Emits the ModRM byte, the SIB if one is needed, and the
    /// displacement of a `[base + disp]` operand.
    ///
    /// Three cases the encoding forces, and getting any of them wrong
    /// produces an instruction that addresses somewhere else:
    ///
    /// * `rsp` and `r12` share the encoding that announces a SIB byte,
    ///   so they always take one -- `0x24`, which means "base only";
    /// * `rbp` and `r13` share the encoding that mode 0 gives to
    ///   `[rip + disp32]`, so they can never use mode 0: a zero
    ///   displacement has to be written out as `disp8`;
    /// * everything else takes the shortest of the three modes.
    fn memory(&mut self, reg: u8, operand: Memory) {
        let base = operand.base;
        let low = base & 7;
        let needs_sib = low == 4;
        let forbids_mode0 = low == 5;

        let mode = if operand.displacement == 0 && !forbids_mode0 {
            0
        } else if (-128..=127).contains(&operand.displacement) {
            1
        } else {
            2
        };

        self.push(modrm(mode, reg, base));
        if needs_sib {
            self.push(0x24);
        }
        match mode {
            1 => self.push(operand.displacement as i8 as u8),
            2 => self.push_u32(operand.displacement as u32),
            _ => {}
        }
    }

    /// Emits a RIP-relative displacement towards a datum.
    ///
    /// The target lives in `.data` or `.bss`, whose address is not
    /// decided yet: a hole is left and the `Fixup` is recorded.
    fn rip_fixup(&mut self, target: &str) {
        let at = self.text.len();
        let next = at + 4;
        if self.emitting {
            self.fixups.push(Fixup {
                at,
                next,
                symbol: String::from(target),
            });
        }
        self.push_u32(0);
    }
}

// ============================================================
//  Assemblage
// ============================================================

/// Assembles a complete source.
///
/// Two passes over the same text: the first fills the label table, the
/// second emits. The object returned still carries its `Fixup`s: it is
/// `sdk::emit` that resolves them, once the memory layout is settled.
pub fn assemble(source: &str) -> Result<Object, AsmError> {
    let mut state = Assembler::new();
    run_pass(&mut state, source)?;

    let symbols = core::mem::take(&mut state.symbols);
    let mut second = Assembler::new();
    second.symbols = symbols;
    second.emitting = true;
    run_pass(&mut second, source)?;

    if second.text.is_empty() {
        return Err(AsmError::EmptyText);
    }
    let Some(entry) = second
        .symbols
        .iter()
        .find(|symbol| symbol.name == "_start" && symbol.section == Section::Text)
        .map(|symbol| symbol.offset)
    else {
        return Err(AsmError::NoEntryPoint);
    };

    // An unresolvable hole must show now, not at load time.
    for fixup in second.fixups.iter() {
        if second
            .symbols
            .iter()
            .all(|symbol| symbol.name != fixup.symbol)
        {
            return Err(AsmError::UndefinedLabel(fixup.symbol.clone()));
        }
    }

    Ok(Object {
        text: second.text,
        data: second.data,
        bss_size: second.bss_size,
        symbols: second.symbols,
        fixups: second.fixups,
        entry,
        instructions: second.instructions,
    })
}

fn run_pass(state: &mut Assembler, source: &str) -> Result<(), AsmError> {
    state.section = Section::Text;

    for (index, raw) in source.lines().enumerate() {
        let number = index + 1;
        let mut line = strip_comment(raw);
        if line.is_empty() {
            continue;
        }

        // A label may precede an instruction on the same line.
        while let Some(position) = label_end(line) {
            let name = &line[..position];
            define_label(state, number, name)?;
            line = line[position + 1..].trim();
            if line.is_empty() {
                break;
            }
        }
        if line.is_empty() {
            continue;
        }

        let (mnemonic, rest) = match line.find(char::is_whitespace) {
            Some(position) => (&line[..position], line[position..].trim()),
            None => (line, ""),
        };
        let operands = split_operands(rest);
        emit_line(state, number, mnemonic, &operands)?;
    }

    Ok(())
}

/// Position of the `:` that ends a label at the start of a line.
fn label_end(line: &str) -> Option<usize> {
    let position = line.find(':')?;
    let name = &line[..position];
    if is_label(name) { Some(position) } else { None }
}

fn define_label(state: &mut Assembler, number: usize, name: &str) -> Result<(), AsmError> {
    if state.emitting {
        // Labels come from pass 1: redefining them in pass 2 would
        // rewrite addresses that are already correct.
        return Ok(());
    }
    if state.symbols.iter().any(|symbol| symbol.name == name) {
        return Err(AsmError::DuplicateLabel(number, String::from(name)));
    }
    state.symbols.push(Symbol {
        name: String::from(name),
        section: state.section,
        offset: state.here(),
    });
    Ok(())
}

fn emit_line(
    state: &mut Assembler,
    number: usize,
    mnemonic: &str,
    operands: &[String],
) -> Result<(), AsmError> {
    let lowered = mnemonic.to_ascii_lowercase();
    let mnemonic = lowered.as_str();

    // --- section directives ---
    match mnemonic {
        ".text" | "section.text" => {
            state.section = Section::Text;
            return Ok(());
        }
        ".data" => {
            state.section = Section::Data;
            return Ok(());
        }
        ".bss" => {
            state.section = Section::Bss;
            return Ok(());
        }
        _ => {}
    }

    if mnemonic == "align" {
        return ext::align(state, number, operands);
    }

    // --- data directives ---
    if let Some(width) = data_width(mnemonic) {
        if state.section == Section::Bss {
            return Err(AsmError::BadOperand(number, String::from(mnemonic)));
        }
        return emit_data(state, number, width, operands);
    }
    if mnemonic == "resb" {
        let [count] = operands else {
            return Err(AsmError::BadOperandCount(number, String::from(mnemonic)));
        };
        let Some(value) = resolve_immediate(state, count) else {
            return Err(AsmError::BadOperand(number, count.clone()));
        };
        if value < 0 {
            return Err(AsmError::ImmediateOutOfRange(number));
        }
        match state.section {
            Section::Bss => state.bss_size += value as u64,
            // `resb` outside `.bss` reserves null bytes in the file:
            // that is the expected behaviour, and it costs the space it
            // announces.
            _ => {
                for _ in 0..value {
                    state.push(0);
                }
            }
        }
        return Ok(());
    }

    if state.section != Section::Text {
        return Err(AsmError::UnknownMnemonic(number, String::from(mnemonic)));
    }
    // The forms 16.6 added for the C compiler (`ext.rs`); the others
    // fall through, encoded as they always were.
    if let Some(result) = ext::emit(state, number, mnemonic, operands) {
        return result;
    }
    state.instructions += 1;

    // --- instructions with no operand ---
    if let Some((_, bytes)) = NULLARY.iter().find(|(name, _)| *name == mnemonic) {
        if !operands.is_empty() {
            return Err(AsmError::BadOperandCount(number, String::from(mnemonic)));
        }
        state.push_slice(bytes);
        return Ok(());
    }

    match mnemonic {
        "push" | "pop" => {
            let [target] = operands else {
                return Err(AsmError::BadOperandCount(number, String::from(mnemonic)));
            };
            let Some(code) = register(target) else {
                return Err(AsmError::BadOperand(number, target.clone()));
            };
            // The eight extended registers take REX.B; the first eight
            // need no prefix, the operand fitting inside the opcode.
            if code >= 8 {
                state.push(0x41);
            }
            let base = if mnemonic == "push" { 0x50 } else { 0x58 };
            state.push(base + (code & 7));
            Ok(())
        }

        "inc" | "dec" => {
            let [target] = operands else {
                return Err(AsmError::BadOperandCount(number, String::from(mnemonic)));
            };
            let Some(code) = register(target) else {
                return Err(AsmError::BadOperand(number, target.clone()));
            };
            let extension = if mnemonic == "inc" { 0 } else { 1 };
            state.push(rex_w(0, code));
            state.push(0xFF);
            state.push(modrm(3, extension, code));
            Ok(())
        }

        "not" | "neg" => {
            let [target] = operands else {
                return Err(AsmError::BadOperandCount(number, String::from(mnemonic)));
            };
            let Some(code) = register(target) else {
                return Err(AsmError::BadOperand(number, target.clone()));
            };
            let extension = if mnemonic == "not" { 2 } else { 3 };
            state.push(rex_w(0, code));
            state.push(0xF7);
            state.push(modrm(3, extension, code));
            Ok(())
        }

        "mul" | "div" => {
            let [target] = operands else {
                return Err(AsmError::BadOperandCount(number, String::from(mnemonic)));
            };
            let Some(code) = register(target) else {
                return Err(AsmError::BadOperand(number, target.clone()));
            };
            let extension = if mnemonic == "mul" { 4 } else { 6 };
            state.push(rex_w(0, code));
            state.push(0xF7);
            state.push(modrm(3, extension, code));
            Ok(())
        }

        "jmp" | "call" => {
            let [target] = operands else {
                return Err(AsmError::BadOperandCount(number, String::from(mnemonic)));
            };
            if !is_label(target) {
                return Err(AsmError::BadOperand(number, target.clone()));
            }
            state.push(if mnemonic == "jmp" { 0xE9 } else { 0xE8 });
            state.branch(target)
        }

        "mov" => emit_mov(state, number, operands),
        "lea" => emit_lea(state, number, operands),

        // imul r64, r64 -- the two-operand form, whose result is
        // truncated to sixty-four bits. The one-operand form of `mul`
        // gives a 128-bit product in rdx:rax; nothing here wants the
        // high half, and asking for it would tie up rdx for nothing.
        "imul" => {
            let [destination, source] = operands else {
                return Err(AsmError::BadOperandCount(number, String::from(mnemonic)));
            };
            let (Some(destination_code), Some(source_code)) =
                (register(destination), register(source))
            else {
                return Err(AsmError::BadOperand(number, destination.clone()));
            };
            state.push(rex_w(destination_code, source_code));
            state.push(0x0F);
            state.push(0xAF);
            state.push(modrm(3, destination_code, source_code));
            Ok(())
        }

        // idiv r64 -- signed division of rdx:rax. The caller owes it a
        // `cqo`: leaving rdx as it was divides by the wrong dividend, and
        // the quotient looks plausible.
        "idiv" => {
            let [target] = operands else {
                return Err(AsmError::BadOperandCount(number, String::from(mnemonic)));
            };
            let Some(code) = register(target) else {
                return Err(AsmError::BadOperand(number, target.clone()));
            };
            state.push(rex_w(0, code));
            state.push(0xF7);
            state.push(modrm(3, 7, code));
            Ok(())
        }

        "shl" | "shr" | "sar" => emit_shift(state, number, mnemonic, operands),

        _ => {
            if let Some((_, opcode)) = CONDITIONS.iter().find(|(name, _)| *name == mnemonic) {
                let [target] = operands else {
                    return Err(AsmError::BadOperandCount(number, String::from(mnemonic)));
                };
                if !is_label(target) {
                    return Err(AsmError::BadOperand(number, target.clone()));
                }
                state.push(0x0F);
                state.push(*opcode);
                return state.branch(target);
            }
            if let Some((_, opcode, extension)) = ARITH.iter().find(|(name, _, _)| *name == mnemonic)
            {
                return emit_arith(state, number, *opcode, *extension, operands);
            }
            state.instructions -= 1;
            Err(AsmError::UnknownMnemonic(number, String::from(mnemonic)))
        }
    }
}

fn data_width(mnemonic: &str) -> Option<usize> {
    Some(match mnemonic {
        "db" => 1,
        "dw" => 2,
        "dd" => 4,
        "dq" => 8,
        _ => return None,
    })
}

fn emit_data(
    state: &mut Assembler,
    number: usize,
    width: usize,
    operands: &[String],
) -> Result<(), AsmError> {
    if operands.is_empty() {
        return Err(AsmError::BadOperandCount(number, String::from("db")));
    }
    for operand in operands {
        let operand = operand.trim();
        if let Some(inner) = operand.strip_prefix('"') {
            let Some(text) = inner.strip_suffix('"') else {
                return Err(AsmError::BadString(number));
            };
            // Everything a Nova binary displays must stay in the
            // printable range: the VGA writer replaces the rest with a
            // solid square, and a solid square on screen is always a bug.
            if !text.bytes().all(|byte| (0x20..=0x7E).contains(&byte)) {
                return Err(AsmError::BadString(number));
            }
            for byte in text.bytes() {
                state.push(byte);
                for _ in 1..width {
                    state.push(0);
                }
            }
            continue;
        }
        let Some(value) = resolve_immediate(state, operand) else {
            return Err(AsmError::BadOperand(number, String::from(operand)));
        };
        let bytes = (value as u64).to_le_bytes();
        if width < 8 {
            let limit = 1i64 << (width * 8);
            if value >= limit || value < -(limit / 2) {
                return Err(AsmError::ImmediateOutOfRange(number));
            }
        }
        state.push_slice(&bytes[..width]);
    }
    Ok(())
}

fn emit_mov(state: &mut Assembler, number: usize, operands: &[String]) -> Result<(), AsmError> {
    let [destination, source] = operands else {
        return Err(AsmError::BadOperandCount(number, String::from("mov")));
    };

    // mov r64, [rip + label] -- loading a datum.
    if let Some(target) = rip_target(source) {
        let Some(code) = register(destination) else {
            return Err(AsmError::BadOperand(number, destination.clone()));
        };
        state.push(rex_w(code, 0));
        state.push(0x8B);
        state.push(modrm(0, code, 5));
        state.rip_fixup(target);
        return Ok(());
    }

    // mov [rip + label], r64 -- storing a datum.
    if let Some(target) = rip_target(destination) {
        let Some(code) = register(source) else {
            return Err(AsmError::BadOperand(number, source.clone()));
        };
        state.push(rex_w(code, 0));
        state.push(0x89);
        state.push(modrm(0, code, 5));
        state.rip_fixup(target);
        return Ok(());
    }

    // mov r64, [base + disp] -- reading a local.
    if let Some(operand) = memory_operand(source) {
        if let Some(code) = register(destination) {
            state.push(rex_w(code, operand.base));
            state.push(0x8B);
            state.memory(code, operand);
            return Ok(());
        }
        // mov r8, [base + disp] -- reading one byte.
        let Some(code) = register8(destination) else {
            return Err(AsmError::BadOperand(number, destination.clone()));
        };
        emit_byte_rex(state, code, operand.base);
        state.push(0x8A);
        state.memory(code, operand);
        return Ok(());
    }

    // mov [base + disp], r64 -- writing a local.
    if let Some(operand) = memory_operand(destination) {
        if let Some(code) = register(source) {
            state.push(rex_w(code, operand.base));
            state.push(0x89);
            state.memory(code, operand);
            return Ok(());
        }
        // mov [base + disp], r8 -- writing one byte. There is no
        // immediate form: a compiler always has the value in a register
        // by the time it stores it, and one encoding is one thing to get
        // wrong.
        let Some(code) = register8(source) else {
            return Err(AsmError::BadOperand(number, source.clone()));
        };
        emit_byte_rex(state, code, operand.base);
        state.push(0x88);
        state.memory(code, operand);
        return Ok(());
    }

    let Some(destination_code) = register(destination) else {
        return Err(AsmError::BadOperand(number, destination.clone()));
    };

    // mov r64, r64
    if let Some(source_code) = register(source) {
        state.push(rex_w(source_code, destination_code));
        state.push(0x89);
        state.push(modrm(3, source_code, destination_code));
        return Ok(());
    }

    // mov r64, immediate -- always the 64-bit form. Encoding small values
    // on 32 bits would save five bytes and would make the size of an
    // instruction vary with its operand, which is of no interest here and
    // complicates reading a disassembly back.
    let Some(value) = resolve_immediate(state, source) else {
        return Err(AsmError::BadOperand(number, source.clone()));
    };
    state.push(rex_w(0, destination_code));
    state.push(0xB8 + (destination_code & 7));
    state.push_slice(&(value as u64).to_le_bytes());
    Ok(())
}

/// Emits the REX prefix a byte-sized `mov` needs, if it needs one.
///
/// Two independent reasons to emit it, and both must be honoured: the
/// register numbers above seven need their extension bits, and `spl`,
/// `bpl`, `sil`, `dil` need the prefix merely to be **named** -- without
/// it the same encodings mean `ah`, `ch`, `dh`, `bh`. The prefix is bare
/// `0x40` plus the extension bits: no `W`, since the operand is a byte.
fn emit_byte_rex(state: &mut Assembler, reg: u8, base: u8) {
    let extensions = ((reg >> 3) << 2) | (base >> 3);
    if extensions != 0 || byte_needs_rex(reg) || base >= 8 {
        state.push(0x40 | extensions);
    }
}

fn emit_lea(state: &mut Assembler, number: usize, operands: &[String]) -> Result<(), AsmError> {
    let [destination, source] = operands else {
        return Err(AsmError::BadOperandCount(number, String::from("lea")));
    };
    let Some(code) = register(destination) else {
        return Err(AsmError::BadOperand(number, destination.clone()));
    };
    let Some(target) = rip_target(source) else {
        return Err(AsmError::BadOperand(number, source.clone()));
    };
    state.push(rex_w(code, 0));
    state.push(0x8D);
    state.push(modrm(0, code, 5));
    state.rip_fixup(target);
    Ok(())
}

/// `shl`, `shr` and `sar`, by an immediate or by `cl`.
///
/// The count is masked to six bits **by the processor**, not here: a
/// shift of 64 is a shift of 0 on x86_64, and silently rewriting it as
/// "zero" would hide a bug in the code that computed the count. Anything
/// outside 0..63 is refused instead.
fn emit_shift(
    state: &mut Assembler,
    number: usize,
    mnemonic: &str,
    operands: &[String],
) -> Result<(), AsmError> {
    let [destination, count] = operands else {
        return Err(AsmError::BadOperandCount(number, String::from(mnemonic)));
    };
    let Some(code) = register(destination) else {
        return Err(AsmError::BadOperand(number, destination.clone()));
    };
    let extension = match mnemonic {
        "shl" => 4,
        "shr" => 5,
        _ => 7,
    };

    // shl r64, cl -- the count is in cl, which is the only register the
    // encoding accepts.
    if count.trim() == "cl" {
        state.push(rex_w(0, code));
        state.push(0xD3);
        state.push(modrm(3, extension, code));
        return Ok(());
    }

    let Some(value) = resolve_immediate(state, count) else {
        return Err(AsmError::BadOperand(number, count.clone()));
    };
    if !(0..64).contains(&value) {
        return Err(AsmError::BadShiftCount(number));
    }
    state.push(rex_w(0, code));
    state.push(0xC1);
    state.push(modrm(3, extension, code));
    state.push(value as u8);
    Ok(())
}

fn emit_arith(
    state: &mut Assembler,
    number: usize,
    opcode: u8,
    extension: u8,
    operands: &[String],
) -> Result<(), AsmError> {
    let [destination, source] = operands else {
        return Err(AsmError::BadOperandCount(number, String::from("arith")));
    };
    let Some(destination_code) = register(destination) else {
        return Err(AsmError::BadOperand(number, destination.clone()));
    };

    if let Some(source_code) = register(source) {
        state.push(rex_w(source_code, destination_code));
        state.push(opcode);
        state.push(modrm(3, source_code, destination_code));
        return Ok(());
    }

    let Some(value) = resolve_immediate(state, source) else {
        return Err(AsmError::BadOperand(number, source.clone()));
    };
    // The immediate of an arithmetic operation fits in 32 bits, signed
    // and extended by the processor. A wider constant must go through a
    // register -- refusing it is worth more than truncating it silently.
    if value > i32::MAX as i64 || value < i32::MIN as i64 {
        return Err(AsmError::ImmediateOutOfRange(number));
    }
    state.push(rex_w(0, destination_code));
    state.push(0x81);
    state.push(modrm(3, extension, destination_code));
    state.push_u32(value as i32 as u32);
    Ok(())
}
