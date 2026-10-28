//! Phase 10.3 self-test (spec 12).
//!
//! ## What is checked, and why these checks
//!
//! * **The order of the decision, not just its branches.** A policy whose
//!   four steps run in the wrong order gives the right answer on most
//!   inputs and the wrong one on exactly the inputs that matter. So the
//!   cases here are built to be **ambiguous unless the order holds**: a
//!   `.ras` that declares `IPC_PUBLIC` (deny-list must beat allow-list),
//!   a `.cus` reaching `/sys/` from a path the sandbox would have opened
//!   (allow-list must beat sandbox).
//! * **The immutability of 12.1.** Registering a second identity on a
//!   live PID must be refused. It is a locked decision of
//!   `DECISIONS.md`, and it was not enforced: the phase 6 registry
//!   overwrote silently.
//! * **Path normalisation.** `..` must not walk out of a sandbox, and a
//!   neighbouring textual prefix must not be mistaken for a parent --
//!   `/apps/mon` does not contain `/apps/monautre`, the same trap phase 8
//!   met on package prefixes.
//! * **The 12.4 table, row by row.** Transcription is verified by
//!   comparison.
//! * **The remembered choice of 12.5.** Written, read back, revoked. A
//!   revocation must restore the "never asked" state, not record a
//!   refusal -- the two are different, and only one shows the prompt
//!   again.
//!
//! ## No prompt is raised
//!
//! Every case goes through `verdict`, which evaluates with no side
//! effect. A self-test that raised a real prompt would block on a machine
//! with nobody in front of it, and would then refuse by expiry -- turning
//! a passing test into a ten-second wait and a wrong answer.
//!
//! ## It leaves nothing behind
//!
//! The four identities it registers are forgotten on the way out, and the
//! remembered-choice file it writes is deleted. PIDs above 90 000 are
//! used so as never to collide with a real task.

use alloc::string::String;
use alloc::vec::Vec;

use crate::binfmt::header::{BinaryFormat, SignatureType};
use crate::binfmt::identity::{AppIdentity, Permissions, Version};
use crate::fs::perm::Credentials;
use crate::process::Pid;
use crate::prompt::Decision;

use super::policy::{self, Context, DenyReason, Resource, Verdict};
use super::sandbox;

/// PIDs of the fictitious applications. Far above anything the scheduler
/// hands out, so nothing real is ever forgotten by the purge.
const PID_EDITOR: Pid = 91_001;
const PID_VIEWER: Pid = 91_002;
const PID_OTHER: Pid = 91_003;
const PID_RAW: Pid = 91_004;
const PID_SYSTEM: Pid = 91_005;

/// Application identifier the memorisation test uses.
const TEST_APP: &str = "test.secu.app";

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
            crate::serial_println!("[secu] ECHEC : {}", label);
        }
    }

    fn denied_for(&mut self, label: &str, verdict: &Verdict, reason: DenyReason) {
        match verdict {
            Verdict::Deny(actual) if *actual == reason => self.passed += 1,
            other => {
                self.failed += 1;
                crate::serial_println!(
                    "[secu] ECHEC : {} - attendu \"{}\", obtenu \"{}\"",
                    label,
                    reason.as_str(),
                    other.as_str()
                );
            }
        }
    }
}

pub fn run() -> (usize, usize) {
    let mut report = Report {
        passed: 0,
        failed: 0,
    };

    purge();
    register_fixtures();

    immutability(&mut report);
    denylist_order(&mut report);
    allowlist(&mut report);
    sandbox_paths(&mut report);
    communication(&mut report);
    table(&mut report);
    memorisation(&mut report);
    enforcement(&mut report);

    forget_fixtures();
    purge();

    (report.passed, report.failed)
}

// ============================================================
//  Fixtures
// ============================================================

fn identity(app_id: &str, group: &str, format: BinaryFormat) -> AppIdentity {
    AppIdentity {
        app_id: String::from(app_id),
        author: String::from("autotest"),
        group: String::from(group),
        display_name: String::from(app_id),
        version: Version::new(1, 0, 0, 0),
        format,
        signature: SignatureType::SelfSigned,
    }
}

fn register_fixtures() {
    // The editor declares nothing: it is the reference for "undeclared".
    crate::apps::register(
        PID_EDITOR,
        identity("com.test.editeur", "com.test", BinaryFormat::Cus),
        Permissions::NONE,
    );
    crate::apps::register(
        PID_VIEWER,
        identity("com.test.visionneuse", "com.test", BinaryFormat::Cus),
        Permissions::NONE,
    );
    crate::apps::register(
        PID_OTHER,
        identity("org.autre.outil", "org.autre", BinaryFormat::Cus),
        Permissions::NONE,
    );
    // The `.ras` declares public IPC. It must change nothing: the
    // deny-list runs first. This is the ambiguous case the order test
    // rests on.
    crate::apps::register(
        PID_RAW,
        identity("com.test.brut", "com.test", BinaryFormat::Ras),
        Permissions(Permissions::IPC_PUBLIC | Permissions::NETWORK_INTERNET),
    );
    crate::apps::register(
        PID_SYSTEM,
        identity("os.nova.explorateur", "os.nova", BinaryFormat::Ss),
        Permissions(Permissions::SYS_DIRECTORY),
    );
}

fn forget_fixtures() {
    for pid in [PID_EDITOR, PID_VIEWER, PID_OTHER, PID_RAW, PID_SYSTEM] {
        crate::apps::forget(pid);
    }
}

fn context(pid: Pid) -> Context {
    Context::of(pid).expect("contexte de l'autotest")
}

// ============================================================
//  10.3.1 -- Immutability (12.1)
// ============================================================

fn immutability(report: &mut Report) {
    let before = crate::apps::identity_of(PID_EDITOR).expect("identite");

    // A different identity on a live PID: 12.1 forbids the change, and
    // `DECISIONS.md` lists it among the points that are not settings.
    crate::apps::register(
        PID_EDITOR,
        identity("com.pirate.autre", "com.pirate", BinaryFormat::Dsa),
        Permissions(Permissions::KNOWN),
    );
    let after = crate::apps::identity_of(PID_EDITOR).expect("identite");

    report.check(
        "12.1 : l'identite d'une tache vivante ne change pas",
        after.app_id == before.app_id
            && after.group == before.group
            && after.format == before.format,
    );
    // And the declaration must not have been swapped either: promoting
    // the permissions while keeping the name would be the same attack
    // with a better disguise.
    report.check(
        "12.1 : la declaration ne change pas non plus",
        crate::apps::permissions_of(PID_EDITOR) == Some(Permissions::NONE),
    );

    // Re-registering the *same* identity is not a mutation and must stay
    // silent: the loader may legitimately replay it.
    crate::apps::register(
        PID_EDITOR,
        identity("com.test.editeur", "com.test", BinaryFormat::Cus),
        Permissions::NONE,
    );
    report.check(
        "12.1 : reenregistrer la meme identite reste accepte",
        crate::apps::identity_of(PID_EDITOR).map(|i| i.app_id) == Some(String::from("com.test.editeur")),
    );

    // A task with no identity is a kernel service, not an application
    // that declares nothing. The two lead to opposite decisions.
    report.check(
        "12.1 : une tache sans identite n'a pas de declaration",
        crate::apps::permissions_of(999_999).is_none(),
    );
    report.check(
        "12.1 : une tache sans identite n'a pas de contexte",
        Context::of(999_999).is_none(),
    );
}

// ============================================================
//  10.3.3 -- The deny-list runs first (12.3)
// ============================================================

fn denylist_order(report: &mut Report) {
    let raw = context(PID_RAW);

    // The `.ras` declares IPC_PUBLIC. If the allow-list ran first it
    // would be allowed. It must be refused, and refused **as a `.ras`**:
    // the reason is what shows which rule spoke.
    report.denied_for(
        "12.3 : la liste noire passe avant la liste blanche",
        &super::verdict(&raw, Resource::IpcTo(PID_EDITOR)),
        DenyReason::RasIsolated,
    );
    report.denied_for(
        "12.3 : un .ras n'atteint pas non plus un port interne",
        &super::verdict(&raw, Resource::PortTo(PID_EDITOR)),
        DenyReason::RasIsolated,
    );
    // Reaching a `.ras` is refused from the other side too: the rule is
    // about the format, not about the direction.
    report.denied_for(
        "12.3 : personne n'atteint un .ras",
        &super::verdict(&context(PID_EDITOR), Resource::IpcTo(PID_RAW)),
        DenyReason::RasIsolated,
    );

    // Foreign memory: no format lifts it, `.dsa` included.
    let system = context(PID_SYSTEM);
    report.denied_for(
        "12.3 : memoire d'un autre processus interdite",
        &super::verdict(&system, Resource::ForeignMemory(PID_EDITOR)),
        DenyReason::ForeignMemory,
    );

    // Another application's binary, named as such and reached by path.
    report.denied_for(
        "12.3 : binaire d'une autre app interdit",
        &super::verdict(&context(PID_EDITOR), Resource::ForeignBinary("com.autre.app")),
        DenyReason::ForeignBinary,
    );
    report.denied_for(
        "12.3 : la meme regle par le chemin",
        &super::verdict(
            &context(PID_EDITOR),
            Resource::FileWrite("/apps/com.autre.app/binaire.cus"),
        ),
        DenyReason::ForeignBinary,
    );
    // Its own directory is not a foreign one.
    report.check(
        "12.3 : une app ecrit dans son propre dossier",
        super::verdict(
            &context(PID_EDITOR),
            Resource::FileWrite("/apps/com.test.editeur/donnees.cfg"),
        )
        .allowed(),
    );
    // A neighbouring textual prefix is not a parent: `/apps/com.test.e`
    // must not stand for `/apps/com.test.editeur`. Phase 8 paid for this
    // on package prefixes.
    report.denied_for(
        "12.3 : un prefixe textuel voisin n'est pas le meme dossier",
        &super::verdict(
            &context(PID_EDITOR),
            Resource::FileWrite("/apps/com.test.editeurautre/x"),
        ),
        DenyReason::ForeignBinary,
    );
}

// ============================================================
//  10.3.3 -- The allow-list (12.3)
// ============================================================

fn allowlist(report: &mut Report) {
    let editor = context(PID_EDITOR);
    let system = context(PID_SYSTEM);

    // Undeclared: refused. Not prompted -- there is nothing a "yes" could
    // write into, since 12.1 fixes the header at compile time.
    report.denied_for(
        "12.3 : permission non declaree refusee",
        &super::verdict(&editor, Resource::Declared(Permissions::MICROPHONE)),
        DenyReason::NotDeclared,
    );
    report.check(
        "12.3 : un refus de declaration ne se demande pas a l'utilisateur",
        !DenyReason::NotDeclared.promptable(),
    );

    // Declared and within the ceiling: allowed.
    report.check(
        "12.3 : permission declaree accordee",
        super::verdict(&system, Resource::Declared(Permissions::SYS_DIRECTORY))
            .allowed(),
    );

    // Above the ceiling of the format: refused even if declared. A `.cus`
    // cannot reach kernel space whatever its header announces (12.4).
    crate::apps::forget(PID_VIEWER);
    crate::apps::register(
        PID_VIEWER,
        identity("com.test.visionneuse", "com.test", BinaryFormat::Cus),
        Permissions(Permissions::KERNEL_SPACE),
    );
    report.denied_for(
        "12.4 : permission au-dessus du plafond du format",
        &super::verdict(&context(PID_VIEWER), Resource::Declared(Permissions::KERNEL_SPACE)),
        DenyReason::AboveCeiling,
    );

    // A `.dsa` is total trust and declares nothing: refusing it for want
    // of a declaration would stop the system applications running. The
    // trust rests on the signature, which `binfmt` requires to be SYSTEM.
    let mut trusted = editor.clone();
    trusted.identity.format = BinaryFormat::Dsa;
    trusted.permissions = Permissions::NONE;
    report.check(
        "12.4 : un .dsa n'a pas besoin de declarer",
        super::verdict(&trusted, Resource::Declared(Permissions::HARDWARE)).allowed(),
    );

    // The allow-list beats the sandbox: `/sys/` is a root of `.ss` *and*
    // a sensitive access. Being in the sandbox is not being entitled.
    let mut undeclared_system = system.clone();
    undeclared_system.permissions = Permissions::NONE;
    report.denied_for(
        "12.3 : la liste blanche passe avant le bac a sable",
        &super::verdict(&undeclared_system, Resource::FileRead("/sys/config/i18n.cfg")),
        DenyReason::NotDeclared,
    );
    report.check(
        "12.3 : le meme chemin passe une fois declare",
        super::verdict(&system, Resource::FileRead("/sys/config/i18n.cfg")).allowed(),
    );
}

// ============================================================
//  10.3.4 -- The path sandbox (12.4)
// ============================================================

fn sandbox_paths(report: &mut Report) {
    let editor = context(PID_EDITOR);
    let raw = context(PID_RAW);

    report.check(
        "12.4 : un .cus voit son propre dossier",
        sandbox::check(&editor, None, "/apps/com.test.editeur/notes.txt").inside(),
    );
    report.check(
        "12.4 : un .cus ne voit pas le dossier d'un autre",
        !sandbox::check(&editor, None, "/apps/org.autre.outil/notes.txt").inside(),
    );
    // The session home is a root of the context, not of the format: it
    // appears only once a session is open.
    report.check(
        "12.4 : le dossier personnel s'ouvre avec la session",
        !sandbox::check(&editor, None, "/users/nova/home/documents/a.txt").inside()
            && sandbox::check(&editor, Some("nova"), "/users/nova/home/documents/a.txt").inside(),
    );
    report.check(
        "12.4 : le dossier personnel d'un autre compte reste ferme",
        !sandbox::check(&editor, Some("nova"), "/users/admin/home/a.txt").inside(),
    );

    // Normalisation: `..` must not walk out.
    report.check(
        "12.4 : un .. ne sort pas du bac a sable",
        !sandbox::check(&editor, None, "/apps/com.test.editeur/../../sys/config/x").inside(),
    );
    report.check(
        "12.4 : un . au milieu ne change rien",
        sandbox::check(&editor, None, "/apps/com.test.editeur/./notes.txt").inside(),
    );
    // A neighbouring prefix is not a parent.
    report.check(
        "12.4 : prefixe textuel voisin exclu",
        !sandbox::check(&editor, None, "/apps/com.test.editeurautre/x").inside(),
    );
    // The root itself counts as inside: a sandbox that excluded its own
    // directory would refuse a listing of it.
    report.check(
        "12.4 : la racine du bac a sable en fait partie",
        sandbox::check(&editor, None, "/apps/com.test.editeur").inside(),
    );

    // A `.ras` sees only its own directory -- the strictest row of 12.4.
    report.check(
        "12.4 : un .ras ne voit que /ras/{app}",
        sandbox::check(&raw, Some("nova"), "/ras/com.test.brut/x").inside()
            && !sandbox::check(&raw, Some("nova"), "/users/nova/home/x").inside()
            && !sandbox::check(&raw, Some("nova"), "/apps/com.test.brut/x").inside(),
    );

    // Outside the sandbox leads to a prompt, not to a refusal: a user
    // asking an editor to open a file they picked themselves is the
    // ordinary case.
    report.check(
        "12.5 : hors bac a sable, on demande a l'utilisateur",
        matches!(
            super::verdict(&editor, Resource::FileRead("/users/nova/home/images/a.png")),
            Verdict::Prompt(_)
        ),
    );

    // `under` on its own, since everything above rests on it.
    report.check(
        "12.4 : appartenance sur une frontiere de segment",
        policy::under("/apps/x/y", "/apps/x")
            && policy::under("/apps/x", "/apps/x")
            && !policy::under("/apps/xy", "/apps/x")
            && policy::under("/quoi/que/ce/soit", "/"),
    );
}

// ============================================================
//  10.3.2 -- The communication table (12.1)
// ============================================================

fn communication(report: &mut Report) {
    let editor = context(PID_EDITOR);

    // Same group, same type: direct.
    report.check(
        "12.1 : meme groupe et meme type, IPC directe",
        super::verdict(&editor, Resource::IpcTo(PID_VIEWER)).allowed(),
    );
    // Different groups: internal port, so direct IPC is refused and the
    // reason names the port.
    report.denied_for(
        "12.1 : groupes differents, port interne exige",
        &super::verdict(&editor, Resource::IpcTo(PID_OTHER)),
        DenyReason::NeedsPort,
    );
    // A recipient with no identity is a system service.
    report.check(
        "12.1 : un destinataire systeme reste joignable",
        super::verdict(&editor, Resource::IpcTo(999_999)).allowed(),
    );
}

// ============================================================
//  The 12.4 table, transcribed
// ============================================================

fn table(report: &mut Report) {
    let rows = sandbox::table();
    report.check("12.4 : cinq lignes, une par format", rows.len() == 5);
    report.check(
        "12.4 : la ligne .dsa dit confiance totale",
        rows[0].0 == ".dsa" && rows[0].1 == "Confiance totale",
    );
    report.check(
        "12.4 : la ligne .ras dit aucune IPC",
        rows[4].0 == ".ras" && rows[4].3 == "Aucune",
    );

    // The static roots must match the row: the table and the code that
    // enforces it have to say the same thing, or the table is decoration.
    for (format, app, expected) in [
        (BinaryFormat::Gs, "jeu.test", "/games/jeu.test"),
        (BinaryFormat::Cus, "app.test", "/apps/app.test"),
        (BinaryFormat::Ras, "brut.test", "/ras/brut.test"),
    ] {
        let roots = crate::fs::tree::sandbox_roots(format, app);
        report.check(
            "12.4 : la racine statique correspond a la ligne du tableau",
            roots.len() == 1 && roots[0] == expected,
        );
    }
    report.check(
        "12.4 : un .dsa a la racine complete",
        crate::fs::tree::sandbox_roots(BinaryFormat::Dsa, "x") == alloc::vec![String::from("/")],
    );

    // The ceilings of 12.4, on the two rows that carry the most weight.
    report.check(
        "12.4 : un .cus n'atteint jamais l'espace noyau",
        Permissions::ceiling(BinaryFormat::Cus) & Permissions::KERNEL_SPACE == 0,
    );
    report.check(
        "12.4 : un .ras n'a aucune permission de communication",
        Permissions::ceiling(BinaryFormat::Ras)
            & (Permissions::IPC_PUBLIC | Permissions::IPC_INTERNAL_PORTS)
            == 0,
    );
}

// ============================================================
//  10.3.5 -- The remembered choice (12.5)
// ============================================================

fn memorisation(report: &mut Report) {
    if !crate::fs::is_ready() {
        report.check("12.5 : memorisation (systeme de fichiers absent)", true);
        return;
    }

    let key = "lecture:/users/nova/home/images/a.png";

    // Nothing recorded: the prompt would be shown.
    report.check(
        "12.5 : aucun choix memorise au depart",
        crate::prompt::remembered(TEST_APP, key).is_none(),
    );

    let written = crate::prompt::remember(TEST_APP, key, Decision::Allow).is_ok();
    report.check("12.5 : le choix s'ecrit", written);
    report.check(
        "12.5 : le choix se relit",
        crate::prompt::remembered(TEST_APP, key) == Some(Decision::Allow),
    );

    // A second resource must not erase the first: the file holds several
    // choices, and rewriting it whole is what keeps them consistent.
    let other = "ecriture:/users/nova/home/images/a.png";
    let _ = crate::prompt::remember(TEST_APP, other, Decision::Deny);
    report.check(
        "12.5 : deux ressources coexistent",
        crate::prompt::remembered(TEST_APP, key) == Some(Decision::Allow)
            && crate::prompt::remembered(TEST_APP, other) == Some(Decision::Deny),
    );

    // Enumeration: a choice one cannot list is not revocable in practice.
    let recorded = super::consent::recorded(TEST_APP);
    report.check(
        "12.5 : les choix sont enumerables",
        recorded.len() == 2
            && recorded.iter().any(|(name, _)| name == key)
            && recorded.iter().any(|(name, _)| name == other),
    );

    // Revocation restores the "never asked" state. Recording a refusal
    // instead would silence the prompt forever, which is the opposite of
    // revoking.
    let revoked = crate::prompt::revoke(TEST_APP, key).is_ok();
    report.check("12.5 : la revocation reussit", revoked);
    report.check(
        "12.5 : apres revocation, plus aucun choix -- ni oui ni non",
        crate::prompt::remembered(TEST_APP, key).is_none(),
    );
    report.check(
        "12.5 : la revocation ne touche pas l'autre ressource",
        crate::prompt::remembered(TEST_APP, other) == Some(Decision::Deny),
    );

    // The key of a resource must not carry a PID: it would change at
    // every launch, and the choice would be silently forgotten.
    let ipc_key = Resource::IpcTo(PID_VIEWER).key();
    report.check(
        "12.5 : la cle d'une ressource ne porte pas de PID",
        ipc_key == "ipc:com.test.visionneuse",
    );
    report.check(
        "12.5 : la cle d'un chemin est le chemin",
        Resource::FileRead("/x/y").key() == "lecture:/x/y",
    );
}

// ============================================================
//  10.3.4 -- The enforcement itself
// ============================================================

/// The rule must hold **on the real call path**, not only in `evaluate`.
///
/// This is the difference the roadmap complained about for six phases: a
/// table nobody consults is documentation. So the check here goes through
/// `security::check_path`, the very function `syscall::domain_fs::guard`
/// calls, and it goes through it on a live registration -- not on a
/// context built for the occasion.
///
/// No prompt is raised: every case is chosen so the answer is settled by
/// the deny-list or the allow-list, both of which decide without asking.
/// A case landing on `Prompt` would block this self-test for twenty
/// seconds on a machine with nobody in front of it, and then answer no.
fn enforcement(report: &mut Report) {
    use super::Outcome;

    // A `.cus` writing another application's directory: deny-list, no
    // question asked.
    report.check(
        "12.3 : le chemin d'une autre app est refuse sur le chemin reel",
        matches!(
            super::check_path(PID_EDITOR, "/apps/org.autre.outil/x", true),
            Outcome::Denied(DenyReason::ForeignBinary)
        ),
    );
    // Its own directory passes.
    report.check(
        "12.4 : son propre dossier passe sur le chemin reel",
        super::check_path(PID_EDITOR, "/apps/com.test.editeur/x", true).allowed(),
    );

    // `/sys/` for a `.cus`: refused as **above the ceiling**, not as
    // undeclared. The distinction is the point -- a `.cus` cannot declare
    // `SYS_DIRECTORY` at all, so "you did not ask for it" would be a
    // misleading answer to a program that could never have asked.
    report.check(
        "12.4 : /sys/ est au-dessus du plafond d'un .cus",
        matches!(
            super::check_path(PID_EDITOR, "/sys/config/i18n.cfg", false),
            Outcome::Denied(DenyReason::AboveCeiling)
        ),
    );
    // The same path for a `.ss` that declared it passes -- and it is the
    // pair that proves the rule is the declaration and not the path.
    report.check(
        "12.3 : /sys/ passe pour un .ss qui l'a declare",
        super::check_path(PID_SYSTEM, "/sys/config/i18n.cfg", false).allowed(),
    );
    // And is refused for a `.ss` that did not.
    crate::apps::forget(PID_SYSTEM);
    crate::apps::register(
        PID_SYSTEM,
        identity("os.nova.explorateur", "os.nova", BinaryFormat::Ss),
        Permissions::NONE,
    );
    report.check(
        "12.3 : /sys/ non declare est refuse pour un .ss",
        matches!(
            super::check_path(PID_SYSTEM, "/sys/config/i18n.cfg", false),
            Outcome::Denied(DenyReason::NotDeclared)
        ),
    );

    // A task with no identity is a kernel service: everything passes.
    report.check(
        "12.4 : une tache sans identite n'est pas bridee",
        super::check_path(999_999, "/sys/config/i18n.cfg", true).allowed(),
    );

    // A `.ras` outside its own directory. Checked through `verdict`, not
    // through `check_path`: this case lands on `Prompt`, and raising a
    // real prompt in a self-test means twenty seconds of waiting for an
    // answer nobody is there to give. What has to be established is that
    // the policy says "ask", not that an absent user says no.
    let mut raw = context(PID_RAW);
    raw.roots = sandbox::roots_for(&raw, Some("nova"));
    report.check(
        "12.4 : un .ras hors de /ras/ n'est jamais autorise d'office",
        matches!(
            super::verdict(&raw, Resource::FileRead("/apps/com.test.brut/x")),
            Verdict::Prompt(_)
        ),
    );
    report.check(
        "12.4 : un .ras dans /ras/ passe sans question",
        super::verdict(&raw, Resource::FileRead("/ras/com.test.brut/x")).allowed(),
    );

    // The counters must have moved: a check that decided nothing would
    // pass every case above by accident.
    let (allowed, denied, _) = super::counters();
    report.check(
        "10.3 : les compteurs enregistrent les decisions",
        allowed >= 3 && denied >= 3,
    );

    // 12.5 -- What was typed before a prompt existed must not answer it.
    //
    // The self-test met this for real: a keystroke left over from the
    // login screen accepted an authorisation the instant it appeared.
    // `drain` is the fix, and this is the check that keeps it.
    for byte in [0x18u8, 0x18 | 0x80] {
        let _ = crate::drivers::keyboard::decode_byte(byte);
    }
    report.check(
        "12.5 : une touche en attente est ecartee avant l'invite",
        crate::drivers::keyboard::drain() >= 1
            && crate::drivers::keyboard::read_key().is_none(),
    );
}

// ============================================================
//  Purge
// ============================================================

fn purge() {
    if !crate::fs::is_ready() {
        return;
    }
    let who = Credentials::kernel();
    let now = crate::idt::ticks();
    let _ = crate::fs::delete(&crate::prompt::decision_path(TEST_APP), who, now);
}

/// Kept so an external caller can list the fixtures without duplicating
/// their PIDs. Used by the state dump of the phase.
pub fn fixture_pids() -> Vec<Pid> {
    alloc::vec![PID_EDITOR, PID_VIEWER, PID_OTHER, PID_RAW, PID_SYSTEM]
}
