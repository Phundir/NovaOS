//! Self-test of 16.4: a debugging session typed into a shell, as a user
//! would type it, on a program compiled and run in ring 3.
//!
//! The program is Nova Rust: `somme(3)` adds the squares `carre(3)`,
//! `carre(2)`, `carre(1)` and exits with the total, 14. The session stops
//! it on its entry, puts a breakpoint on `carre`, looks at the registers
//! (the argument 3 in `rdi`), the call stack (`carre`, `somme`, `main`),
//! the memory, **changes `rdi` to 10**, steps, removes the breakpoint and
//! lets the program finish: it must exit with 100 + 4 + 1 = 105, a number
//! only the changed register explains. A second session checks that the
//! debugger's own `int3` is hidden from `memoire`, that `pas 3` counts,
//! and that `quitter` ends the program. Last, a hand-written `int3` in an
//! assembly program: under the debugger it is a stop the program goes on
//! from; without it, the program dies of it as before (code -131).

use alloc::string::String;
use alloc::vec::Vec;

use crate::fs::perm::{Credentials, Mode};
use crate::sdk::manifest;
use crate::sdk::runtime::Language;
use crate::sdk::template::{self, Template};
use crate::shell::Shell;

const TEST_ROOT: &str = "/cache/debug-autotest";

const RUST_SOURCE: &str = "fn carre(x: i64) -> i64 {
    x * x
}

fn somme(n: i64) -> i64 {
    if n == 0 { 0 } else { carre(n) + somme(n - 1) }
}

fn main() {
    let total = somme(3);
    exit(total);
}
";

const ASM_SOURCE: &str = "; un int3 ecrit a la main
.text
_start:
    int3
    mov rax, 0x020B
    mov rdi, 7
    syscall
    hlt
";

struct Report {
    passed: usize,
    failed: usize,
}

impl Report {
    fn check(&mut self, ok: bool, what: &str, lines: &[String]) {
        if ok {
            self.passed += 1;
        } else {
            self.failed += 1;
            crate::serial_println!("[ndb] ECHEC : {}", what);
            for line in lines {
                crate::serial_println!("[ndb]   {}", line);
            }
        }
    }
}

fn any(lines: &[String], needle: &str) -> bool {
    lines.iter().any(|line| line.contains(needle))
}

fn write_project(name: &str, language: Language, source: &str, now: u64) -> Option<String> {
    let who = Credentials::kernel();
    let project = alloc::format!("{}/{}", TEST_ROOT, name);
    let _ = crate::fs::create_directory(&project, who, Mode::private_dir(), now);
    let _ = crate::fs::create_directory(&alloc::format!("{}/src", project), who, Mode::private_dir(), now);
    let entry = template::entry_name(Template::CliTool, language);
    let manifest = template::manifest_for(Template::CliTool, &alloc::format!("test.ndb.{}", name), "autotest", language);
    let ok = crate::fs::write_file(&alloc::format!("{}/{}", project, entry), source.as_bytes(), who, Mode::private_file(), now).is_ok()
        && crate::fs::write_file(
            &alloc::format!("{}/{}", project, manifest::FILE_NAME),
            manifest::to_toml(&manifest).as_bytes(),
            who,
            Mode::private_file(),
            now,
        )
        .is_ok();
    ok.then_some(project)
}

fn purge(path: &str, now: u64) {
    let who = Credentials::kernel();
    if let Ok(entries) = crate::fs::list(path, who) {
        for entry in entries {
            purge(&alloc::format!("{}/{}", path, entry.name), now);
        }
    }
    let _ = crate::fs::delete(path, who, now);
}

/// Runs the self-test. Returns `(passed, failed)`.
pub fn run() -> (usize, usize) {
    let mut report = Report { passed: 0, failed: 0 };
    let now = crate::idt::ticks();
    purge(TEST_ROOT, now);
    let _ = crate::fs::create_directory(TEST_ROOT, Credentials::kernel(), Mode::system_dir(), now);
    let (Some(rust), Some(asm)) = (
        write_project("carres", Language::Rust, RUST_SOURCE, now),
        write_project("piege", Language::Asm, ASM_SOURCE, now),
    ) else {
        report.check(false, "projets d'essai ecrits", &[]);
        return (report.passed, report.failed);
    };

    let mut shell = Shell::new("root", Credentials::kernel());
    let say = |shell: &mut Shell, line: &str| -> Vec<String> { shell.run(line).lines };

    // --- first session: breakpoint, registers, stack, a changed register ---
    let lines = say(&mut shell, &alloc::format!("dev run --debug --projet {}", rust));
    report.check(shell.debugging.is_some() && any(&lines, "arret a l'entree") && any(&lines, "<_start>"), "arret a l'entree", &lines);
    report.check(shell.prompt() == "(ndb) ", "invite du debogueur", &lines);
    let lines = say(&mut shell, "arret carre");
    report.check(any(&lines, "point d'arret 1") && any(&lines, "<carre>"), "arret carre", &lines);
    let lines = say(&mut shell, "continuer");
    report.check(any(&lines, "arret au point 1") && any(&lines, "<carre>"), "continuer jusqu'a carre", &lines);
    let lines = say(&mut shell, "registres");
    report.check(any(&lines, "rdi    0x0000000000000003"), "l'argument 3 dans rdi", &lines);
    let lines = say(&mut shell, "pile");
    let ordered = lines.len() >= 3 && lines[0].contains("<carre>") && lines[1].contains("<somme+") && lines[2].contains("<main+");
    report.check(ordered, "pile : carre, somme, main", &lines);
    let lines = say(&mut shell, "ecrire rdi 10");
    report.check(any(&lines, "rdi = 0xa"), "ecrire rdi 10", &lines);
    let lines = say(&mut shell, "pas");
    report.check(any(&lines, "pas : ") && any(&lines, "<carre+1>"), "un pas", &lines);
    let lines = say(&mut shell, "memoire rsp 16");
    report.check(lines.len() == 1 && lines[0].starts_with("  0x"), "memoire rsp", &lines);
    let lines = say(&mut shell, "effacer 1");
    report.check(any(&lines, "retire"), "effacer 1", &lines);
    let lines = say(&mut shell, "continuer");
    report.check(any(&lines, "code de sortie 105"), "le registre change decide du resultat (105)", &lines);
    report.check(shell.debugging.is_none() && shell.prompt() != "(ndb) ", "fin de session au terme du programme", &lines);

    // --- second session: hidden int3, counted steps, quitting ---
    let lines = say(&mut shell, &alloc::format!("dev run --debug --projet {}", rust));
    let pid = shell.debugging;
    report.check(pid.is_some(), "seconde session", &lines);
    let lines = say(&mut shell, "arret somme");
    report.check(any(&lines, "<somme>"), "arret somme", &lines);
    let lines = say(&mut shell, "memoire somme 4");
    report.check(any(&lines, "55 48 89 e5"), "memoire montre le code, pas l'int3", &lines);
    let lines = say(&mut shell, "pas 3");
    report.check(any(&lines, "pas : "), "pas 3", &lines);
    let lines = say(&mut shell, "symboles");
    report.check(any(&lines, " carre") && any(&lines, " somme") && any(&lines, " main"), "symboles", &lines);
    let lines = say(&mut shell, "inconnue");
    report.check(any(&lines, "commande du debogueur inconnue") && shell.debugging.is_some(), "commande inconnue", &lines);
    let lines = say(&mut shell, "quitter");
    report.check(any(&lines, "termine par le debogueur") && shell.debugging.is_none(), "quitter", &lines);
    if let Some(pid) = pid {
        report.check(!crate::scheduler::exists(pid), "le programme quitte n'existe plus", &lines);
    }

    // --- an int3 of the program's own ---
    let lines = say(&mut shell, &alloc::format!("dev run --debug --projet {}", asm));
    report.check(shell.debugging.is_some(), "session sur le programme assembleur", &lines);
    let lines = say(&mut shell, "continuer");
    report.check(any(&lines, "int3 du programme"), "int3 du programme : un arret", &lines);
    let lines = say(&mut shell, "continuer");
    report.check(any(&lines, "code de sortie 7"), "le programme repart apres son int3", &lines);
    let lines = say(&mut shell, &alloc::format!("dev run --projet {}", asm));
    report.check(any(&lines, "code de sortie -131"), "sans debogueur, int3 termine le programme (#3)", &lines);

    // The sandboxes the sessions kept.
    let lines = say(&mut shell, "dev nettoyer");
    report.check(any(&lines, "3 bac(s)"), "dev nettoyer purge les bacs des trois sessions", &lines);

    purge(TEST_ROOT, now);
    (report.passed, report.failed)
}
