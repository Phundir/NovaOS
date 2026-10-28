//! SPIR-V validation (spec 6.8, "Shaders").
//!
//! ## What is checked, and why that much
//!
//! A shader is a program for the GPU, handed over by a program Nova does
//! not trust. Nova compiles nothing: the SPIR-V goes on to the host's
//! driver, which compiles it for the card. What the kernel must refuse is
//! what would make that compilation or the draw that follows undefined:
//!
//! - a module that is not SPIR-V at all, or whose instructions run past
//!   its end -- a driver's parser reading beyond the buffer;
//! - a capability the device was not created with: using `Float64` or
//!   `Int64` without the feature enabled is undefined behaviour in Vulkan,
//!   not an error the driver must report;
//! - an extension, a memory model or an instruction set the Nova interface
//!   does not offer -- the same argument, for everything that can widen
//!   what a shader does;
//! - an entry point other than `main` for the stage announced.
//!
//! What is not checked is whether the shader is *correct* -- types, ids,
//! control flow. A wrong but well-formed module is the program's own
//! failure: it draws garbage in its own images, or loops, and the
//! watchdog takes its context (spec 6.8, "délai de garde").

use alloc::vec::Vec;

const MAGIC: u32 = 0x0723_0203;
/// SPIR-V 1.0 to 1.6: what Vulkan 1.3 consumes.
const VERSION_MIN: u32 = 0x0001_0000;
const VERSION_MAX: u32 = 0x0001_0600;

/// Largest module accepted, in bytes.
pub const MAX_SIZE: usize = 1 << 20;
/// Largest id bound: a module declaring more ids than this is refused
/// before a driver sizes a table for them.
const MAX_BOUND: u32 = 1 << 22;

const OP_EXTENSION: u32 = 10;
const OP_EXT_INST_IMPORT: u32 = 11;
const OP_MEMORY_MODEL: u32 = 14;
const OP_ENTRY_POINT: u32 = 15;
const OP_CAPABILITY: u32 = 17;

/// Capabilities available without any device feature: `Matrix`,
/// `Shader`, `ImageQuery`, `DerivativeControl`, `InterpolationFunction`.
/// They are what `SYS_GPU_INFO` announces.
pub const CAPABILITIES: [u32; 5] = [0, 1, 50, 51, 52];

/// Execution models of the stages Nova offers.
const MODEL_VERTEX: u32 = 0;
const MODEL_FRAGMENT: u32 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Vertex,
    Fragment,
}

impl Stage {
    pub fn from_u32(value: u32) -> Option<Stage> {
        match value {
            1 => Some(Stage::Vertex),
            2 => Some(Stage::Fragment),
            _ => None,
        }
    }
}

/// Why a module was refused.
pub type Refusal = &'static str;

/// The module as words, if its size is a whole number of them.
pub fn words(bytes: &[u8]) -> Option<Vec<u32>> {
    if bytes.len() % 4 != 0 {
        return None;
    }
    let mut out = Vec::new();
    out.try_reserve_exact(bytes.len() / 4).ok()?;
    out.extend(bytes.chunks_exact(4).map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]])));
    Some(out)
}

/// A literal string operand: bytes up to the first NUL, and the words it
/// took. `None` if no NUL ends it within `words`.
fn literal(words: &[u32]) -> Option<(Vec<u8>, usize)> {
    let mut bytes = Vec::new();
    for (index, word) in words.iter().enumerate() {
        for byte in word.to_le_bytes() {
            if byte == 0 {
                return Some((bytes, index + 1));
            }
            bytes.push(byte);
        }
    }
    None
}

/// Checks a module for `stage`.
pub fn validate(code: &[u32], stage: Stage) -> Result<(), Refusal> {
    if code.len() * 4 > MAX_SIZE {
        return Err("module SPIR-V trop grand");
    }
    if code.len() < 5 {
        return Err("module SPIR-V tronque");
    }
    if code[0] != MAGIC {
        return Err("ce n'est pas du SPIR-V");
    }
    if !(VERSION_MIN..=VERSION_MAX).contains(&code[1]) || code[1] & 0xFF00_00FF != 0 {
        return Err("version SPIR-V non prise en charge");
    }
    if code[3] == 0 || code[3] > MAX_BOUND {
        return Err("borne d'identifiants SPIR-V hors limites");
    }
    if code[4] != 0 {
        return Err("schema SPIR-V inconnu");
    }

    let wanted_model = match stage {
        Stage::Vertex => MODEL_VERTEX,
        Stage::Fragment => MODEL_FRAGMENT,
    };
    let mut entry = false;
    let mut memory_model = false;
    let mut position = 5;
    // Each turn advances by at least one word: the word count is checked
    // non-zero before the step.
    while position < code.len() {
        let word = code[position];
        let count = (word >> 16) as usize;
        let opcode = word & 0xFFFF;
        if count == 0 || position + count > code.len() {
            return Err("instruction SPIR-V hors du module");
        }
        let operands = &code[position + 1..position + count];
        match opcode {
            OP_CAPABILITY => {
                let capability = *operands.first().ok_or("OpCapability sans operande")?;
                if !CAPABILITIES.contains(&capability) {
                    return Err("capacite SPIR-V non offerte par Nova");
                }
            }
            OP_EXTENSION => return Err("extension SPIR-V non offerte par Nova"),
            OP_EXT_INST_IMPORT => {
                let (name, _) = literal(operands.get(1..).unwrap_or(&[])).ok_or("OpExtInstImport mal forme")?;
                if name != b"GLSL.std.450" {
                    return Err("jeu d'instructions etendu non offert par Nova");
                }
            }
            OP_MEMORY_MODEL => {
                // Logical addressing, GLSL450 memory model.
                if operands.len() != 2 || operands[0] != 0 || operands[1] != 1 {
                    return Err("modele memoire SPIR-V non offert par Nova");
                }
                memory_model = true;
            }
            OP_ENTRY_POINT => {
                let model = *operands.first().ok_or("OpEntryPoint mal forme")?;
                let (name, _) = literal(operands.get(2..).unwrap_or(&[])).ok_or("OpEntryPoint mal forme")?;
                if model != MODEL_VERTEX && model != MODEL_FRAGMENT {
                    return Err("etage de shader non offert par Nova");
                }
                if model == wanted_model && name == b"main" {
                    entry = true;
                }
            }
            _ => {}
        }
        position += count;
    }
    if !memory_model {
        return Err("module SPIR-V sans modele memoire");
    }
    if !entry {
        return Err("pas de point d'entree \"main\" pour cet etage");
    }
    Ok(())
}
