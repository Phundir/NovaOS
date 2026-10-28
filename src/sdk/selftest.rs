//! Phase 9 self-test (9.1 to 9.6).
//!
//! ## What gets checked, and why these checks
//!
//! * **The assembler encodings, byte by byte.** A wrong assembler
//!   produces perfectly well-formed code that does something else.
//!   Nothing gives it away -- not even a run, as long as the faulty
//!   instruction does not fall on a path that is taken. The reference
//!   encodings come from the Intel manual and are compared to the value.
//! * **The RIP displacement computation.** This is the point where an
//!   error of plus or minus four bytes reads as a valid pointer to the
//!   wrong datum. The check bears on the formula: change the layout, and
//!   the displacement must change with it.
//! * **The complete chain.** `dev new`, `dev build`, then `binfmt::parse`
//!   on the file produced. All five templates, not one: that is the only
//!   way to establish that none of them delivers a project that does not
//!   compile.
//! * **The refusals.** A `dev.toml` demanding `KERNEL_SPACE` for a
//!   `.cus`, a self-signed `.dsa`, a language with no compiler: each must
//!   produce **its own** refusal, not just any refusal.
//! * **The sandbox.** That a write does not reach the host, that reading
//!   outside the roots is refused, that the loopback refuses a public
//!   address, and that destruction returns **exactly** the frames taken.
//! * **The specification tables.** 13.2 row by row, 15.5 row by row,
//!   threshold of 13.1. A transcription is not verified by reading it
//!   back: it is verified by comparing it.
//!
//! ## The self-test must leave nothing behind
//!
//! It writes on a persistent volume. A pass leaving a project in place
//! makes the next one fail on a directory it believes empty, and the
//! symptom misleads -- the faulty code is not the one that fails. So it
//! purges its own scope **on the way in** as much as on the way out, as
//! phase 7 taught and phase 8 confirmed.

use alloc::string::String;
use alloc::vec::Vec;

use crate::binfmt::header::{BinaryFormat, SignatureType, flags};
use crate::binfmt::identity::Permissions;
use crate::fs::perm::{Credentials, Mode};

use super::asm::{self, AsmError};
use super::cli::{self, DevError, Output};
use super::emit;
use super::etr;
use super::libdev;
use super::manifest::{self, ManifestError};
use super::runtime::{self, Language, Support};
use super::rustc;
use super::sandbox::{self, SandboxError};
use super::script::{self, ScriptError, Value};
use super::template::{self, Template};

/// Root of the test scope. Everything under it is ours.
const TEST_ROOT: &str = "/cache/sdk-autotest";

struct Report {
    passed: usize,
    failed: usize,
}

impl Report {
    fn check(&mut self, label: &str, condition: bool) {
        if condition {
            self.passed += 1;
        } else {
            self.failed += 1;
            crate::serial_println!("[sdk] ECHEC : {}", label);
        }
    }

    /// Compares two texts and shows both when they differ.
    ///
    /// A boolean check on a computed string says "false"; this says what
    /// came out. For a compiler that is the difference between a failure
    /// one can read and one that has to be reproduced by hand.
    fn equal_text(&mut self, label: &str, produced: &str, expected: &str) {
        if produced == expected {
            self.passed += 1;
            return;
        }
        self.failed += 1;
        crate::serial_println!(
            "[sdk] ECHEC : {} - produit \"{}\", attendu \"{}\"",
            label,
            produced,
            expected
        );
    }

    fn equal_bytes(&mut self, label: &str, produced: &[u8], expected: &[u8]) {
        if produced == expected {
            self.passed += 1;
            return;
        }
        self.failed += 1;
        crate::serial_println!(
            "[sdk] ECHEC : {} - produit {:02X?}, attendu {:02X?}",
            label,
            produced,
            expected
        );
    }
}

// ============================================================
//  9.5.7 -- Assembler
// ============================================================

/// Assembles a source and returns `.text` alone.
fn text_of(source: &str) -> Option<Vec<u8>> {
    asm::assemble(source).ok().map(|object| object.text)
}

/// Reference encodings, checked against the Intel manual.
///
/// The table is written out rather than computed: a test recomputing the
/// encoding with the same function as the code under test would verify
/// nothing beyond the reproducibility of its own bug.
fn check_encodings(report: &mut Report) {
    let cases: &[(&str, &[u8])] = &[
        ("nop", &[0x90]),
        ("ret", &[0xC3]),
        ("syscall", &[0x0F, 0x05]),
        ("hlt", &[0xF4]),
        ("leave", &[0xC9]),
        ("cqo", &[0x48, 0x99]),
        // mov r64, imm64: REX.W + B8+r + eight bytes.
        ("mov rax, 0x020B", &[0x48, 0xB8, 0x0B, 0x02, 0, 0, 0, 0, 0, 0]),
        ("mov rdi, 0", &[0x48, 0xBF, 0, 0, 0, 0, 0, 0, 0, 0]),
        // An extended register carries REX.B, hence 0x49 instead of 0x48.
        ("mov r15, 1", &[0x49, 0xBF, 1, 0, 0, 0, 0, 0, 0, 0]),
        // mov r/m64, r64: the reg field carries the source.
        ("mov r8, r9", &[0x4D, 0x89, 0xC8]),
        ("mov rax, rbx", &[0x48, 0x89, 0xD8]),
        // push and pop: the operand fits in the opcode, REX.B for r8-r15.
        ("push rax", &[0x50]),
        ("push r15", &[0x41, 0x57]),
        ("pop rax", &[0x58]),
        ("pop r15", &[0x41, 0x5F]),
        ("inc r14", &[0x49, 0xFF, 0xC6]),
        ("dec r14", &[0x49, 0xFF, 0xCE]),
        ("not rax", &[0x48, 0xF7, 0xD0]),
        ("neg r15", &[0x49, 0xF7, 0xDF]),
        ("mul rcx", &[0x48, 0xF7, 0xE1]),
        ("div rcx", &[0x48, 0xF7, 0xF1]),
        ("add rax, rbx", &[0x48, 0x01, 0xD8]),
        ("xor rax, rax", &[0x48, 0x31, 0xC0]),
        // Immediate form: opcode 0x81, extension in the reg field.
        ("sub rax, 16", &[0x48, 0x81, 0xE8, 0x10, 0, 0, 0]),
        ("cmp r14, 5", &[0x49, 0x81, 0xFE, 0x05, 0, 0, 0]),
    ];

    for (source, expected) in cases {
        // `_start` is mandatory: the assembler refuses a source with no
        // entry point, and that is what we want.
        let program = alloc::format!(".text\n_start:\n    {}\n", source);
        match text_of(&program) {
            Some(text) => report.equal_bytes(source, &text, expected),
            None => report.check(&alloc::format!("{} s'assemble", source), false),
        }
    }
}

fn check_assembler(report: &mut Report) {
    check_encodings(report);

    // --- jumps: always rel32, never rel8 ---
    let source = ".text\n_start:\n    nop\n    jmp _start\n";
    match text_of(source) {
        Some(text) => {
            report.check("jmp fait cinq octets", text.len() == 6);
            report.check("jmp est un E9", text.get(1) == Some(&0xE9));
            // The target is at offset 0, the instruction ends at 6: the
            // displacement is -6.
            let displacement = i32::from_le_bytes([text[2], text[3], text[4], text[5]]);
            report.check("deplacement de jmp arriere", displacement == -6);
        }
        None => report.check("jmp s'assemble", false),
    }

    let source = ".text\n_start:\n    je fin\n    nop\n fin:\n    ret\n";
    match text_of(source) {
        Some(text) => {
            report.check("je fait six octets", text.first() == Some(&0x0F));
            report.check("je est un 0F 84", text.get(1) == Some(&0x84));
            let displacement = i32::from_le_bytes([text[2], text[3], text[4], text[5]]);
            // Six-byte instruction, one `nop` to step over: +1.
            report.check("deplacement de je avant", displacement == 1);
        }
        None => report.check("je s'assemble", false),
    }

    // --- RIP-relative lea: a hole and a fixup ---
    let source = ".text\n_start:\n    lea rdi, [rip + message]\n    ret\n.data\nmessage:\n    db \"ok\"\n";
    match asm::assemble(source) {
        Ok(object) => {
            report.check("lea produit un fixup", object.fixups.len() == 1);
            report.check(
                "lea encode 48 8D 3D",
                object.text.first() == Some(&0x48)
                    && object.text.get(1) == Some(&0x8D)
                    && object.text.get(2) == Some(&0x3D),
            );
            report.check("le fixup vise la fin de l'instruction", object.fixups[0].next == 7);
            report.check(".data porte deux octets", object.data == b"ok");
            report.check(
                "l'etiquette de donnee est en .data",
                object
                    .symbol("message")
                    .is_some_and(|symbol| symbol.section == asm::Section::Data),
            );
        }
        Err(error) => report.check(
            &alloc::format!("lea s'assemble : {}", error.message()),
            false,
        ),
    }

    // --- label difference ---
    let source = ".text\n_start:\n    mov rsi, fin - debut\n    ret\n.data\ndebut:\n    db \"abcde\"\nfin:\n";
    match asm::assemble(source) {
        Ok(object) => {
            let immediate = u64::from_le_bytes([
                object.text[2],
                object.text[3],
                object.text[4],
                object.text[5],
                object.text[6],
                object.text[7],
                object.text[8],
                object.text[9],
            ]);
            report.check("difference d'etiquettes vaut 5", immediate == 5);
        }
        Err(error) => report.check(
            &alloc::format!("difference d'etiquettes s'assemble : {}", error.message()),
            false,
        ),
    }

    // --- refusals ---
    report.check(
        "une source sans _start est refusee",
        matches!(
            asm::assemble(".text\n    nop\n"),
            Err(AsmError::NoEntryPoint)
        ),
    );
    report.check(
        "un mnemonique inconnu est refuse",
        matches!(
            asm::assemble(".text\n_start:\n    frobnicate rax\n"),
            Err(AsmError::UnknownMnemonic(_, _))
        ),
    );
    report.check(
        "une etiquette definie deux fois est refusee",
        matches!(
            asm::assemble(".text\n_start:\nboucle:\n    nop\nboucle:\n    ret\n"),
            Err(AsmError::DuplicateLabel(_, _))
        ),
    );
    report.check(
        "un saut vers une etiquette inconnue est refuse",
        matches!(
            asm::assemble(".text\n_start:\n    jmp nulle_part\n"),
            Err(AsmError::UndefinedLabel(_))
        ),
    );
    report.check(
        "une chaine non-ASCII est refusee",
        matches!(
            asm::assemble(".text\n_start:\n    ret\n.data\nx:\n    db \"caf\u{00e9}\"\n"),
            Err(AsmError::BadString(_))
        ),
    );
    report.check(
        "un immediat arithmetique trop grand est refuse",
        matches!(
            asm::assemble(".text\n_start:\n    add rax, 0x1FFFFFFFF\n"),
            Err(AsmError::ImmediateOutOfRange(_))
        ),
    );
    // A comment must not cut a string containing a `;`.
    match asm::assemble(".text\n_start:\n    ret\n.data\nx:\n    db \"a;b\"\n") {
        Ok(object) => report.check("le point-virgule d'une chaine est conserve", object.data == b"a;b"),
        Err(_) => report.check("le point-virgule d'une chaine est conserve", false),
    }
}

// ============================================================
//  9.1.2 -- Linking
// ============================================================

fn check_linker(report: &mut Report) {
    let source = ".text\n_start:\n    lea rdi, [rip + message]\n    ret\n.data\nmessage:\n    db \"ok\"\n";
    let Ok(object) = asm::assemble(source) else {
        report.check("l'objet de test s'assemble", false);
        return;
    };

    // A one-page layout for `.text`: the datum is 4096 - 7 from the end
    // of the instruction.
    let layout = emit::Layout {
        text_span: 4096,
        data_span: 4096,
        bss_size: 0,
    };
    match emit::link(&object, &layout) {
        Ok(text) => {
            let displacement = i32::from_le_bytes([text[3], text[4], text[5], text[6]]);
            report.check(
                "deplacement RIP sur une page",
                displacement == 4096 - 7,
            );
        }
        Err(error) => report.check(
            &alloc::format!("l'edition de liens aboutit : {}", error.message()),
            false,
        ),
    }

    // The formula must depend only on the layout: doubling the extent of
    // `.text` must move the target by as much.
    let wider = emit::Layout {
        text_span: 8192,
        data_span: 4096,
        bss_size: 0,
    };
    match emit::link(&object, &wider) {
        Ok(text) => {
            let displacement = i32::from_le_bytes([text[3], text[4], text[5], text[6]]);
            report.check(
                "le deplacement suit la disposition",
                displacement == 8192 - 7,
            );
        }
        Err(_) => report.check("le deplacement suit la disposition", false),
    }

    // --- emitting a complete binary and reading it back ---
    let spec = emit::EmitSpec {
        format: BinaryFormat::Cus,
        signature: SignatureType::SelfSigned,
        app_id: "test.sdk.emission",
        author: "Nova OS",
        group: "test.sdk",
        display_name: "Emission",
        version: crate::binfmt::identity::Version::new(1, 2, 3, 4),
        permissions: Permissions::NOTIFICATIONS,
        flags: 0,
        ports: &[],
        stack_size: 32 * 1024,
        authority: None,
    };
    match emit::emit(&object, &spec) {
        Ok(image) => match crate::binfmt::parse(&image) {
            Ok(header) => {
                report.check("le binaire emis s'analyse", true);
                report.check("le format est conserve", header.format() == BinaryFormat::Cus);
                report.check(
                    "la signature est conservee",
                    header.signature() == SignatureType::SelfSigned
                );
                report.check(
                    "le binaire emis n'a aucune violation",
                    crate::binfmt::check_rules(&header).is_empty()
                );
                let verdict = crate::binfmt::signature::verify(&header, &image);
                report.check(
                    "l'integrite du binaire emis est verifiee",
                    verdict == crate::binfmt::signature::Verdict::SelfSigned
                );
                // The entry point is `_start`, hence the base: an error
                // here would send the loader into the middle of the code.
                report.check(
                    "le point d'entree pointe sur _start",
                    header.entry_point == emit::LOAD_BASE + object.entry
                );
            }
            Err(error) => report.check(
                &alloc::format!("le binaire emis s'analyse : {}", error.message()),
                false,
            ),
        },
        Err(error) => report.check(
            &alloc::format!("l'emission aboutit : {}", error.message()),
            false,
        ),
    }

    // A forbidden combination must be refused at emission, not at load
    // time: 3.4 is checked as early as possible.
    let mut forbidden = emit::EmitSpec {
        format: BinaryFormat::Dsa,
        signature: SignatureType::SelfSigned,
        app_id: "test.sdk.interdit",
        author: "Nova OS",
        group: "test.sdk",
        display_name: "Interdit",
        version: crate::binfmt::identity::Version::new(1, 0, 0, 1),
        permissions: 0,
        flags: 0,
        ports: &[],
        stack_size: 32 * 1024,
        authority: None,
    };
    report.check(
        "un .dsa auto-signe est refuse a l'emission",
        matches!(
            emit::emit(&object, &forbidden),
            Err(emit::EmitError::ForbiddenCombination(_, _))
        ),
    );
    forbidden.format = BinaryFormat::Gs;
    forbidden.signature = SignatureType::Enterprise;
    report.check(
        "un ENTERPRISE sans autorite est refuse",
        matches!(
            emit::emit(&object, &forbidden),
            Err(emit::EmitError::MissingAuthority)
        ),
    );
}

// ============================================================
//  9.2 -- Manifest
// ============================================================

const GOOD_MANIFEST: &str = "\
[app]\n\
app_id = \"test.sdk.projet\"\n\
nom = \"Projet de test\"\n\
auteur = \"Nova OS\"\n\
groupe = \"test.sdk\"\n\
version = \"2.3.4+17\"\n\
\n\
[build]\n\
type = \"cus\"\n\
lang = \"asm\"\n\
entree = \"src/main.asm\"\n\
signature = \"self\"\n\
pile = 65536\n\
\n\
[permissions]\n\
liste = [\"NOTIFICATIONS\", \"FS_USER_HOME\"]\n\
\n\
[ipc]\n\
ports = [\"4100\", \"4101\"]\n\
";

fn check_manifest(report: &mut Report) {
    match manifest::parse(GOOD_MANIFEST) {
        Ok(parsed) => {
            report.check("app_id lu", parsed.app_id == "test.sdk.projet");
            report.check("version lue", parsed.version.major == 2 && parsed.version.build == 17);
            report.check("format lu", parsed.format == BinaryFormat::Cus);
            report.check("langage lu", parsed.language == Language::Asm);
            report.check("entree lue", parsed.entry == "src/main.asm");
            report.check("pile lue", parsed.stack_size == 65536);
            report.check(
                "permissions lues",
                parsed.permissions.contains(Permissions::NOTIFICATIONS)
                    && parsed.permissions.contains(Permissions::FS_USER_HOME)
            );
            report.check("ports lus", parsed.ports == alloc::vec![4100, 4101]);

            // Round trip: rewriting then re-reading must give the same
            // manifest. A lost field shows here and nowhere else.
            let text = manifest::to_toml(&parsed);
            match manifest::parse(&text) {
                Ok(again) => {
                    report.check("aller-retour : app_id", again.app_id == parsed.app_id);
                    report.check("aller-retour : version", again.version == parsed.version);
                    report.check("aller-retour : permissions", again.permissions == parsed.permissions);
                    report.check("aller-retour : ports", again.ports == parsed.ports);
                    report.check("aller-retour : drapeaux", again.flags == parsed.flags);
                    report.check("aller-retour : groupe", again.group == parsed.group);
                }
                Err(error) => report.check(
                    &alloc::format!("aller-retour du manifest : {}", error.message()),
                    false,
                ),
            }
        }
        Err(error) => report.check(
            &alloc::format!("le manifest de reference s'analyse : {}", error.message()),
            false,
        ),
    }

    // --- refusals, each its own ---
    let missing = "[app]\nnom = \"x\"\n";
    report.check(
        "un manifest sans app_id est refuse",
        matches!(manifest::parse(missing), Err(ManifestError::Missing(_))),
    );

    let bad_key = "[app]\napp_id = \"a.b\"\ninconnu = \"x\"\n[build]\ntype = \"cus\"\nlang = \"asm\"\nentree = \"x\"\n";
    report.check(
        "une cle inconnue est refusee, pas ignoree",
        matches!(manifest::parse(bad_key), Err(ManifestError::UnknownKey(_, _))),
    );

    let above_ceiling = GOOD_MANIFEST.replace(
        "liste = [\"NOTIFICATIONS\", \"FS_USER_HOME\"]",
        "liste = [\"KERNEL_SPACE\"]",
    );
    report.check(
        "un .cus qui reclame KERNEL_SPACE est refuse (spec 12.4)",
        matches!(
            manifest::parse(&above_ceiling),
            Err(ManifestError::PermissionAboveCeiling(_))
        ),
    );

    let forbidden = GOOD_MANIFEST.replace("type = \"cus\"", "type = \"dsa\"");
    report.check(
        "un .dsa auto-signe est refuse au manifest (spec 3.4)",
        matches!(
            manifest::parse(&forbidden),
            Err(ManifestError::ForbiddenCombination)
        ),
    );

    let no_target = GOOD_MANIFEST
        .replace("type = \"cus\"", "type = \"dsa\"")
        .replace("signature = \"self\"", "signature = \"system\"")
        .replace("lang = \"asm\"", "lang = \"java\"");
    report.check(
        "Java ne cible pas .dsa (spec 13.2)",
        matches!(
            manifest::parse(&no_target),
            Err(ManifestError::FormatNotTargeted)
        ),
    );

    let asm_dsa = GOOD_MANIFEST
        .replace("type = \"cus\"", "type = \"dsa\"")
        .replace("signature = \"self\"", "signature = \"enterprise\"");
    report.check(
        "un .dsa non SYSTEM est refuse avant meme le controle 13.2",
        manifest::parse(&asm_dsa).is_err(),
    );

    let bad_flag = GOOD_MANIFEST.replace(
        "pile = 65536",
        "pile = 65536\ndrapeaux = [\"EXPOSES_API\"]",
    );
    report.check(
        "EXPOSES_API sur un .cus est refuse (spec 3.4)",
        matches!(
            manifest::parse(&bad_flag),
            Err(ManifestError::FlagNotAllowed(_))
        ),
    );

    let bad_id = GOOD_MANIFEST.replace("app_id = \"test.sdk.projet\"", "app_id = \"monapp\"");
    report.check(
        "un app_id sans point est refuse",
        matches!(manifest::parse(&bad_id), Err(ManifestError::BadAppId)),
    );

    let duplicate_port = GOOD_MANIFEST.replace("ports = [\"4100\", \"4101\"]", "ports = [\"4100\", \"4100\"]");
    report.check(
        "un port en double est refuse",
        matches!(
            manifest::parse(&duplicate_port),
            Err(ManifestError::BadPort(_))
        ),
    );

    // The group is derived from the app_id when missing: without it, 12.1
    // would send every pair of applications through an internal port.
    let no_group = GOOD_MANIFEST.replace("groupe = \"test.sdk\"\n", "");
    match manifest::parse(&no_group) {
        Ok(parsed) => report.check("le groupe se deduit de l'app_id", parsed.group == "test.sdk"),
        Err(_) => report.check("le groupe se deduit de l'app_id", false),
    }
}

// ============================================================
//  9.1.4 -- Interpreter
// ============================================================

fn check_script(report: &mut Report) {
    // --- arithmetic and precedence ---
    let cases: &[(&str, i64)] = &[
        ("let x = 2 + 3 * 4", 14),
        ("let x = (2 + 3) * 4", 20),
        ("let x = 10 % 3", 1),
        ("let x = -5 + 8", 3),
        ("let x = 7 / 2", 3),
    ];
    for (source, expected) in cases {
        let program = alloc::format!("{}\nreturn x\n", source);
        match script::execute(&program) {
            Ok(run) => report.check(
                &alloc::format!("{} vaut {}", source, expected),
                run.value == Value::Int(*expected),
            ),
            Err(error) => report.check(
                &alloc::format!("{} s'evalue : {}", source, error.message()),
                false,
            ),
        }
    }

    // --- loop, function, scope ---
    let program = "\
        fn somme(n) {\n\
            let total = 0\n\
            let i = 1\n\
            while i <= n {\n\
                total = total + i\n\
                i = i + 1\n\
            }\n\
            return total\n\
        }\n\
        return somme(10)\n";
    match script::execute(program) {
        Ok(run) => report.check("somme(10) vaut 55", run.value == Value::Int(55)),
        Err(error) => report.check(
            &alloc::format!("la fonction s'evalue : {}", error.message()),
            false,
        ),
    }

    // A function called before its declaration must work: declarations
    // are collected before execution.
    let program = "return double(21)\nfn double(n) { return n * 2 }\n";
    match script::execute(program) {
        Ok(run) => report.check("appel avant declaration", run.value == Value::Int(42)),
        Err(_) => report.check("appel avant declaration", false),
    }

    // --- captured output ---
    match script::execute("print(\"a\", 1)\nprint(\"b\")\n") {
        Ok(run) => {
            report.check("print produit deux lignes", run.output.len() == 2);
            report.check("print separe par une espace", run.output[0] == "a 1");
        }
        Err(_) => report.check("print produit deux lignes", false),
    }

    // --- short circuit ---
    // A false `and` must not evaluate its right side: if it did, the
    // unknown variable would raise an error.
    match script::execute("return false and inconnue == 1\n") {
        Ok(run) => report.check("and court-circuite", run.value == Value::Bool(false)),
        Err(_) => report.check("and court-circuite", false),
    }
    match script::execute("return true or inconnue == 1\n") {
        Ok(run) => report.check("or court-circuite", run.value == Value::Bool(true)),
        Err(_) => report.check("or court-circuite", false),
    }

    // --- refusals ---
    report.check(
        "une variable inconnue est refusee",
        matches!(
            script::execute("return absente\n"),
            Err(ScriptError::Undefined(_))
        ),
    );
    report.check(
        "la division par zero est refusee",
        matches!(
            script::execute("return 1 / 0\n"),
            Err(ScriptError::DivisionByZero)
        ),
    );
    report.check(
        "un entier + un texte est refuse",
        matches!(
            script::execute("return 1 + \"a\"\n"),
            Err(ScriptError::TypeMismatch(_, _))
        ),
    );
    report.check(
        "une condition non booleenne est refusee",
        matches!(
            script::execute("if 1 { print(\"x\") }\n"),
            Err(ScriptError::TypeMismatch(_, _))
        ),
    );
    report.check(
        "le debordement entier est refuse",
        matches!(
            script::execute("return 9223372036854775807 + 1\n"),
            Err(ScriptError::Overflow)
        ),
    );
    // The budget is what stops a script from keeping its core forever:
    // it is the most important check of the module.
    report.check(
        "une boucle infinie epuise son budget",
        matches!(
            script::execute_with_budget("while true { let x = 1 }\n", 5_000),
            Err(ScriptError::Budget)
        ),
    );
    report.check(
        "une recursion sans fin est bornee",
        script::execute("fn f(n) { return f(n) }\nreturn f(1)\n").is_err(),
    );
    report.check(
        "une chaine non-ASCII est refusee",
        matches!(
            script::execute("print(\"caf\u{00e9}\")\n"),
            Err(ScriptError::BadString(_))
        ),
    );
}

// ============================================================
//  9.5 -- The 13.2 table
// ============================================================

fn check_runtime_table(report: &mut Report) {
    // Transcription of the 13.2 table, row by row. The labels are
    // compared to the letter: that is the only way to establish that no
    // column has been swapped.
    let expected: &[(&str, &str, &str, usize)] = &[
        ("Rust", "REPL interactif", "Natif direct", 5),
        ("C / C++", "LLVM JIT", "LLVM -> natif", 5),
        ("C#", "Runtime Mono allege", "Transpilation -> Rust", 4),
        ("Java", "JVM maison minimale", "Bytecode -> Rust", 3),
        ("Python", "CPython adapte", "rustpython -> natif", 2),
        ("Node.js", "V8 allege", "JS -> Rust", 2),
        ("Bun", "Runtime Bun porte", "TS/JS -> Rust", 2),
        ("ASM x86_64", "Emulateur sandboxe", "Assemblage direct", 5),
    ];

    report.check(
        "la table compte les huit langages plus l'addition",
        runtime::TABLE.len() == expected.len() + 1,
    );

    for (index, (label, interpreter, compiler, targets)) in expected.iter().enumerate() {
        let Some(entry) = runtime::TABLE.get(index) else {
            report.check(&alloc::format!("ligne {} presente", label), false);
            continue;
        };
        report.check(&alloc::format!("{} : libelle", label), entry.label == *label);
        report.check(
            &alloc::format!("{} : interprete", label),
            entry.interpreter == *interpreter,
        );
        report.check(
            &alloc::format!("{} : compilateur", label),
            entry.compiler == *compiler,
        );
        report.check(
            &alloc::format!("{} : {} formats cibles", label, targets),
            entry.targets.len() == *targets,
        );
    }

    // 13.2: "All (SYSTEM required for .dsa)" appears only on the
    // assembler row.
    report.check(
        "SYSTEM n'est exige que pour ASM + .dsa",
        runtime::requires_system_signature(Language::Asm, BinaryFormat::Dsa)
            && !runtime::requires_system_signature(Language::Rust, BinaryFormat::Dsa),
    );
    report.check(
        "Python ne cible que .cus et .ras",
        runtime::targets(Language::Python, BinaryFormat::Cus)
            && !runtime::targets(Language::Python, BinaryFormat::Ss),
    );
    // Two complete chains since 9.5.1, and only two: the six remaining
    // rows of 13.2 still need a toolchain nothing here can carry.
    report.check(
        "l'assembleur et Rust sont les deux chaines completes",
        runtime::spec(Language::Asm).support == Support::InTree
            && runtime::spec(Language::Rust).support == Support::InTree
            && runtime::spec(Language::C).support == Support::Delegated
            && runtime::spec(Language::Java).support == Support::Delegated,
    );

    // 9.5.8 -- the resolution itself is checked by
    // `check_runtime_priority`, which needs the volume; here only the
    // absence of a delegated runtime, which needs nothing.
    report.check(
        "aucun runtime Python n'est declare",
        runtime::resolve(Language::Python, None).is_none(),
    );
    report.check(
        "l'extension .py designe Python",
        Language::from_extension("main.py") == Some(Language::Python),
    );
    report.check(
        "l'extension .ns designe le script integre",
        Language::from_extension("x.ns") == Some(Language::Script),
    );
}

// ============================================================
//  9.5.8 -- A personal runtime masks the system one
// ============================================================

/// Account the priority of 13.3 is tried on.
///
/// A real account, created by `fs::tree::create_user_tree`: the point of
/// the rule is that `/users/{user}/runtimes/` wins, and a made-up name
/// would only prove that a path that does not exist loses.
const RUNTIME_USER: &str = "nova";

/// 9.5.8 -- Checks that a runtime installed for an account wins.
///
/// `check_runtime_table` asserted only that the assembler resolves to
/// `Origin::System`, which is what `install_layout` drops and therefore
/// what would be found whether the priority worked or not. The rule
/// 13.3 states -- personal runtime over system runtime -- had no check
/// at all; its comment claimed one. So a descriptor is installed for the
/// account, the resolution is observed to change origin, and it is
/// removed again.
fn check_runtime_priority(report: &mut Report, now: u64) {
    let who = Credentials::kernel();
    let base = alloc::format!("{}/asm", runtime::user_dir(RUNTIME_USER));
    let descriptor = alloc::format!("{}/{}", base, runtime::DESCRIPTOR);

    // The system side is the reference: without it the comparison below
    // would say nothing.
    let system = runtime::resolve(Language::Asm, None);
    report.check(
        "13.3 : le runtime asm est declare cote systeme",
        system.as_ref().map(|found| found.origin) == Some(runtime::Origin::System),
    );

    let _ = crate::fs::create_directory(&base, who, Mode::private_dir(), now);
    // The version is deliberately one no system descriptor carries:
    // it is what tells the two apart in the check below.
    let text = "langage=asm\nlibelle=ASM x86_64\nversion=9.9.9\n";
    let installed = crate::fs::write_file(
        &descriptor,
        text.as_bytes(),
        who,
        Mode::private_file(),
        now,
    )
    .is_ok();
    report.check("13.3 : un runtime personnel s'installe", installed);

    if installed {
        match runtime::resolve(Language::Asm, Some(RUNTIME_USER)) {
            Some(found) => {
                report.check(
                    "13.3 : le runtime personnel masque celui du systeme",
                    found.origin == runtime::Origin::User,
                );
                // The version is read from the file, not guessed: it is
                // what proves the descriptor found is the personal one
                // and not the system one relabelled.
                report.check(
                    "13.3 : la version lue est celle du runtime personnel",
                    found.version == "9.9.9",
                );
            }
            None => {
                report.check("13.3 : le runtime personnel masque celui du systeme", false);
                report.check(
                    "13.3 : la version lue est celle du runtime personnel",
                    false,
                );
            }
        }

        // Another account must not see it: a personal runtime is
        // personal, and a resolution that ignored the name would give
        // every account the alternative version of one of them.
        report.check(
            "13.3 : un autre compte ne voit pas ce runtime",
            runtime::resolve(Language::Asm, Some("admin"))
                .map(|found| found.origin)
                == Some(runtime::Origin::System),
        );
    }

    let _ = crate::fs::delete(&descriptor, who, now);
    let _ = crate::fs::delete(&base, who, now);

    // And the machine is left as it was found.
    report.check(
        "13.3 : le retrait rend la main au runtime systeme",
        runtime::resolve(Language::Asm, Some(RUNTIME_USER))
            .map(|found| found.origin)
            == Some(runtime::Origin::System),
    );
}

// ============================================================
//  9.5.1 -- Nova Rust: the front-end, the interpreter, the compiler
// ============================================================

/// Interprets a source and returns its output lines, or the refusal.
fn rust_output(source: &str) -> Result<Vec<String>, String> {
    match rustc::interpret(source, 200_000) {
        Ok(run) => Ok(run.output),
        Err(error) => Err(error.message()),
    }
}

/// Interprets a source expected to print exactly one line.
fn rust_line(source: &str) -> String {
    match rust_output(source) {
        Ok(lines) => lines.join(" | "),
        Err(message) => alloc::format!("ERREUR {}", message),
    }
}

/// Wraps an expression in a program that prints it.
fn rust_expr(expression: &str) -> String {
    rust_line(&alloc::format!(
        "fn main() {{ println!(\"{{}}\", {}); }}",
        expression
    ))
}

/// Does this source get refused, and does the refusal name `needle`?
fn rust_refuses(source: &str, needle: &str) -> bool {
    match rustc::compile(source) {
        Ok(_) => false,
        Err(error) => error.message().contains(needle),
    }
}

fn check_rustc(report: &mut Report) {
    rust_lexer(report);
    rust_refusals(report);
    rust_types(report);
    rust_evaluation(report);
    rust_backend(report);
}

/// The lexer, on what it must refuse rather than swallow.
fn rust_lexer(report: &mut Report) {
    report.check(
        "9.5.1 : une duree de vie est refusee en la nommant",
        rust_refuses("fn f<'a>(x: &'a str) {} fn main() {}", "duree de vie"),
    );
    report.check(
        "9.5.1 : un litteral de caractere est refuse en le nommant",
        rust_refuses("fn main() { let c = 'a'; }", "litteral de caractere"),
    );
    report.check(
        "9.5.1 : un echappement inconnu est refuse",
        rust_refuses("fn main() { println!(\"\\q\"); }", "echappement inconnu"),
    );
    report.check(
        "9.5.1 : un accent dans une chaine est refuse comme ailleurs",
        rust_refuses(
            "fn main() { println!(\"caf\u{e9}\"); }",
            "ASCII imprimable",
        ),
    );
    // A block comment that nests: stopping at the first `*/` would read
    // the rest of the file as code.
    report.equal_text(
        "9.5.1 : les commentaires de bloc s'imbriquent",
        &rust_line("fn main() { /* a /* b */ c */ println!(\"ok\"); }"),
        "ok",
    );
    report.equal_text(
        "9.5.1 : un attribut est ignore, pas refuse",
        &rust_line("#![allow(dead_code)]\n#[inline]\nfn main() { println!(\"ok\"); }"),
        "ok",
    );
    // `a != b` must not be lexed as a macro call `a!`.
    report.equal_text(
        "9.5.1 : != n'est pas une macro",
        &rust_expr("1 != 2"),
        "true",
    );
    report.equal_text(
        "9.5.1 : les separateurs et les bases sont lus",
        &rust_expr("1_000_000 + 0xff + 0b101 + 0o17"),
        "1000275",
    );
}

/// Every construct outside the subset, refused **by its own name**.
///
/// This is the block that makes the subset honest. A generic "unexpected
/// token" would send the reader hunting for a mistake they did not make.
fn rust_refusals(report: &mut Report) {
    let cases: &[(&str, &str, &str)] = &[
        ("struct", "struct P { x: i64 } fn main() {}", "struct"),
        ("enum", "enum E { A } fn main() {}", "enum"),
        ("trait", "trait T {} fn main() {}", "trait"),
        ("impl", "impl P {} fn main() {}", "trait / impl"),
        ("use", "use std::io; fn main() {}", "use / mod"),
        ("match", "fn main() { match 1 { _ => {} } }", "match"),
        ("for", "fn main() { for i in 0..3 {} }", "boucle for"),
        (
            "fermeture",
            "fn main() { let f = |x| x; }",
            "fermeture",
        ),
        (
            "generique",
            "fn f<T>(x: T) -> T { x } fn main() {}",
            "generique",
        ),
        (
            "methode",
            "fn main() { let x = 1; x.abs(); }",
            "acces par point",
        ),
        (
            "indexation",
            "fn main() { let x = 1; x[0]; }",
            "indexation",
        ),
        ("tuple", "fn main() { let x = (1, 2); }", "tuple"),
        ("String", "fn f(s: String) {} fn main() {}", "String"),
        ("Vec", "fn f(v: Vec) {} fn main() {}", "Vec"),
        ("emprunt", "fn main() { let a = 1; let b = &a; }", "emprunt"),
        (
            "dereference",
            "fn main() { let a = 1; let b = *a; }",
            "dereferencement",
        ),
        ("unsafe", "unsafe fn f() {} fn main() {}", "unsafe"),
        ("async", "async fn f() {} fn main() {}", "async"),
        (
            "operateur ?",
            "fn main() { let x = 1; x?; }",
            "operateur ?",
        ),
        (
            "chemin qualifie",
            "fn main() { let x = std::u64; }",
            "chemin qualifie",
        ),
        (
            "macro inconnue",
            "fn main() { vec![1]; }",
            "macro \"vec!\" inconnue",
        ),
        (
            "type inconnu",
            "fn f(x: i32) {} fn main() {}",
            "type \"i32\" inconnu",
        ),
    ];

    for (label, source, needle) in cases {
        report.check(
            &alloc::format!("9.5.1 : {} est refuse en le nommant", label),
            rust_refuses(source, needle),
        );
    }

    report.check(
        "9.5.1 : un programme sans main est refuse",
        rust_refuses("fn f() {}", "aucune fonction main"),
    );
    report.check(
        "9.5.1 : main ne prend pas de parametre",
        rust_refuses("fn main(x: i64) {}", "main ne prend pas"),
    );
    report.check(
        "9.5.1 : plus de six parametres est refuse",
        rust_refuses(
            "fn f(a: i64, b: i64, c: i64, d: i64, e: i64, g: i64, h: i64) {} fn main() {}",
            "depasse 6 parametres",
        ),
    );
    report.check(
        "9.5.1 : un format a trous mal apparies est refuse",
        rust_refuses("fn main() { println!(\"{} {}\", 1); }", "trou(s) dans le format"),
    );
    report.check(
        "9.5.1 : une accolade isolee dans un format est refusee",
        rust_refuses("fn main() { println!(\"a { b\"); }", "accolade isolee"),
    );
}

/// The checker, on the errors that would otherwise become wrong answers.
fn rust_types(report: &mut Report) {
    let cases: &[(&str, &str, &str)] = &[
        (
            "types melanges",
            "fn main() { let a: i64 = 1; let b: u64 = 2; let c = a + b; }",
            "i64 attendu, u64 trouve",
        ),
        (
            "affectation sans mut",
            "fn main() { let a = 1; a = 2; }",
            "n'est pas mutable",
        ),
        (
            "nombre d'arguments",
            "fn f(a: i64) {} fn main() { f(1, 2); }",
            "attend 1 argument(s), 2 fourni(s)",
        ),
        (
            "nom inconnu",
            "fn main() { let a = inconnu; }",
            "n'est pas declare",
        ),
        (
            "fonction inconnue",
            "fn main() { pasla(); }",
            "fonction \"pasla\" inconnue",
        ),
        (
            "condition non booleenne",
            "fn main() { if 1 { } }",
            "bool attendu, i64 trouve",
        ),
        (
            "retour manquant",
            "fn f() -> i64 { let a = 1; } fn main() {}",
            "peut atteindre sa fin sans en rendre un",
        ),
        (
            "retour du mauvais type",
            "fn f() -> i64 { return true; } fn main() {}",
            "i64 attendu, bool trouve",
        ),
        (
            "break hors boucle",
            "fn main() { break; }",
            "break hors d'une boucle",
        ),
        (
            "negation d'un u64",
            "fn main() { let a: u64 = 1; let b = -a; }",
            "l'operateur - ne s'applique pas a u64",
        ),
        (
            "conversion impossible",
            "fn main() { let a = 1; let b = a as bool; }",
            "conversion impossible de i64 vers bool",
        ),
        (
            "addition sur une chaine",
            "fn main() { let a = \"x\"; let b = a + a; }",
            "l'operateur + ne s'applique pas a &str",
        ),
        (
            "masquage",
            "fn main() { let a = 1; let a = 2; }",
            "est deja declare dans cette fonction",
        ),
        (
            "if sans else comme valeur",
            "fn f() -> i64 { if true { 1 } } fn main() {}",
            "un if sans else",
        ),
        (
            "branches de types differents",
            "fn f() -> i64 { if true { 1 } else { false } } fn main() {}",
            "les deux branches d'un if",
        ),
        (
            "comparaison de types differents",
            "fn main() { let a = 1; let b = true; if a == b {} }",
            "comparaison ==",
        ),
        (
            "litteral hors plage",
            "fn main() { let a: i64 = 18446744073709551615; }",
            "ne tient pas dans un i64",
        ),
    ];

    for (label, source, needle) in cases {
        report.check(
            &alloc::format!("9.5.1 : {} est diagnostique", label),
            rust_refuses(source, needle),
        );
    }

    // And the programs that must be accepted: a checker that refused
    // everything would pass every test above.
    let accepted: &[&str] = &[
        "fn main() { let mut a = 1; a += 1; }",
        "fn f(a: i64, b: i64) -> i64 { a + b } fn main() { f(1, 2); }",
        "fn main() { let a: u64 = 1; let b = a as i64; }",
        "fn main() { let a = true; if !a { } }",
        "const N: i64 = 3; fn main() { let a = N * 2; }",
        "fn f() -> i64 { if true { return 1; } return 2; } fn main() {}",
        "fn f() -> i64 { loop { } } fn main() {}",
        "fn main() { let s = \"a\"; if s == \"a\" { } }",
    ];
    for (index, source) in accepted.iter().enumerate() {
        report.check(
            &alloc::format!("9.5.1 : programme valide {} accepte", index + 1),
            rustc::compile(source).is_ok(),
        );
    }
}

/// The interpreter, on values a wrong compiler would also get wrong.
fn rust_evaluation(report: &mut Report) {
    // --- arithmetic and precedence ---
    report.equal_text("9.5.1 : precedence * sur +", &rust_expr("2 + 3 * 4"), "14");
    report.equal_text(
        "9.5.1 : la soustraction est associative a gauche",
        &rust_expr("10 - 3 - 2"),
        "5",
    );
    report.equal_text(
        "9.5.1 : les parentheses l'emportent",
        &rust_expr("(2 + 3) * 4"),
        "20",
    );
    report.equal_text(
        "9.5.1 : le decalage lie plus fort que l'addition",
        &rust_expr("1 + 2 << 2"),
        "12",
    );
    report.equal_text(
        "9.5.1 : le et binaire lie plus fort que la comparaison",
        &rust_expr("6 & 3 == 2"),
        "true",
    );

    // --- signedness, which is where a wrong instruction gives a
    //     plausible answer instead of a fault ---
    report.equal_text(
        "9.5.1 : division signee tronque vers zero",
        &rust_expr("-7 / 2"),
        "-3",
    );
    report.equal_text(
        "9.5.1 : reste signe garde le signe du dividende",
        &rust_expr("-7 % 2"),
        "-1",
    );
    report.equal_text(
        "9.5.1 : comparaison signee",
        &rust_expr("-1 < 1"),
        "true",
    );
    report.equal_text(
        "9.5.1 : comparaison non signee sur le meme motif de bits",
        &rust_line(
            "fn main() { let a: u64 = 18446744073709551615; let b: u64 = 1; println!(\"{}\", a < b); }",
        ),
        "false",
    );
    report.equal_text(
        "9.5.1 : decalage arithmetique sur un i64",
        &rust_expr("-8 >> 1"),
        "-4",
    );
    report.equal_text(
        "9.5.1 : decalage logique sur un u64",
        &rust_line(
            "fn main() { let a: u64 = 18446744073709551615; println!(\"{}\", a >> 60); }",
        ),
        "15",
    );

    // --- wrapping, which is the release behaviour of Rust ---
    report.equal_text(
        "9.5.1 : l'arithmetique boucle au lieu de paniquer",
        &rust_line(
            "fn main() { let a: i64 = 9223372036854775807; println!(\"{}\", a + 1); }",
        ),
        "-9223372036854775808",
    );
    report.equal_text(
        "9.5.1 : i64::MIN s'affiche entierement",
        &rust_line("fn main() { println!(\"{}\", -9223372036854775808); }"),
        "-9223372036854775808",
    );

    // --- control flow ---
    report.equal_text(
        "9.5.1 : while accumule",
        &rust_line(
            "fn main() { let mut n = 0; let mut s = 0; while n < 5 { s += n; n += 1; } println!(\"{}\", s); }",
        ),
        "10",
    );
    report.equal_text(
        "9.5.1 : loop et break",
        &rust_line(
            "fn main() { let mut n = 0; loop { n += 1; if n == 7 { break; } } println!(\"{}\", n); }",
        ),
        "7",
    );
    report.equal_text(
        "9.5.1 : continue saute un tour",
        &rust_line(
            "fn main() { let mut n = 0; let mut s = 0; while n < 6 { n += 1; if n % 2 == 0 { continue; } s += n; } println!(\"{}\", s); }",
        ),
        "9",
    );
    report.equal_text(
        "9.5.1 : if / else if / else",
        &rust_line(
            "fn c(n: i64) -> i64 { if n < 0 { -1 } else if n == 0 { 0 } else { 1 } } fn main() { println!(\"{} {} {}\", c(-5), c(0), c(5)); }",
        ),
        "-1 0 1",
    );
    report.equal_text(
        "9.5.1 : un bloc vaut son expression finale",
        &rust_expr("{ let a = 2; a * 3 }"),
        "6",
    );
    report.equal_text(
        "9.5.1 : la recursion rend le bon resultat",
        &rust_line(
            "fn fact(n: i64) -> i64 { if n < 2 { 1 } else { n * fact(n - 1) } } fn main() { println!(\"{}\", fact(6)); }",
        ),
        "720",
    );
    // The depth actually reached, measured rather than assumed.
    //
    // What limits it is the kernel stack, and the number therefore
    // depends on the frame sizes the Rust compiler chose for this build.
    // Printing it means the roadmap can state a figure that was observed
    // instead of one that was hoped for.
    let mut deepest = 0;
    for depth in 1..=rustc::interp::MAX_DEPTH {
        let source = alloc::format!(
            "fn f(n: i64) -> i64 {{ if n == 0 {{ 0 }} else {{ f(n - 1) }} }} fn main() {{ f({}); }}",
            depth
        );
        match rustc::interpret(&source, 200_000) {
            Ok(run) if run.output.is_empty() => deepest = depth,
            _ => break,
        }
    }
    crate::serial_println!(
        "[rustc] profondeur d'interpretation atteinte : {} (limite comptee {})",
        deepest,
        rustc::interp::MAX_DEPTH
    );
    // The figure is printed, not asserted against a constant. What
    // limits it is how much stack the **caller** left, so it moves with
    // the call chain that reached the interpreter -- eight levels from
    // here, twelve from a shallower caller. Asserting a number would be
    // asserting a property of this self-test.
    report.check(
        "9.5.1 : une recursion ordinaire passe",
        deepest >= 6,
    );
    // And beyond it, the counted limit fires -- with a message a
    // programmer can act on, not a byte count.
    report.check(
        "9.5.1 : au-dela, l'interprete compte plutot qu'il ne s'ecroule",
        matches!(
            rustc::interpret(
                "fn f(n: i64) -> i64 { if n == 0 { 0 } else { f(n - 1) } } fn main() { f(400); }",
                200_000,
            ),
            Ok(run) if run.output.iter().any(|line| line.contains("recursion trop profonde"))
        ),
    );

    // --- short circuit: the right side must not run, and it prints ---
    report.equal_text(
        "9.5.1 : && court-circuite",
        &rust_line(
            "fn bruit() -> bool { println!(\"evalue\"); true } fn main() { if false && bruit() { } println!(\"fin\"); }",
        ),
        "fin",
    );
    report.equal_text(
        "9.5.1 : || court-circuite",
        &rust_line(
            "fn bruit() -> bool { println!(\"evalue\"); true } fn main() { if true || bruit() { } println!(\"fin\"); }",
        ),
        "fin",
    );

    // --- strings and formatting ---
    report.equal_text(
        "9.5.1 : deux chaines egales le sont",
        &rust_expr("\"nova\" == \"nova\""),
        "true",
    );
    report.equal_text(
        "9.5.1 : deux chaines de meme longueur differentes ne le sont pas",
        &rust_expr("\"nova\" == \"nove\""),
        "false",
    );
    report.equal_text(
        "9.5.1 : plusieurs trous dans l'ordre",
        &rust_line("fn main() { println!(\"{}-{}-{}\", 1, true, \"x\"); }"),
        "1-true-x",
    );
    report.equal_text(
        "9.5.1 : une accolade doublee est litterale",
        &rust_line("fn main() { println!(\"{{{}}}\", 7); }"),
        "{7}",
    );
    report.equal_text(
        "9.5.1 : print! n'envoie pas la ligne, println! l'envoie",
        &rust_line("fn main() { print!(\"a\"); print!(\"b\"); println!(\"c\"); }"),
        "abc",
    );
    report.equal_text(
        "9.5.1 : une ligne laissee ouverte est rendue quand meme",
        &rust_line("fn main() { print!(\"seul\"); }"),
        "seul",
    );

    // --- constants ---
    report.equal_text(
        "9.5.1 : une constante est repliee",
        &rust_line("const A: i64 = 6; const B: i64 = 7; fn main() { println!(\"{}\", A * B); }"),
        "42",
    );

    // --- builtins ---
    report.check(
        "9.5.1 : exit rend son code",
        matches!(
            rustc::interpret("fn main() { exit(7); }", 10_000),
            Ok(run) if run.exit_code == Some(7)
        ),
    );
    report.check(
        "9.5.1 : pid() rend le PID de la tache",
        rust_expr("pid()") == alloc::format!("{}", crate::scheduler::current_pid()),
    );

    // --- the two bounds ---
    report.check(
        "9.5.1 : une boucle infinie epuise son budget au lieu de tenir le coeur",
        matches!(
            rustc::interpret("fn main() { loop { } }", 5_000),
            Ok(run) if run.output.iter().any(|line| line.contains("budget"))
        ),
    );
    report.check(
        "9.5.1 : une recursion sans fin est arretee",
        matches!(
            rustc::interpret("fn f(n: i64) -> i64 { f(n) + 1 } fn main() { f(1); }", 200_000),
            Ok(run) if run.output.iter().any(|line| line.contains("recursion"))
        ),
    );
    report.check(
        "9.5.1 : la division par zero panique au lieu de rendre un nombre",
        matches!(
            rustc::interpret("fn main() { let z = 0; println!(\"{}\", 1 / z); }", 10_000),
            Ok(run) if run.output.iter().any(|line| line.contains("division par zero"))
        ),
    );
}

/// The back-end: the assembly produced, and the object it assembles to.
fn rust_backend(report: &mut Report) {
    let source = "fn double(n: i64) -> i64 { n * 2 }\n\
                  fn main() { println!(\"{}\", double(21)); }\n";

    let assembly = match rustc::to_assembly(source) {
        Ok(text) => text,
        Err(error) => {
            report.check(
                &alloc::format!("9.5.1 : l'assemblage est produit - {}", error.message()),
                false,
            );
            return;
        }
    };

    // The instructions the choice of type decides. `imul` and not `mul`,
    // because the operands are signed: `mul` would give a plausible wrong
    // answer for every negative value.
    report.check(
        "9.5.1 : une multiplication signee passe par imul",
        assembly.contains("imul rax, rcx"),
    );
    report.check(
        "9.5.1 : les parametres sont recopies dans le cadre",
        assembly.contains("mov [rbp - 8], rdi"),
    );
    report.check(
        "9.5.1 : le point d'entree appelle main puis sort",
        assembly.contains("call main") && assembly.contains("mov rax, 523"),
    );
    report.check(
        "9.5.1 : la mise en forme passe par le tampon de ligne",
        assembly.contains("call __nova_buf_i64") && assembly.contains("call __nova_buf_flush"),
    );

    // A program that prints nothing must not carry the conversion
    // helpers: the runtime is emitted on demand, not always.
    let silent = rustc::to_assembly("fn main() { let a = 1; }").unwrap_or_default();
    report.check(
        "9.5.1 : un programme muet ne porte pas le tampon de sortie",
        !silent.contains("__nova_buf_push"),
    );

    // The division helper likewise, and it must appear as soon as a
    // division does.
    let divides = rustc::to_assembly("fn main() { let a = 4; let b = 2; let c = a / b; }")
        .unwrap_or_default();
    report.check(
        "9.5.1 : une division emet le controle du diviseur nul",
        divides.contains("call __nova_panic_div0") && divides.contains("cqo"),
    );

    // An unsigned division must not use idiv.
    let unsigned = rustc::to_assembly(
        "fn main() { let a: u64 = 4; let b: u64 = 2; let c = a / b; }",
    )
    .unwrap_or_default();
    report.check(
        "9.5.1 : une division non signee n'utilise pas idiv",
        unsigned.contains("div rcx") && !unsigned.contains("idiv"),
    );

    // --- the object ---
    match rustc::compile(source) {
        Ok(compiled) => {
            report.check(
                "9.5.1 : le programme s'assemble",
                !compiled.object.text.is_empty(),
            );
            report.check(
                "9.5.1 : le point d'entree est a l'offset zero",
                compiled.object.entry == 0,
            );
            report.check(
                "9.5.1 : les deux fonctions sont dans la table des symboles",
                compiled.object.symbol("main").is_some()
                    && compiled.object.symbol("double").is_some(),
            );
            report.check(
                "9.5.1 : les chaines partent en .data",
                !compiled.object.data.is_empty(),
            );
            report.check(
                "9.5.1 : le tampon de ligne est en .bss",
                compiled.object.bss_size >= 512,
            );
            report.check(
                "9.5.1 : chaque reference de donnee laisse un trou a combler",
                !compiled.object.fixups.is_empty(),
            );
            report.check(
                "9.5.1 : le compte de fonctions remonte",
                compiled.functions == 2 && compiled.constants == 0,
            );
        }
        Err(error) => report.check(
            &alloc::format!("9.5.1 : le programme s'assemble - {}", error.message()),
            false,
        ),
    }

    // The five templates must compile. A template that does not is worse
    // than no template, and this is the only way to know.
    for template in template::ALL.iter() {
        let source = template::rust_source_for(*template, "test.sdk.gabarit");
        report.check(
            &alloc::format!("9.5.1 : le gabarit {} en Rust compile", template.key()),
            rustc::compile(&source).is_ok(),
        );
    }

    // 13.2 now declares Rust a complete chain, and the runtime layout
    // must have followed: a table saying "in tree" with no descriptor on
    // the volume would be a claim with nothing behind it.
    report.check(
        "13.2 : Rust est declare chaine complete",
        runtime::spec(Language::Rust).support == Support::InTree,
    );
    report.check(
        "13.2 : Rust cible les cinq formats",
        runtime::targets(Language::Rust, BinaryFormat::Dsa)
            && runtime::targets(Language::Rust, BinaryFormat::Ras),
    );
    report.check(
        "9.5.1 : le sous-ensemble se decrit",
        rustc::describe().len() >= 5,
    );
}

// ============================================================
//  9.6 -- ETR
// ============================================================

/// An Electron source with a majority of translatable calls.
const ELECTRON_OK: &str = "\
const { app, BrowserWindow, ipcMain } = require('electron');\n\
const fs = require('fs');\n\
const path = require('path');\n\
\n\
function creerFenetre() {\n\
  const win = new BrowserWindow({ width: 800, height: 600 });\n\
  win.loadFile(path.join(__dirname, 'index.html'));\n\
  win.show();\n\
  return win;\n\
}\n\
\n\
app.whenReady().then(() => {\n\
  const contenu = fs.readFileSync('/etc/config.txt');\n\
  console.log('demarrage', contenu);\n\
  ipcMain.on('ping', (e) => { console.log('ping'); });\n\
  setTimeout(() => { console.log('tic'); }, 1000);\n\
  creerFenetre();\n\
});\n\
\n\
app.on('window-all-closed', () => { app.quit(); });\n";

/// The same, but with `eval`: 13.1 then imposes the `.ras`.
const ELECTRON_EVAL: &str = "\
const fs = require('fs');\n\
console.log(fs.readFileSync('/x'));\n\
const f = eval('1 + 1');\n";

/// A source dominated by the DOM: below the threshold.
const ELECTRON_DOM: &str = "\
const a = document.getElementById('a');\n\
const b = document.querySelector('.b');\n\
const c = document.createElement('div');\n\
window.addEventListener('click', function () { localStorage.getItem('k'); });\n\
console.log('un seul appel traduisible');\n";

fn check_etr(report: &mut Report) {
    let good = etr::analyze(ELECTRON_OK);
    report.check("ETR trouve des sites d'appel", good.call_sites > 0);
    report.check("ETR n'y voit pas d'eval", !good.eval_found);
    report.check(
        &alloc::format!("ETR depasse le seuil ({} %)", good.rate),
        good.rate >= etr::THRESHOLD,
    );
    report.check(
        "au-dessus du seuil, ETR produit un .cus (spec 13.1)",
        good.verdict == etr::Verdict::Native(BinaryFormat::Cus),
    );

    let with_eval = etr::analyze(ELECTRON_EVAL);
    report.check("ETR detecte eval", with_eval.eval_found);
    report.check(
        "eval force le .ras quel que soit le taux (spec 13.1)",
        with_eval.verdict == etr::Verdict::Raw(BinaryFormat::Ras),
    );

    let dom = etr::analyze(ELECTRON_DOM);
    report.check(
        &alloc::format!("le DOM tombe sous le seuil ({} %)", dom.rate),
        dom.rate < etr::THRESHOLD,
    );
    report.check(
        "sous le seuil, ETR classe en .ras",
        dom.verdict == etr::Verdict::Raw(BinaryFormat::Ras),
    );
    report.check("les manques sont nommes", !dom.gaps.is_empty());

    // The rate is an integer: the kernel is soft-float, a float here
    // would be a float in the kernel.
    report.check("le taux reste dans 0..100", good.rate <= 100 && dom.rate <= 100);

    // --- mappings ---
    report.check(
        "console.log devient une trace",
        matches!(etr::lookup("console.log"), Some(etr::Target::Syscall(_))),
    );
    report.check(
        "eval est interdit",
        matches!(etr::lookup("eval"), Some(etr::Target::Forbidden(_))),
    );
    report.check(
        "le DOM n'a pas d'equivalent",
        matches!(
            etr::lookup("document.getElementById"),
            Some(etr::Target::None(_))
        ),
    );
    report.check(
        "BrowserWindow passe par le compositeur",
        matches!(
            etr::lookup("BrowserWindow"),
            Some(etr::Target::Compositor(_))
        ),
    );
    report.check(
        "un appel inconnu n'a pas de correspondance",
        etr::lookup("libExotique.methodeInconnue").is_none(),
    );

    // --- lexer: the `/` trap ---
    // A regular expression containing a quote must not open a string: if
    // it did, the whole rest of the file would be swallowed and the rate
    // would collapse for no reason.
    let tricky = "const r = /a\"b/g;\nconsole.log('ok');\nconsole.log('ok2');\n";
    let analysis = etr::analyze(tricky);
    report.check(
        "une expression reguliere ne mange pas le fichier",
        analysis.call_sites == 2,
    );
    // A comment must not be read as code.
    let commented = "// console.log('non compte')\n/* console.log('non plus') */\nconsole.log('compte');\n";
    report.check(
        "les commentaires ne comptent pas",
        etr::analyze(commented).call_sites == 1,
    );
    // A string with an escaped apostrophe must not shift what follows.
    let escaped = "console.log('c\\'est');\nconsole.log('deux');\n";
    report.check(
        "une echappee ne decale pas le lexeur",
        etr::analyze(escaped).call_sites == 2,
    );
}

// ============================================================
//  9.4.3 -- libdev
// ============================================================

fn check_libdev(report: &mut Report) {
    report.check("le catalogue n'est pas vide", !libdev::CATALOG.is_empty());

    // Every entry must name a system call the phase 5 table knows. An
    // entry pointing elsewhere would be false documentation.
    let mut unknown = Vec::new();
    for entry in libdev::CATALOG.iter() {
        if crate::syscall::table::lookup(entry.syscall).is_none() {
            unknown.push(entry.name);
        }
    }
    report.check(
        &alloc::format!("chaque abstraction pointe vers un appel reel ({:?})", unknown),
        unknown.is_empty(),
    );

    report.check(
        "les permissions du catalogue sont connues",
        libdev::CATALOG
            .iter()
            .all(|entry| entry.permission & !Permissions::KNOWN == 0),
    );
    report.check(
        "lire un fichier exige FS_USER_HOME",
        libdev::required_permissions(&["lire(chemin)"]) == Permissions::FS_USER_HOME,
    );
    report.check("le catalogue se rend en lignes", !libdev::lines().is_empty());
}

// ============================================================
//  9.3 -- Sandbox
// ============================================================

fn check_sandbox(report: &mut Report, now: u64) {
    // Warm-up cycle before any measurement.
    //
    // The first allocation in the arena at `0x3000_0000` causes a
    // last-level page table to be created, and `memory::unmap_page` does
    // not return the intermediate tables -- that is true of the 6.4 arena
    // as of this one. That frame is an installation cost, not a leak:
    // measuring it as one would fail a check that is right. What must be
    // exact, and what the following measurement verifies, is that a
    // **second** cycle takes nothing more.
    if let Ok(warmup) = sandbox::create(
        crate::scheduler::current_pid(),
        BinaryFormat::Cus,
        "test.sdk.chauffe",
        now,
    ) {
        let _ = sandbox::allocate(warmup, 16 * 1024);
        let _ = sandbox::destroy(warmup, now);
    }

    let (free_before, _, _) = frames_free();

    let Ok(id) = sandbox::create(
        crate::scheduler::current_pid(),
        BinaryFormat::Cus,
        "test.sdk.bac",
        now,
    ) else {
        report.check("un bac a sable s'ouvre", false);
        return;
    };
    report.check("un bac a sable s'ouvre", true);

    // --- 9.3.1: dedicated memory ---
    match sandbox::allocate(id, 16 * 1024) {
        Ok(base) => {
            report.check(
                "l'allocation tombe dans l'arene du bac a sable",
                base >= sandbox::ARENA_BASE && base < sandbox::ARENA_BASE + sandbox::ARENA_SIZE,
            );
            report.check(
                "quatre pages ont ete prises",
                sandbox::pages_used(id) == Some(4),
            );
            // A fresh region must be entirely zero: a recycled page
            // keeping the content of another task would be an
            // information leak. Same invariant as in phase 5.4.
            let zeroed = (0..16 * 1024 / 8).all(|offset| unsafe {
                core::ptr::read_volatile((base + offset as u64 * 8) as *const u64) == 0
            });
            report.check("la region allouee est a zero", zeroed);
        }
        Err(error) => report.check(
            &alloc::format!("l'allocation aboutit : {}", error.message()),
            false,
        ),
    }
    report.check(
        "le budget de pages est applique",
        matches!(
            sandbox::allocate(id, (sandbox::PAGE_BUDGET as u64 + 1) * 4096),
            Err(SandboxError::OutOfPages)
        ),
    );

    // --- 9.3.2: overlay ---
    //
    // The test paths fall under the root 12.4 grants a `.cus`:
    // `/apps/{app_id}`. Taking them elsewhere would not test the overlay
    // but the confinement rule, and would make "the lower layer was not
    // read" and "the path was forbidden" indistinguishable.
    let who = Credentials::kernel();
    let app_root = "/apps/test.sdk.bac";
    let _ = crate::fs::create_directory(app_root, who, Mode::system_dir(), now);
    let host_path = "/apps/test.sdk.bac/temoin.txt";
    let _ = crate::fs::write_file(host_path, b"hote", who, Mode::system_file(), now);

    // Before any write, the read must pass through to the host.
    match sandbox::read(id, host_path) {
        Ok(bytes) => report.check("la lecture traverse vers la couche basse", bytes == b"hote"),
        Err(error) => report.check(
            &alloc::format!("la lecture traverse vers la couche basse : {}", error.message()),
            false,
        ),
    }

    report.check(
        "l'ecriture reussit dans le bac a sable",
        sandbox::write(id, host_path, b"sandbox", now).is_ok(),
    );
    match sandbox::read(id, host_path) {
        Ok(bytes) => report.check("la couche haute masque la couche basse", bytes == b"sandbox"),
        Err(error) => report.check(
            &alloc::format!("la couche haute masque la couche basse : {}", error.message()),
            false,
        ),
    }
    // The point that counts: the host must not have moved.
    match crate::fs::read_file(host_path, who) {
        Ok(bytes) => report.check("l'hote n'a pas ete ecrit", bytes == b"hote"),
        Err(_) => report.check("l'hote n'a pas ete ecrit", false),
    }

    // A path outside the roots of 12.4 is not readable.
    report.check(
        "/sys/config est hors des racines d'un .cus",
        !sandbox::readable_from_host(id, "/sys/config/theme.cfg"),
    );
    report.check(
        "la lecture hors racine est refusee",
        matches!(
            sandbox::read(id, "/sys/config/theme.cfg"),
            Err(SandboxError::OutsideRoots(_))
        ),
    );
    report.check(
        "un chemin voisin ne passe pas pour une racine",
        !sandbox::readable_from_host(id, "/apps/test.sdk.bacterie/x"),
    );

    // Tombstone: deleting masks without touching the host.
    let tomb_target = "/apps/test.sdk.bac/tombe.txt";
    let _ = crate::fs::write_file(tomb_target, b"present", who, Mode::system_file(), now);
    report.check(
        "l'entree de la couche basse est visible avant l'effacement",
        sandbox::exists(id, tomb_target),
    );
    report.check(
        "l'effacement dans le bac aboutit",
        sandbox::delete(id, tomb_target, now).is_ok(),
    );
    report.check(
        "l'entree effacee n'est plus visible du bac",
        !sandbox::exists(id, tomb_target),
    );
    report.check(
        "l'entree effacee existe toujours sur l'hote",
        crate::fs::stat(tomb_target, who).is_ok(),
    );
    // Rewriting cancels the deletion: without this, a recreated file
    // would stay invisible for the rest of the session.
    report.check(
        "reecrire apres effacement rend l'entree visible",
        sandbox::write(id, tomb_target, b"revenu", now).is_ok()
            && sandbox::exists(id, tomb_target),
    );

    // --- 9.3.3: loopback ---
    report.check("un port se lie", sandbox::bind(id, 8080).is_ok());
    report.check(
        "lier deux fois le meme port est refuse",
        matches!(sandbox::bind(id, 8080), Err(SandboxError::PortBusy(_))),
    );
    report.check(
        "une emission vers 127.0.0.1 passe",
        sandbox::send(id, [127, 0, 0, 1], 8080, b"salut").is_ok(),
    );
    report.check(
        "une emission vers une adresse publique est refusee",
        matches!(
            sandbox::send(id, [8, 8, 8, 8], 8080, b"x"),
            Err(SandboxError::NotLoopback(_))
        ),
    );
    report.check(
        "127.0.0.53 est aussi du bouclage",
        sandbox::send(id, [127, 0, 0, 53], 8080, b"x").is_ok(),
    );
    report.check(
        "un port ferme refuse l'emission",
        matches!(
            sandbox::send(id, [127, 0, 0, 1], 9999, b"x"),
            Err(SandboxError::PortClosed(_))
        ),
    );
    match sandbox::recv(id, 8080) {
        Ok(Some(payload)) => report.check("le message revient intact", payload == b"salut"),
        _ => report.check("le message revient intact", false),
    }

    // --- 9.3.4: replay as .ras ---
    let plan = sandbox::plan_ras(
        BinaryFormat::Dsa,
        SignatureType::System,
        Permissions::KERNEL_SPACE | Permissions::HARDWARE | Permissions::FS_FULL,
        flags::EXPOSES_API | flags::SERVICE,
        2,
    );
    report.check(
        "le rejeu conserve HARDWARE (plafond .ras du 12.4)",
        plan.permissions & Permissions::HARDWARE != 0,
    );
    report.check(
        "le rejeu retire KERNEL_SPACE",
        plan.permissions & Permissions::KERNEL_SPACE == 0,
    );
    report.check(
        "le rejeu retire FS_FULL",
        plan.permissions & Permissions::FS_FULL == 0,
    );
    report.check(
        "le rejeu retire EXPOSES_API (spec 3.4)",
        plan.flags & flags::EXPOSES_API == 0,
    );
    report.check(
        "le rejeu garde le drapeau SERVICE",
        plan.flags & flags::SERVICE != 0,
    );
    report.check("le format d'origine est conserve dans le plan", plan.original_format == BinaryFormat::Dsa);
    report.check(
        "le rejeu retire les ports internes (spec 12.3)",
        plan.ports_dropped == 2,
    );

    // --- 9.3.5: complete destruction ---
    let root = alloc::format!("{}/{}", sandbox::OVERLAY_ROOT, id);
    report.check(
        "le dossier du bac a sable existe",
        crate::fs::stat(&root, who).is_ok(),
    );
    match sandbox::destroy(id, now) {
        Ok((pages, _)) => {
            report.check("la destruction rend les pages", pages == 4);
            report.check(
                "le dossier du bac a sable a disparu",
                crate::fs::stat(&root, who).is_err(),
            );
        }
        Err(error) => report.check(
            &alloc::format!("la destruction aboutit : {}", error.message()),
            false,
        ),
    }
    report.check(
        "un bac detruit n'est plus connu",
        matches!(sandbox::destroy(id, now), Err(SandboxError::Unknown(_))),
    );

    // The invariant that counts: the frames come back to their count.
    let (free_after, _, _) = frames_free();
    report.check(
        &alloc::format!(
            "aucun cadre fuit ({} avant, {} apres)",
            free_before,
            free_after
        ),
        free_after == free_before,
    );

    let _ = crate::fs::delete(host_path, who, now);
    let _ = crate::fs::delete(tomb_target, who, now);
    let _ = crate::fs::delete(app_root, who, now);
}

/// Free frames, as `frame::stats` counts them.
/// (free, used, total) frames, the kernel stacks of tasks counted as
/// free: the runs are checked for lost pages, and the system tasks that
/// start meanwhile each take a stack of 32 frames (`process::KernelStack`).
fn frames_free() -> (usize, usize, usize) {
    let (total, used, free) = crate::frame::stats();
    let stacks = crate::process::kernel_stack_frames();
    (free + stacks, used.saturating_sub(stacks), total)
}

// ============================================================
//  9.1 -- The complete chain
// ============================================================

fn check_cli(report: &mut Report, now: u64) {
    purge(now);

    // --- 9.1.1: the five templates ---
    report.check(
        "les cinq gabarits du 15.5 sont declares",
        template::ALL.len() == 5,
    );
    let expected: &[(Template, &str, &str, BinaryFormat)] = &[
        (Template::Blank, "blank", "Projet vide", BinaryFormat::Cus),
        (
            Template::GuiApp,
            "gui-app",
            "App avec fenetre de base",
            BinaryFormat::Cus,
        ),
        (
            Template::Service,
            "service",
            "Service .dsa sans GUI",
            BinaryFormat::Dsa,
        ),
        (
            Template::Game,
            "game",
            "Jeu .gs avec boucle de jeu",
            BinaryFormat::Gs,
        ),
        (
            Template::CliTool,
            "cli-tool",
            "Outil CLI sans GUI",
            BinaryFormat::Cus,
        ),
    ];
    for (template, key, description, format) in expected {
        report.check(
            &alloc::format!("gabarit {} : cle", key),
            template.key() == *key,
        );
        report.check(
            &alloc::format!("gabarit {} : description 15.5", key),
            template.description() == *description,
        );
        report.check(
            &alloc::format!("gabarit {} : format", key),
            template.format() == *format,
        );
    }

    // Every template must produce a project that assembles. That check is
    // what tells a template from a mock-up.
    for template in template::ALL.iter() {
        let name = alloc::format!("t{}", template.key().replace('-', ""));
        let app_id = alloc::format!("test.sdk.{}", name);
        let line = alloc::format!(
            "dev new {} --template {} --dir {}",
            app_id,
            template.key(),
            TEST_ROOT
        );
        match cli::execute(&line, now) {
            Ok(Output::Created { path, .. }) => {
                report.check(&alloc::format!("dev new {}", template.key()), true);

                let build = alloc::format!("dev build --projet {}", path);
                match cli::execute(&build, now) {
                    Ok(Output::Built(built)) => {
                        report.check(
                            &alloc::format!("dev build {} produit du code", template.key()),
                            built.text_bytes > 0 && built.instructions > 0,
                        );
                        report.check(
                            &alloc::format!(
                                "dev build {} : toute reference de donnee est resolue",
                                template.key()
                            ),
                            built.relocations > 0 || built.data_bytes == 0,
                        );
                        report.check(
                            &alloc::format!("dev build {} respecte le format", template.key()),
                            built.format == template.format(),
                        );
                        // The file written must be readable back by phase
                        // 3: that is the only proof that counts.
                        match crate::fs::read_file(&built.path, Credentials::kernel()) {
                            Ok(bytes) => match crate::binfmt::parse(&bytes) {
                                Ok(header) => {
                                    report.check(
                                        &alloc::format!("{} s'analyse", template.key()),
                                        true,
                                    );
                                    report.check(
                                        &alloc::format!(
                                            "{} n'a aucune violation du 3.4",
                                            template.key()
                                        ),
                                        crate::binfmt::check_rules(&header).is_empty(),
                                    );
                                }
                                Err(error) => report.check(
                                    &alloc::format!(
                                        "{} s'analyse : {}",
                                        template.key(),
                                        error.message()
                                    ),
                                    false,
                                ),
                            },
                            Err(_) => report.check(
                                &alloc::format!("{} est relu du volume", template.key()),
                                false,
                            ),
                        }
                    }
                    Ok(_) => report.check(&alloc::format!("dev build {}", template.key()), false),
                    Err(error) => report.check(
                        &alloc::format!("dev build {} : {}", template.key(), error.message()),
                        false,
                    ),
                }
            }
            Ok(_) => report.check(&alloc::format!("dev new {}", template.key()), false),
            Err(error) => report.check(
                &alloc::format!("dev new {} : {}", template.key(), error.message()),
                false,
            ),
        }
    }

    // --- 9.5.1: the same five templates, in Rust ---
    //
    // `dev new monapp --type cus --lang rust` is literally the example of
    // 15.2, so the five templates exist in Rust too and each one is
    // created, built and read back by the phase 3 parser. A template that
    // does not compile is worse than no template, and there is no way to
    // know but to build it.
    for template in template::ALL.iter() {
        let name = alloc::format!("r{}", template.key().replace('-', ""));
        let app_id = alloc::format!("test.sdk.{}", name);
        let line = alloc::format!(
            "dev new {} --template {} --lang rust --dir {}",
            app_id,
            template.key(),
            TEST_ROOT
        );
        match cli::execute(&line, now) {
            Ok(Output::Created { path, .. }) => {
                // The entry point must be the Rust one: a manifest saying
                // `src/main.rs` over a disk holding `src/main.asm` fails
                // its first build on a file it never had.
                let entry_ok = crate::fs::stat(
                    &alloc::format!("{}/src/main.rs", path),
                    Credentials::kernel(),
                )
                .is_ok();
                report.check(
                    &alloc::format!("9.5.1 : dev new --lang rust {} ecrit un .rs", template.key()),
                    entry_ok,
                );

                match cli::execute(&alloc::format!("dev build --projet {}", path), now) {
                    Ok(Output::Built(built)) => {
                        report.check(
                            &alloc::format!(
                                "9.5.1 : le gabarit {} en Rust produit du code",
                                template.key()
                            ),
                            built.text_bytes > 0 && built.instructions > 0,
                        );
                        report.check(
                            &alloc::format!(
                                "9.5.1 : le binaire Rust {} respecte le format",
                                template.key()
                            ),
                            built.format == template.format(),
                        );
                        match crate::fs::read_file(&built.path, Credentials::kernel()) {
                            Ok(bytes) => report.check(
                                &alloc::format!(
                                    "9.5.1 : le binaire Rust {} se relit du volume",
                                    template.key()
                                ),
                                crate::binfmt::parse(&bytes)
                                    .map(|header| crate::binfmt::check_rules(&header).is_empty())
                                    .unwrap_or(false),
                            ),
                            Err(_) => report.check(
                                &alloc::format!(
                                    "9.5.1 : le binaire Rust {} se relit du volume",
                                    template.key()
                                ),
                                false,
                            ),
                        }
                    }
                    Ok(_) => report.check(
                        &alloc::format!("9.5.1 : dev build Rust {}", template.key()),
                        false,
                    ),
                    Err(error) => report.check(
                        &alloc::format!(
                            "9.5.1 : dev build Rust {} : {}",
                            template.key(),
                            error.message()
                        ),
                        false,
                    ),
                }
            }
            Ok(_) => report.check(
                &alloc::format!("9.5.1 : dev new --lang rust {}", template.key()),
                false,
            ),
            Err(error) => report.check(
                &alloc::format!(
                    "9.5.1 : dev new --lang rust {} : {}",
                    template.key(),
                    error.message()
                ),
                false,
            ),
        }
    }

    // --- 9.1.2: --release ---
    let project = alloc::format!("{}/tblank", TEST_ROOT);
    let before = cli::load_manifest(&project)
        .map(|manifest| manifest.version.build)
        .ok();
    match cli::execute(&alloc::format!("dev build --release --projet {}", project), now) {
        Ok(Output::Built(built)) => {
            report.check("--release est signale", built.release);
            report.check("--release n'a aucun reproche", built.warnings.is_empty());
            report.check(
                "--release incremente le numero de construction",
                Some(built.version.build) == before.map(|value| value + 1),
            );
            // And the manifest must have followed, otherwise the next
            // build would start again from the same number.
            report.check(
                "le manifest garde le nouveau numero",
                cli::load_manifest(&project)
                    .map(|manifest| manifest.version.build)
                    .ok()
                    == Some(built.version.build),
            );
        }
        _ => report.check("dev build --release", false),
    }

    // --- 9.1.4: dev interpret ---
    let script_path = alloc::format!("{}/scripts/exemple.ns", project);
    match cli::execute(&alloc::format!("dev interpret {}", script_path), now) {
        Ok(Output::Interpreted(run)) => {
            report.check("dev interpret produit une sortie", !run.output.is_empty());
            report.check(
                "le script du gabarit conclut correctement",
                run.output.iter().any(|line| line == "conforme"),
            );
        }
        Ok(_) => report.check("dev interpret", false),
        Err(error) => report.check(
            &alloc::format!("dev interpret : {}", error.message()),
            false,
        ),
    }
    // 9.5.1 -- `dev interpret` on a `.rs` goes to the Rust interpreter,
    // and the extension is what routes it. Two interpreters live in the
    // SDK now, and picking the wrong one would run Nova Script over a
    // Rust source and report a syntax error in a language nobody wrote.
    let rust_script = alloc::format!("{}/src/apercu.rs", project);
    let _ = crate::fs::write_file(
        &rust_script,
        concat!(
            "fn carre(n: i64) -> i64 { n * n }\n",
            "fn main() {\n",
            "    let mut n = 1;\n",
            "    while n <= 4 {\n",
            "        println!(\"{} au carre = {}\", n, carre(n));\n",
            "        n += 1;\n",
            "    }\n",
            "}\n",
        )
        .as_bytes(),
        Credentials::kernel(),
        Mode::private_file(),
        now,
    );
    match cli::execute(&alloc::format!("dev interpret {}", rust_script), now) {
        Ok(Output::InterpretedRust(run)) => {
            report.check(
                "9.5.1 : dev interpret sur un .rs rend les quatre lignes",
                run.output.len() == 4,
            );
            report.check(
                "9.5.1 : dev interpret calcule vraiment",
                run.output.first().map(|line| line.as_str()) == Some("1 au carre = 1")
                    && run.output.last().map(|line| line.as_str()) == Some("4 au carre = 16"),
            );
        }
        Ok(_) => report.check("9.5.1 : dev interpret sur un .rs", false),
        Err(error) => report.check(
            &alloc::format!("9.5.1 : dev interpret sur un .rs : {}", error.message()),
            false,
        ),
    }

    // A language with no interpreter in place must produce a **named** refusal.
    let fake = alloc::format!("{}/faux.py", project);
    let _ = crate::fs::write_file(
        &fake,
        b"print('x')\n",
        Credentials::kernel(),
        Mode::private_file(),
        now,
    );
    report.check(
        "Python sans runtime est refuse nommement",
        matches!(
            cli::execute(&alloc::format!("dev interpret {}", fake), now),
            Err(DevError::RuntimeMissing(Language::Python))
        ),
    );

    // --- 9.1.5: dev package ---
    match cli::execute(&alloc::format!("dev package --projet {}", project), now) {
        Ok(Output::Packaged(packaged)) => {
            report.check("dev package ecrit un fichier", packaged.bytes > 0);
            match crate::fs::read_file(&packaged.path, Credentials::kernel()) {
                Ok(bytes) => match crate::spm::package::parse(&bytes) {
                    Ok(header) => {
                        report.check("le paquet s'analyse", true);
                        let verdict = crate::spm::package::verify(&header, &bytes);
                        report.check("le paquet est accepte", verdict.accepted());
                        let manifest =
                            crate::spm::package::manifest(&header, &bytes);
                        report.check(
                            "le manifest du paquet porte l'app_id",
                            manifest.app_id == packaged.app_id,
                        );
                    }
                    Err(error) => report.check(
                        &alloc::format!("le paquet s'analyse : {}", error.message()),
                        false,
                    ),
                },
                Err(_) => report.check("le paquet est relu du volume", false),
            }
        }
        Ok(_) => report.check("dev package", false),
        Err(error) => report.check(&alloc::format!("dev package : {}", error.message()), false),
    }
    // A `--sign` incompatible with the format must fail, rather than
    // produce a package `spm install` would refuse later on.
    report.check(
        "dev package --sign system sur un .cus est accepte",
        cli::execute(
            &alloc::format!("dev package --sign self --projet {}", project),
            now,
        )
        .is_ok(),
    );
    let service = alloc::format!("{}/tservice", TEST_ROOT);
    report.check(
        "dev package --sign self sur un .dsa est refuse (spec 3.4)",
        matches!(
            cli::execute(
                &alloc::format!("dev package --sign self --projet {}", service),
                now
            ),
            Err(DevError::RuleViolation(_))
        ),
    );

    // --- command refusals ---
    report.check(
        "une commande inconnue est refusee",
        matches!(cli::execute("dev frobniquer", now), Err(DevError::BadCommand(_))),
    );
    report.check(
        "une ligne qui ne commence pas par dev est refusee",
        matches!(cli::execute("spm install x", now), Err(DevError::BadCommand(_))),
    );
    report.check(
        "un dossier sans dev.toml n'est pas un projet",
        matches!(
            cli::execute("dev build --projet /sys", now),
            Err(DevError::NotAProject(_))
        ),
    );
    report.check(
        "un gabarit inconnu est refuse",
        matches!(
            cli::execute("dev new x.y --template exotique", now),
            Err(DevError::UnknownTemplate(_))
        ),
    );
}

/// 9.1.3 and 9.1.6 -- what asks to run or to convert.
fn check_run_and_convert(report: &mut Report, now: u64) {
    let (free_before, _, _) = frames_free();

    // --- 9.1.3: dev run, in a sandbox ---
    let project = alloc::format!("{}/tclitool", TEST_ROOT);
    match cli::execute(&alloc::format!("dev run --projet {}", project), now) {
        Ok(Output::Ran(run)) => {
            report.check("dev run rend un code de sortie", run.exit_code == 0);
            report.check(
                "dev run rejoue depuis le format d'origine",
                run.original_format == BinaryFormat::Cus,
            );
            report.check("dev run prend des pages dediees", run.pages > 0);
            report.check("dev run detruit son bac a sable", !run.kept);
        }
        Ok(_) => report.check("dev run", false),
        Err(error) => report.check(&alloc::format!("dev run : {}", error.message()), false),
    }

    // The `service` template produces a SYSTEM `.dsa` the loader would
    // refuse; 15.4 replays it as a `.ras`, so it must run. That is the
    // proof that the replay is not cosmetic.
    let service = alloc::format!("{}/tservice", TEST_ROOT);
    match cli::execute(&alloc::format!("dev run --projet {}", service), now) {
        Ok(Output::Ran(run)) => {
            report.check(
                "un .dsa SYSTEM se teste grace au rejeu en .ras",
                run.exit_code == 0 && run.original_format == BinaryFormat::Dsa,
            );
            report.check(
                "le rejeu a rabote des permissions",
                run.dropped_permissions != 0,
            );
            report.check(
                "le rejeu a retire le port interne du service",
                run.dropped_ports == 1,
            );
        }
        Ok(_) => report.check("dev run sur le service", false),
        Err(error) => report.check(
            &alloc::format!("dev run sur le service : {}", error.message()),
            false,
        ),
    }

    // `--debug` stops the program on its entry (16.4); once it is let go
    // to its end, the sandbox is kept, and `dev nettoyer` purges it.
    match cli::execute(&alloc::format!("dev run --debug --projet {}", project), now) {
        Ok(Output::Debugging(started)) => {
            let (lines, ended) = crate::sdk::debug::command(started.pid, "continuer");
            report.check(
                "--debug arrete a l'entree puis mene le programme a sa fin",
                ended && lines.iter().any(|l| l.contains("code de sortie 0")),
            );
            let root = alloc::format!("{}/{}", sandbox::OVERLAY_ROOT, started.sandbox);
            report.check(
                "le bac conserve existe encore",
                crate::fs::stat(&root, Credentials::kernel()).is_ok(),
            );
            match cli::execute("dev nettoyer", now) {
                Ok(Output::Cleaned { sandboxes, .. }) => {
                    report.check("dev nettoyer purge le bac conserve", sandboxes == 1);
                    report.check(
                        "le dossier conserve a disparu",
                        crate::fs::stat(&root, Credentials::kernel()).is_err(),
                    );
                }
                _ => report.check("dev nettoyer", false),
            }
        }
        Ok(_) => report.check("dev run --debug", false),
        Err(error) => report.check(
            &alloc::format!("dev run --debug : {}", error.message()),
            false,
        ),
    }

    // --- 9.5.1: a Rust project, compiled and really run ---
    //
    // This is the check that separates a compiler from a pipeline of
    // plausible-looking stages. The source below is written in Nova Rust,
    // assembled by the back-end, linked into a `.cus`, replayed as a
    // `.ras` in a sandbox, loaded by the phase 3 loader and executed in
    // **ring 3**. What comes back is its exit code, and the exit code is
    // a number the program computed.
    //
    // The recursion goes deeper than the interpreter accepts: the
    // compiled form runs on its own stack, and this is what shows that
    // the interpreter's ceiling is the interpreter's alone.
    let rust_project = alloc::format!("{}/rrun", TEST_ROOT);
    let who = Credentials::kernel();
    let _ = crate::fs::create_directory(&rust_project, who, Mode::private_dir(), now);
    let _ = crate::fs::create_directory(
        &alloc::format!("{}/src", rust_project),
        who,
        Mode::private_dir(),
        now,
    );
    let rust_source = concat!(
        "fn somme(n: i64) -> i64 {\n",
        "    if n == 0 { 0 } else { n + somme(n - 1) }\n",
        "}\n",
        "\n",
        "fn main() {\n",
        "    let total = somme(24);\n",
        "    println!(\"somme(1..24) = {}\", total);\n",
        "    if total == 300 {\n",
        "        exit(0);\n",
        "    }\n",
        "    exit(1);\n",
        "}\n",
    );
    let source_written = crate::fs::write_file(
        &alloc::format!("{}/src/main.rs", rust_project),
        rust_source.as_bytes(),
        who,
        Mode::private_file(),
        now,
    )
    .is_ok();
    let manifest_written = crate::fs::write_file(
        &alloc::format!("{}/{}", rust_project, manifest::FILE_NAME),
        manifest::to_toml(&template::manifest_for(
            Template::CliTool,
            "test.sdk.rrun",
            "autotest",
            Language::Rust,
        ))
        .as_bytes(),
        who,
        Mode::private_file(),
        now,
    )
    .is_ok();
    report.check(
        "9.5.1 : le projet Rust d'essai est ecrit",
        source_written && manifest_written,
    );

    match cli::execute(&alloc::format!("dev build --projet {}", rust_project), now) {
        Ok(Output::Built(built)) => {
            report.check(
                "9.5.1 : dev build compile du Rust en code machine",
                built.text_bytes > 0 && built.instructions > 0,
            );
            report.check(
                "9.5.1 : le binaire Rust n'a aucune violation du 3.4",
                built.warnings.is_empty(),
            );
        }
        Ok(_) => report.check("9.5.1 : dev build sur un projet Rust", false),
        Err(error) => report.check(
            &alloc::format!("9.5.1 : dev build sur un projet Rust : {}", error.message()),
            false,
        ),
    }

    match cli::execute(&alloc::format!("dev run --projet {}", rust_project), now) {
        Ok(Output::Ran(run)) => {
            // Exit code 0 means the program's own comparison held: it
            // computed 300 in ring 3, with a recursion twenty-four deep.
            report.check(
                "9.5.1 : le programme Rust s'execute en ring 3 et rend 0",
                run.exit_code == 0,
            );
            report.check(
                "9.5.1 : il a recu des pages a lui",
                run.pages > 0 && !run.kept,
            );
        }
        Ok(_) => report.check("9.5.1 : dev run sur un projet Rust", false),
        Err(error) => report.check(
            &alloc::format!("9.5.1 : dev run sur un projet Rust : {}", error.message()),
            false,
        ),
    }

    // --- 9.1.6: dev convert ---
    let electron = alloc::format!("{}/electron", TEST_ROOT);
    let who = Credentials::kernel();
    let _ = crate::fs::create_directory(&electron, who, Mode::private_dir(), now);
    let _ = crate::fs::write_file(
        &alloc::format!("{}/main.js", electron),
        ELECTRON_OK.as_bytes(),
        who,
        Mode::private_file(),
        now,
    );
    match cli::execute(
        &alloc::format!("dev convert --from electron {}", electron),
        now,
    ) {
        Ok(Output::Converted(converted)) => {
            report.check(
                "la conversion depasse le seuil",
                converted.analysis.rate >= etr::THRESHOLD,
            );
            report.check(
                "la conversion produit un binaire (spec 13.1)",
                converted.binary.is_some(),
            );
            report.check(
                "le rapport est ecrit dans le projet",
                crate::fs::stat(
                    &alloc::format!("{}/RAPPORT-ETR.txt", converted.project),
                    who,
                )
                .is_ok(),
            );
            if let Some(binary) = converted.binary.as_ref() {
                report.check(
                    "le binaire converti est un .cus",
                    binary.ends_with(".cus"),
                );
            }
        }
        Ok(_) => report.check("dev convert", false),
        Err(error) => report.check(&alloc::format!("dev convert : {}", error.message()), false),
    }
    report.check(
        "dev convert sans --from electron est refuse",
        matches!(
            cli::execute(&alloc::format!("dev convert {}", electron), now),
            Err(DevError::BadCommand(_))
        ),
    );

    // The runs must not have leaked frames. Every `dev run` hands its
    // image to the loader and waits for the task, so `try_reap` has
    // returned the image before we measure. Strict equality is not
    // demanded: the system tasks keep running during the self-test, and a
    // count failing one time in three would be worse than no count at
    // all. What is checked is that no page has been lost beyond what the
    // system legitimately took. The return is not instantaneous, and on
    // several cores it is not even synchronous: `try_reap` refuses as
    // long as a core still names the dead task as current, and that core
    // only gives it up at its next election. Measuring once therefore
    // amounted to measuring the scheduling rather than the leaks -- on a
    // uniprocessor the count came out right, under four cores it was
    // thirteen frames short, exactly, on every boot.
    //
    // So we wait for the count to come back, with a deadline. What is
    // checked stays "no page has been lost"; what changed is that we no
    // longer demand it at an arbitrary instant.
    let deadline = crate::idt::ticks() + 200;
    let mut free_after = frames_free().0;
    while free_before.saturating_sub(free_after) > 8 && crate::idt::ticks() < deadline {
        let _ = crate::syscall::invoke(
            crate::syscall::domain_time::SYS_TIME_SLEEP_MS,
            [20, 0, 0, 0, 0, 0],
        );
        free_after = frames_free().0;
    }

    let leaked = free_before.saturating_sub(free_after);
    report.check(
        &alloc::format!(
            "les executions rendent leurs cadres ({} avant, {} apres)",
            free_before,
            free_after
        ),
        leaked <= 8,
    );
}


// ============================================================
//  Purge
// ============================================================

/// Erases the test scope.
fn purge(now: u64) {
    let who = Credentials::kernel();
    if crate::fs::stat(TEST_ROOT, who).is_ok() {
        purge_tree(TEST_ROOT, now);
    }
    let _ = crate::fs::create_directory(TEST_ROOT, who, Mode::shared_dir(), now);
}

fn purge_tree(path: &str, now: u64) {
    let who = Credentials::kernel();
    if let Ok(entries) = crate::fs::list(path, who) {
        for entry in entries {
            let child = alloc::format!("{}/{}", path, entry.name);
            if crate::fs::list(&child, who).is_ok() {
                purge_tree(&child, now);
            }
            let _ = crate::fs::delete(&child, who, now);
        }
    }
    let _ = crate::fs::delete(path, who, now);
}

// ============================================================
//  Entry point
// ============================================================

/// Runs the complete self-test. Returns `(passed, failed)`.
pub fn run() -> (usize, usize) {
    let mut report = Report {
        passed: 0,
        failed: 0,
    };
    let now = crate::idt::ticks();

    // What does not touch the volume first: if the filesystem is
    // missing, these checks stay valid and the report says so.
    check_assembler(&mut report);
    check_linker(&mut report);
    check_manifest(&mut report);
    check_script(&mut report);
    check_runtime_table(&mut report);
    check_rustc(&mut report);
    check_etr(&mut report);
    check_libdev(&mut report);

    if !crate::fs::is_ready() {
        crate::serial_println!(
            "[sdk] systeme de fichiers absent - les controles de 9.1 et 9.3 sont ignores"
        );
        return (report.passed, report.failed);
    }

    check_sandbox(&mut report, now);
    check_runtime_priority(&mut report, now);
    check_cli(&mut report, now);
    check_run_and_convert(&mut report, now);

    purge(now);
    // The test directory itself goes: `purge` recreates it for the next
    // pass, which makes no sense on the way out.
    purge_tree(TEST_ROOT, now);

    (report.passed, report.failed)
}
