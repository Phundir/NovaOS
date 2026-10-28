//! Phase 4 self-test.
//!
//! For want of a user space able to exercise the filesystem --
//! `SYS_FS_*` only arrives in 5.2 -- this is where every rule of 4 and 5
//! is confronted with a concrete case. The principle is that of phase 3:
//! every assertion bears on an *observed* behaviour, not on an
//! intention.

use alloc::string::String;
use alloc::vec::Vec;

use super::block::{FrameDisk, RamDisk};
use super::export::{self, ExportFormat};
use super::format::{MAX_FILE_SIZE, NAME_MAX};
use super::perm::{Credentials, Mode, Permissions};
use super::{FsError, audit};

/// Recursively deletes a path, ignoring what does not exist.
///
/// The self-test must be able to run on the **second** boot: the volume
/// survives power-off, and a test set assuming a blank disk would fail
/// from the second boot on names already taken. Starting again from a
/// clean workspace is the only way to make the assertions reproducible.
fn reset(path: &str, who: Credentials, now: u64) {
    if let Ok(links) = super::list(path, who) {
        for link in links {
            let child = alloc::format!("{}/{}", path, link.name);
            reset(&child, who, now);
        }
    }
    let _ = super::delete(path, who, now);
}

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
            crate::serial_println!("[fs] ECHEC : {}", label);
        }
    }

    /// Checks that an operation fails, and for the right reason.
    fn expect_error<T>(&mut self, label: &str, result: Result<T, FsError>, expected: FsError) {
        match result {
            Ok(_) => {
                self.failed += 1;
                crate::serial_println!("[fs] ECHEC : {} aurait du echouer", label);
            }
            Err(error) if error == expected => {
                self.passed += 1;
                crate::serial_println!(
                    "[fs]   {} -> {} {}",
                    label,
                    error.message(),
                    error.detail()
                );
            }
            Err(other) => {
                self.failed += 1;
                crate::serial_println!(
                    "[fs] ECHEC : {} - attendu {:?}, obtenu {:?}",
                    label,
                    expected,
                    other
                );
            }
        }
    }

    fn expect_ok<T>(&mut self, label: &str, result: Result<T, FsError>) -> Option<T> {
        match result {
            Ok(value) => {
                self.passed += 1;
                Some(value)
            }
            Err(error) => {
                self.failed += 1;
                crate::serial_println!(
                    "[fs] ECHEC : {} - {} {}",
                    label,
                    error.message(),
                    error.detail()
                );
                None
            }
        }
    }
}

/// Exercises the whole of phase 4. Returns (passed, failed).
pub fn run(now: u64) -> (usize, usize) {
    let mut report = Report {
        passed: 0,
        failed: 0,
    };

    crate::serial_println!("[fs] autotest de la phase 4");

    paths(&mut report);
    disks(&mut report);
    persistence(&mut report, now);
    node_and_links(&mut report, now);
    permissions(&mut report, now);
    audit_journal(&mut report);
    large_file(&mut report, now);
    export_formats(&mut report);
    novafs_in_memory(&mut report, now);
    consistency(&mut report);

    let (passed, failed) = super::lifs::selftest::run(now);
    report.passed += passed;
    report.failed += failed;

    crate::serial_println!(
        "[fs] autotest termine : {} reussites, {} echecs",
        report.passed,
        report.failed
    );

    (report.passed, report.failed)
}

// ============================================================
//  4.2 -- Paths and disks
// ============================================================

fn paths(report: &mut Report) {
    crate::serial_println!("[fs] --- chemins et lettres de disque (spec 4.2) ---");

    report.check(
        "chemin absolu sur le disque de demarrage",
        super::split_disk("/sys/config") == Ok(('/', "/sys/config")),
    );
    report.check(
        "chemin sur un disque secondaire",
        super::split_disk("A:/apps/editeur") == Ok(('A', "/apps/editeur")),
    );
    report.check(
        "lettre minuscule refusee",
        super::split_disk("a:/apps") == Err(FsError::InvalidPath),
    );
    report.check(
        "chemin relatif refuse",
        super::split_disk("apps/editeur") == Err(FsError::InvalidPath),
    );

    report.check(
        "normalisation des segments vides",
        super::normalize("//sys///config/") == "/sys/config",
    );
    report.check(
        "resolution du point",
        super::normalize("/sys/./config") == "/sys/config",
    );
    report.check(
        "resolution du double point",
        super::normalize("/sys/kernel/../config") == "/sys/config",
    );
    report.check(
        "impossible de remonter au-dessus de la racine",
        super::normalize("/../../etc") == "/etc",
    );

    // A name containing a slash would make two distinct paths
    // indistinguishable: it is the only truly structural prohibition.
    report.check(
        "nom contenant une barre oblique refuse",
        !super::format::valid_name("a/b"),
    );
    report.check("nom vide refuse", !super::format::valid_name(""));
    report.check(
        "nom trop long refuse",
        !super::format::valid_name(&"x".repeat(NAME_MAX)),
    );
    report.check(
        "nom de longueur maximale accepte",
        super::format::valid_name(&"x".repeat(NAME_MAX - 1)),
    );
}

fn disks(report: &mut Report) {
    crate::serial_println!("[fs] --- volumes montes (spec 4.2.3) ---");

    let disks = super::disk_list();
    report.check("au moins un volume monte", !disks.is_empty());

    for disk in disks.iter() {
        crate::serial_println!(
            "[fs]   {} - \"{}\" sur {} ({}) - {} Mio dont {} libres, {} noeuds libres",
            disk.root(),
            disk.label,
            disk.device,
            disk.medium,
            disk.capacity_mib(),
            disk.free_mib(),
            disk.free_nodes
        );
    }

    report.check(
        "le disque de demarrage repond a /",
        super::disk_mount('/').is_ok(),
    );
    report.check(
        "une lettre inconnue est refusee",
        super::disk_mount('Z').err() == Some(FsError::NoSuchDisk),
    );
}

// ============================================================
//  4.3.2 -- Persistence
// ============================================================

/// Checks that a volume finds its content again after power-off.
///
/// It is the only check neither the format nor the structures can give:
/// a filesystem entirely correct in memory but writing nothing to the
/// medium would pass every other test. A counter incremented on every
/// boot settles the question -- and shows along the way how many times
/// the machine has booted since the formatting.
fn persistence(report: &mut Report, now: u64) {
    crate::serial_println!("[fs] --- persistance sur support (spec 4.3.2) ---");

    let kernel = Credentials::kernel();
    const COUNTER: &str = "/logs/kernel/demarrages.txt";

    let previous = super::read_file(COUNTER, kernel)
        .ok()
        .and_then(|data| String::from_utf8(data).ok())
        .and_then(|text| text.trim().parse::<u64>().ok());

    let count = previous.unwrap_or(0) + 1;
    let written = super::write_file(
        COUNTER,
        alloc::format!("{}", count).as_bytes(),
        kernel,
        Mode::system_binary(),
        now,
    );
    report.expect_ok("ecriture du compteur de demarrages", written);

    // Immediate read-back: the cache must return what was just written.
    let reread = super::read_file(COUNTER, kernel)
        .ok()
        .and_then(|data| String::from_utf8(data).ok())
        .and_then(|text| text.trim().parse::<u64>().ok());
    report.check("relecture immediate du compteur", reread == Some(count));

    match previous {
        Some(value) => {
            report.passed += 1;
            crate::serial_println!(
                "[fs]   demarrage n {} - le volume conserve son contenu depuis {} extinction(s)",
                count,
                value
            );
        }
        None => {
            crate::serial_println!(
                "[fs]   premier demarrage sur ce volume - le compteur est cree a 1"
            );
            crate::serial_println!(
                "[fs]   (relancer sans `make disk-reset` pour verifier la persistance)"
            );
        }
    }
}

// ============================================================
//  4.1 -- Nodes and virtual links
// ============================================================

fn node_and_links(report: &mut Report, now: u64) {
    crate::serial_println!("[fs] --- noeuds et liens virtuels (spec 4.1) ---");

    let admin = super::credentials_of("admin").unwrap_or(Credentials::kernel());

    // Workspace made fresh again: see `reset`.
    reset("/cache/autotest", admin, now);
    report.expect_ok(
        "creation de l'espace de travail",
        super::create_directory("/cache/autotest", admin, Mode::private_dir(), now),
    );
    for sub in ["documents", "archive", "bureau"] {
        super::create_directory(
            &alloc::format!("/cache/autotest/{}", sub),
            admin,
            Mode::private_dir(),
            now,
        )
        .ok();
    }
    let base = "/cache/autotest/documents";

    // --- write then read back ---
    let content = b"Nova OS - phase 4 - systeme de fichiers a liens virtuels";
    report.expect_ok(
        "creation d'un fichier",
        super::write_file(
            &alloc::format!("{}/notes.txt", base),
            content,
            admin,
            Mode::private_file(),
            now,
        ),
    );

    let read = report.expect_ok(
        "relecture du fichier",
        super::read_file(&alloc::format!("{}/notes.txt", base), admin),
    );
    report.check(
        "le contenu relu est identique",
        read.as_deref() == Some(&content[..]),
    );

    // --- 4.1.3: moving copies nothing ---
    let node_before = report.expect_ok(
        "metadonnees avant deplacement",
        super::stat(&alloc::format!("{}/notes.txt", base), admin),
    );

    let copied = report.expect_ok(
        "deplacement vers un autre dossier",
        super::move_entry(
            &alloc::format!("{}/notes.txt", base),
            "/cache/autotest/archive/notes.txt",
            admin,
            now,
        ),
    );
    report.check("aucun octet recopie au deplacement", copied == Some(false));

    let node_after = report.expect_ok(
        "metadonnees apres deplacement",
        super::stat("/cache/autotest/archive/notes.txt", admin),
    );
    // That is *the* point of 4.1: the node is the same object, only the
    // link naming it has changed table.
    report.check(
        "le noeud est inchange - seul le lien a bouge",
        match (&node_before, &node_after) {
            (Some(before), Some(after)) => {
                before.id == after.id && before.direct == after.direct && before.size == after.size
            }
            _ => false,
        },
    );
    report.expect_error(
        "l'ancien chemin ne repond plus",
        super::stat(&alloc::format!("{}/notes.txt", base), admin),
        FsError::NotFound,
    );

    // --- many moves at once, one flush ---
    //
    // A directory of 100 entries moved one link at a time into another:
    // the lookups compare names in place, and the batch writes once.
    for index in 0..100 {
        super::write_file(
            &alloc::format!("{}/lot-{}.txt", base, index),
            b"x",
            admin,
            Mode::private_file(),
            now,
        )
        .ok();
    }
    let batch: Vec<(String, String)> = (0..100)
        .map(|index| {
            (
                alloc::format!("{}/lot-{}.txt", base, index),
                alloc::format!("/cache/autotest/archive/lot-{}.txt", index),
            )
        })
        .collect();
    let started = crate::idt::ticks();
    let moved = super::move_entries(&batch, admin, now);
    let elapsed = crate::idt::ticks() - started;
    report.check("cent deplacements en un lot", moved == Ok(100));
    report.check(
        "le lot a tout deplace, rien laisse",
        super::list(base, admin)
            .map(|links| !links.iter().any(|link| link.name.starts_with("lot-")))
            .unwrap_or(false)
            && super::stat("/cache/autotest/archive/lot-99.txt", admin).is_ok(),
    );
    crate::serial_println!(
        "[fs]   100 deplacements en un lot : {} ms",
        elapsed * (1000 / crate::TIMER_HZ as u64)
    );
    report.check(
        "un lot refuse une destination dans la source",
        super::move_entries(
            &[(String::from("/cache/autotest/archive"), String::from("/cache/autotest/archive/x"))],
            admin,
            now,
        )
        .is_err(),
    );

    // --- extra virtual link ---
    report.expect_ok(
        "creation d'un second lien vers le meme noeud",
        super::link(
            "/cache/autotest/archive/notes.txt",
            "/cache/autotest/bureau/raccourci.txt",
            admin,
            now,
        ),
    );

    let linked = report.expect_ok(
        "lecture par le second lien",
        super::read_file("/cache/autotest/bureau/raccourci.txt", admin),
    );
    report.check(
        "les deux liens donnent le meme contenu",
        linked.as_deref() == Some(&content[..]),
    );

    let via_link = super::stat("/cache/autotest/bureau/raccourci.txt", admin);
    report.check(
        "les deux liens designent le meme noeud",
        match (&node_after, &via_link) {
            (Some(direct), Ok(indirect)) => direct.id == indirect.id && indirect.links == 2,
            _ => false,
        },
    );

    // Removing a link does not destroy the node while one is left.
    report.expect_ok(
        "suppression du premier lien",
        super::delete("/cache/autotest/archive/notes.txt", admin, now),
    );
    let survivor = report.expect_ok(
        "le contenu survit par le second lien",
        super::read_file("/cache/autotest/bureau/raccourci.txt", admin),
    );
    report.check(
        "contenu intact apres suppression d'un lien",
        survivor.as_deref() == Some(&content[..]),
    );

    // --- renaming ---
    report.expect_ok(
        "renommage",
        super::rename(
            "/cache/autotest/bureau/raccourci.txt",
            "notes-finales.txt",
            admin,
            now,
        ),
    );
    report.check(
        "le nouveau nom repond",
        super::stat("/cache/autotest/bureau/notes-finales.txt", admin).is_ok(),
    );

    // --- copy: this one really copies ---
    report.expect_ok(
        "copie dans le meme volume",
        super::copy(
            "/cache/autotest/bureau/notes-finales.txt",
            "/cache/autotest/documents/copie.txt",
            admin,
            now,
        ),
    );
    let original = super::stat("/cache/autotest/bureau/notes-finales.txt", admin);
    let duplicate = super::stat("/cache/autotest/documents/copie.txt", admin);
    report.check(
        "la copie est un noeud distinct",
        match (&original, &duplicate) {
            (Ok(a), Ok(b)) => a.id != b.id && a.size == b.size,
            _ => false,
        },
    );

    // --- edge cases ---
    report.expect_error(
        "dossier non vide non supprimable",
        super::delete("/cache/autotest", admin, now),
        FsError::DirectoryNotEmpty,
    );
    report.expect_error(
        "chemin inexistant",
        super::read_file("/cache/autotest/introuvable", admin),
        FsError::NotFound,
    );
    report.expect_error(
        "lecture d'un dossier comme fichier",
        super::read_file("/cache/autotest/documents", admin),
        FsError::IsADirectory,
    );
    // A link to a name already taken is refused: without that, the table
    // of the directory would carry two entries of the same name and one
    // of the two would be unreachable.
    report.expect_error(
        "lien vers un nom deja pris",
        super::link(
            "/cache/autotest/documents/copie.txt",
            "/cache/autotest/bureau/notes-finales.txt",
            admin,
            now,
        ),
        FsError::AlreadyExists,
    );
    // `create_directory` rests on `create_path`, which accepts a
    // directory that is already there: creating a tree twice in a row
    // must not fail, that is what makes step 4.4 replayable.
    report.expect_ok(
        "creation idempotente d'un dossier existant",
        super::create_directory("/cache/autotest/bureau", admin, Mode::private_dir(), now),
    );

    // --- file of several blocks ---
    // Passes the mark of the direct blocks and forces the use of the
    // indirection block: the code path is not the same.
    let free_before = free_blocks();
    let large: Vec<u8> = (0..80_000u32).map(|i| (i % 251) as u8).collect();
    report.expect_ok(
        "ecriture d'un fichier de 80 Kio (indirection)",
        super::write_file(
            "/cache/autotest/documents/gros.bin",
            &large,
            admin,
            Mode::private_file(),
            now,
        ),
    );
    let reread = super::read_file("/cache/autotest/documents/gros.bin", admin);
    report.check(
        "relecture d'un fichier a indirection",
        reread.as_deref() == Ok(&large[..]),
    );

    // --- truncation ---
    report.expect_ok(
        "reecriture plus courte",
        super::write_file(
            "/cache/autotest/documents/gros.bin",
            b"court",
            admin,
            Mode::private_file(),
            now,
        ),
    );
    let shortened = super::read_file("/cache/autotest/documents/gros.bin", admin);
    report.check(
        "les blocs excedentaires sont rendus",
        shortened.as_deref() == Ok(&b"court"[..]),
    );

    // The file now weighs one block: all the rest must have come back to
    // the volume, **indirection table included**. It is that last point
    // that was missing: it stayed marked taken without describing
    // anything, and no walk could find it again.
    let free_after = free_blocks();
    report.check(
        "la table d'indirection est rendue avec les blocs",
        free_after + 1 == free_before,
    );
    if free_after + 1 != free_before {
        crate::serial_println!(
            "[fs]   blocs libres : {} avant, {} apres - {} manquants",
            free_before,
            free_after,
            free_before.saturating_sub(free_after + 1)
        );
    }

    crate::serial_println!(
        "[fs]   taille maximale d'un noeud NovaFS : {} Kio ({} blocs directs + {} indirects) ; LIFS : la partition",
        MAX_FILE_SIZE / 1024,
        super::format::DIRECT_BLOCKS,
        super::format::INDIRECT_ENTRIES
    );
}

// ============================================================
//  4.5 -- Permissions
// ============================================================

/// Free blocks of the boot volume.
/// Free blocks of the root, once the open transaction is committed: on
/// LIFS a block freed joins the free count at the commit (`lifs`).
fn free_blocks() -> u64 {
    let _ = super::flush_all();
    super::disk_list()
        .first()
        .map(|disk| disk.free_blocks)
        .unwrap_or(0)
}

fn permissions(report: &mut Report, now: u64) {
    crate::serial_println!("[fs] --- permissions et roles (spec 5) ---");

    let admin = super::credentials_of("admin").unwrap_or(Credentials::kernel());
    let user = super::credentials_of("nova").unwrap_or(Credentials::kernel());
    let guest = super::credentials_of("invite").unwrap_or(Credentials::kernel());

    for (name, role, uid) in super::account_names() {
        crate::serial_println!("[fs]   compte \"{}\" - uid {} - role {}", name, uid, role.as_str());
    }

    let kernel = Credentials::kernel();
    for path in [
        "/users/nova/home/documents/prive.txt",
        "/users/nova/home/documents/lecture.txt",
        "/sys/config/pirate.cfg",
    ] {
        reset(path, kernel, now);
    }
    reset("/apps/demo", kernel, now);

    // A user writes at home.
    report.expect_ok(
        "un USER ecrit dans son propre espace",
        super::write_file(
            "/users/nova/home/documents/prive.txt",
            b"contenu prive",
            user,
            Mode::private_file(),
            now,
        ),
    );

    // And does not enter other people's: `access` is missing on the directory.
    report.expect_error(
        "un USER n'entre pas chez un autre",
        super::read_file("/cache/autotest/documents/copie.txt", user),
        FsError::PermissionDenied(Permissions::ACCESS),
    );
    report.expect_error(
        "un GUEST n'entre pas chez un USER",
        super::list("/users/nova/home", guest),
        FsError::PermissionDenied(Permissions::ACCESS),
    );

    // ADMIN bypasses the masks (spec 5.2: near-total access).
    report.expect_ok(
        "un ADMIN traverse l'espace d'un autre compte",
        super::read_file("/users/nova/home/documents/prive.txt", admin),
    );

    // A USER creates nothing in the system tree.
    report.expect_error(
        "un USER n'ecrit pas dans /sys/config",
        super::write_file(
            "/sys/config/pirate.cfg",
            b"non",
            user,
            Mode::private_file(),
            now,
        ),
        FsError::PermissionDenied(Permissions::WRITE),
    );
    // But can traverse and list it: `access` is granted to others on the
    // system directories.
    report.expect_ok("un USER liste /sys/config", super::list("/sys/config", user));

    // --- 4.5.2: the two-level rule ---
    super::create_directory("/apps/demo", kernel, Mode::system_dir(), now).ok();
    super::write_file(
        "/apps/demo/outil.cus",
        b"binaire",
        kernel,
        Mode::system_binary(),
        now,
    )
    .ok();

    report.check(
        "binaire executable dans un dossier executable",
        super::is_executable("/apps/demo/outil.cus", user) == Ok(true),
    );

    // The directory loses `execute`: the file itself does not change.
    let no_execute = Mode::new(
        Mode::system_dir().owner.without(Permissions::EXECUTE).0,
        Permissions::ACCESS,
        Permissions::ACCESS,
    );
    report.expect_ok(
        "retrait d'execute sur le dossier",
        super::chmod("/apps/demo", no_execute, kernel, now),
    );
    report.check(
        "le binaire devient inerte malgre ses propres droits",
        super::is_executable("/apps/demo/outil.cus", user) == Ok(false),
    );
    report.check(
        "le fichier a pourtant conserve execute",
        super::stat("/apps/demo/outil.cus", kernel)
            .map(|n| n.perm_other.contains(Permissions::EXECUTE))
            == Ok(true),
    );
    super::chmod("/apps/demo", Mode::system_dir(), kernel, now).ok();

    // --- inapplicable permissions ---
    // `read` does not exist on a directory ("File" column only).
    report.expect_error(
        "read n'a pas de sens sur un dossier",
        super::read_file("/apps/demo", admin),
        FsError::IsADirectory,
    );

    // --- invalid mask ---
    let bogus = Mode::new(1 << 40, 0, 0);
    report.expect_error(
        "masque de permission inconnu refuse",
        super::chmod("/apps/demo/outil.cus", bogus, kernel, now),
        FsError::InvalidPermission,
    );
    // The scaffolding goes: left in /apps, it showed as an application
    // called "demo" in every user's menu.
    reset("/apps/demo", kernel, now);
    report.check("le dossier d'essai /apps/demo est retire", super::stat("/apps/demo", kernel).is_err());

    // --- overwrite distinct from write ---
    let create_only = Mode::new(
        Permissions::READ | Permissions::WRITE | Permissions::DELETE,
        0,
        0,
    );
    super::write_file(
        "/users/nova/home/documents/lecture.txt",
        b"v1",
        user,
        create_only,
        now,
    )
    .ok();
    report.expect_error(
        "ecraser exige overwrite, pas write",
        super::write_file(
            "/users/nova/home/documents/lecture.txt",
            b"v2",
            user,
            create_only,
            now,
        ),
        FsError::PermissionDenied(Permissions::OVERWRITE),
    );

    // --- chown ---
    report.expect_ok(
        "transfert de propriete par le proprietaire",
        super::chown(
            "/users/nova/home/documents/prive.txt",
            1000,
            super::perm::USERS_GID,
            user,
            now,
        ),
    );
    report.check(
        "le nouveau proprietaire est enregistre",
        super::stat("/users/nova/home/documents/prive.txt", admin).map(|n| n.owner) == Ok(1000),
    );

    crate::serial_println!(
        "[fs]   masque d'un fichier prive : {}",
        Mode::private_file().owner.compact()
    );
    crate::serial_println!(
        "[fs]   masque d'un dossier systeme : {} (proprietaire) / {} (autres)",
        Mode::system_dir().owner.compact(),
        Mode::system_dir().other.compact()
    );
}

// ============================================================
//  4.5.4 -- Audit log
// ============================================================

fn audit_journal(report: &mut Report) {
    crate::serial_println!("[fs] --- journal d'audit (spec 5.1) ---");

    let admin = super::credentials_of("admin").unwrap_or(Credentials::kernel());
    let user = super::credentials_of("nova").unwrap_or(Credentials::kernel());
    let guest = super::credentials_of("invite").unwrap_or(Credentials::kernel());

    let (total, denied) = audit::stats();
    report.check("le journal a enregistre des acces", total > 0);
    report.check("le journal a enregistre des refus", denied > 0);

    report.check(
        "un ADMIN consulte le journal",
        audit::read(admin).is_ok(),
    );
    report.check(
        "un USER ne consulte pas le journal",
        audit::read(user).is_err(),
    );
    report.check(
        "un GUEST ne consulte pas le journal",
        audit::read(guest).is_err(),
    );

    audit::dump_denials(admin, 6);
}

// ============================================================
//  4.6 -- Export
// ============================================================

fn export_formats(report: &mut Report) {
    crate::serial_println!("[fs] --- multi-compatibilite (spec 4.4) ---");

    let settings = export::Settings::new();
    for format in settings.offered() {
        crate::serial_println!(
            "[export]   {} - {} - {}",
            format.as_str(),
            if format.implemented() {
                "implemente"
            } else {
                "NON IMPLEMENTE"
            },
            format.warning()
        );
    }

    let admin = super::credentials_of("admin").unwrap_or(Credentials::kernel());

    // The export bears on `/users`: three complete trees, dozens of
    // directories and names too long for the 8.3 format. That is what
    // really exercises the long name entries, the depth of the NTFS index
    // tree and the filling of the ext4 directory blocks.
    const SOURCE: &str = "/users";

    // One medium per format: the three images stay readable by the host
    // after power-off, which is the only real proof of compatibility.
    // `make check-export` puts them through the checkers.
    const FORMATS: [ExportFormat; 3] = [
        ExportFormat::Fat32,
        ExportFormat::Ext4,
        ExportFormat::Ntfs,
    ];

    let media = super::media_list();
    for (name, blocks) in media.iter() {
        crate::serial_println!(
            "[export]   support disponible : \"{}\" - {} Mio, non monte",
            name,
            blocks * super::block::BLOCK_SIZE as u64 / (1024 * 1024)
        );
    }

    for (index, format) in FORMATS.iter().enumerate() {
        if index < media.len() {
            export_to_real_medium(report, index, SOURCE, *format, admin);
        } else {
            // No removable medium plugged in for this format: one is
            // simulated from the free frames. 96 MiB, that is above the
            // threshold of 65 525 clusters that tells FAT32 from FAT16
            // with 1 KiB clusters. The kernel heap, which is 4 MiB, could
            // not host such a medium.
            let Some(mut target) = FrameDisk::new("support simule", 24_576) else {
                report.failed += 1;
                crate::serial_println!(
                    "[export] ECHEC : pas assez de cadres pour le support de test {}",
                    format.as_str()
                );
                continue;
            };

            let (_, _, free_before) = crate::frame::stats();
            run_export(report, SOURCE, &mut target, *format, admin);
            target.release();

            let (_, _, free_after) = crate::frame::stats();
            report.check(
                "le support simule rend tous ses cadres",
                free_after >= free_before + 24_576,
            );
        }
    }

    // The native format is not an export: it must refuse outright.
    let mut small = RamDisk::new("support", 64);
    report.expect_error(
        "export natif",
        export::export("/apps", &mut small, ExportFormat::Native, admin),
        FsError::Unsupported,
    );
    // A medium too small for the format requested must fail on the spot,
    // not produce a truncated image. 64 KiB: ext4 would still place an
    // inode table there, NTFS not even its journal.
    let mut tiny = RamDisk::new("support minuscule", 16);
    report.expect_error(
        "export ext4 sur support minuscule",
        export::export("/apps", &mut tiny, ExportFormat::Ext4, admin),
        FsError::NoSpace,
    );
    report.expect_error(
        "export NTFS sur support minuscule",
        export::export("/apps", &mut tiny, ExportFormat::Ntfs, admin),
        FsError::NoSpace,
    );
}

/// Exports to a real removable medium, then reads the image produced back.
fn export_to_real_medium(
    report: &mut Report,
    index: usize,
    source: &str,
    format: ExportFormat,
    who: Credentials,
) {
    match super::export_to_medium(index, source, format, who) {
        Ok(exported) => {
            report.passed += 1;
            exported.dump();
            report.check(
                "l'export a produit des fichiers",
                exported.files > 0 && exported.directories > 0,
            );
        }
        Err(error) => {
            report.failed += 1;
            crate::serial_println!(
                "[export] ECHEC : export {} - {}",
                format.as_str(),
                error.message()
            );
            return;
        }
    }

    match super::verify_medium(index, format) {
        Ok((first, second)) => {
            report.passed += 1;
            crate::serial_println!(
                "[export] image {} relue : {} / {} - signature et geometrie conformes",
                format.as_str(),
                first,
                second
            );
        }
        Err(error) => {
            report.failed += 1;
            crate::serial_println!(
                "[export] ECHEC : image {} illisible - {}",
                format.as_str(),
                error.message()
            );
        }
    }
}

/// Exports to a simulated medium then reads the header produced back.
fn run_export(
    report: &mut Report,
    source: &str,
    target: &mut dyn super::block::BlockDevice,
    format: ExportFormat,
    who: Credentials,
) {
    match export::export(source, target, format, who) {
        Ok(exported) => {
            report.passed += 1;
            exported.dump();
            report.check(
                "l'export a produit des fichiers",
                exported.files > 0 && exported.directories > 0,
            );

            match export::verify(target, format) {
                Ok((first, second)) => {
                    report.passed += 1;
                    crate::serial_println!(
                        "[export] image {} relue : {} / {} - signature et geometrie conformes",
                        format.as_str(),
                        first,
                        second
                    );
                }
                Err(error) => {
                    report.failed += 1;
                    crate::serial_println!(
                        "[export] ECHEC : image {} illisible - {}",
                        format.as_str(),
                        error.message()
                    );
                }
            }
        }
        Err(error) => {
            report.failed += 1;
            crate::serial_println!(
                "[export] ECHEC : export {} - {}",
                format.as_str(),
                error.message()
            );
        }
    }
}

// ============================================================
//  Coherence
// ============================================================

/// A file beyond the single level of indirection (spec 4.3.2): 3 MiB,
/// where the format stopped at a little over 2. The browser's libraries
/// weigh a hundred times that; what is checked here is the second level
/// itself -- written, read back, counted by the checker, and given back.
fn large_file(report: &mut Report, now: u64) {
    use super::format::SINGLE_LEVEL_BLOCKS;
    crate::serial_println!("[fs] --- fichier au-dela d'un niveau d'indirection ---");
    let kernel = Credentials::kernel();
    let path = "/cache/autotest/grand.bin";
    let _ = super::delete(path, kernel, now);
    if super::disk_mount('/').is_err() {
        report.failed += 1;
        crate::serial_println!("[fs] ECHEC : volume systeme absent");
        return;
    }
    let free_before = free_blocks();

    // 3 MiB: 768 blocks, where a single level stops at 517.
    let size = 3 * 1024 * 1024usize;
    let mut content = alloc::vec![0u8; size];
    for (index, byte) in content.iter_mut().enumerate() {
        *byte = (index % 251) as u8;
    }
    let written = super::write_file(path, &content, kernel, Mode::private_file(), now);
    report.check(
        &alloc::format!("ecriture de {} blocs, au-dela des {} d'un niveau", size / 4096, SINGLE_LEVEL_BLOCKS),
        written.is_ok(),
    );
    match super::read_file(path, kernel) {
        Ok(read) => {
            report.check("relecture de la meme taille", read.len() == size);
            report.check("chaque octet est celui qui a ete ecrit", read == content);
        }
        Err(error) => {
            report.failed += 2;
            crate::serial_println!("[fs] ECHEC : relecture - {}", error.message());
        }
    }
    match super::check_all() {
        Ok(_) => report.passed += 1,
        Err(error) => {
            report.failed += 1;
            crate::serial_println!("[fs] ECHEC : coherence avec le grand fichier - {}", error.message());
        }
    }
    let _ = super::delete(path, kernel, now);
    let free_after = free_blocks();
    report.check(
        "les blocs du grand fichier, tables comprises, sont rendus",
        free_after == free_before,
    );
}

/// NovaFS is no longer the root's format (phase 14) but stays mounted for
/// the volumes that exist: it keeps a trial of its own, on a volume in
/// memory, so that its code does not go untested because `/` is LIFS.
fn novafs_in_memory(report: &mut Report, now: u64) {
    use super::filesystem::Filesystem;
    crate::serial_println!("[fs] --- NovaFS sur un volume en memoire (volumes existants) ---");
    let kernel = Credentials::kernel();
    let cache = super::cache::BlockCache::new(alloc::boxed::Box::new(RamDisk::new("novafs d'essai", 512)));
    let Some(mut volume) = report.expect_ok("formatage NovaFS explicite", super::volume::Volume::format(cache, "essai-novafs", now)) else {
        return;
    };
    let v: &mut dyn Filesystem = &mut volume;
    report.expect_ok("dossier", v.create_path("/d", kernel, Mode::shared_dir(), now));
    report.expect_ok("fichier", v.write_file("/d/f", b"novafs", kernel, Mode::shared_file(), now));
    report.expect_ok("lien", v.link("/d/f", "/g", kernel, now));
    report.check("relecture par le lien", v.read_file("/g", kernel).ok().as_deref() == Some(&b"novafs"[..]));
    report.expect_error("pas d'appel par identifiant sur NovaFS", v.node_tags(1, kernel), FsError::Unsupported);
    report.expect_ok("coherence NovaFS", v.check());
}

fn consistency(report: &mut Report) {
    crate::serial_println!("[fs] --- coherence des volumes ---");

    match super::check_all() {
        Ok((nodes, blocks)) => {
            report.passed += 1;
            crate::serial_println!(
                "[fs]   {} noeuds occupes, {} blocs references - tous marques dans les bitmaps",
                nodes,
                blocks
            );
        }
        Err(error) => {
            report.failed += 1;
            crate::serial_println!("[fs] ECHEC : coherence - {}", error.message());
        }
    }

    match super::flush_all() {
        Ok(()) => report.passed += 1,
        Err(error) => {
            report.failed += 1;
            crate::serial_println!("[fs] ECHEC : ecriture des caches - {}", error.message());
        }
    }
}
