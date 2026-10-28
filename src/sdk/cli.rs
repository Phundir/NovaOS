//! 9.1 -- The `dev` command (spec 15.2).
//!
//! ## `dev` is a function, not a binary
//!
//! Same choice as in phase 8 for `spm`: 15.2 describes a command line,
//! and `execute` parses that string literally. There is no terminal to
//! type it in yet -- the IDE of 9.4 that was to offer one is retired; a
//! terminal will call `execute` without the latter changing by a line.
//!
//! ## The current project
//!
//! 15.2 writes `dev build --release` without saying on what. A shell
//! would resolve it through the current directory; the kernel has none.
//! The command therefore accepts an explicit `--projet <path>`, and
//! remembers the last project touched -- the one `dev new` has just
//! created, or the one a `--projet` named. That is the closest
//! translation of what a developer expects, and it is explicit: `dev
//! etat` says what the commands are going to act on.
//!
//! ## What `--release` changes, exactly
//!
//! Two observable points, and nothing else:
//!
//! 1. **Verification becomes blocking.** The binary produced goes back
//!    through `binfmt::parse`, `check_rules` and `signature::verify`; the
//!    slightest violation cancels the write. In debug mode the same
//!    checks run but only fill the warning list, and the file is written
//!    -- a developer must be able to look at the binary their code
//!    produces, faulty or not.
//! 2. **The build number increments.** A release binary carries a number
//!    different from the one before it, otherwise two deliveries would be
//!    confused in the SPM database.
//!
//! What it does not change: there is no optimiser. The assembler emits
//! what it is given. Announcing an "optimised compilation" would be false.
//!
//! ## What `--debug` changes, exactly
//!
//! There is **no debugger** -- no breakpoint, no register inspection.
//! `--debug` does two real things: it traces the launch life cycle on the
//! serial console, and it **keeps the sandbox** after the program ends
//! instead of destroying it, so that its content can be inspected.
//! Cleanup then becomes explicit, and `dev nettoyer` takes care of it.

use alloc::string::String;
use alloc::vec::Vec;

use crate::binfmt::header::{BinaryFormat, SignatureType};
use crate::binfmt::identity::{Permissions, Version};
use crate::binfmt::{self, signature};
use crate::fs::perm::{Credentials, Mode};
use crate::process::Pid;
use crate::vga::SpinLock;

use super::asm::{self, AsmError, Object};
use super::emit::{self, EmitError, EmitSpec};
use super::etr;
use super::manifest::{self, DevManifest, ManifestError};
use super::runtime::{self, Language, Support};
use super::rustc;
use super::sandbox::{self, SandboxError};
use super::script;
use super::template::{self, Template};

/// Sub-directory where what `dev build` produces lands.
pub const BUILD_DIR: &str = "build";

/// Default root of an account's projects.
pub fn default_root(user: &str) -> String {
    alloc::format!("/users/{}/home/documents/projets", user)
}

// ============================================================
//  Errors
// ============================================================

#[derive(Debug, Clone)]
pub enum DevError {
    /// Command line the grammar does not recognise.
    BadCommand(String),
    /// No current project and no `--projet`.
    NoProject,
    /// The path contains no `dev.toml`.
    NotAProject(String),
    Manifest(ManifestError),
    Assembler(AsmError),
    Emit(EmitError),
    Script(script::ScriptError),
    /// The Nova Rust front-end refused the source (9.5.1).
    Rust(rustc::RustError),
    Sandbox(SandboxError),
    Filesystem(String, crate::fs::FsError),
    /// The language has no compiler in place (13.2).
    NoCompiler(Language),
    /// The language has no interpreter in place.
    NoInterpreter(Language),
    /// The delegated runtime is installed nowhere (13.3).
    RuntimeMissing(Language),
    /// The source is not UTF-8.
    NotText(String),
    /// The binary produced violates a rule of 3.4.
    RuleViolation(String),
    /// The loader refused the image.
    LoadRefused(String),
    /// An unknown template was requested.
    UnknownTemplate(String),
    /// A `--sign` the spec does not know.
    UnknownSignature(String),
}

impl DevError {
    pub fn message(&self) -> String {
        match self {
            DevError::BadCommand(line) => alloc::format!("commande invalide : {}", line),
            DevError::NoProject => {
                String::from("aucun projet courant - preciser --projet <chemin>")
            }
            DevError::NotAProject(path) => {
                alloc::format!("{} ne contient pas de dev.toml", path)
            }
            DevError::Manifest(error) => alloc::format!("dev.toml : {}", error.message()),
            DevError::Assembler(error) => alloc::format!("assemblage : {}", error.message()),
            DevError::Emit(error) => alloc::format!("edition de liens : {}", error.message()),
            DevError::Script(error) => alloc::format!("script : {}", error.message()),
            DevError::Rust(error) => alloc::format!("rust : {}", error.message()),
            DevError::Sandbox(error) => alloc::format!("bac a sable : {}", error.message()),
            DevError::Filesystem(path, error) => {
                alloc::format!("{} : {}", path, error.message())
            }
            DevError::NoCompiler(language) => alloc::format!(
                "aucun compilateur en place pour {} - voir spec 13.2",
                language.key()
            ),
            DevError::NoInterpreter(language) => alloc::format!(
                "aucun interprete en place pour {} - voir spec 13.2",
                language.key()
            ),
            DevError::RuntimeMissing(language) => alloc::format!(
                "runtime {} absent de /sys/runtimes et de /users/*/runtimes (spec 13.3)",
                language.key()
            ),
            DevError::NotText(path) => alloc::format!("{} n'est pas du texte", path),
            DevError::RuleViolation(reason) => alloc::format!("spec 3.4 : {}", reason),
            DevError::LoadRefused(reason) => alloc::format!("chargement refuse : {}", reason),
            DevError::UnknownTemplate(name) => alloc::format!("gabarit inconnu : {}", name),
            DevError::UnknownSignature(name) => alloc::format!("signature inconnue : {}", name),
        }
    }
}

// ============================================================
//  Reports
// ============================================================

/// What `dev build` produced.
#[derive(Debug, Clone)]
pub struct BuildReport {
    pub app_id: String,
    pub format: BinaryFormat,
    pub signature: SignatureType,
    pub path: String,
    pub bytes: usize,
    pub text_bytes: usize,
    pub data_bytes: usize,
    pub bss_bytes: u64,
    pub instructions: usize,
    pub relocations: usize,
    pub release: bool,
    pub version: Version,
    pub warnings: Vec<String>,
}

/// What `dev run` observed.
#[derive(Debug, Clone)]
pub struct RunReport {
    pub sandbox: u32,
    pub pid: Pid,
    pub exit_code: i64,
    /// Original format, before the replay imposed by 15.4.
    pub original_format: BinaryFormat,
    /// Permissions lost to the trimming.
    pub dropped_permissions: u64,
    /// Internal ports removed by the replay.
    pub dropped_ports: usize,
    pub pages: usize,
    pub kept: bool,
    pub loopback_refused: u64,
}

/// What `dev package` wrote.
#[derive(Debug, Clone)]
pub struct PackageReport {
    pub path: String,
    pub bytes: usize,
    pub signature: SignatureType,
    pub app_id: String,
    pub verdict: String,
}

/// What `dev convert` concluded.
#[derive(Debug, Clone)]
pub struct ConvertReport {
    pub analysis: etr::Report,
    /// Nova project produced from the analysis.
    pub project: String,
    pub binary: Option<String>,
}

/// Result of a command.
pub enum Output {
    Created {
        path: String,
        template: Template,
        files: usize,
    },
    Built(BuildReport),
    Ran(RunReport),
    Interpreted(script::Run),
    /// `dev interpret` on a `.rs`: the Rust interpreter of 9.5.1.
    InterpretedRust(rustc::Interpreted),
    Packaged(PackageReport),
    Converted(ConvertReport),
    Cleaned {
        sandboxes: usize,
        entries: usize,
    },
    State {
        project: Option<String>,
        runtimes: usize,
    },
}

impl Output {
    /// Returns the report as displayable lines.
    pub fn lines(&self) -> Vec<String> {
        match self {
            Output::Created {
                path,
                template,
                files,
            } => alloc::vec![
                alloc::format!("projet cree : {}", path),
                alloc::format!("gabarit {} - {} fichier(s)", template.key(), files),
            ],
            Output::Built(report) => {
                let mut lines = alloc::vec![
                    alloc::format!(
                        "{} -> {} ({} octets)",
                        report.app_id,
                        report.path,
                        report.bytes
                    ),
                    alloc::format!(
                        "  .text {} o ({} instructions), .data {} o, .bss {} o",
                        report.text_bytes,
                        report.instructions,
                        report.data_bytes,
                        report.bss_bytes
                    ),
                    alloc::format!(
                        "  mode {} - version {} - {} relocation(s)",
                        if report.release { "release" } else { "debogage" },
                        report.version,
                        report.relocations
                    ),
                ];
                for warning in report.warnings.iter() {
                    lines.push(alloc::format!("  avertissement : {}", warning));
                }
                lines
            }
            Output::Ran(report) => {
                let mut lines = alloc::vec![
                    alloc::format!(
                        "bac a sable {} - PID {} - code de sortie {}",
                        report.sandbox,
                        report.pid,
                        report.exit_code
                    ),
                    alloc::format!(
                        "  rejoue en .ras depuis .{} (spec 15.4)",
                        report.original_format.extension()
                    ),
                ];
                if report.dropped_permissions != 0 {
                    lines.push(alloc::format!(
                        "  {} permission(s) rabotee(s) au plafond .ras (spec 12.4)",
                        report.dropped_permissions.count_ones()
                    ));
                }
                if report.dropped_ports != 0 {
                    lines.push(alloc::format!(
                        "  {} port(s) interne(s) retire(s) (spec 12.3)",
                        report.dropped_ports
                    ));
                }
                lines.push(alloc::format!(
                    "  {} page(s) dediee(s), {} emission(s) hors loopback refusee(s)",
                    report.pages,
                    report.loopback_refused
                ));
                if report.kept {
                    lines.push(String::from(
                        "  bac a sable conserve (--debug) - purger avec \"dev nettoyer\"",
                    ));
                }
                lines
            }
            Output::Interpreted(run) => {
                let mut lines = Vec::new();
                for line in run.output.iter() {
                    lines.push(alloc::format!("  {}", line));
                }
                lines.push(alloc::format!(
                    "resultat : {} - {} pas",
                    run.value.display(),
                    run.steps
                ));
                lines
            }
            Output::InterpretedRust(run) => {
                let mut lines = Vec::new();
                for line in run.output.iter() {
                    lines.push(alloc::format!("  {}", line));
                }
                lines.push(alloc::format!(
                    "resultat : {} - {} pas",
                    match run.exit_code {
                        Some(code) => alloc::format!("exit({})", code),
                        None => String::from("()"),
                    },
                    run.steps
                ));
                lines
            }
            Output::Packaged(report) => alloc::vec![
                alloc::format!("paquet : {} ({} octets)", report.path, report.bytes),
                alloc::format!("  {} - {}", report.app_id, report.verdict),
            ],
            Output::Converted(report) => {
                let mut lines = report.analysis.lines();
                lines.push(alloc::format!("projet produit : {}", report.project));
                if let Some(binary) = report.binary.as_ref() {
                    lines.push(alloc::format!("binaire produit : {}", binary));
                }
                lines
            }
            Output::Cleaned {
                sandboxes,
                entries,
            } => alloc::vec![alloc::format!(
                "{} bac(s) a sable detruit(s), {} entree(s) effacee(s)",
                sandboxes,
                entries
            )],
            Output::State { project, runtimes } => alloc::vec![
                alloc::format!(
                    "projet courant : {}",
                    project.as_deref().unwrap_or("aucun")
                ),
                alloc::format!("{} runtime(s) resolus", runtimes),
            ],
        }
    }
}

// ============================================================
//  Command state
// ============================================================

struct State {
    project: Option<String>,
    /// Sandboxes kept by `--debug`, to be purged on request.
    kept: Vec<u32>,
    builds: u64,
    runs: u64,
    packages: u64,
    conversions: u64,
}

static STATE: SpinLock<State> = SpinLock::new(State {
    project: None,
    kept: Vec::new(),
    builds: 0,
    runs: 0,
    packages: 0,
    conversions: 0,
});

/// Project the commands without `--projet` act on.
pub fn current_project() -> Option<String> {
    STATE.lock().project.clone()
}

/// Sets the current project.
pub fn set_project(path: &str) {
    STATE.lock().project = Some(String::from(path));
}

// ============================================================
//  Line parsing
// ============================================================

/// One command line, split up.
struct Command<'a> {
    verb: &'a str,
    positional: Vec<&'a str>,
    options: Vec<(&'a str, Option<&'a str>)>,
}

impl<'a> Command<'a> {
    /// Splits `dev <verb> [positionals] [--option [value]]`.
    ///
    /// An option takes a value if the next word is not itself an option:
    /// `--release` has none, `--sign enterprise` has one.
    fn parse(line: &'a str) -> Result<Command<'a>, DevError> {
        let words: Vec<&str> = line.split_whitespace().collect();
        if words.first() != Some(&"dev") {
            return Err(DevError::BadCommand(String::from(line)));
        }
        let Some(verb) = words.get(1) else {
            return Err(DevError::BadCommand(String::from(line)));
        };

        let mut positional = Vec::new();
        let mut options = Vec::new();
        let mut index = 2;

        while index < words.len() {
            let word = words[index];
            if let Some(name) = word.strip_prefix("--") {
                let value = words
                    .get(index + 1)
                    .filter(|next| !next.starts_with("--"))
                    .copied();
                if value.is_some() {
                    index += 1;
                }
                options.push((name, value));
            } else {
                positional.push(word);
            }
            index += 1;
        }

        Ok(Command {
            verb,
            positional,
            options,
        })
    }

    fn has(&self, name: &str) -> bool {
        self.options.iter().any(|(key, _)| *key == name)
    }

    fn value(&self, name: &str) -> Option<&'a str> {
        self.options
            .iter()
            .find(|(key, _)| *key == name)
            .and_then(|(_, value)| *value)
    }
}

/// Entry point: runs a `dev ...` line.
pub fn execute(line: &str, now: u64) -> Result<Output, DevError> {
    let command = Command::parse(line)?;

    match command.verb {
        "new" => new_project(&command, now),
        "build" => build(&command, now).map(Output::Built),
        "run" => run(&command, now).map(Output::Ran),
        "interpret" => interpret(&command),
        "package" => package(&command, now).map(Output::Packaged),
        "convert" => convert(&command, now).map(Output::Converted),
        "nettoyer" | "clean" => clean(now),
        "etat" | "status" => Ok(Output::State {
            project: current_project(),
            runtimes: runtime::list(crate::gui::login::current_user().as_deref()).len(),
        }),
        _ => Err(DevError::BadCommand(String::from(line))),
    }
}

// ============================================================
//  9.1.1 -- dev new
// ============================================================

/// `dev new <app_id> [--template t] [--type f] [--lang l] [--dir path]`
///
/// 15.2 writes `dev new monapp --type cus --lang rust`. Both forms are
/// accepted: `--template` chooses a template from 15.5, and `--type` /
/// `--lang` adjust what the template decided. A `--type` the template
/// cannot honour is refused at the manifest, not silently corrected.
fn new_project(command: &Command, now: u64) -> Result<Output, DevError> {
    let Some(name) = command.positional.first() else {
        return Err(DevError::BadCommand(String::from("dev new attend un nom")));
    };

    let template = match command.value("template") {
        Some(key) => Template::from_key(key)
            .ok_or_else(|| DevError::UnknownTemplate(String::from(key)))?,
        None => Template::Blank,
    };

    // A name with no dot is not an app_id: it is given the conventional
    // prefix rather than refused, because `dev new monapp` is literally
    // the example of 15.2.
    let app_id = if name.contains('.') {
        String::from(*name)
    } else {
        alloc::format!("local.{}", name)
    };
    let short = app_id.rsplit('.').next().unwrap_or(&app_id);

    let user = crate::gui::login::current_user();
    let root = match command.value("dir") {
        Some(path) => String::from(path),
        None => default_root(user.as_deref().unwrap_or("nova")),
    };
    let path = alloc::format!("{}/{}", root, short);

    // The language is settled **before** the manifest, because it
    // decides the name of the entry file: a project whose manifest said
    // `src/main.rs` and whose disk held `src/main.asm` would fail its
    // first build on a file it never had.
    let language = match command.value("lang") {
        Some(key) => Language::from_key(key)
            .ok_or_else(|| DevError::BadCommand(alloc::format!("--lang {}", key)))?,
        None => Language::Asm,
    };

    let author = command.value("auteur").unwrap_or("Nova OS");
    let mut manifest = template::manifest_for(template, &app_id, author, language);
    if let Some(key) = command.value("type") {
        manifest.format = match key {
            "dsa" => BinaryFormat::Dsa,
            "ss" => BinaryFormat::Ss,
            "gs" => BinaryFormat::Gs,
            "cus" => BinaryFormat::Cus,
            "ras" => BinaryFormat::Ras,
            _ => return Err(DevError::BadCommand(alloc::format!("--type {}", key))),
        };
        // Changing format changes the admissible signatures (3.4): the
        // least demanding level the table allows is taken.
        manifest.signature = weakest_signature(manifest.format);
    }
    manifest::validate(&manifest).map_err(DevError::Manifest)?;

    let who = Credentials::kernel();
    ensure_directory(&root, now)?;
    ensure_directory(&path, now)?;

    let mut files = template::files(template, &app_id, author, language);
    // The manifest written is the one just adjusted, not the template's:
    // without this, `--type` and `--lang` would have no effect at all.
    if let Some(entry) = files.iter_mut().find(|file| file.path == manifest::FILE_NAME) {
        entry.content = manifest::to_toml(&manifest);
    }

    let mut written = 0;
    for file in files.iter() {
        let target = alloc::format!("{}/{}", path, file.path);
        if let Some((parent, _)) = target.rsplit_once('/') {
            ensure_directory(parent, now)?;
        }
        crate::fs::write_file(&target, file.content.as_bytes(), who, Mode::private_file(), now)
            .map_err(|error| DevError::Filesystem(target.clone(), error))?;
        written += 1;
    }
    ensure_directory(&alloc::format!("{}/{}", path, BUILD_DIR), now)?;

    set_project(&path);

    Ok(Output::Created {
        path,
        template,
        files: written,
    })
}

/// Least demanding signature 3.4 admits for a format.
fn weakest_signature(format: BinaryFormat) -> SignatureType {
    for candidate in [
        SignatureType::None,
        SignatureType::SelfSigned,
        SignatureType::Enterprise,
        SignatureType::System,
    ] {
        if signature::combination_allowed(format, candidate) {
            return candidate;
        }
    }
    SignatureType::System
}

fn ensure_directory(path: &str, now: u64) -> Result<(), DevError> {
    let who = Credentials::kernel();
    if path.is_empty() || crate::fs::stat(path, who).is_ok() {
        return Ok(());
    }
    if let Some((parent, _)) = path.rsplit_once('/')
        && !parent.is_empty()
    {
        ensure_directory(parent, now)?;
    }
    match crate::fs::create_directory(path, who, Mode::private_dir(), now) {
        Ok(_) => Ok(()),
        Err(_) if crate::fs::stat(path, who).is_ok() => Ok(()),
        Err(error) => Err(DevError::Filesystem(String::from(path), error)),
    }
}

// ============================================================
//  9.1.2 -- dev build
// ============================================================

/// Finds the project a command aims at.
fn resolve_project(command: &Command) -> Result<String, DevError> {
    let path = match command.value("projet") {
        Some(path) => String::from(path),
        None => match command.positional.first() {
            // A positional naming an existing directory is taken for the
            // project: `dev build ./monapp` must work.
            Some(candidate)
                if crate::fs::stat(
                    &alloc::format!("{}/{}", candidate, manifest::FILE_NAME),
                    Credentials::kernel(),
                )
                .is_ok() =>
            {
                String::from(*candidate)
            }
            _ => current_project().ok_or(DevError::NoProject)?,
        },
    };
    let descriptor = alloc::format!("{}/{}", path, manifest::FILE_NAME);
    if crate::fs::stat(&descriptor, Credentials::kernel()).is_err() {
        return Err(DevError::NotAProject(path));
    }
    set_project(&path);
    Ok(path)
}

fn read_text(path: &str) -> Result<String, DevError> {
    let bytes = crate::fs::read_file(path, Credentials::kernel())
        .map_err(|error| DevError::Filesystem(String::from(path), error))?;
    match core::str::from_utf8(&bytes) {
        Ok(text) => Ok(String::from(text)),
        Err(_) => Err(DevError::NotText(String::from(path))),
    }
}

/// Reads and validates the manifest of a project.
pub fn load_manifest(project: &str) -> Result<DevManifest, DevError> {
    let descriptor = alloc::format!("{}/{}", project, manifest::FILE_NAME);
    let text = read_text(&descriptor)?;
    manifest::parse(&text).map_err(DevError::Manifest)
}

/// Assembles the source of the project.
///
/// The per-language dispatch is here, and it is explicit: a language with
/// no compiler in place produces a named refusal, never an empty binary.
fn compile(project: &str, manifest: &DevManifest) -> Result<Object, DevError> {
    let support = runtime::spec(manifest.language).support;
    if !support.can_compile() {
        // An installed runtime could take it on: we look before refusing,
        // so that the message says which of the two things is missing.
        let user = crate::gui::login::current_user();
        if support == Support::Delegated
            && runtime::resolve(manifest.language, user.as_deref()).is_none()
        {
            return Err(DevError::RuntimeMissing(manifest.language));
        }
        return Err(DevError::NoCompiler(manifest.language));
    }

    let source_path = alloc::format!("{}/{}", project, manifest.entry);
    let source = read_text(&source_path)?;

    // Two front-ends produce the same `Object`, and the linker below
    // knows about neither. That is what makes adding a language a matter
    // of one arm here rather than of a second build path.
    match manifest.language {
        Language::Rust => rustc::compile(&source)
            .map(|compiled| compiled.object)
            .map_err(DevError::Rust),
        _ => asm::assemble(&source).map_err(DevError::Assembler),
    }
}

/// Translates the manifest into emission parameters.
fn emit_spec<'a>(
    manifest: &'a DevManifest,
    format: BinaryFormat,
    signature: SignatureType,
    permissions: u64,
    flags: u32,
    version: Version,
    authority: Option<emit::Authority<'a>>,
) -> EmitSpec<'a> {
    EmitSpec {
        format,
        signature,
        app_id: &manifest.app_id,
        author: &manifest.author,
        group: &manifest.group,
        display_name: &manifest.display_name,
        version,
        permissions,
        flags,
        ports: &manifest.ports,
        stack_size: manifest.stack_size,
        authority,
    }
}

/// Demonstration authority, for an ENTERPRISE binary.
///
/// Its key is drawn at each boot (`signature::demo_authority_seed`): a
/// binary signed with it verifies during this boot, where the self-tests
/// registered it, and nowhere else. A real ENTERPRISE binary is signed by
/// its authority's own key (`tools/nova-sign.py`), registered in
/// `/sys/config/autorites.cfg`.
fn demo_authority() -> emit::Authority<'static> {
    emit::Authority {
        name: "Nova Certification",
        legal_id: "FR-000-000-000",
        seed: binfmt::signature::demo_authority_seed(),
        valid_from: 9000,
        valid_until: 12000,
    }
}

/// Checks the binary produced and returns the list of complaints.
fn inspect(image: &[u8]) -> Vec<String> {
    let mut warnings = Vec::new();

    let header = match binfmt::parse(image) {
        Ok(header) => header,
        Err(error) => {
            warnings.push(alloc::format!("analyse : {}", error.message()));
            return warnings;
        }
    };
    for violation in binfmt::check_rules(&header) {
        warnings.push(alloc::format!("regle 3.4 : {}", violation.message()));
    }
    let verdict = signature::verify(&header, image);
    if let signature::Verdict::Rejected(reason) = verdict {
        warnings.push(alloc::format!("signature : {}", reason.message()));
    }
    warnings
}

/// 9.1.2 -- `dev build [--release]`.
fn build(command: &Command, now: u64) -> Result<BuildReport, DevError> {
    let project = resolve_project(command)?;
    let manifest = load_manifest(&project)?;
    let release = command.has("release");

    let object = compile(&project, &manifest)?;

    // The build number increments on release: two deliveries carrying the
    // same number would be confused in the database of installed
    // packages.
    let version = if release {
        Version::new(
            manifest.version.major,
            manifest.version.minor,
            manifest.version.patch,
            manifest.version.build.saturating_add(1),
        )
    } else {
        manifest.version
    };

    let authority = (manifest.signature == SignatureType::Enterprise).then(demo_authority);
    let spec = emit_spec(
        &manifest,
        manifest.format,
        manifest.signature,
        manifest.permissions.0,
        manifest.flags,
        version,
        authority,
    );
    let image = emit::emit(&object, &spec).map_err(DevError::Emit)?;

    let warnings = inspect(&image);
    if release && !warnings.is_empty() {
        return Err(DevError::RuleViolation(warnings.join(" ; ")));
    }

    let short = manifest.app_id.rsplit('.').next().unwrap_or(&manifest.app_id);
    let target = alloc::format!(
        "{}/{}/{}.{}",
        project,
        BUILD_DIR,
        short,
        manifest.format.extension()
    );
    ensure_directory(&alloc::format!("{}/{}", project, BUILD_DIR), now)?;
    crate::fs::write_file(
        &target,
        &image,
        Credentials::kernel(),
        Mode::private_file(),
        now,
    )
    .map_err(|error| DevError::Filesystem(target.clone(), error))?;

    // The manifest is rewritten when the number moved: without this, the
    // next build would start again from the same number.
    if release {
        let mut updated = manifest.clone();
        updated.version = version;
        let descriptor = alloc::format!("{}/{}", project, manifest::FILE_NAME);
        let _ = crate::fs::write_file(
            &descriptor,
            manifest::to_toml(&updated).as_bytes(),
            Credentials::kernel(),
            Mode::private_file(),
            now,
        );
    }

    STATE.lock().builds += 1;

    Ok(BuildReport {
        app_id: manifest.app_id.clone(),
        format: manifest.format,
        signature: manifest.signature,
        path: target,
        bytes: image.len(),
        text_bytes: object.text.len(),
        data_bytes: object.data.len(),
        bss_bytes: object.bss_size,
        instructions: object.instructions,
        relocations: object.fixups.len(),
        release,
        version,
        warnings,
    })
}

// ============================================================
//  9.1.3 -- dev run
// ============================================================

/// 9.1.3 -- `dev run [--debug]`, in a sandbox of 15.4.
fn run(command: &Command, now: u64) -> Result<RunReport, DevError> {
    let project = resolve_project(command)?;
    let manifest = load_manifest(&project)?;
    let debug = command.has("debug");

    let object = compile(&project, &manifest)?;

    // 9.3.4 -- 15.4 imposes the replay as `.ras`. This is not a label
    // being changed: a `.ras` is unsigned (3.4) and caps at three
    // permissions (12.4). So the binary is rebuilt.
    let plan = sandbox::plan_ras(
        manifest.format,
        manifest.signature,
        manifest.permissions.0,
        manifest.flags,
        manifest.ports.len(),
    );
    // The internal ports fall with the rest: 12.3 puts the `.ras` on the
    // deny-list of all communication, and `check_rules` refuses a `.ras`
    // whose port table is not empty. Keeping them would produce a binary
    // the loader would refuse, hence a test impossible to launch --
    // precisely for the format 15.4 imposes.
    let mut spec = emit_spec(
        &manifest,
        BinaryFormat::Ras,
        SignatureType::None,
        plan.permissions,
        plan.flags,
        manifest.version,
        None,
    );
    spec.ports = &[];
    let image = emit::emit(&object, &spec).map_err(DevError::Emit)?;

    let id = sandbox::create(
        crate::scheduler::current_pid(),
        BinaryFormat::Ras,
        &manifest.app_id,
        now,
    )
    .map_err(DevError::Sandbox)?;

    // 9.3.1 -- the dedicated memory space is taken at open time, not on
    // demand: the test must fail if the machine cannot provide it, and
    // fail before anything has been launched.
    let pages = match sandbox::allocate(id, 64 * 1024) {
        Ok(_) => sandbox::pages_used(id).unwrap_or(0),
        Err(error) => {
            let _ = sandbox::destroy(id, now);
            return Err(DevError::Sandbox(error));
        }
    };

    // 9.3.2 -- the binary enters the upper layer. Nothing is written on
    // the host: that is the very definition of the overlay.
    let short = manifest.app_id.rsplit('.').next().unwrap_or(&manifest.app_id);
    let virtual_path = alloc::format!("/ras/{}.ras", short);
    if let Err(error) = sandbox::write(id, &virtual_path, &image, now) {
        let _ = sandbox::destroy(id, now);
        return Err(DevError::Sandbox(error));
    }

    if debug {
        crate::serial_println!(
            "[dev] run --debug : {} rejoue en .ras, bac a sable {}, {} page(s)",
            manifest.app_id,
            id,
            pages
        );
    }

    let (loaded, pid) = match binfmt::loader::load_and_spawn(&image) {
        Ok(pair) => pair,
        Err(error) => {
            let _ = sandbox::destroy(id, now);
            return Err(DevError::LoadRefused(String::from(error.message())));
        }
    };
    // The image must be handed to the kernel, not merely launched.
    // `LoadedImage` has no `Drop`: abandoning it here would leave its
    // frames taken, its pages mapped and its arena slot occupied, with
    // nothing to flag it. `keep` is what files it under its PID, and that
    // is where `scheduler::try_reap` will go looking when `wait_for`
    // reaps the task.
    binfmt::loader::keep(pid, loaded);

    if debug {
        crate::serial_println!("[dev] tache {} lancee - attente de sa fin", pid);
    }

    // `wait_for` returns the exit code. The template programs always
    // terminate: an infinite loop would block here, and that is why the
    // templates are bounded.
    let exit_code = crate::scheduler::wait_for(pid).unwrap_or(-1);

    let (_, refused, _) = sandbox::net_stats(id).unwrap_or((0, 0, 0));

    // 9.3.5 -- complete destruction, unless `--debug` asks to keep the
    // context for inspection.
    if debug {
        STATE.lock().kept.push(id);
    } else {
        sandbox::destroy(id, now).map_err(DevError::Sandbox)?;
    }

    STATE.lock().runs += 1;

    Ok(RunReport {
        sandbox: id,
        pid,
        exit_code,
        original_format: plan.original_format,
        dropped_permissions: plan.dropped,
        dropped_ports: plan.ports_dropped,
        pages,
        kept: debug,
        loopback_refused: refused,
    })
}

/// Purges the sandboxes kept by `--debug`.
fn clean(now: u64) -> Result<Output, DevError> {
    let kept: Vec<u32> = core::mem::take(&mut STATE.lock().kept);
    let mut entries = 0;
    let mut destroyed = 0;
    for id in kept {
        if let Ok((_, removed)) = sandbox::destroy(id, now) {
            entries += removed;
            destroyed += 1;
        }
    }
    Ok(Output::Cleaned {
        sandboxes: destroyed,
        entries,
    })
}

// ============================================================
//  9.1.4 -- dev interpret
// ============================================================

/// 9.1.4 -- `dev interpret <file>`.
///
/// The language is derived from the extension, as 15.2 assumes when it
/// writes `dev interpret main.py` without repeating the language.
fn interpret(command: &Command) -> Result<Output, DevError> {
    let Some(target) = command.positional.first() else {
        return Err(DevError::BadCommand(String::from(
            "dev interpret attend un fichier",
        )));
    };

    // A relative path resolves inside the current project: that is what a
    // developer writes, and demanding an absolute path would make `dev` a
    // tool nobody uses from inside a project.
    let path = if target.starts_with('/') {
        String::from(*target)
    } else {
        match current_project() {
            Some(project) => alloc::format!("{}/{}", project, target),
            None => String::from(*target),
        }
    };

    let language = Language::from_extension(&path)
        .ok_or_else(|| DevError::BadCommand(alloc::format!("extension inconnue : {}", path)))?;
    let support = runtime::spec(language).support;
    if !support.can_interpret() {
        let user = crate::gui::login::current_user();
        if runtime::resolve(language, user.as_deref()).is_none() {
            return Err(DevError::RuntimeMissing(language));
        }
        return Err(DevError::NoInterpreter(language));
    }

    let source = read_text(&path)?;
    // The two interpreters the SDK carries. Nova Script is the one added
    // outside 13.2 in 9.1.4; Rust is the "interpreter" column of 13.2
    // itself, answered by 9.5.1.
    if language == Language::Rust {
        let run = rustc::interpret(&source, rustc::interp::DEFAULT_BUDGET)
            .map_err(DevError::Rust)?;
        return Ok(Output::InterpretedRust(run));
    }
    let run = script::execute(&source).map_err(DevError::Script)?;
    Ok(Output::Interpreted(run))
}

// ============================================================
//  9.1.5 -- dev package
// ============================================================

/// Translates a `--sign` into a level of 3.2.
fn signature_of(key: &str) -> Option<SignatureType> {
    Some(match key {
        "system" => SignatureType::System,
        "enterprise" => SignatureType::Enterprise,
        "self" | "auto" => SignatureType::SelfSigned,
        "none" | "aucune" => SignatureType::None,
        _ => return None,
    })
}

/// 9.1.5 -- `dev package [--sign level]`.
///
/// The package takes the signature of the binary by default. An explicit
/// `--sign` replaces it, and 3.4 decides: a level incompatible with the
/// format fails the command instead of producing a package `spm install`
/// would refuse.
fn package(command: &Command, now: u64) -> Result<PackageReport, DevError> {
    use crate::spm::package as spm_package;

    let project = resolve_project(command)?;
    let manifest = load_manifest(&project)?;

    let signature = match command.value("sign") {
        Some(key) => signature_of(key)
            .ok_or_else(|| DevError::UnknownSignature(String::from(key)))?,
        None => manifest.signature,
    };
    if !signature::combination_allowed(manifest.format, signature) {
        return Err(DevError::RuleViolation(alloc::format!(
            ".{} n'admet pas la signature {}",
            manifest.format.extension(),
            signature.as_str()
        )));
    }

    // The binary is rebuilt with the requested signature: packaging a
    // self-signed binary under an ENTERPRISE label would produce a
    // package whose wrapper and content contradict each other, and 10.4
    // would display a badge the binary does not deserve.
    let object = compile(&project, &manifest)?;
    let authority = (signature == SignatureType::Enterprise).then(demo_authority);
    let spec = emit_spec(
        &manifest,
        manifest.format,
        signature,
        manifest.permissions.0,
        manifest.flags,
        manifest.version,
        authority,
    );
    let image = emit::emit(&object, &spec).map_err(DevError::Emit)?;

    let short = manifest.app_id.rsplit('.').next().unwrap_or(&manifest.app_id);
    let binary_name = alloc::format!("{}.{}", short, manifest.format.extension());

    let spm_manifest = spm_package::Manifest {
        app_id: manifest.app_id.clone(),
        name: manifest.display_name.clone(),
        publisher: manifest.author.clone(),
        description: manifest.description.clone(),
        binary: binary_name,
        prefix: String::new(),
        depends: manifest.depends.clone(),
        extra: Vec::new(),
    };

    let authority_seed =
        (signature == SignatureType::Enterprise).then(binfmt::signature::demo_authority_seed);
    let file = spm_package::build(&spm_package::BuildSpec {
        // A first installation is not an update: 10.2 has no code for
        // "new", and `Standard` is the only one that asks for a
        // confirmation without making it an emergency.
        update_code: spm_package::UpdateCode::Standard,
        signature,
        format: manifest.format,
        flags: manifest.flags,
        version: [
            manifest.version.major,
            manifest.version.minor,
            manifest.version.patch,
            manifest.version.build,
        ],
        display_name: &manifest.display_name,
        publisher: &manifest.author,
        manifest: &spm_manifest,
        binary: &image,
        resources: &[],
        pre_script: "",
        post_script: "",
        authority_seed,
    });

    // The verdict is the one `spm install` will pronounce: giving it here
    // spares the developer discovering at install time that a digest is
    // missing from the trust store.
    let verdict = match spm_package::parse(&file) {
        Ok(header) => {
            let outcome = spm_package::verify(&header, &file);
            alloc::format!("{} - {}", signature.as_str(), outcome.as_str())
        }
        Err(error) => alloc::format!("paquet illisible : {}", error.message()),
    };

    let target = alloc::format!("{}/{}/{}.spm", project, BUILD_DIR, short);
    ensure_directory(&alloc::format!("{}/{}", project, BUILD_DIR), now)?;
    crate::fs::write_file(
        &target,
        &file,
        Credentials::kernel(),
        Mode::private_file(),
        now,
    )
    .map_err(|error| DevError::Filesystem(target.clone(), error))?;

    STATE.lock().packages += 1;

    Ok(PackageReport {
        path: target,
        bytes: file.len(),
        signature,
        app_id: manifest.app_id.clone(),
        verdict,
    })
}

// ============================================================
//  9.1.6 -- dev convert
// ============================================================

/// 9.1.6 -- `dev convert --from electron <path>`.
///
/// 13.1 fixes what follows: at least 70 % converted and no `eval` gives a
/// native `.cus`, otherwise a `.ras` with a warning. Both branches
/// produce a complete Nova project -- manifest, source, binary -- and not
/// a mere report: "native .cus binary produced" is what the spec writes.
fn convert(command: &Command, now: u64) -> Result<ConvertReport, DevError> {
    if command.value("from") != Some("electron") {
        return Err(DevError::BadCommand(String::from(
            "dev convert attend --from electron",
        )));
    }
    let Some(source) = command.positional.first() else {
        return Err(DevError::BadCommand(String::from(
            "dev convert attend un chemin de projet",
        )));
    };

    let analysis = etr::analyze_project(source)
        .map_err(|error| DevError::Filesystem(String::from(*source), error))?;

    let name = source
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|segment| !segment.is_empty())
        .unwrap_or("converti");
    let app_id = alloc::format!("converti.{}", name);

    let user = crate::gui::login::current_user();
    let root = default_root(user.as_deref().unwrap_or("nova"));
    let project = alloc::format!("{}/{}-nova", root, name);

    ensure_directory(&root, now)?;
    ensure_directory(&project, now)?;

    // The template depends on the verdict: a successful conversion gives
    // an application, a refused conversion gives a `.ras` whose warning
    // at launch is exactly what 13.1 wants to make visible.
    let (template_kind, format) = match analysis.verdict {
        etr::Verdict::Native(format) => (Template::GuiApp, format),
        etr::Verdict::Raw(format) => (Template::Blank, format),
    };

    // 13.1 is called Electron-**To-Rust**, and until 9.5.1 the tool
    // could only lay down an assembly skeleton -- the one thing the name
    // rules out. The converted project is now generated in Nova Rust,
    // which is also the language the mapping table talks about.
    let mut manifest = template::manifest_for(template_kind, &app_id, "ETR", Language::Rust);
    manifest.format = format;
    manifest.signature = weakest_signature(format);
    manifest.description = alloc::format!("converti par ETR - taux {} %", analysis.rate);
    // A `.ras` keeps neither GUI nor permissions beyond its ceiling.
    if format == BinaryFormat::Ras {
        manifest.flags = 0;
        manifest.permissions =
            Permissions(manifest.permissions.0 & Permissions::ceiling(BinaryFormat::Ras));
    }
    manifest::validate(&manifest).map_err(DevError::Manifest)?;

    let who = Credentials::kernel();
    let mut files = template::files(template_kind, &app_id, "ETR", Language::Rust);
    if let Some(entry) = files.iter_mut().find(|file| file.path == manifest::FILE_NAME) {
        entry.content = manifest::to_toml(&manifest);
    }
    files.push(template::File {
        path: String::from("RAPPORT-ETR.txt"),
        content: analysis.lines().join("\n"),
    });

    for file in files.iter() {
        let target = alloc::format!("{}/{}", project, file.path);
        if let Some((parent, _)) = target.rsplit_once('/') {
            ensure_directory(parent, now)?;
        }
        crate::fs::write_file(&target, file.content.as_bytes(), who, Mode::private_file(), now)
            .map_err(|error| DevError::Filesystem(target.clone(), error))?;
    }
    ensure_directory(&alloc::format!("{}/{}", project, BUILD_DIR), now)?;

    set_project(&project);
    STATE.lock().conversions += 1;

    // The binary is produced right away: 13.1 says "binary produced", not
    // "project produced". A build failure does not cancel the conversion
    // -- the report stays useful -- but it is reported.
    let build_command = Command {
        verb: "build",
        positional: Vec::new(),
        options: alloc::vec![("projet", Some(project.as_str()))],
    };
    let binary = build(&build_command, now).ok().map(|report| report.path);

    Ok(ConvertReport {
        analysis,
        project,
        binary,
    })
}

// ============================================================
//  State
// ============================================================

/// Counters: (builds, runs, packages, conversions).
pub fn stats() -> (u64, u64, u64, u64) {
    let state = STATE.lock();
    (state.builds, state.runs, state.packages, state.conversions)
}

pub fn dump_state() {
    let state = STATE.lock();
    crate::serial_println!(
        "[dev] projet courant : {}",
        state.project.as_deref().unwrap_or("aucun")
    );
    crate::serial_println!(
        "[dev] {} construction(s), {} execution(s), {} paquet(s), {} conversion(s)",
        state.builds,
        state.runs,
        state.packages,
        state.conversions
    );
    if !state.kept.is_empty() {
        crate::serial_println!(
            "[dev] {} bac(s) a sable conserve(s) par --debug",
            state.kept.len()
        );
    }
}
