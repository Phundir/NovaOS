//! Self-test of phase 15: atomic updates of applications.
//!
//! Everything the proposal promises, on a registry laid on the volume
//! (`file:`), since QEMU gives the guest no server to talk to:
//!
//! - the shared foundations: JSON, the streamed download (on a scripted
//!   connection: redirect, chunks split anywhere, resume, cut);
//! - a version installed apart and made active in one step; a process
//!   started from it; the next version installed **while it runs**; the
//!   old process keeps its version, a new launch gets the new one; the
//!   old version deleted once neither active, previous, nor running;
//! - rollback; refusals (manifest altered, signed by another key,
//!   checksum false, package refused) that leave nothing on the disk;
//! - the requests of an application, for itself only;
//! - the leftovers of an installation cut halfway, cleaned.
//!
//! It purges its tree **on entry** as well as on exit (DECISIONS,
//! invariants): an interrupted pass must not make the next one fail.

use alloc::string::String;
use alloc::vec::Vec;

use crate::binfmt::header::{BinaryFormat, SignatureType};
use crate::fs::perm::{Credentials, Mode};
use crate::json::{Number, Object, Value};
use crate::web::http::{self, Net};

use super::db::{self, Scope};
use super::manager::{self, Output, SpmError};
use super::package::{self, UpdateCode};
use super::registry;
use super::versions;

const APP: &str = "os.nova.essai15";
const REGISTRY_NAME: &str = "essai15";
const BASE: &str = "/cache/autotest15";
const REGISTRY_DIR: &str = "/cache/autotest15/registre";

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
            crate::serial_println!("[maj] ECHEC : {}", label);
        }
    }
}

fn kernel() -> Credentials {
    Credentials::kernel()
}

// ============================================================
//  15.0.4 -- the streamed download, on a scripted connection
// ============================================================

/// A connection that hands over a canned answer, a few bytes at a time:
/// the network splits wherever it likes, and so does this.
struct Scripted {
    data: Vec<u8>,
    at: usize,
    step: usize,
}

impl crate::web::tls::Transport for Scripted {
    fn send(&mut self, _data: &[u8]) -> Result<(), &'static str> {
        Ok(())
    }

    fn recv(&mut self, max: usize) -> Result<Vec<u8>, &'static str> {
        let end = (self.at + self.step.min(max)).min(self.data.len());
        let piece = self.data[self.at..end].to_vec();
        self.at = end;
        Ok(piece)
    }
}

/// Answers each connection with the next script.
struct ScriptNet {
    answers: Vec<Vec<u8>>,
    step: usize,
}

impl Net for ScriptNet {
    type Connection = Scripted;

    fn connect(&mut self, _host: &str, _port: u16) -> Result<Scripted, String> {
        if self.answers.is_empty() {
            return Err(String::from("plus de reponse"));
        }
        Ok(Scripted { data: self.answers.remove(0), at: 0, step: self.step })
    }

    fn now(&self) -> u64 {
        0
    }

    fn random(&mut self, buffer: &mut [u8]) {
        buffer.fill(0x42);
    }
}

fn downloads(report: &mut Report) {
    let url = || crate::web::url::Url::parse("http://depot.essai/paquet.spm").expect("adresse valide");

    // A redirect, then a chunked body whose chunk headers fall across the
    // reads (7 bytes at a time).
    let mut net = ScriptNet {
        answers: alloc::vec![
            b"HTTP/1.1 302 Found\r\nLocation: /ailleurs/paquet.spm\r\n\r\n".to_vec(),
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nNova \r\nb;ext=1\r\nOS en flux.\r\n0\r\nX-Fin: 1\r\n\r\n".to_vec(),
        ],
        step: 7,
    };
    let mut sink = super::fetch::MemorySink::new(1024);
    let done = http::download(&mut net, url(), 0, 1 << 20, &mut sink);
    report.check("redirection suivie puis corps par blocs reassemble", sink.data == b"Nova OS en flux." && done.is_ok());
    report.check(
        "adresse finale apres la redirection",
        done.as_ref().is_ok_and(|d| d.url.target() == "/ailleurs/paquet.spm"),
    );

    // Resume: the server honours the range.
    let mut net = ScriptNet {
        answers: alloc::vec![b"HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 5-9/10\r\nContent-Length: 5\r\n\r\nmonde".to_vec()],
        step: 3,
    };
    let mut sink = super::fetch::MemorySink::new(64);
    sink.data.extend_from_slice(b"Salut");
    let done = http::download(&mut net, url(), 5, 64, &mut sink);
    report.check("reprise par Range", done.as_ref().is_ok_and(|d| d.start == 5) && sink.data == b"Salutmonde");

    // A server that ignores the range sends everything again: the sink
    // starts over.
    let mut net = ScriptNet {
        answers: alloc::vec![b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nSalutmonde".to_vec()],
        step: 4,
    };
    let mut sink = super::fetch::MemorySink::new(64);
    sink.data.extend_from_slice(b"Sal");
    let done = http::download(&mut net, url(), 3, 64, &mut sink);
    report.check("plage ignoree : le fichier recommence", done.is_ok_and(|d| d.start == 0) && sink.data == b"Salutmonde");

    // A cut before the announced end is an error, not a short file.
    let mut net = ScriptNet {
        answers: alloc::vec![b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\nseulement quarante octets avant la coupure".to_vec()],
        step: 16,
    };
    let mut sink = super::fetch::MemorySink::new(1024);
    report.check("coupure avant la fin annoncee signalee", http::download(&mut net, url(), 0, 1024, &mut sink).is_err());

    // Beyond the limit, refused before reading the body.
    let mut net = ScriptNet {
        answers: alloc::vec![b"HTTP/1.1 200 OK\r\nContent-Length: 5000\r\n\r\n".to_vec()],
        step: 64,
    };
    let mut sink = super::fetch::MemorySink::new(1 << 20);
    report.check("taille annoncee au-dela de la limite refusee", http::download(&mut net, url(), 0, 4096, &mut sink).is_err());

    // The decoder on its own: a size line cut in the middle.
    let mut decoder = http::Dechunker::new();
    let mut out = Vec::new();
    let parts: [&[u8]; 4] = [b"1", b"0\r\n0123456789abcdef\r", b"\n0\r", b"\n\r\n"];
    let ok = parts.iter().all(|part| decoder.feed(part, &mut out).is_ok());
    report.check("decodeur par blocs : en-tete coupe", ok && decoder.done() && out == b"0123456789abcdef");
}

// ============================================================
//  The application under test
// ============================================================

/// A ring 3 program that sleeps 20 ms at a time until it is killed:
/// `mov eax, SYS_TIME_SLEEP_MS ; mov edi, 20 ; syscall ; jmp back`.
static SLEEPER: [u8; 14] = [0xB8, 0x06, 0x07, 0x00, 0x00, 0xBF, 0x14, 0x00, 0x00, 0x00, 0x0F, 0x05, 0xEB, 0xF2];

fn package_of(version: [u16; 4], format: BinaryFormat, signature: SignatureType) -> Vec<u8> {
    let mut spec = crate::binfmt::sample::BuildSpec::minimal(format, signature);
    spec.app_id = APP;
    spec.display_name = "Essai 15";
    spec.code = Some(&SLEEPER);
    spec.data_size = 64;
    spec.bss_size = 0;
    spec.stack_size = 16 * 1024;
    let binary = crate::binfmt::sample::build(&spec);
    let manifest = package::Manifest {
        app_id: String::from(APP),
        name: String::from("Essai 15"),
        publisher: String::from("Nova OS"),
        description: String::new(),
        binary: String::from("essai15.cus"),
        prefix: String::new(),
        depends: Vec::new(),
        extra: Vec::new(),
    };
    package::build(&package::BuildSpec {
        update_code: UpdateCode::Standard,
        signature,
        format,
        flags: 0,
        version,
        display_name: "Essai 15",
        publisher: "Nova OS",
        manifest: &manifest,
        binary: &binary,
        resources: &[],
        pre_script: "",
        post_script: "",
        authority_seed: None,
    })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| alloc::format!("{:02x}", b)).collect()
}

/// Writes the package of `version` into the registry and publishes a
/// manifest whose release channel names it, signed with `seed`.
/// `tamper`: alters the manifest after signing; `bad_checksum`: announces
/// another digest.
fn publish(seed: &[u8; 32], version: [u16; 4], package: &[u8], tamper: bool, bad_checksum: bool, now: u64) {
    let text = versions::version_text(&version);
    let file = alloc::format!("essai15-{}.spm", text);
    let path = alloc::format!("{}/{}", REGISTRY_DIR, file);
    let _ = crate::fs::delete(&path, kernel(), now);
    let _ = crate::fs::write_file(&path, package, kernel(), Mode::shared_file(), now);

    let mut digest = crate::binfmt::sha256::digest(package);
    if bad_checksum {
        digest[0] ^= 0xFF;
    }
    let mut release = Object::new();
    release.insert("version", Value::String(text.clone()));
    release.insert("code", Value::Number(Number::from_i64(0)));
    release.insert("size", Value::Number(Number::from_u64(package.len() as u64)));
    release.insert("checksum", Value::String(alloc::format!("sha256-{}", hex(&digest))));
    release.insert("download_url", Value::String(file));
    let mut channels = Object::new();
    channels.insert("release", Value::Object(release));
    let mut manifest = Object::new();
    manifest.insert("app_name", Value::String(String::from("Essai 15")));
    manifest.insert("app_id", Value::String(String::from(APP)));
    manifest.insert("current_stable_version", Value::String(text));
    manifest.insert("channels", Value::Object(channels));

    let document = Value::Object(manifest.clone());
    let signed = crate::crypto::ed25519::sign(seed, &registry::signed_message(&document));
    let mut signature = Object::new();
    signature.insert("cle", Value::String(hex(&crate::crypto::ed25519::public_key(seed))));
    signature.insert("ed25519", Value::String(hex(&signed)));
    manifest.insert("signature", Value::Object(signature));
    if tamper {
        manifest.insert("app_name", Value::String(String::from("Essai 15 altere")));
    }

    let manifest_path = alloc::format!("{}/{}.json", REGISTRY_DIR, APP);
    let _ = crate::fs::delete(&manifest_path, kernel(), now);
    let _ = crate::fs::write_file(
        &manifest_path,
        crate::json::to_string_pretty(&Value::Object(manifest)).as_bytes(),
        kernel(),
        Mode::shared_file(),
        now,
    );
}

fn root() -> String {
    versions::app_root(&Scope::Global, BinaryFormat::Cus, APP)
}

fn exists(path: &str) -> bool {
    crate::fs::stat(path, kernel()).is_ok()
}

fn purge(now: u64) {
    if db::find(&Scope::Global, APP).is_some() {
        let _ = manager::remove(APP, Scope::Global, now);
    }
    versions::remove_tree(&root(), now);
    versions::remove_tree(BASE, now);
    registry::remove(REGISTRY_NAME, now);
}

fn wait_until_ended(pid: crate::process::Pid) -> bool {
    crate::scheduler::kill(pid, -1);
    for _ in 0..200 {
        let alive = crate::scheduler::task_info(pid).is_some_and(|t| t.state != crate::process::State::Dead);
        if !alive {
            let _ = crate::scheduler::try_reap(pid);
            return true;
        }
        let _ = crate::syscall::invoke(crate::syscall::domain_time::SYS_TIME_SLEEP_MS, [10, 0, 0, 0, 0, 0]);
    }
    false
}

fn update(now: u64) -> Result<Output, SpmError> {
    manager::execute(&alloc::format!("spm update {}", APP), now)
}

fn lifecycle(report: &mut Report, now: u64) {
    let mut seed = [0u8; 32];
    crate::random::fill(&mut seed);
    let key = crate::crypto::ed25519::public_key(&seed);
    let _ = crate::fs::create_directory(BASE, kernel(), Mode::shared_dir(), now);
    let _ = crate::fs::create_directory(REGISTRY_DIR, kernel(), Mode::shared_dir(), now);
    let url = alloc::format!("file:{}", REGISTRY_DIR);
    let command = alloc::format!("spm registry add {} {} {}", REGISTRY_NAME, url, hex(&key));
    report.check(
        "15.2.1 registre declare",
        matches!(manager::execute(&command, now), Ok(Output::RegistryAdded(_))),
    );

    // --- 15.3: first installation ---
    let v1 = package_of([1, 0, 0, 0], BinaryFormat::Cus, SignatureType::SelfSigned);
    publish(&seed, [1, 0, 0, 0], &v1, false, false, now);
    let installed = manager::execute(&alloc::format!("spm install {}", APP), now);
    report.check(
        "15.3.1 installation depuis le registre",
        matches!(&installed, Ok(Output::Versioned(done)) if done.version == "1.0.0"),
    );
    if let Err(error) = &installed {
        crate::serial_println!("[maj]   {}", error.message());
    }
    let root = root();
    report.check("15.3.2 pointeur sur 1.0.0", versions::active(&root).as_deref() == Some("1.0.0"));
    report.check("15.3.1 version dans son dossier", exists(&versions::version_dir(&root, "1.0.0")));
    report.check("15.3.1 aucun .tmp laisse", !exists(&alloc::format!("{}.tmp", versions::version_dir(&root, "1.0.0"))));
    report.check(
        "15.3.1 telechargement retire apres installation",
        crate::fs::list(&root, kernel()).is_ok_and(|entries| !entries.iter().any(|e| e.name.ends_with(".spm"))),
    );
    report.check(
        "15.3 deja a jour",
        matches!(update(now), Ok(Output::UpToDate(version)) if version == "1.0.0"),
    );

    // --- 15.4: launched through the pointer ---
    let first = versions::launch_by_name(APP);
    let Some(Ok(first)) = first else {
        report.check("15.4.1 lancement par le pointeur", false);
        return;
    };
    report.check("15.4.1 lancement par le pointeur", true);
    report.check(
        "15.4.2 le processus retient sa version",
        versions::version_of(first) == Some((root.clone(), String::from("1.0.0"))),
    );

    // --- update while it runs ---
    let v2 = package_of([2, 0, 0, 0], BinaryFormat::Cus, SignatureType::SelfSigned);
    publish(&seed, [2, 0, 0, 0], &v2, false, false, now);
    report.check(
        "15.7.1 mise a jour pendant l'execution",
        matches!(update(now), Ok(Output::Versioned(done)) if done.version == "2.0.0" && done.replaced.as_deref() == Some("1.0.0")),
    );
    report.check("15.7.1 pointeur sur 2.0.0", versions::active(&root).as_deref() == Some("2.0.0"));
    report.check("15.7.1 l'ancienne version reste", exists(&versions::version_dir(&root, "1.0.0")));
    report.check(
        "15.7.1 l'ancien processus tourne toujours",
        crate::scheduler::task_info(first).is_some_and(|t| t.state != crate::process::State::Dead),
    );
    let second = match versions::launch_by_name(APP) {
        Some(Ok(pid)) => pid,
        _ => 0,
    };
    report.check(
        "15.7.1 un nouveau lancement prend la nouvelle version",
        second != 0 && versions::version_of(second).map(|(_, v)| v).as_deref() == Some("2.0.0"),
    );

    // A third version: 1.0.0 is neither active nor previous, but it runs.
    let v3 = package_of([3, 0, 0, 0], BinaryFormat::Cus, SignatureType::SelfSigned);
    publish(&seed, [3, 0, 0, 0], &v3, false, false, now);
    report.check("15.3 troisieme version installee", matches!(update(now), Ok(Output::Versioned(_))));
    report.check("15.5.1 version encore executee gardee", exists(&versions::version_dir(&root, "1.0.0")));
    report.check("15.5.1 suppression differee notee", versions::pending() > 0);
    report.check("15.5.1 precedente = 2.0.0", versions::previous(&root).as_deref() == Some("2.0.0"));

    // Its process ends: the next tick deletes it.
    report.check("15.5.1 fin du premier processus", wait_until_ended(first));
    versions::tick(now);
    report.check("15.5.1 version supprimee a la fin de son processus", !exists(&versions::version_dir(&root, "1.0.0")));
    report.check("15.5.1 la precedente reste", exists(&versions::version_dir(&root, "2.0.0")));

    // --- the requests of an application (15.6.1) ---
    report.check(
        "15.6.1 une application demande pour elle-meme",
        super::service::answer(second, &alloc::format!("verifier {}", APP), now).starts_with("a-jour"),
    );
    report.check(
        "15.6.1 une application ne demande pas pour une autre",
        super::service::answer(second, "installer os.nova.explorateur", now).starts_with("erreur"),
    );
    report.check(
        "15.6.1 un processus non lance par SPM est refuse",
        super::service::answer(crate::scheduler::current_pid(), &alloc::format!("verifier {}", APP), now).starts_with("erreur"),
    );
    report.check("15.6.1 fin du second processus", second == 0 || wait_until_ended(second));

    // --- rollback ---
    report.check(
        "15.5.2 retour a la version precedente",
        matches!(manager::execute(&alloc::format!("spm rollback {}", APP), now), Ok(Output::RolledBack(v)) if v == "2.0.0"),
    );
    report.check("15.5.2 pointeur revenu sur 2.0.0", versions::active(&root).as_deref() == Some("2.0.0"));

    // --- 15.7.2: refusals leave nothing ---
    let before = crate::fs::list(&alloc::format!("{}/versions", root), kernel()).map(|l| l.len()).unwrap_or(0);
    let v4 = package_of([4, 0, 0, 0], BinaryFormat::Cus, SignatureType::SelfSigned);
    publish(&seed, [4, 0, 0, 0], &v4, true, false, now);
    report.check("15.7.2 manifeste altere refuse", matches!(update(now), Err(SpmError::Registry(_))));
    let mut other = [0u8; 32];
    crate::random::fill(&mut other);
    publish(&other, [4, 0, 0, 0], &v4, false, false, now);
    report.check("15.7.2 manifeste d'une autre cle refuse", matches!(update(now), Err(SpmError::Registry(_))));
    publish(&seed, [4, 0, 0, 0], &v4, false, true, now);
    report.check("15.7.2 empreinte fausse refusee", matches!(update(now), Err(SpmError::Registry(_))));
    let forbidden = package_of([4, 0, 0, 0], BinaryFormat::Dsa, SignatureType::SelfSigned);
    publish(&seed, [4, 0, 0, 0], &forbidden, false, false, now);
    report.check("15.7.2 paquet refuse par sa signature", matches!(update(now), Err(SpmError::Rejected(_))));
    let after = crate::fs::list(&alloc::format!("{}/versions", root), kernel()).map(|l| l.len()).unwrap_or(0);
    report.check("15.7.2 rien de plus sur le disque", after == before && versions::active(&root).as_deref() == Some("2.0.0"));
    report.check(
        "15.7.2 aucun telechargement laisse",
        crate::fs::list(&root, kernel()).is_ok_and(|entries| !entries.iter().any(|e| e.name.ends_with(".spm")))
            && crate::fs::list(super::update::UPDATES_DIR, kernel())
                .is_ok_and(|entries| !entries.iter().any(|e| e.name.starts_with(APP))),
    );

    // --- 15.7.3: what a cut installation leaves is cleaned ---
    let stale = alloc::format!("{}.tmp", versions::version_dir(&root, "9.9.9"));
    let _ = crate::fs::create_directory(&stale, kernel(), Mode::system_dir(), now);
    let _ = crate::fs::write_file(&alloc::format!("{}/morceau", stale), b"x", kernel(), Mode::system_file(), now);
    let _ = crate::fs::write_file(&alloc::format!("{}/current.nouveau", root), b"9.9.9\n", kernel(), Mode::system_file(), now);
    versions::clean(&root, now);
    report.check("15.7.3 .tmp d'une installation coupee purge", !exists(&stale));
    report.check("15.7.3 pointeur intact apres la purge", versions::active(&root).as_deref() == Some("2.0.0"));

    // --- removal: every version goes ---
    report.check(
        "15.5 desinstallation de toutes les versions",
        matches!(manager::execute(&alloc::format!("spm remove {}", APP), now), Ok(Output::Removed(_))) && !exists(&root),
    );
}

/// Runs the phase 15 self-test. Returns (passed, failed).
pub fn run() -> (usize, usize) {
    let mut report = Report { passed: 0, failed: 0 };
    let now = crate::idt::ticks();

    let (passed, failed) = crate::json::selftest();
    report.passed += passed;
    report.failed += failed;
    crate::serial_println!("[maj]   JSON : {} verification(s), {} echec(s)", passed, failed);

    downloads(&mut report);

    if !crate::fs::is_ready() {
        crate::serial_println!("[maj]   aucun volume - cycle de vie non eprouve");
        return (report.passed, report.failed);
    }
    purge(now);
    lifecycle(&mut report, now);
    purge(now);
    // The purge is committed before the verdict: a machine stopped right
    // after it (the boot test does exactly that) must not find the test's
    // registry again at the next boot.
    let _ = crate::fs::flush_all();
    (report.passed, report.failed)
}
