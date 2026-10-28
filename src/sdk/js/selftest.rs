//! Self-test of 16.3: real programs on the real volume, through the task
//! and the rights `dev interpret` uses.
//!
//! The expected lines are what Node 22 prints for the same files (checked
//! on the host): a difference is a bug of Nova's, not a matter of taste.
//! Three programs: the ordinary one (local modules, JSON, cache, `fs`,
//! `path`, `events`, `argv`, microtasks, a timer and `process.exit`), the
//! corner cases (a cycle of requires, `require.main`, a module that fails
//! twice, `fs` error codes and directories, `stdout.write` in pieces,
//! `setInterval`, `await`, `nextTick`, `process.exitCode`), and the
//! uncaught error. Then three more for 16.3.2: an ES module importing
//! CommonJS, JSON and built-in modules (checked against Node), and
//! TypeScript as an ES module and as CommonJS (checked against Bun, the
//! runtime the 13.2 row names). Then `dev interpret` itself, through the
//! command line, for a `.js` and a `.ts`.
//!
//! `nextTick` comes before the `async` function in the second program:
//! `await` waits in place in this engine (16.1), so code written after a
//! call that awaits runs after the wait, where Node runs it before. The
//! test keeps to what both do alike; the difference is an open limit.

use alloc::string::String;
use alloc::vec::Vec;

use crate::fs::perm::{Credentials, Mode};

const TEST_ROOT: &str = "/cache/js-autotest";

/// The files of the test, relative to `TEST_ROOT`.
const FILES: &[(&str, &str)] = &[
    ("un/lib/maths.js", "const { carre } = require('./aide');
exports.somme = (a, b) => a + b;
exports.carre = carre;
global.chargements = (global.chargements || 0) + 1;
"),
    ("un/lib/aide.js", "module.exports = { carre: n => n * n };\n"),
    ("un/donnees.json", "{\"nom\": \"Nova\", \"valeurs\": [1, 2, 3]}\n"),
    ("un/main.js", "#!/usr/bin/env node
const path = require('path');
const fs = require('fs');
const EventEmitter = require('events');
const maths = require('./lib/maths');
const encore = require('./lib/maths.js');
const donnees = require('./donnees.json');
console.log('somme', maths.somme(2, 3), 'carre', maths.carre(7));
console.log('cache', maths === encore, global.chargements);
console.log('json', donnees.nom, donnees.valeurs.length);
console.log('chemin', path.join('/a', 'b', '../c'), path.basename('/x/y.txt', '.txt'), path.extname('a.tar.gz'));
fs.writeFileSync(path.join(__dirname, 'sortie.txt'), 'ligne 1\\n');
fs.appendFileSync(path.join(__dirname, 'sortie.txt'), 'ligne 2\\n');
console.log('fichier', fs.readFileSync(path.join(__dirname, 'sortie.txt'), 'utf8').split('\\n').length, fs.existsSync(path.join(__dirname, 'absent.txt')));
const e = new EventEmitter();
e.on('ping', v => console.log('evenement', v));
e.emit('ping', 42);
console.log('argv', process.argv.slice(2).join(','));
setTimeout(() => { console.log('minuterie'); process.exit(3); }, 100);
Promise.resolve().then(() => console.log('micro'));
try { require('express'); } catch (err) { console.log('npm', err.message.startsWith(\"Cannot find module 'express'\")); }
"),
    ("deux/a.js", "exports.debut = 1;
const b = require('./b');
exports.fin = 2;
console.log('a voit', b.vu);
"),
    ("deux/b.js", "const a = require('./a');\nexports.vu = JSON.stringify(a);\n"),
    ("deux/casse.js", "throw new Error('casse ' + (global.essais = (global.essais || 0) + 1));\n"),
    ("deux/main.js", "const fs = require('fs');
const path = require('path');
const a = require('./a');
console.log('principal', require.main === module, module.id, a.fin);
try { require('./casse'); } catch (e) { console.log('casse 1', e.message); }
try { require('./casse'); } catch (e) { console.log('casse 2', e.message); }
try { fs.readFileSync(path.join(__dirname, 'absent.txt')); } catch (e) { console.log('erreur', e.code); }
fs.mkdirSync(path.join(__dirname, 'sous'));
fs.writeFileSync(path.join(__dirname, 'sous', 'f.txt'), 'abc');
console.log('liste', fs.readdirSync(path.join(__dirname, 'sous')).join(','), fs.statSync(path.join(__dirname, 'sous')).isDirectory(), fs.statSync(path.join(__dirname, 'sous', 'f.txt')).size);
fs.unlinkSync(path.join(__dirname, 'sous', 'f.txt'));
fs.rmdirSync(path.join(__dirname, 'sous'));
console.log('efface', fs.existsSync(path.join(__dirname, 'sous')));
process.stdout.write('sans ');
process.stdout.write('retour\\n');
let n = 0;
const id = setInterval(() => { n++; if (n === 3) { clearInterval(id); console.log('intervalle', n); } }, 10);
process.nextTick(() => console.log('tick'));
const pause = ms => new Promise(r => setTimeout(r, ms));
(async () => { await pause(50); console.log('attendu'); process.exitCode = 4; process.stdout.write('fin'); })();
"),
    ("trois/main.js", "console.log('avant');
setTimeout(() => console.log('jamais'), 10);
null.x;
"),
    ("bonjour.js", "console.log('bonjour', process.argv.length - 2);\nprocess.exitCode = 2;\n"),
    // 16.3.2: an ES module with CommonJS, JSON and built-in imports
    // (Node), TypeScript as an ES module and as CommonJS (Bun).
    ("quatre/d.json", "{\"v\": 7}\n"),
    ("quatre/esp.mjs", "export const double = n => n * 2;\n"),
    ("quatre/main.mjs", "import fs from 'fs';\nimport { join } from 'node:path';\nimport vieux, { nomme } from './vieux.cjs';\nimport * as esp from './esp.mjs';\nimport donnees from './d.json' with { type: 'json' };\nconsole.log(typeof fs.readFileSync, join('/a', 'b'), vieux.autre, nomme, esp.double(4), donnees.v);\nconsole.log(typeof require, typeof module, import.meta.filename.endsWith('/quatre/main.mjs'));\nconst r = await import('./vieux.cjs');\nconsole.log(r.default === vieux, r.nomme);\n"),
    ("quatre/vieux.cjs", "exports.nomme = 'n';\nexports.autre = 2;\n"),
    ("cinq/main.ts", "import { readFileSync, writeFileSync } from 'fs';\nimport path from 'path';\nimport { Pile } from './pile';\nconst outil = require('./outil.cjs');\nconst p = new Pile<number>().pousse(1).pousse(2);\nwriteFileSync(path.join(__dirname, 'f.txt'), 'abc');\nconsole.log(p.taille, outil.nom, readFileSync(path.join(__dirname, 'f.txt'), 'utf8'), typeof __filename);\n"),
    ("cinq/outil.cjs", "exports.nom = 'outil';\n"),
    ("cinq/pile.ts", "export class Pile<T> {\n  private items: T[] = [];\n  pousse(v: T): this { this.items.push(v); return this; }\n  get taille(): number { return this.items.length; }\n}\n"),
    ("six/main.ts", "const fs = require('fs');\ninterface A { x: number }\nconst a: A = { x: 1 };\nenum E { Un = 1 }\nconsole.log(a.x, typeof module, typeof fs.existsSync, E.Un, require.main === module);\n"),
    ("bonjour.ts", "const n: number = process.argv.length - 2;\nconsole.log('bonjour', n);\n"),
];

/// What the three programs must print, and exit with.
const EXPECTED: &[(&str, &[&str], &[&str], i64, usize)] = &[
    (
        "un/main.js",
        &["un", "deux"],
        &[
            "somme 5 carre 49",
            "cache true 1",
            "json Nova 3",
            "chemin /a/c y .gz",
            "fichier 3 false",
            "evenement 42",
            "argv un,deux",
            "npm true",
            "micro",
            "minuterie",
        ],
        3,
        4,
    ),
    (
        "deux/main.js",
        &[],
        &[
            "a voit {\"debut\":1}",
            "principal true . 2",
            "casse 1 casse 1",
            "casse 2 casse 2",
            "erreur ENOENT",
            "liste f.txt true 3",
            "efface false",
            "sans retour",
            "tick",
            "intervalle 3",
            "attendu",
            "fin",
        ],
        4,
        3,
    ),
    (
        "quatre/main.mjs",
        &[],
        &["function /a/b 2 n 8 7", "undefined undefined true", "true n"],
        0,
        4,
    ),
    ("cinq/main.ts", &[], &["2 outil abc string"], 0, 3),
    ("six/main.ts", &[], &["1 object function 1 true"], 0, 1),
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
            crate::serial_println!("[js-node] ECHEC : {}", what);
        }
    }
}

fn purge_tree(path: &str, now: u64) {
    let who = Credentials::kernel();
    if let Ok(entries) = crate::fs::list(path, who) {
        for entry in entries {
            let child = alloc::format!("{}/{}", path, entry.name);
            purge_tree(&child, now);
        }
    }
    let _ = crate::fs::delete(path, who, now);
}

/// Writes the files of the test, readable by every account (the command
/// line runs with the session's rights, whoever is logged in).
fn install(now: u64) -> bool {
    let who = Credentials::kernel();
    purge_tree(TEST_ROOT, now);
    let mut ok = crate::fs::create_directory(TEST_ROOT, who, Mode::system_dir(), now).is_ok();
    for (path, text) in FILES {
        let full = alloc::format!("{}/{}", TEST_ROOT, path);
        let mut directory = String::new();
        let parts: Vec<&str> = path.split('/').collect();
        for part in &parts[..parts.len() - 1] {
            directory.push('/');
            directory.push_str(part);
            let _ = crate::fs::create_directory(&alloc::format!("{}{}", TEST_ROOT, directory), who, Mode::system_dir(), now);
        }
        ok &= crate::fs::write_file(&full, text.as_bytes(), who, Mode::system_file(), now).is_ok();
    }
    ok
}

/// Runs the self-test. Returns `(passed, failed)`.
pub fn run() -> (usize, usize) {
    let mut report = Report { passed: 0, failed: 0 };
    let now = crate::idt::ticks();
    if !install(now) {
        report.check(false, "ecriture des fichiers de test");
        return (report.passed, report.failed);
    }
    let kernel = Credentials::kernel();

    for (path, arguments, lines, code, modules) in EXPECTED {
        let full = alloc::format!("{}/{}", TEST_ROOT, path);
        let arguments: Vec<String> = arguments.iter().map(|a| String::from(*a)).collect();
        match super::run_file(&full, &arguments, kernel) {
            Ok(run) => {
                let same = run.output.len() == lines.len() && run.output.iter().zip(lines.iter()).all(|(a, b)| a == b);
                if !same {
                    for line in &run.output {
                        crate::serial_println!("[js-node]   {}", line);
                    }
                }
                report.check(same, &alloc::format!("{} : sortie", path));
                report.check(run.exit_code == *code, &alloc::format!("{} : code {} au lieu de {}", path, run.exit_code, code));
                report.check(run.modules == *modules, &alloc::format!("{} : {} modules au lieu de {}", path, run.modules, modules));
                report.check(run.error.is_none(), &alloc::format!("{} : arret {:?}", path, run.error));
            }
            Err(error) => report.check(false, &alloc::format!("{} : {}", path, error)),
        }
    }

    // The file the first program wrote, as `fs` wrote it.
    let written = crate::fs::read_file(&alloc::format!("{}/un/sortie.txt", TEST_ROOT), kernel);
    report.check(written.as_deref() == Ok(b"ligne 1\nligne 2\n".as_slice()), "un/sortie.txt sur le volume");

    // The uncaught error: the line before it, the error named, code 1,
    // and the timer it armed never runs.
    match super::run_file(&alloc::format!("{}/trois/main.js", TEST_ROOT), &[], kernel) {
        Ok(run) => {
            report.check(run.output.first().map(String::as_str) == Some("avant"), "trois : ligne d'avant");
            let named = run
                .output
                .get(1)
                .is_some_and(|line| line.starts_with("[erreur] exception non rattrapee : TypeError"));
            report.check(named, "trois : erreur nommee");
            report.check(run.output.len() == 2, "trois : la minuterie n'a pas tourne");
            report.check(run.exit_code == 1, "trois : code 1");
        }
        Err(error) => report.check(false, &alloc::format!("trois : {}", error)),
    }

    // `dev interpret` itself: the extension routes to the engine, the
    // arguments follow the file, the code is shown.
    let line = alloc::format!("dev interpret {}/bonjour.js a b c", TEST_ROOT);
    match crate::sdk::cli::execute(&line, now) {
        Ok(output) => {
            let lines = output.lines();
            report.check(lines.first().map(String::as_str) == Some("  bonjour 3"), "dev interpret : sortie");
            report.check(
                lines.last().map(String::as_str) == Some("code de sortie : 2 - 1 module(s)"),
                "dev interpret : code de sortie",
            );
        }
        Err(error) => report.check(false, &alloc::format!("dev interpret : {:?}", error)),
    }
    // A `.ts` goes to Bun's row, run by the same engine.
    let line = alloc::format!("dev interpret {}/bonjour.ts x", TEST_ROOT);
    match crate::sdk::cli::execute(&line, now) {
        Ok(output) => {
            let lines = output.lines();
            report.check(lines.first().map(String::as_str) == Some("  bonjour 1"), "dev interpret .ts : sortie");
            report.check(
                lines.last().map(String::as_str) == Some("code de sortie : 0 - 1 module(s)"),
                "dev interpret .ts : code de sortie",
            );
        }
        Err(error) => report.check(false, &alloc::format!("dev interpret .ts : {:?}", error)),
    }
    // The file the TypeScript program wrote.
    let written = crate::fs::read_file(&alloc::format!("{}/cinq/f.txt", TEST_ROOT), kernel);
    report.check(written.as_deref() == Ok(b"abc".as_slice()), "cinq/f.txt sur le volume");

    purge_tree(TEST_ROOT, now);
    (report.passed, report.failed)
}
