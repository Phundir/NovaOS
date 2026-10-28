//! Boot self-test of the shell (16.0.1) and of the terminal (16.0.2).
//!
//! The shell is a library, so it is tested as one: lines run through
//! `Shell::run`, answers compared. The cases are the ones that would lie
//! silently -- a relative path resolved against the wrong directory, a
//! quoted word cut in two, a command that "succeeds" on a file the
//! account may not touch. The terminal's editing logic (history, cursor,
//! Ctrl+L) is driven by synthetic key events, without a window: what the
//! compositor adds is drawing, which phase 7 already tests.
//!
//! Everything happens under `/cache/shell-autotest`, removed first: the
//! second boot on the same disk must find the same answers.

use alloc::string::String;

use super::{Shell, normalize, split};
use crate::fs::perm::{Credentials, Role, USERS_GID};

const ROOT: &str = "/cache/shell-autotest";

struct Checks {
    passed: usize,
    failed: usize,
}

impl Checks {
    fn check(&mut self, ok: bool, what: &str) {
        if ok {
            self.passed += 1;
        } else {
            self.failed += 1;
            crate::serial_println!("[shell] ECHEC : {}", what);
        }
    }
}

fn joined(lines: &[String]) -> String {
    lines.join("\n")
}

/// Runs the cases. Returns (passed, failed).
pub fn run() -> (usize, usize) {
    let mut checks = Checks { passed: 0, failed: 0 };
    words(&mut checks);
    paths(&mut checks);
    commands(&mut checks);
    rights(&mut checks);
    match crate::apps::terminal::exercise() {
        Ok(count) => checks.passed += count,
        Err(reason) => checks.check(false, &alloc::format!("terminal : {}", reason)),
    }
    window(&mut checks);
    let mut root = Shell::new("root", Credentials::kernel());
    let _ = root.run(&alloc::format!("supprimer -r {}", ROOT));
    (checks.passed, checks.failed)
}

fn words(checks: &mut Checks) {
    let cut = split("ecrire  \"a b\" 'c d'e f").unwrap_or_default();
    checks.check(cut == ["ecrire", "a b", "c de", "f"], "mots et guillemets");
    checks.check(split("echo \"ouvert").is_err(), "guillemet non ferme refuse");
    checks.check(split("echo \"\"").map(|w| w.len()) == Ok(2), "mot vide entre guillemets garde");
}

fn paths(checks: &mut Checks) {
    checks.check(normalize("/a/./b/../c//d/") == "/a/c/d", "normalisation");
    checks.check(normalize("/../..") == "/", "jamais au-dessus de /");
    let mut shell = Shell::new("root", Credentials::kernel());
    shell.cwd = String::from("/users");
    checks.check(shell.resolve("x/../y") == "/users/y", "chemin relatif");
    checks.check(shell.resolve("/sys") == "/sys", "chemin absolu");
    checks.check(shell.resolve("B:/a/../b") == "B:/b", "lettre de disque");
}

fn commands(checks: &mut Checks) {
    let mut shell = Shell::new("root", Credentials::kernel());
    let _ = shell.run(&alloc::format!("supprimer -r {}", ROOT));

    let made = shell.run(&alloc::format!("dossier {}/a/b", ROOT));
    checks.check(!made.failed, "dossier et ses parents");
    let moved = shell.run(&alloc::format!("va {}", ROOT));
    checks.check(!moved.failed && shell.cwd == ROOT, "va");
    checks.check(joined(&shell.run("ici").lines) == ROOT, "ici");

    let written = shell.run("ecrire note.txt \"bonjour   le monde\" !");
    checks.check(!written.failed, "ecrire relatif");
    checks.check(joined(&shell.run("lire note.txt").lines) == "bonjour   le monde !", "lire ce qui a ete ecrit");
    checks.check(!shell.run("cp note.txt a/copie.txt").failed, "copier (alias cp)");
    checks.check(!shell.run("deplacer a/copie.txt a/b/deplace.txt").failed, "deplacer");
    let listed = joined(&shell.run("liste a/b").lines);
    checks.check(listed.contains("deplace.txt"), "liste montre le fichier deplace");
    checks.check(!joined(&shell.run("ls a").lines).contains("copie.txt"), "la source deplacee a disparu");
    checks.check(joined(&shell.run("cat a/b/deplace.txt").lines) == "bonjour   le monde !", "contenu deplace intact");

    checks.check(shell.run("supprimer a").failed, "supprimer un dossier plein sans -r refuse");
    checks.check(!shell.run("supprimer -r a").failed, "supprimer -r");
    checks.check(shell.run("lire a/b/deplace.txt").failed, "le dossier supprime a disparu");
    checks.check(shell.run("lire absent.txt").failed, "fichier absent : echec");

    checks.check(!shell.run("va ..").failed && shell.cwd == "/cache", "va ..");
    checks.check(shell.run("va /cache/shell-autotest/note.txt").failed, "va sur un fichier refuse");
    checks.check(shell.cwd == "/cache", "va refuse ne bouge pas");

    let unknown = shell.run("commande-qui-n-existe-pas");
    checks.check(unknown.failed && joined(&unknown.lines).contains("inconnue"), "commande inconnue");
    checks.check(joined(&shell.run("aide ls").lines).starts_with("liste"), "aide d'un alias");
    checks.check(joined(&shell.run("echo a   b").lines) == "a b", "echo");
    checks.check(shell.run("effacer").clear, "effacer");
    checks.check(shell.run("quitter").exit, "quitter");
    let version = joined(&shell.run("systeme").lines);
    checks.check(version.contains(crate::version::VERSION), "systeme donne la version");
    checks.check(!shell.run("taches").lines.is_empty(), "taches");

    let before = shell.history.len();
    let _ = shell.run("echo une");
    let _ = shell.run("echo une");
    checks.check(shell.history.len() == before + 1, "historique sans doublon consecutif");
}

fn sleep_ms(ms: u64) {
    let _ = crate::syscall::invoke(crate::syscall::domain_time::SYS_TIME_SLEEP_MS, [ms, 0, 0, 0, 0, 0]);
}

/// Waits up to `ms` for `condition`, by steps of 20 ms.
fn wait_for(ms: u64, mut condition: impl FnMut() -> bool) -> bool {
    for _ in 0..ms / 20 {
        if condition() {
            return true;
        }
        sleep_ms(20);
    }
    condition()
}

/// The real window: launched as the menu launches it, it must get its
/// surface, name it after its directory (which happens on the lap that
/// paints first), and close on the stop signal, giving the surface back.
/// Skipped without a compositor (`-vga none`), where no window can exist.
fn window(checks: &mut Checks) {
    use crate::apps::terminal;
    if !crate::gui::is_active() {
        return;
    }
    let Some(pid) = crate::apps::launch(terminal::FILE_NAME) else {
        checks.check(false, "terminal.ss ne se lance pas");
        return;
    };
    checks.check(wait_for(10_000, || crate::gui::surface_titled("Terminal - ")), "fenetre du terminal ouverte et titree");
    sleep_ms(200);
    let stop = crate::ipc::Message::new(
        crate::scheduler::current_pid(),
        crate::ipc::MessageKind::Signal,
        crate::gui::TAG_MODULE_STOP,
        &[],
    );
    checks.check(crate::ipc::send_unchecked(pid, stop).is_ok(), "signal d'arret remis");
    let ended = wait_for(10_000, || {
        crate::scheduler::task_info(pid).is_none_or(|task| task.state == crate::process::State::Dead)
    });
    checks.check(ended, "le terminal se termine sur le signal d'arret");
    checks.check(!crate::gui::surface_titled("Terminal"), "surface du terminal rendue");
}

/// The shell grants nothing: an ordinary account is refused what the
/// filesystem refuses it.
fn rights(checks: &mut Checks) {
    let mut user = Shell::new("autotest", Credentials::new(4242, USERS_GID, Role::User));
    checks.check(user.run("ecrire /sys/config/shell-autotest.cfg x").failed, "compte ordinaire : /sys en ecriture refuse");
    checks.check(crate::fs::stat("/sys/config/shell-autotest.cfg", Credentials::kernel()).is_err(), "rien n'a ete ecrit");
    let mut root = Shell::new("root", Credentials::kernel());
    let _ = root.run(&alloc::format!("dossier {}", ROOT));
    let _ = root.run(&alloc::format!("ecrire {}/prive.txt secret", ROOT));
    let refused = user.run(&alloc::format!("supprimer {}/prive.txt", ROOT));
    checks.check(refused.failed, "compte ordinaire : fichier de root non supprime");
    checks.check(
        joined(&root.run(&alloc::format!("lire {}/prive.txt", ROOT)).lines) == "secret",
        "le fichier de root est intact",
    );
}
