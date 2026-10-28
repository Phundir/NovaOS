//! Phase 8 self-test (8.1 to 8.5).
//!
//! ## What is checkable with no network and no intervention
//!
//! * **The format** -- building then parsing must compose to the
//!   identity, and every alteration must produce the refusal matching
//!   it. It is the only place where an offset shift shows: a wrong header
//!   stays perfectly readable, it just describes something else.
//!
//! * **The spec tables** -- 10.2 and 10.4 for the packages, 11.1 for the
//!   OS. A transcription is checked line by line, and it is the only way
//!   to establish that two columns have not been swapped.
//! * **The confinement of the scripts** -- a `write` outside the prefix
//!   must be refused, and refused **before** the preceding instructions
//!   run.
//! * **The complete chain** -- a local repository laid on the volume, a
//!   sealed index, a package fetched, verified, installed, then
//!   uninstalled. That test is the only one proving the six modules agree.
//! * **The rollback** -- the state machine, from the dropping of an
//!   update to its cancellation.
//!
//! ## The self-test must leave nothing behind it
//!
//! It writes on a persistent volume. A pass leaving a package installed
//! makes every later boot fail on an "already installed" whose cause is
//! three weeks back. So it purges its own scope **on the way in** as much
//! as on the way out -- the lesson is that of phase 7, and it holds here
//! unchanged.

use alloc::string::String;
use alloc::vec::Vec;

use crate::binfmt::header::{BinaryFormat, SignatureType};
use crate::binfmt::sha256;
use crate::binfmt::signature::{Reason, Verdict};
use crate::fs::perm::{Credentials, Mode};

use super::db::{self, Scope};
use super::package::{self, Manifest, UpdateCode, WarningLevel};
use super::repo::{self, OsEntry, PackageEntry};
use super::rollback;
use super::script;
use super::update::OsUpdateKind;

/// app_id of the test packages. Recognisable prefix: everything carrying
/// it is ours, and can be erased without hesitation.
const TEST_ID: &str = "test.spm.exemple";
const TEST_DEP: &str = "test.spm.bibliotheque";

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
            crate::serial_println!("[spm] ECHEC : {}", label);
        }
    }
}

// ============================================================
//  Building test packages
// ============================================================

fn manifest_for(app_id: &str, depends: &[&str]) -> Manifest {
    Manifest {
        app_id: String::from(app_id),
        name: String::from("Exemple SPM"),
        publisher: String::from("Nova OS"),
        description: String::from("paquet de test de la phase 8"),
        binary: String::from("exemple.cus"),
        prefix: String::new(),
        depends: depends.iter().map(|item| String::from(*item)).collect(),
        extra: Vec::new(),
    }
}

/// A minimal package, parameterised by what the test wants to exercise.
fn sample(
    app_id: &str,
    signature: SignatureType,
    format: BinaryFormat,
    code: UpdateCode,
    version: [u16; 4],
    pre: &str,
    post: &str,
    depends: &[&str],
) -> Vec<u8> {
    let manifest = manifest_for(app_id, depends);
    let binary = alloc::vec![0x90u8; 64];
    let icon = alloc::vec![0x11u8; 32];
    let resources: [(&str, &[u8]); 2] = [("icone.bin", &icon), ("licence.txt", b"test")];

    package::build(&package::BuildSpec {
        update_code: code,
        signature,
        format,
        flags: 0,
        version,
        display_name: "Exemple SPM",
        publisher: "Nova OS",
        manifest: &manifest,
        binary: &binary,
        resources: &resources,
        pre_script: pre,
        post_script: post,
        authority_seed: None,
    })
}

/// Ordinary self-signed package, the one installed for real.
fn ordinary(app_id: &str, version: [u16; 4], depends: &[&str]) -> Vec<u8> {
    sample(
        app_id,
        SignatureType::SelfSigned,
        BinaryFormat::Cus,
        UpdateCode::Silent,
        version,
        "",
        "",
        depends,
    )
}

// ============================================================
//  Cleanup
// ============================================================

/// Erases everything the self-test may have laid down, here or during an
/// interrupted pass.
fn purge(now: u64) {
    let who = Credentials::kernel();
    let scope = Scope::Global;

    for app_id in [TEST_ID, TEST_DEP] {
        if db::find(&scope, app_id).is_some() {
            let _ = super::manager::remove(app_id, scope.clone(), now);
        }
        // Even if the database does not know it: a pass interrupted
        // between the writing of the files and that of the database
        // leaves an orphan directory the database cannot name.
        let prefix = alloc::format!("/apps/{}", app_id);
        if let Ok(links) = crate::fs::list(&prefix, who) {
            for link in links {
                let _ = crate::fs::delete(&alloc::format!("{}/{}", prefix, link.name), who, now);
            }
        }
        let _ = crate::fs::delete(&prefix, who, now);
        db::forget_files(&scope, app_id, now);
    }

    // The test repository and its index.
    if let Ok(links) = crate::fs::list(super::LOCAL_REPO_DIR, who) {
        for link in links {
            if link.name.starts_with("test.") || link.name == "index.spmi" {
                let _ = crate::fs::delete(
                    &alloc::format!("{}/{}", super::LOCAL_REPO_DIR, link.name),
                    who,
                    now,
                );
            }
        }
    }
}

/// Number of entries of a directory, zero if it does not exist.
fn count_entries(path: &str) -> usize {
    crate::fs::list(path, Credentials::kernel())
        .map(|links| links.len())
        .unwrap_or(0)
}

// ============================================================
//  8.1 -- Format
// ============================================================

fn test_format(report: &mut Report) {
    let bytes = sample(
        TEST_ID,
        SignatureType::SelfSigned,
        BinaryFormat::Cus,
        UpdateCode::Standard,
        [1, 2, 3, 4],
        "echo pre\n",
        "echo post\n",
        &[],
    );

    let header = match package::parse(&bytes) {
        Ok(header) => header,
        Err(error) => {
            report.check(&alloc::format!("paquet valide analysable ({})", error.message()), false);
            return;
        }
    };

    report.check("8.1.1 magic et version", header.version == package::FORMAT_VERSION);
    report.check("8.1.1 app_id conserve", header.app_id == TEST_ID);
    report.check("8.1.1 version du paquet", header.package_version == [1, 2, 3, 4]);
    report.check(
        "8.1.1 code de mise a jour",
        header.update_code == UpdateCode::Standard,
    );
    report.check("8.1.1 format du binaire", header.format == BinaryFormat::Cus);
    report.check(
        "8.1.1 signature",
        header.signature == SignatureType::SelfSigned,
    );

    // 8.1.2 -- manifest: decoding then re-encoding must compose to the
    // identity. A field lost along the way would show here and nowhere
    // else.
    let manifest = package::manifest(&header, &bytes);
    report.check("8.1.2 app_id du manifest", manifest.app_id == TEST_ID);
    report.check("8.1.2 binaire nomme", manifest.binary == "exemple.cus");
    let reparsed = Manifest::parse(&manifest.to_text());
    report.check("8.1.2 manifest idempotent", reparsed == manifest);

    // 8.1.3 -- scripts.
    report.check(
        "8.1.3 script pre present",
        core::str::from_utf8(package::pre_script(&header, &bytes)).unwrap_or("") == "echo pre\n",
    );
    report.check(
        "8.1.3 script post present",
        core::str::from_utf8(package::post_script(&header, &bytes)).unwrap_or("") == "echo post\n",
    );

    // Resources.
    let resources = package::read_resources(&header, &bytes).unwrap_or_default();
    report.check("8.1 deux ressources", resources.len() == 2);
    if resources.len() == 2 {
        let icon = package::resource_bytes(&header, &bytes, &resources[0]);
        report.check(
            "8.1 contenu de ressource",
            icon.len() == 32 && icon.iter().all(|byte| *byte == 0x11),
        );
        report.check("8.1 nom de ressource", resources[1].name == "licence.txt");
    }

    report.check(
        "8.1 binaire principal restitue",
        package::binary(&header, &bytes).len() == 64,
    );

    // --- refusals ---
    report.check(
        "8.1 fichier trop court refuse",
        package::parse(&bytes[..64]) == Err(package::PackageError::TooShort),
    );

    let mut broken = bytes.clone();
    broken[0] = b'X';
    report.check(
        "8.1 magic altere refuse",
        package::parse(&broken) == Err(package::PackageError::BadMagic),
    );

    let mut broken = bytes.clone();
    broken[0x04] = 0x09;
    report.check(
        "8.1 version inconnue refusee",
        package::parse(&broken) == Err(package::PackageError::BadVersion),
    );

    let mut broken = bytes.clone();
    broken[0x06] = 7;
    report.check(
        "8.1 code 10.2 hors table refuse",
        package::parse(&broken) == Err(package::PackageError::UnknownUpdateCode),
    );

    // A section that overflows: the manifest announces longer than the
    // file.
    let mut broken = bytes.clone();
    broken[0x14..0x18].copy_from_slice(&(bytes.len() as u32 + 4096).to_le_bytes());
    report.check(
        "8.1 section debordante refusee",
        package::parse(&broken) == Err(package::PackageError::SectionOutOfBounds),
    );

    // Two overlapping sections: the binary points at the manifest.
    let mut broken = bytes.clone();
    let manifest_offset = header.manifest_offset;
    broken[0x18..0x1C].copy_from_slice(&manifest_offset.to_le_bytes());
    report.check(
        "8.1 sections superposees refusees",
        package::parse(&broken) == Err(package::PackageError::SectionOverlap),
    );

    // --- integrity and verdicts ---
    report.check(
        "8.1 paquet intact accepte",
        package::verify(&header, &bytes) == Verdict::SelfSigned,
    );

    let mut tampered = bytes.clone();
    let last = tampered.len() - 1;
    tampered[last] ^= 0xff;
    let tampered_header = package::parse(&tampered).expect("en-tete lisible");
    report.check(
        "8.1 charge utile alteree detectee",
        package::verify(&tampered_header, &tampered)
            == Verdict::Rejected(Reason::PayloadTampered),
    );

    // A combination forbidden by 3.4: a self-signed `.dsa`.
    let forbidden = sample(
        TEST_ID,
        SignatureType::SelfSigned,
        BinaryFormat::Dsa,
        UpdateCode::Silent,
        [1, 0, 0, 0],
        "",
        "",
        &[],
    );
    let forbidden_header = package::parse(&forbidden).expect("en-tete lisible");
    report.check(
        "8.1 combinaison 3.4 interdite refusee",
        package::verify(&forbidden_header, &forbidden)
            == Verdict::Rejected(Reason::ForbiddenCombination),
    );

    // SYSTEM with no digest in the store.
    let official = sample(
        TEST_ID,
        SignatureType::System,
        BinaryFormat::Dsa,
        UpdateCode::Silent,
        [1, 0, 0, 0],
        "",
        "",
        &[],
    );
    let official_header = package::parse(&official).expect("en-tete lisible");
    report.check(
        "8.1 SYSTEM inconnu refuse",
        package::verify(&official_header, &official)
            == Verdict::Rejected(Reason::UnknownSystemBinary),
    );

    // The same, once its digest is registered: it is the path a binary
    // shipped with the OS takes.
    crate::binfmt::signature::register_system_digest(package::authenticity_digest(&official));
    report.check(
        "8.1 SYSTEM enregistre accepte",
        package::verify(&official_header, &official) == Verdict::System,
    );

    // ENTERPRISE: unknown authority, then known. A real key pair, drawn
    // for this boot: its public key enters the store only after the
    // first check.
    let mut seed = [0u8; 32];
    crate::random::fill(&mut seed);
    let authority = crate::crypto::ed25519::public_key(&seed);
    let manifest = manifest_for(TEST_ID, &[]);
    let certified = package::build(&package::BuildSpec {
        update_code: UpdateCode::Silent,
        signature: SignatureType::Enterprise,
        format: BinaryFormat::Cus,
        flags: 0,
        version: [2, 0, 0, 0],
        display_name: "Exemple certifie",
        publisher: "Editeur",
        manifest: &manifest,
        binary: &[0x90; 16],
        resources: &[],
        pre_script: "",
        post_script: "",
        authority_seed: Some(seed),
    });
    let certified_header = package::parse(&certified).expect("en-tete lisible");
    report.check(
        "8.1 autorite inconnue refusee",
        package::verify(&certified_header, &certified)
            == Verdict::Rejected(Reason::UntrustedAuthority),
    );
    crate::binfmt::signature::register_enterprise_root(authority);
    report.check(
        "8.1 autorite connue acceptee",
        package::verify(&certified_header, &certified) == Verdict::Enterprise,
    );

    // The signature is attached to *this* package: copying it onto
    // another must not verify.
    let mut moved = ordinary(TEST_ID, [3, 0, 0, 0], &[]);
    moved[0x07] = SignatureType::Enterprise as u8;
    moved[0x100..0x120].copy_from_slice(&authority);
    moved[0xC0..0x100].copy_from_slice(&certified_header.signature_bytes);
    let payload = package::payload_digest(&moved);
    moved[0xA0..0xA0 + 32].copy_from_slice(&payload);
    if let Ok(moved_header) = package::parse(&moved) {
        report.check(
            "8.1 liaison de certificat non transferable",
            package::verify(&moved_header, &moved) == Verdict::Rejected(Reason::BindingMismatch),
        );
    } else {
        report.check("8.1 liaison de certificat non transferable", false);
    }

    // 15.0.3: a version 1 ENTERPRISE package (binding by hash) is refused
    // by name, a version 1 SELF one still reads.
    let mut old = ordinary(TEST_ID, [1, 0, 0, 0], &[]);
    old[0x04..0x06].copy_from_slice(&1u16.to_le_bytes());
    let parsed_old = package::parse(&old);
    report.check("15.0.3 paquet v1 encore lu", parsed_old.is_ok());
    old[0x07] = SignatureType::Enterprise as u8;
    let payload = package::payload_digest(&old);
    old[0xA0..0xA0 + 32].copy_from_slice(&payload);
    if let Ok(old_header) = package::parse(&old) {
        report.check(
            "15.0.3 paquet ENTERPRISE v1 refuse",
            package::verify(&old_header, &old) == Verdict::Rejected(Reason::ObsoleteCertificate),
        );
    } else {
        report.check("15.0.3 paquet ENTERPRISE v1 refuse", false);
    }
}

// ============================================================
//  10.2 -- Update codes
// ============================================================

fn test_update_codes(report: &mut Report) {
    // Transcription of the table, line by line. No rule derived: code -1
    // is the only one that may reboot, and nothing in the order of the
    // numbers gave that away.
    let table = [
        (UpdateCode::Mandatory, -1i8, false, true, true, false),
        (UpdateCode::Standard, 0, true, true, false, true),
        (UpdateCode::Optional, 1, true, true, false, false),
        (UpdateCode::Silent, 2, true, false, false, false),
    ];

    for (code, number, dismissible, notifies, reboot, confirm) in table {
        report.check(
            &alloc::format!("10.2 code {}", number),
            code.code() == number && UpdateCode::from_code(number) == Some(code),
        );
        report.check(
            &alloc::format!("10.2 ignorable, code {}", number),
            code.dismissible() == dismissible,
        );
        report.check(
            &alloc::format!("10.2 notification, code {}", number),
            code.notifies() == notifies,
        );
        report.check(
            &alloc::format!("10.2 redemarrage, code {}", number),
            code.may_require_reboot() == reboot,
        );
        report.check(
            &alloc::format!("10.2 confirmation, code {}", number),
            code.needs_confirmation() == confirm,
        );
    }

    report.check(
        "10.2 code hors table",
        UpdateCode::from_code(3).is_none() && UpdateCode::from_code(-2).is_none(),
    );
}

// ============================================================
//  10.4 -- Warning levels
// ============================================================

fn test_warning_levels(report: &mut Report) {
    use BinaryFormat::*;
    use SignatureType::*;

    // The five lines of the table, on the formats 3.4 allows for each
    // signature.
    let table = [
        (System, Dsa, WarningLevel::OfficialBadge),
        (System, Ss, WarningLevel::OfficialBadge),
        (System, Cus, WarningLevel::OfficialBadge),
        (Enterprise, Cus, WarningLevel::VerifiedBadge),
        (Enterprise, Gs, WarningLevel::VerifiedBadge),
        (Enterprise, Ss, WarningLevel::AdminPrompt),
        (SelfSigned, Gs, WarningLevel::Unverified),
        (SelfSigned, Cus, WarningLevel::Unverified),
        (None, Ras, WarningLevel::Dangerous),
    ];

    for (signature, format, expected) in table {
        report.check(
            &alloc::format!(
                "10.4 {} + .{}",
                signature.as_str(),
                format.extension()
            ),
            WarningLevel::of(signature, format) == expected,
        );
    }

    // The actions: two lines demand an answer, only one demands the
    // administrator role, two display nothing.
    report.check(
        "10.4 badge officiel sans avertissement",
        !WarningLevel::OfficialBadge.warns() && !WarningLevel::OfficialBadge.requires_consent(),
    );
    report.check(
        "10.4 badge verifie sans avertissement",
        !WarningLevel::VerifiedBadge.warns(),
    );
    report.check(
        "10.4 prompt admin obligatoire",
        WarningLevel::AdminPrompt.requires_consent() && WarningLevel::AdminPrompt.requires_admin(),
    );
    report.check(
        "10.4 jaune avertit sans bloquer",
        WarningLevel::Unverified.warns() && !WarningLevel::Unverified.requires_consent(),
    );
    report.check(
        "10.4 rouge bloquant",
        WarningLevel::Dangerous.warns()
            && WarningLevel::Dangerous.requires_consent()
            && !WarningLevel::Dangerous.requires_admin(),
    );

    // Choice left open by the spec, recorded in `WarningLevel::of`: an
    // absence of signature is dangerous whatever the format.
    report.check(
        "10.4 aucune signature sur .cus - rouge bloquant",
        WarningLevel::of(None, Cus) == WarningLevel::Dangerous,
    );
}

// ============================================================
//  8.1.3 -- Scripts
// ============================================================

fn test_scripts(report: &mut Report, now: u64) {
    let who = Credentials::kernel();
    let root = alloc::format!("/apps/{}", TEST_ID);
    let _ = crate::fs::create_directory(&root, who, Mode::system_dir(), now);
    let roots = alloc::vec![root.clone()];

    // Parsing.
    let program = script::parse("mkdir /a\nwrite /b contenu\ncopy /a /b\ndelete /c\nrequire /d\necho salut\n");
    report.check("8.1.3 six verbes reconnus", matches!(&program, Ok(list) if list.len() == 6));

    report.check(
        "8.1.3 verbe inconnu refuse",
        matches!(script::parse("exec /bin/sh"), Err((1, script::ScriptError::UnknownVerb))),
    );
    report.check(
        "8.1.3 argument manquant refuse",
        matches!(script::parse("mkdir"), Err((1, script::ScriptError::MissingArgument))),
    );
    report.check(
        "8.1.3 write sans contenu refuse",
        matches!(script::parse("write /a"), Err((1, script::ScriptError::MissingArgument))),
    );
    report.check(
        "8.1.3 commentaires et lignes vides ignores",
        matches!(script::parse("# commentaire\n\n  \necho x\n"), Ok(list) if list.len() == 1),
    );

    // Confinement.
    report.check(
        "8.1.3 chemin interne accepte",
        script::within(&alloc::format!("{}/config.cfg", root), &roots),
    );
    report.check(
        "8.1.3 chemin externe refuse",
        !script::within("/sys/gui/modules/compositor_core.mod", &roots),
    );
    report.check(
        "8.1.3 remontee par .. refusee",
        !script::within(&alloc::format!("{}/../../sys/kernel", root), &roots),
    );
    report.check(
        "8.1.3 prefixe textuel insuffisant",
        !script::within(&alloc::format!("{}-autre/fichier", root), &roots),
    );
    report.check(
        "8.1.3 racine elle-meme acceptee",
        script::within(&root, &roots),
    );

    // Real execution.
    let program = alloc::format!(
        "echo installation\nmkdir {root}/donnees\nwrite {root}/donnees/config.cfg cle=valeur\nrequire {root}/donnees/config.cfg\n",
        root = root
    );
    let outcome = script::execute(&program, &roots, now);
    report.check("8.1.3 script execute en entier", outcome.executed == 4);
    report.check("8.1.3 script sans echec", outcome.succeeded());
    report.check(
        "8.1.3 fichier reellement ecrit",
        crate::fs::read_file(&alloc::format!("{}/donnees/config.cfg", root), who)
            .map(|bytes| bytes == b"cle=valeur")
            .unwrap_or(false),
    );
    report.check(
        "8.1.3 creations enregistrees",
        outcome.created.len() == 2,
    );

    // The confinement is checked before any execution: the first
    // instruction must not have happened if the second overflows.
    let evasion = alloc::format!(
        "write {root}/temoin.txt marqueur\nwrite /sys/config/vole.cfg pirate\n",
        root = root
    );
    let outcome = script::execute(&evasion, &roots, now);
    report.check(
        "8.1.3 evasion refusee",
        outcome
            .failures
            .first()
            .map(|failure| failure.error == script::ScriptError::OutsideSandbox)
            .unwrap_or(false),
    );
    report.check("8.1.3 evasion : rien execute", outcome.executed == 0);
    report.check(
        "8.1.3 evasion : temoin absent",
        crate::fs::stat(&alloc::format!("{}/temoin.txt", root), who).is_err(),
    );
    report.check(
        "8.1.3 evasion : cible externe intacte",
        crate::fs::stat("/sys/config/vole.cfg", who).is_err(),
    );

    // A missing prerequisite stops the script.
    let outcome = script::execute(
        &alloc::format!("require {}/absent.txt\necho jamais\n", root),
        &roots,
        now,
    );
    report.check(
        "8.1.3 prerequis absent arrete",
        outcome.executed == 0
            && outcome
                .failures
                .first()
                .map(|failure| failure.error == script::ScriptError::RequirementMissing)
                .unwrap_or(false),
    );

    // Cleanup of the sandbox of this test.
    let _ = crate::fs::delete(&alloc::format!("{}/donnees/config.cfg", root), who, now);
    let _ = crate::fs::delete(&alloc::format!("{}/donnees", root), who, now);
    let _ = crate::fs::delete(&root, who, now);
}

// ============================================================
//  8.3 -- Repositories
// ============================================================

/// Publishes a complete local repository: two packages, a sealed index.
fn publish_test_repo(now: u64) -> Option<(repo::Repo, Vec<u8>, Vec<u8>)> {
    let who = Credentials::kernel();
    let library = ordinary(TEST_DEP, [1, 0, 0, 0], &[]);
    let application = ordinary(TEST_ID, [1, 0, 0, 0], &[TEST_DEP]);

    let library_path = alloc::format!("{}/{}.spm", super::LOCAL_REPO_DIR, TEST_DEP);
    let application_path = alloc::format!("{}/{}.spm", super::LOCAL_REPO_DIR, TEST_ID);

    let _ = crate::fs::delete(&library_path, who, now);
    let _ = crate::fs::delete(&application_path, who, now);
    crate::fs::write_file(&library_path, &library, who, Mode::system_file(), now).ok()?;
    crate::fs::write_file(&application_path, &application, who, Mode::system_file(), now).ok()?;

    let entries = alloc::vec![
        PackageEntry {
            app_id: String::from(TEST_DEP),
            version: [1, 0, 0, 0],
            format: BinaryFormat::Cus,
            signature: SignatureType::SelfSigned,
            update_code: UpdateCode::Silent,
            size: library.len() as u32,
            digest: sha256::digest(&library),
            path: alloc::format!("{}.spm", TEST_DEP),
        },
        PackageEntry {
            app_id: String::from(TEST_ID),
            version: [1, 0, 0, 0],
            format: BinaryFormat::Cus,
            signature: SignatureType::SelfSigned,
            update_code: UpdateCode::Silent,
            size: application.len() as u32,
            digest: sha256::digest(&application),
            path: alloc::format!("{}.spm", TEST_ID),
        },
    ];

    let os_entries = alloc::vec![OsEntry {
        version: [99, 0, 0, 0],
        kind: OsUpdateKind::Patch,
        size: 0,
        digest: [0u8; 32],
        path: String::from("nova-99.0.0.0.spm"),
    }];

    let anchor = super::local_anchor();
    let index = repo::build_index(&entries, &os_entries, Some(&anchor));
    let index_path = alloc::format!("{}/index.spmi", super::LOCAL_REPO_DIR);
    let _ = crate::fs::delete(&index_path, who, now);
    crate::fs::write_file(&index_path, index.as_bytes(), who, Mode::system_file(), now).ok()?;

    let declared = repo::load()
        .into_iter()
        .find(|item| item.name == super::LOCAL_REPO_NAME)?;

    Some((declared, application, library))
}

fn test_repo(report: &mut Report, now: u64) {
    // Hexadecimal encoding: round trip, and refusal of a value that is
    // not one.
    let digest = sha256::digest(b"empreinte");
    report.check(
        "8.3 hexadecimal aller-retour",
        repo::unhex(&repo::hex(&digest)) == Some(digest),
    );
    report.check(
        "8.3 hexadecimal trop court refuse",
        repo::unhex("00ff").is_none(),
    );
    report.check(
        "8.3 caractere non hexadecimal refuse",
        repo::unhex(&alloc::format!("{}zz", &repo::hex(&digest)[..62])).is_none(),
    );

    // Index: build then parse.
    let anchor = super::local_anchor();
    let entry = PackageEntry {
        app_id: String::from(TEST_ID),
        version: [2, 1, 0, 0],
        format: BinaryFormat::Gs,
        signature: SignatureType::Enterprise,
        update_code: UpdateCode::Optional,
        size: 1234,
        digest,
        path: String::from("jeu.spm"),
    };
    let os = OsEntry {
        version: [1, 0, 0, 0],
        kind: OsUpdateKind::Major,
        size: 4096,
        digest,
        path: String::from("nova-1.0.0.0.spm"),
    };
    let text = repo::build_index(core::slice::from_ref(&entry), core::slice::from_ref(&os), Some(&anchor));

    match repo::parse_index(&text) {
        Ok(index) => {
            report.check("8.3.1 une application indexee", index.packages.len() == 1);
            report.check("8.3.1 une MAJ OS indexee", index.os.len() == 1);
            report.check(
                "8.3.1 entree restituee a l'identique",
                index.packages.first() == Some(&entry),
            );
            report.check("8.3.1 entree OS restituee", index.os.first() == Some(&os));
            report.check("8.3.1 sceau present", index.seal.is_some());

            let declared = repo::Repo {
                name: String::from("test"),
                url: String::from("file:/inexistant"),
                anchor,
            };
            report.check(
                "8.3.2 sceau conforme accepte",
                repo::check_seal(&declared, &text, &index).is_ok(),
            );

            // A modified index no longer reproduces the seal.
            let altered = text.replace("jeu.spm", "piege.spm");
            let altered_index = repo::parse_index(&altered).expect("index analysable");
            report.check(
                "8.3.2 index altere refuse",
                repo::check_seal(&declared, &altered, &altered_index)
                    == Err(repo::RepoError::SealMismatch),
            );

            // A different anchor does not validate the same index.
            let other = repo::Repo {
                name: String::from("test"),
                url: String::from("file:/inexistant"),
                anchor: sha256::digest(b"autre ancre"),
            };
            report.check(
                "8.3.2 ancre etrangere refusee",
                repo::check_seal(&other, &text, &index) == Err(repo::RepoError::SealMismatch),
            );

            // An anchored repository demands an `ancre` line.
            let unsealed = repo::build_index(core::slice::from_ref(&entry), &[], Option::None);
            let unsealed_index = repo::parse_index(&unsealed).expect("index analysable");
            report.check(
                "8.3.2 sceau absent refuse",
                repo::check_seal(&declared, &unsealed, &unsealed_index)
                    == Err(repo::RepoError::SealMissing),
            );
        }
        Err(error) => report.check(
            &alloc::format!("8.3.1 index analysable ({})", error.message()),
            false,
        ),
    }

    report.check(
        "8.3.1 en-tete absent refuse",
        repo::parse_index("pkg|a|1.0|4|1|2|10|00|a.spm\n")
            == Err(repo::RepoError::MalformedIndex),
    );

    // A line of an unknown type is ignored, not fatal: it is what lets a
    // newer repository stay readable.
    let forward = alloc::format!("{}\nfutur|quelque chose\n", repo::INDEX_HEADER);
    report.check(
        "8.3.1 enregistrement inconnu ignore",
        matches!(repo::parse_index(&forward), Ok(index) if index.packages.is_empty()),
    );

    // Transport.
    report.check(
        "8.3 schema inconnu refuse",
        repo::fetch("ftp://exemple/index", 1024) == Err(repo::RepoError::UnknownScheme),
    );
    report.check(
        "8.3 fichier absent refuse",
        repo::fetch("file:/sys/tools/spm/absent", 1024) == Err(repo::RepoError::NotFound),
    );

    // The local repository shipped with the system must be readable.
    let Some((declared, application, _)) = publish_test_repo(now) else {
        report.check("8.3 depot local publiable", false);
        return;
    };
    report.check("8.3 depot local declare", declared.has_anchor());

    match repo::fetch_index(&declared) {
        Ok(index) => {
            report.check(
                "8.3 index du depot local recupere et scelle",
                index.packages.len() == 2,
            );

            // 8.3.2 -- the file received must be the one the index announces.
            let Some(entry) = index
                .packages
                .iter()
                .find(|item| item.app_id == TEST_ID)
                .cloned()
            else {
                report.check("8.3 paquet present dans l'index", false);
                return;
            };
            report.check(
                "8.3.2 paquet recupere conforme",
                repo::fetch_package(&declared, &entry)
                    .map(|bytes| bytes == application)
                    .unwrap_or(false),
            );

            // Wrong digest in the index: the fetch must fail.
            let mut lying = entry.clone();
            lying.digest = sha256::digest(b"pas la bonne");
            report.check(
                "8.3.2 empreinte fausse detectee",
                repo::fetch_package(&declared, &lying) == Err(repo::RepoError::DigestMismatch),
            );

            // Wrong size: detected before the hashing even starts.
            let mut wrong_size = entry.clone();
            wrong_size.size += 1;
            report.check(
                "8.3.2 taille fausse detectee",
                repo::fetch_package(&declared, &wrong_size)
                    == Err(repo::RepoError::DigestMismatch),
            );

            report.check(
                "8.2.3 recherche dans les depots",
                repo::search(TEST_ID).iter().any(|(_, item)| item.app_id == TEST_ID),
            );
            report.check(
                "8.2.3 recherche sans resultat",
                repo::search("motif.qui.n.existe.pas").is_empty(),
            );
            report.check(
                "8.3 resolution rend la meilleure version",
                repo::resolve(TEST_ID)
                    .map(|(_, item)| item.version == [1, 0, 0, 0])
                    .unwrap_or(false),
            );
        }
        Err(error) => report.check(
            &alloc::format!("8.3 index du depot local ({})", error.message()),
            false,
        ),
    }
}

// ============================================================
//  8.2 -- The manager
// ============================================================

fn test_manager(report: &mut Report, now: u64) {
    use super::manager::{self, Output, SpmError};

    let scope = Scope::Global;

    // A package one of whose dependencies is missing must lay nothing down.
    let before = count_entries("/apps");
    let orphan = ordinary(TEST_ID, [1, 0, 0, 0], &[TEST_DEP]);
    let result = manager::install_bytes(&orphan, scope.clone(), "test", now);
    report.check(
        "8.2.1 dependance absente refusee",
        matches!(&result, Err(SpmError::MissingDependency(app)) if app == TEST_DEP),
    );
    report.check(
        "8.2.1 refus sans trace sur le disque",
        count_entries("/apps") == before,
    );

    // The dependency, then the application.
    let library = ordinary(TEST_DEP, [1, 0, 0, 0], &[]);
    let installed_library = manager::install_bytes(&library, scope.clone(), "test", now);
    report.check(
        "8.2.1 installation d'une dependance",
        installed_library.is_ok(),
    );

    let installed = match manager::install_bytes(&orphan, scope.clone(), "test", now) {
        Ok(installed) => installed,
        Err(error) => {
            report.check(
                &alloc::format!("8.2.1 installation ({})", error.message()),
                false,
            );
            return;
        }
    };

    report.check("8.2.1 app_id enregistre", installed.app_id == TEST_ID);
    report.check(
        "8.2.1 prefixe deduit du format",
        installed.prefix == alloc::format!("/apps/{}", TEST_ID),
    );
    report.check(
        "8.2.1 avertissement jaune pour un auto-signe",
        installed.warning == WarningLevel::Unverified,
    );
    report.check(
        "8.2.1 verdict auto-signe",
        installed.verdict == Verdict::SelfSigned,
    );
    report.check(
        "8.2.1 statut complet",
        installed.status == db::Status::Installed,
    );

    let who = Credentials::kernel();
    report.check(
        "8.2.1 binaire pose",
        crate::fs::stat(&alloc::format!("{}/exemple.cus", installed.prefix), who).is_ok(),
    );
    report.check(
        "8.2.1 ressources posees",
        crate::fs::stat(&alloc::format!("{}/icone.bin", installed.prefix), who).is_ok()
            && crate::fs::stat(&alloc::format!("{}/licence.txt", installed.prefix), who).is_ok(),
    );
    report.check(
        "8.2.1 manifeste conserve a cote du binaire",
        crate::fs::stat(&alloc::format!("{}/manifeste.cfg", installed.prefix), who).is_ok(),
    );

    // Database.
    let entry = db::find(&scope, TEST_ID);
    report.check("8.2.1 entree en base", entry.is_some());
    if let Some(entry) = &entry {
        report.check("8.2.1 version en base", entry.version == [1, 0, 0, 0]);
        report.check("8.2.1 provenance en base", entry.origin == "test");
        report.check(
            "8.2.1 format en base",
            entry.format == BinaryFormat::Cus,
        );
    }
    report.check(
        "8.2.1 liste de fichiers tenue",
        db::files_of(&scope, TEST_ID).len() >= 4,
    );

    // A second pass must be refused.
    report.check(
        "8.2.1 reinstallation refusee",
        matches!(
            manager::install_bytes(&orphan, scope.clone(), "test", now),
            Err(SpmError::AlreadyInstalled(_))
        ),
    );

    // A package whose post script fails is marked partial, without the
    // installation being cancelled.
    let with_bad_post = sample(
        "test.spm.partiel",
        SignatureType::SelfSigned,
        BinaryFormat::Cus,
        UpdateCode::Silent,
        [1, 0, 0, 0],
        "",
        "require /apps/test.spm.partiel/inexistant\n",
        &[],
    );
    match manager::install_bytes(&with_bad_post, scope.clone(), "test", now) {
        Ok(partial) => {
            report.check(
                "8.2.1 script post en echec marque partiel",
                partial.status == db::Status::Partial,
            );
            report.check(
                "8.2.1 fichiers poses malgre le script post",
                crate::fs::stat(&alloc::format!("{}/exemple.cus", partial.prefix), who).is_ok(),
            );
            let _ = manager::remove("test.spm.partiel", scope.clone(), now);
        }
        Err(_) => report.check("8.2.1 script post en echec marque partiel", false),
    }

    // A pre script that overflows cancels everything.
    let before = count_entries("/apps");
    let evasive = sample(
        "test.spm.evasif",
        SignatureType::SelfSigned,
        BinaryFormat::Cus,
        UpdateCode::Silent,
        [1, 0, 0, 0],
        "write /sys/config/pirate.cfg vole\n",
        "",
        &[],
    );
    report.check(
        "8.2.1 script pre debordant annule l'installation",
        matches!(
            manager::install_bytes(&evasive, scope.clone(), "test", now),
            Err(SpmError::PreScriptFailed(_))
        ),
    );
    report.check(
        "8.2.1 annulation sans trace",
        count_entries("/apps") == before
            && crate::fs::stat("/sys/config/pirate.cfg", who).is_err(),
    );

    // 8.2.5 -- the commands of 10.3, literally.
    report.check(
        "10.3 spm search",
        matches!(
            manager::execute(&alloc::format!("spm search {}", TEST_ID), now),
            Ok(Output::Results(found)) if found.iter().any(|item| item.app_id == TEST_ID && item.installed)
        ),
    );
    report.check(
        "10.3 spm update",
        matches!(manager::execute("spm update", now), Ok(Output::Updates(_))),
    );
    report.check(
        "10.3 spm repo add",
        matches!(
            manager::execute("spm repo add file:/sys/tools/spm/tiers", now),
            Ok(Output::RepoAdded(name)) if name == "tiers"
        ),
    );
    report.check(
        "10.3 spm repo add deux fois",
        matches!(
            manager::execute("spm repo add file:/sys/tools/spm/tiers", now),
            Ok(Output::RepoExists(_))
        ),
    );
    let _ = repo::remove("tiers", now);

    report.check(
        "10.3 verbe inconnu refuse",
        matches!(manager::execute("spm frobnique paquet", now), Err(SpmError::BadCommand)),
    );
    report.check(
        "10.3 commande sans le prefixe spm refusee",
        matches!(manager::execute("install firefox", now), Err(SpmError::BadCommand)),
    );
    report.check(
        "10.3 install sans argument refuse",
        matches!(manager::execute("spm install", now), Err(SpmError::BadCommand)),
    );

    // From a repository, complete path: index, fetch, verification,
    // laying down. The local package is removed first.
    let _ = manager::remove(TEST_ID, scope.clone(), now);
    match manager::execute(&alloc::format!("spm install {}", TEST_ID), now) {
        Ok(Output::Installed(from_repo)) => {
            report.check(
                "10.3 spm install depuis un depot",
                from_repo.app_id == TEST_ID,
            );
            report.check(
                "8.3 provenance enregistree",
                db::find(&scope, TEST_ID)
                    .map(|entry| entry.origin == super::LOCAL_REPO_NAME)
                    .unwrap_or(false),
            );
        }
        other => {
            let _ = other;
            report.check("10.3 spm install depuis un depot", false);
        }
    }

    // 8.2.2 -- uninstallation.
    let files = db::files_of(&scope, TEST_ID);
    match manager::execute(&alloc::format!("spm remove {}", TEST_ID), now) {
        Ok(Output::Removed(removed)) => {
            report.check(
                "8.2.2 tous les fichiers supprimes",
                removed.removed_files >= files.len() && removed.kept_files == 0,
            );
        }
        _ => report.check("8.2.2 desinstallation", false),
    }
    report.check(
        "8.2.2 entree retiree de la base",
        db::find(&scope, TEST_ID).is_none(),
    );
    report.check(
        "8.2.2 prefixe efface",
        crate::fs::stat(&alloc::format!("/apps/{}", TEST_ID), who).is_err(),
    );
    report.check(
        "8.2.2 liste de fichiers oubliee",
        db::files_of(&scope, TEST_ID).is_empty(),
    );
    report.check(
        "8.2.2 desinstallation d'un paquet absent refusee",
        matches!(
            manager::remove(TEST_ID, scope.clone(), now),
            Err(SpmError::NotFound(_))
        ),
    );

    // The dependency is still there: uninstalling a package must not
    // carry off what it depended on.
    report.check(
        "8.2.2 dependance conservee",
        db::find(&scope, TEST_DEP).is_some(),
    );
    let _ = manager::remove(TEST_DEP, scope, now);
}

// ============================================================
//  Package database
// ============================================================

fn test_database(report: &mut Report, now: u64) {
    let scope = Scope::Global;

    report.check(
        "db version courte completee",
        db::parse_version("1.2") == Some([1, 2, 0, 0]),
    );
    report.check(
        "db version complete",
        db::parse_version("3.2.1.7") == Some([3, 2, 1, 7]),
    );
    report.check(
        "db version a cinq champs refusee",
        db::parse_version("1.2.3.4.5").is_none(),
    );
    report.check(
        "db version non numerique refusee",
        db::parse_version("1.x.0.0").is_none(),
    );

    // Round trip of an entry through the disk.
    let entry = db::Entry {
        app_id: String::from(TEST_ID),
        version: [4, 3, 2, 1],
        format: BinaryFormat::Gs,
        signature: SignatureType::Enterprise,
        update_code: UpdateCode::Mandatory,
        prefix: alloc::format!("/games/{}", TEST_ID),
        origin: String::from("depot-test"),
        installed_at: 1234,
        status: db::Status::Partial,
    };
    let saved = db::upsert(&scope, entry.clone(), now).is_ok();
    report.check("db entree ecrite", saved);
    report.check(
        "db entree relue a l'identique",
        db::find(&scope, TEST_ID) == Some(entry.clone()),
    );

    // A second write replaces, it does not append.
    let mut newer = entry.clone();
    newer.version = [5, 0, 0, 0];
    let _ = db::upsert(&scope, newer.clone(), now);
    let matching = db::load(&scope)
        .into_iter()
        .filter(|item| item.app_id == TEST_ID)
        .count();
    report.check("db une seule ligne par paquet", matching == 1);
    report.check(
        "db remplacement effectif",
        db::find(&scope, TEST_ID).map(|item| item.version) == Some([5, 0, 0, 0]),
    );

    report.check(
        "db separateur refuse dans un champ",
        db::upsert(
            &scope,
            db::Entry {
                app_id: String::from("mauvais|identifiant"),
                ..entry.clone()
            },
            now,
        )
        .is_err(),
    );

    report.check(
        "db suppression",
        db::remove(&scope, TEST_ID, now) == Ok(true),
    );
    report.check(
        "db suppression d'une entree absente",
        db::remove(&scope, TEST_ID, now) == Ok(false),
    );

    // Scopes: the personal one precedes the global one.
    let scopes = db::scopes_for(Some("alice"));
    report.check(
        "db portee personnelle prioritaire",
        scopes.first() == Some(&Scope::User(String::from("alice"))),
    );
    report.check(
        "db portee globale en second",
        scopes.get(1) == Some(&Scope::Global),
    );
    report.check(
        "db racine personnelle",
        Scope::User(String::from("alice")).install_root(BinaryFormat::Cus) == "/users/alice/apps",
    );
    report.check(
        "db racine globale par format",
        Scope::Global.install_root(BinaryFormat::Gs) == "/games"
            && Scope::Global.install_root(BinaryFormat::Ras) == "/ras"
            && Scope::Global.install_root(BinaryFormat::Dsa) == "/sysapp",
    );
}

// ============================================================
//  11.1 -- OS update types
// ============================================================

fn test_os_updates(report: &mut Report, now: u64) {
    use super::update::{self, Config};

    // Transcription of the 11.1 table.
    let table = [
        (OsUpdateKind::Patch, 0i8, false, false),
        (OsUpdateKind::Update, 1, false, false),
        (OsUpdateKind::Major, 2, true, true),
        (OsUpdateKind::Critical, -1, false, false),
    ];
    for (kind, code, reboot, new_slot) in table {
        report.check(
            &alloc::format!("11.1 code {}", kind.as_str()),
            kind.code() == code && OsUpdateKind::from_code(code) == Some(kind),
        );
        report.check(
            &alloc::format!("11.1 redemarrage {}", kind.as_str()),
            kind.reboot(false) == reboot,
        );
        report.check(
            &alloc::format!("11.1 nouvel emplacement {}", kind.as_str()),
            kind.uses_new_slot() == new_slot,
        );
    }

    // CRITICAL is the only one whose reboot depends on the package.
    report.check(
        "11.1 CRITICAL suit le paquet",
        !OsUpdateKind::Critical.reboot(false) && OsUpdateKind::Critical.reboot(true),
    );
    report.check(
        "11.1 MAJOR redemarre quoi qu'en dise le paquet",
        OsUpdateKind::Major.reboot(false),
    );

    // The translation to 10.2: no type of OS update is silent.
    report.check(
        "11.1 CRITICAL est obligatoire",
        OsUpdateKind::Critical.notification_code() == UpdateCode::Mandatory,
    );
    report.check(
        "11.1 MAJOR demande confirmation",
        OsUpdateKind::Major.notification_code() == UpdateCode::Standard,
    );
    report.check(
        "11.1 PATCH est optionnel",
        OsUpdateKind::Patch.notification_code() == UpdateCode::Optional,
    );
    report.check(
        "11.1 aucune MAJ OS silencieuse",
        [
            OsUpdateKind::Patch,
            OsUpdateKind::Update,
            OsUpdateKind::Major,
            OsUpdateKind::Critical,
        ]
        .iter()
        .all(|kind| kind.notification_code() != UpdateCode::Silent),
    );

    report.check(
        "11.1 code hors table",
        OsUpdateKind::from_code(3).is_none() && OsUpdateKind::from_code(-2).is_none(),
    );

    // 11.2 -- configuration of the service.
    let config = Config::parse(
        "intervalle_heures=6\nautomatique=0\ntelechargement_arriere_plan=0\n",
    );
    report.check("11.2 intervalle configurable", config.interval_hours == 6);
    report.check("11.2 verification desactivable", !config.automatic);
    report.check(
        "11.2 telechargement desactivable",
        !config.background_download,
    );
    report.check(
        "11.2 intervalle par defaut de 24 h",
        Config::default().interval_hours == update::DEFAULT_INTERVAL_HOURS,
    );
    report.check(
        "11.2 intervalle nul borne a 1 h",
        Config::parse("intervalle_heures=0\n").interval_hours == 1,
    );
    report.check(
        "11.2 configuration idempotente",
        Config::parse(&config.to_cfg()) == config,
    );

    // The configuration file exists on the disk.
    report.check(
        "11.2 configuration ecrite dans /sys/config/",
        crate::fs::stat(update::CONFIG_PATH, Credentials::kernel()).is_ok(),
    );

    // Naming of the dropped files, and read-back of the directory.
    report.check(
        "11.2 nom de fichier de mise a jour",
        update::staging_name(&[1, 2, 3, 4]) == "nova-1.2.3.4.spm",
    );

    let bytes = ordinary("test.spm.os", [1, 0, 0, 0], &[]);
    let staged = update::stage(&[9, 9, 9, 9], &bytes, now);
    report.check("11.2 mise a jour deposee", staged.is_ok());
    report.check(
        "11.2 depot relu depuis le dossier",
        update::pending()
            .iter()
            .any(|(_, version)| *version == [9, 9, 9, 9]),
    );
    if let Ok(path) = staged {
        let _ = crate::fs::delete(&path, Credentials::kernel(), now);
    }
    report.check(
        "11.2 depot retire du dossier",
        !update::pending()
            .iter()
            .any(|(_, version)| *version == [9, 9, 9, 9]),
    );
}

// ============================================================
//  11.3 -- Rollback
// ============================================================

fn test_rollback(report: &mut Report, now: u64) {
    use rollback::{Cause, Slot, State};

    // Slots.
    report.check("11.3 deux emplacements", Slot::A.other() == Slot::B && Slot::B.other() == Slot::A);
    report.check(
        "11.3 emplacement A est le volume de demarrage",
        Slot::A.root() == "/" && Slot::A.is_separate_volume(),
    );

    // State: round trip through the text.
    let state = State {
        current: [2, 0, 0, 0],
        previous: Some([1, 9, 0, 0]),
        slot: Slot::B,
        awaiting_validation: true,
        attempts: 1,
        applied_at: 555,
        kind: Some(OsUpdateKind::Major),
        staged: Some(String::from("/sys/updates/nova-2.0.0.0.spm")),
    };
    report.check(
        "11.3 etat idempotent",
        State::parse(&state.to_cfg()) == state,
    );
    report.check(
        "11.3 etat par defaut sur la version courante",
        State::default().current == super::update::OS_VERSION,
    );

    // Save of the real state to restore it at the end: the self-test must
    // not leave the system believing an update is waiting for it.
    let original = rollback::state();

    // State machine: apply a MAJOR, fail, come back.
    let applied = rollback::apply(OsUpdateKind::Major, [9, 9, 9, 9], &[], now);
    report.check("11.3 MAJOR appliquee", applied);

    let after = rollback::state();
    report.check("11.3 version basculee", after.current == [9, 9, 9, 9]);
    report.check(
        "11.3 version precedente conservee",
        after.previous == Some(original.current),
    );
    report.check("11.3 emplacement bascule", after.slot == original.slot.other());
    report.check("11.3 validation exigee", after.awaiting_validation);
    report.check("11.3 compteur d'essais remis a zero", after.attempts == 0);

    // 8.5.2 -- the validation window increments before waiting.
    report.check("11.3 premier essai autorise", rollback::begin_validation(now));
    report.check("11.3 essai compte", rollback::state().attempts == 1);
    report.check("11.3 second essai autorise", rollback::begin_validation(now));
    report.check(
        "11.3 troisieme essai refuse",
        !rollback::begin_validation(now),
    );

    // 8.5.3 -- rollback.
    report.check(
        "11.3 rollback effectue",
        rollback::rollback(Cause::CoreTimeout, now),
    );
    let restored = rollback::state();
    report.check("11.3 version restauree", restored.current == original.current);
    report.check("11.3 emplacement restaure", restored.slot == original.slot);
    report.check("11.3 validation levee", !restored.awaiting_validation);
    report.check("11.3 essais remis a zero", restored.attempts == 0);
    report.check(
        "11.3 plus de version precedente",
        restored.previous.is_none(),
    );

    // A rollback with no previous version must do nothing.
    report.check(
        "11.3 rollback sans recours refuse",
        !rollback::rollback(Cause::CoreFailed, now),
    );

    // Every cause has its explanation (11.3 demands it).
    for cause in [Cause::CoreTimeout, Cause::CoreFailed, Cause::AttemptsExhausted] {
        report.check(
            "11.3 cause expliquee",
            !cause.message().is_empty() && cause.message().is_ascii(),
        );
    }

    // 8.5.4 -- snapshots.
    let who = Credentials::kernel();
    let witness = "/sys/tools/spm/temoin-snapshot.cfg";
    let _ = crate::fs::write_file(witness, b"origine", who, Mode::system_file(), now);

    let taken = rollback::snapshot("test", &alloc::vec![String::from(witness)], now);
    report.check("11.3 snapshot pris", taken.is_ok());

    if let Ok(directory) = taken {
        report.check(
            "11.3 manifeste du snapshot",
            crate::fs::stat(&alloc::format!("{}/manifeste.txt", directory), who).is_ok(),
        );

        // The file is modified, then restored.
        let _ = crate::fs::delete(witness, who, now);
        let _ = crate::fs::write_file(witness, b"modifie", who, Mode::system_file(), now);

        let found = rollback::snapshots()
            .into_iter()
            .find(|snapshot| snapshot.path == directory);
        report.check("11.3 snapshot recense", found.is_some());

        if let Some(snapshot) = found {
            report.check("11.3 restauration", rollback::restore(&snapshot, now) == 1);
            report.check(
                "11.3 contenu d'origine retrouve",
                crate::fs::read_file(witness, who)
                    .map(|bytes| bytes == b"origine")
                    .unwrap_or(false),
            );
            report.check("11.3 snapshot supprimable", rollback::discard(&snapshot, now));
        }
    }
    let _ = crate::fs::delete(witness, who, now);

    report.check(
        "11.3 retention de 30 jours",
        rollback::RETENTION_SECONDS == 30 * 24 * 3600,
    );
    report.check(
        "11.3 echeance de validation de 60 s",
        rollback::VALIDATION_TIMEOUT_MS == 60_000,
    );

    // The original state is put back in place.
    let _ = rollback::save_state(&original, now);
    report.check(
        "11.3 etat du systeme restaure",
        rollback::state() == original,
    );
}

// ============================================================
//  Entry point
// ============================================================

pub fn run() -> (usize, usize) {
    let mut report = Report {
        passed: 0,
        failed: 0,
    };

    if !crate::fs::is_ready() {
        crate::serial_println!("[spm] systeme de fichiers absent - autotest phase 8 ignore");
        return (0, 0);
    }

    let now = crate::idt::ticks();

    // On the way in, not only on the way out: a pass interrupted at the
    // wrong moment leaves files behind it, and every later boot would
    // fail on an "already installed" whose cause would be invisible.
    purge(now);

    test_format(&mut report);
    test_update_codes(&mut report);
    test_warning_levels(&mut report);
    test_scripts(&mut report, now);
    test_database(&mut report, now);
    test_repo(&mut report, now);
    test_manager(&mut report, now);
    test_os_updates(&mut report, now);
    test_rollback(&mut report, now);

    purge(now);

    // The exit invariant: nothing the self-test laid down survives.
    report.check(
        "phase 8 sans residu en base",
        db::find(&Scope::Global, TEST_ID).is_none()
            && db::find(&Scope::Global, TEST_DEP).is_none(),
    );
    report.check(
        "phase 8 sans residu sur le disque",
        crate::fs::stat(
            &alloc::format!("/apps/{}", TEST_ID),
            Credentials::kernel(),
        )
        .is_err(),
    );

    (report.passed, report.failed)
}
