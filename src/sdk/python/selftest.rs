//! Self-test of 16.5: Python programs on the real volume, through the
//! task and the rights `dev interpret` uses.
//!
//! The corpus is `tests/python/`: each program with the output CPython
//! 3.13 gave for it (`*.out`, written on the host by
//! `tools/pyhost/reference.sh`), compiled into the kernel. A difference
//! is a bug of Nova's. The host runs the same corpus in a second
//! (`make py-test`); here it runs on the kernel's heap, its file system
//! and its clock. Then a program of several files (a local module, a
//! package, a data file, standard input, an uncaught exception), and
//! `dev interpret` itself through the command line.

use alloc::string::String;
use alloc::vec::Vec;

use crate::fs::perm::{Credentials, Mode};

const TEST_ROOT: &str = "/cache/py-autotest";

/// (name, program, CPython's output, standard input)
const CORPUS: &[(&str, &str, &str, Option<&str>)] = &[
    ("01_bases", include_str!("../../../tests/python/01_bases.py"), include_str!("../../../tests/python/01_bases.out"), None),
    ("02_chaines", include_str!("../../../tests/python/02_chaines.py"), include_str!("../../../tests/python/02_chaines.out"), None),
    ("03_listes", include_str!("../../../tests/python/03_listes.py"), include_str!("../../../tests/python/03_listes.out"), None),
    ("04_dicts_ensembles", include_str!("../../../tests/python/04_dicts_ensembles.py"), include_str!("../../../tests/python/04_dicts_ensembles.out"), None),
    ("05_fonctions", include_str!("../../../tests/python/05_fonctions.py"), include_str!("../../../tests/python/05_fonctions.out"), None),
    ("06_classes", include_str!("../../../tests/python/06_classes.py"), include_str!("../../../tests/python/06_classes.out"), None),
    ("07_exceptions", include_str!("../../../tests/python/07_exceptions.py"), include_str!("../../../tests/python/07_exceptions.out"), None),
    ("08_generateurs", include_str!("../../../tests/python/08_generateurs.py"), include_str!("../../../tests/python/08_generateurs.out"), None),
    ("09_portee", include_str!("../../../tests/python/09_portee.py"), include_str!("../../../tests/python/09_portee.out"), None),
    ("10_entiers", include_str!("../../../tests/python/10_entiers.py"), include_str!("../../../tests/python/10_entiers.out"), None),
    ("11_texte_format", include_str!("../../../tests/python/11_texte_format.py"), include_str!("../../../tests/python/11_texte_format.out"), None),
    ("12_fichiers", include_str!("../../../tests/python/12_fichiers.py"), include_str!("../../../tests/python/12_fichiers.out"), None),
    ("13_erreur_finale", include_str!("../../../tests/python/13_erreur_finale.py"), include_str!("../../../tests/python/13_erreur_finale.out"), None),
    ("14_entree", include_str!("../../../tests/python/14_entree.py"), include_str!("../../../tests/python/14_entree.out"), Some(include_str!("../../../tests/python/14_entree.in"))),
    ("15_algorithmes", include_str!("../../../tests/python/15_algorithmes.py"), include_str!("../../../tests/python/15_algorithmes.out"), None),
    ("16_collections", include_str!("../../../tests/python/16_collections.py"), include_str!("../../../tests/python/16_collections.out"), None),
    ("17_itertools_functools", include_str!("../../../tests/python/17_itertools_functools.py"), include_str!("../../../tests/python/17_itertools_functools.out"), None),
    ("18_dataclasses_enum", include_str!("../../../tests/python/18_dataclasses_enum.py"), include_str!("../../../tests/python/18_dataclasses_enum.out"), None),
    ("19_modules_divers", include_str!("../../../tests/python/19_modules_divers.py"), include_str!("../../../tests/python/19_modules_divers.out"), None),
    ("20_divers", include_str!("../../../tests/python/20_divers.py"), include_str!("../../../tests/python/20_divers.out"), None),
    ("21_match", include_str!("../../../tests/python/21_match.py"), include_str!("../../../tests/python/21_match.out"), None),
    ("22_asyncio", include_str!("../../../tests/python/22_asyncio.py"), include_str!("../../../tests/python/22_asyncio.out"), None),
    ("23_complexes", include_str!("../../../tests/python/23_complexes.py"), include_str!("../../../tests/python/23_complexes.out"), None),
    ("24_octets", include_str!("../../../tests/python/24_octets.py"), include_str!("../../../tests/python/24_octets.out"), None),
    ("25_metaclasses", include_str!("../../../tests/python/25_metaclasses.py"), include_str!("../../../tests/python/25_metaclasses.out"), None),
];

/// A program of several files, relative to `TEST_ROOT`.
const FILES: &[(&str, &str)] = &[
    ("projet/outils.py", "PI_APPROCHE = 355 / 113\n\ndef aire(r):\n    return PI_APPROCHE * r * r\n"),
    ("projet/formes/__init__.py", "from .carre import Carre\nNOM = 'formes'\n"),
    ("projet/formes/carre.py", "class Carre:\n    def __init__(self, c):\n        self.c = c\n    def aire(self):\n        return self.c ** 2\n"),
    ("projet/donnees.txt", "3\n4\n5\n"),
    ("projet/main.py", "import sys\nimport outils\nfrom formes import Carre, NOM\nimport formes.carre as fc\nprint('aire', round(outils.aire(2), 4), Carre(3).aire(), NOM, fc.Carre is Carre)\nwith open('donnees.txt') as f:\n    nombres = [int(l) for l in f]\nprint('somme', sum(nombres), 'args', sys.argv[1:])\nwith open('sortie.txt', 'w') as f:\n    f.write('ecrit par python\\n')\nnom = input()\nprint('bonjour', nom)\ndef echoue(x):\n    return 10 // x\nechoue(0)\n"),
    ("bonjour.py", "import sys\nprint('bonjour', len(sys.argv) - 1)\nsys.exit(2)\n"),
];

struct Report {
    passed: usize,
    failed: usize,
}

impl Report {
    fn check(&mut self, ok: bool, what: &str) {
        if ok {
            self.passed += 1;
        } else {
            self.failed += 1;
            crate::serial_println!("[python] ECHEC : {}", what);
        }
    }
}

fn purge_tree(path: &str, now: u64) {
    let who = Credentials::kernel();
    if let Ok(entries) = crate::fs::list(path, who) {
        for entry in entries {
            if entry.name == "." || entry.name == ".." {
                continue;
            }
            let child = alloc::format!("{}/{}", path, entry.name);
            purge_tree(&child, now);
        }
    }
    let _ = crate::fs::delete(path, who, now);
}

fn write(path: &str, text: &str, now: u64) -> bool {
    let who = Credentials::kernel();
    let full = alloc::format!("{}/{}", TEST_ROOT, path);
    let mut directory = String::new();
    let parts: Vec<&str> = path.split('/').collect();
    for part in &parts[..parts.len() - 1] {
        directory.push('/');
        directory.push_str(part);
        let _ = crate::fs::create_directory(&alloc::format!("{}{}", TEST_ROOT, directory), who, Mode::system_dir(), now);
    }
    crate::fs::write_file(&full, text.as_bytes(), who, Mode::system_file(), now).is_ok()
}

/// The output of a run as the reference writes it: stdout, then
/// "--- exit N" and the last line of stderr when the program failed.
fn as_reference(run: &super::PyRun) -> Vec<String> {
    let mut lines = run.output.clone();
    if run.exit_code != 0 || !run.errors.is_empty() {
        lines.push(alloc::format!("--- exit {}", run.exit_code));
        if let Some(last) = run.errors.iter().rev().find(|l| !l.trim().is_empty()) {
            lines.push(last.clone());
        }
    }
    lines
}

/// Runs the self-test: (programs of the corpus conforming, checks
/// passed, checks failed).
pub fn run() -> (usize, usize, usize) {
    let mut report = Report { passed: 0, failed: 0 };
    let now = crate::idt::ticks();
    let kernel = Credentials::kernel();
    purge_tree(TEST_ROOT, now);
    let created = crate::fs::create_directory(TEST_ROOT, kernel, Mode::system_dir(), now).is_ok();
    report.check(created, "repertoire de test");
    let mut conforming = 0;
    for (name, program, expected, input) in CORPUS {
        let path = alloc::format!("{}.py", name);
        if !write(&path, program, now) {
            report.check(false, &alloc::format!("{} : ecriture", name));
            continue;
        }
        let input: Vec<String> = input.map(|i| i.lines().map(|l| alloc::format!("{}\n", l)).collect()).unwrap_or_default();
        let started = crate::idt::ticks();
        match super::run_file(&alloc::format!("{}/{}", TEST_ROOT, path), &[], input, kernel) {
            Ok(run) => {
                let got = as_reference(&run);
                let want: Vec<&str> = expected.lines().collect();
                let same = got.len() == want.len() && got.iter().zip(want.iter()).all(|(a, b)| a == b);
                if same {
                    conforming += 1;
                } else {
                    for (i, w) in want.iter().enumerate() {
                        let g = got.get(i).map(String::as_str).unwrap_or("<fin>");
                        if g != *w {
                            crate::serial_println!("[python] {} ligne {} : attendu {}", name, i + 1, w);
                            crate::serial_println!("[python] {} ligne {} : obtenu  {}", name, i + 1, g);
                            break;
                        }
                    }
                    for line in run.errors.iter().take(8) {
                        crate::serial_println!("[python]   | {}", line);
                    }
                }
                let ms = (crate::idt::ticks() - started) * 1000 / crate::TIMER_HZ as u64;
                crate::serial_println!("[python] {} : {} ({} ms)", name, if same { "conforme" } else { "DIFFERENT" }, ms);
                report.check(same, &alloc::format!("{} : sortie conforme a CPython", name));
                report.check(run.halted.is_none(), &alloc::format!("{} : arret {:?}", name, run.halted));
            }
            Err(error) => report.check(false, &alloc::format!("{} : {}", name, error)),
        }
    }

    // The program of several files.
    let mut ok = true;
    for (path, text) in FILES {
        ok &= write(path, text, now);
    }
    report.check(ok, "ecriture du projet");
    let main = alloc::format!("{}/projet/main.py", TEST_ROOT);
    let arguments = [String::from("x"), String::from("y")];
    match super::run_file(&main, &arguments, alloc::vec![String::from("Nova\n")], kernel) {
        Ok(run) => {
            let expected = [
                "aire 12.5664 9 formes True",
                "somme 12 args ['x', 'y']",
                "bonjour Nova",
            ];
            let same = run.output.len() == expected.len() && run.output.iter().zip(expected.iter()).all(|(a, b)| a == b);
            if !same {
                for line in run.output.iter().chain(run.errors.iter()) {
                    crate::serial_println!("[python]   {}", line);
                }
            }
            report.check(same, "projet : sortie");
            report.check(run.exit_code == 1, "projet : code 1 apres l'exception");
            report.check(run.errors.first().map(String::as_str) == Some("Traceback (most recent call last):"), "projet : trace");
            report.check(run.errors.last().map(String::as_str) == Some("ZeroDivisionError: integer division or modulo by zero"), "projet : exception nommee");
            report.check(run.errors.iter().any(|l| l.trim() == "return 10 // x"), "projet : ligne source dans la trace");
        }
        Err(error) => report.check(false, &alloc::format!("projet : {}", error)),
    }
    let written = crate::fs::read_file(&alloc::format!("{}/projet/sortie.txt", TEST_ROOT), kernel);
    report.check(written.as_deref() == Ok(b"ecrit par python\n".as_slice()), "projet/sortie.txt sur le volume");

    // `dev interpret` itself: the extension routes to the interpreter.
    let line = alloc::format!("dev interpret {}/bonjour.py a b c", TEST_ROOT);
    match crate::sdk::cli::execute(&line, now) {
        Ok(output) => {
            let lines = output.lines();
            report.check(lines.first().map(String::as_str) == Some("  bonjour 3"), "dev interpret .py : sortie");
            report.check(lines.last().map(String::as_str) == Some("code de sortie : 2"), "dev interpret .py : code de sortie");
        }
        Err(error) => report.check(false, &alloc::format!("dev interpret .py : {:?}", error)),
    }

    purge_tree(TEST_ROOT, now);
    (conforming, report.passed, report.failed)
}
