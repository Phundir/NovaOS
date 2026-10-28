//! `dev interpret main.js` (16.3): the "interpreter" column of the
//! Node.js row of spec 13.2, answered by Nova's own engine (`web::js`)
//! rather than a V8 that does not exist here.
//!
//! ## What a script finds
//!
//! The language and its library (16.1), and what Node adds that scripts
//! lean on: `require` of local CommonJS modules (relative paths, `.js`
//! and `.json`, `index.js` of a directory, each module run once, a cycle
//! seeing the partial exports, a module that failed tried again),
//! `module.exports`/`exports`, `require.main`, `__filename`, `__dirname`,
//! `process` and the built-in modules of `node.rs`. No `npm` package: a
//! `require` of a bare name that is not built in is a named error. The
//! "compiler" column stays ETR (`dev convertir`).
//!
//! ## With whose rights
//!
//! The caller says: `dev interpret` passes the session's credentials, or
//! ROOT's on a machine without a session (the recovery console). A script
//! is code the user wrote or downloaded; it reads and writes what they
//! may, nothing more. The rights live in a static for the length of the
//! run, which `BUSY` keeps to one at a time.
//!
//! ## Where it runs
//!
//! The engine walks the syntax tree and keeps 96 KiB of stack below it;
//! the shell's task has 128 KiB in all. The script runs in a task of its
//! own with a stack of 1 MiB (`dev-js`), and the caller waits for it.
//! After the script, the job queue and the timers run until nothing is
//! left -- the clock jumping to the next timer, as `await` does -- so
//! `setTimeout` works without waiting for real.

mod node;
pub mod selftest;

use alloc::string::String;
use alloc::vec::Vec;

use crate::fs::perm::Credentials;
use crate::vga::SpinLock;
use crate::web::js::builtins;
use crate::web::js::interp::{private_key, Ctrl, Ctx, Host, R};
use crate::web::js::parse::Options;
use crate::web::js::ops;
use crate::web::js::value::*;

/// Steps a script may take, all included.
const FUEL: u64 = 50_000_000;
/// Stack of the task that runs it.
const STACK: usize = 1024 * 1024;
/// Modules a program may load.
const MAX_MODULES: usize = 256;
/// Largest source accepted.
const MAX_SOURCE: usize = 4 * 1024 * 1024;

/// What a run gives back.
#[derive(Debug, Clone)]
pub struct JsRun {
    /// The console, in order.
    pub output: Vec<String>,
    /// `process.exit(code)` or `process.exitCode`, 1 after an uncaught
    /// error, else 0.
    pub exit_code: i64,
    /// Why the run stopped early, if it did.
    pub error: Option<String>,
    /// The program's own modules loaded, the main one included.
    pub modules: usize,
}

/// The host of a script: no page, no network, the machine's clock and
/// zone. The console goes to the realm, which keeps it in order.
struct NodeHost;

impl Host for NodeHost {
    fn now_ms(&mut self) -> i64 {
        crate::rtc::now_millis().unwrap_or(crate::idt::ticks() * (1000 / crate::TIMER_HZ as u64)) as i64
    }

    fn tz_offset_minutes(&mut self) -> i32 {
        let user = crate::apps::session_credentials().map(|(user, _)| user);
        let settings = crate::i18n::resolve(user.as_deref(), None);
        crate::i18n::tz::find(&settings.timezone).unwrap_or_else(crate::i18n::tz::default_zone).offset_minutes as i32
    }

    fn random(&mut self) -> u64 {
        crate::random::u64()
    }

    fn memory_ok(&mut self) -> bool {
        crate::heap::room_for(0)
    }

    fn can_allocate(&mut self, bytes: usize) -> bool {
        crate::heap::room_for(bytes)
    }

    fn fetch(
        &mut self,
        _method: &str,
        _url: &str,
        _headers: &[(String, String)],
        _body: Option<Vec<u8>>,
    ) -> Result<(u16, String, Vec<(String, String)>, Vec<u8>, String), String> {
        Err(String::from("fetch n'est pas disponible dans dev interpret"))
    }

    fn cookies(&mut self, _url: &str) -> String {
        String::new()
    }

    fn set_cookie(&mut self, _url: &str, _line: &str) {}

    fn storage(&mut self, _origin: &str, _session: bool) -> Vec<(String, String)> {
        Vec::new()
    }

    fn set_storage(&mut self, _origin: &str, _session: bool, _items: &[(String, String)]) {}

    fn navigate(&mut self, _url: &str, _replace: bool) {}

    fn log(&mut self, _line: &str) {}
}

// ------------------------------------------------------------
//  Rights, paths and files
// ------------------------------------------------------------

/// The rights of the run under way.
static RIGHTS: SpinLock<Option<Credentials>> = SpinLock::new(None);

/// The rights the script's file calls run with: those `run_file` was
/// given (outside a run, which does not happen, the session's).
fn rights() -> Credentials {
    (*RIGHTS.lock()).unwrap_or_else(session_rights)
}

/// The rights `dev interpret` gives a script: the session's, or ROOT's on
/// a machine without one (the recovery console).
pub fn session_rights() -> Credentials {
    crate::apps::session_credentials().map(|(_, who)| who).unwrap_or_else(Credentials::kernel)
}

fn normalize(path: &str) -> String {
    crate::shell::normalize(path)
}

fn dirname(path: &str) -> String {
    match path.rfind('/') {
        Some(0) => String::from("/"),
        Some(index) => String::from(&path[..index]),
        None => String::from("."),
    }
}

fn join(base: &str, relative: &str) -> String {
    if relative.starts_with('/') {
        normalize(relative)
    } else {
        normalize(&alloc::format!("{}/{}", base, relative))
    }
}

fn read_source(path: &str) -> Result<String, String> {
    let bytes = crate::fs::read_file(path, rights()).map_err(|error| alloc::format!("{} : {}", path, error.message()))?;
    if bytes.len() > MAX_SOURCE {
        return Err(alloc::format!("{} : source trop grande", path));
    }
    String::from_utf8(bytes).map_err(|_| alloc::format!("{} : pas du texte UTF-8", path))
}

fn is_file(path: &str) -> bool {
    crate::fs::stat(path, rights()).is_ok_and(|node| node.kind != crate::fs::format::NodeKind::Directory)
}

/// Where `require(name)` or `import ... from name` leads from
/// `directory`, as Node and Bun resolve a relative module: the file, then
/// with an extension, then `index` of a directory; and, as TypeScript
/// writes it, `./lib.js` naming `lib.ts`.
fn resolve_module(directory: &str, name: &str) -> Option<String> {
    let base = join(directory, name);
    let mut candidates = alloc::vec![base.clone()];
    for extension in ["js", "json", "ts", "mjs", "cjs", "mts", "cts"] {
        candidates.push(alloc::format!("{}.{}", base, extension));
    }
    candidates.push(alloc::format!("{}/index.js", base));
    candidates.push(alloc::format!("{}/index.ts", base));
    for (javascript, typescript) in [(".js", ".ts"), (".mjs", ".mts"), (".cjs", ".cts")] {
        if let Some(stem) = base.strip_suffix(javascript) {
            candidates.push(alloc::format!("{}{}", stem, typescript));
        }
    }
    candidates.into_iter().find(|candidate| is_file(candidate))
}

/// How a file runs.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ModuleKind {
    Json,
    CommonJs,
    EsModule,
}

fn is_typescript(path: &str) -> bool {
    [".ts", ".mts", ".cts", ".tsx"].iter().any(|extension| path.ends_with(extension))
}

/// `.mjs`/`.mts` are ES modules and `.cjs`/`.cts` CommonJS, whatever they
/// contain; a `.js` or `.ts` is an ES module when it has `import` or
/// `export` at its top level -- the detection Node 22 does, and Bun.
fn module_kind(ctx: &mut Ctx, path: &str, source: &str) -> ModuleKind {
    if path.ends_with(".json") {
        return ModuleKind::Json;
    }
    if path.ends_with(".mjs") || path.ends_with(".mts") {
        return ModuleKind::EsModule;
    }
    if path.ends_with(".cjs") || path.ends_with(".cts") {
        return ModuleKind::CommonJs;
    }
    let options = Options { module: true, typescript: is_typescript(path) };
    match crate::web::js::parse::parse_with(source, ctx.headroom, options) {
        Ok(program) => match program.module {
            Some(info) if !info.imports.is_empty() || !info.exports.is_empty() => ModuleKind::EsModule,
            _ => ModuleKind::CommonJs,
        },
        Err(_) => ModuleKind::CommonJs,
    }
}

// ------------------------------------------------------------
//  require
// ------------------------------------------------------------

fn string_arg(ctx: &mut Ctx, args: &[Value], index: usize) -> R<String> {
    Ok(String::from(ops::to_string(ctx, &builtins::arg(args, index))?.as_str()))
}

/// The module cache: an object keyed by absolute path, out of reach.
/// The built-in modules sit in it too, under keys starting with NUL.
fn cache(ctx: &mut Ctx) -> Obj {
    let key = private_key("modules");
    let global = ctx.realm.global.clone();
    if let Some(Slot::Data(Value::Obj(table), _)) = global.borrow().props.get(&key) {
        return table.clone();
    }
    let table = ctx.alloc(Kind::Ordinary, None, "Modules");
    global.put_key(key, Value::Obj(table.clone()), 0);
    table
}

/// `require`: data is the directory of the requiring module.
fn require(ctx: &mut Ctx, _: &Value, args: &[Value], data: &Value) -> R<Value> {
    let name = string_arg(ctx, args, 0)?;
    let name = name.strip_prefix("node:").unwrap_or(&name);
    if let Some(module) = node::builtin_module(ctx, name)? {
        return Ok(module);
    }
    if !(name.starts_with("./") || name.starts_with("../") || name.starts_with('/')) {
        let message = alloc::format!("Cannot find module '{}' (aucun paquet npm dans Nova : seuls les modules locaux et fs, path, os, util, events)", name);
        return ctx.throw_error("Error", &message);
    }
    let directory = match data {
        Value::Str(d) => String::from(d.as_str()),
        _ => String::from("/"),
    };
    let Some(path) = resolve_module(&directory, name) else {
        let message = alloc::format!("Cannot find module '{}' depuis {}", name, directory);
        return ctx.throw_error("Error", &message);
    };
    load_module(ctx, &path)
}

/// Runs a module once and gives its `module.exports` -- the namespace,
/// for an ES module.
fn load_module(ctx: &mut Ctx, path: &str) -> R<Value> {
    let table = cache(ctx);
    if let Some(module) = table.own_value(path) {
        return ops::get(ctx, &module, "exports");
    }
    if ops::own_keys(&table, false, false).len() >= MAX_MODULES {
        return ctx.throw_range("trop de modules charges");
    }
    if path.ends_with(".tsx") {
        let message = alloc::format!("{} : JSX n'est pas pris en charge", path);
        return ctx.throw_error("SyntaxError", &message);
    }
    let source = match read_source(path) {
        Ok(source) => source,
        Err(message) => return ctx.throw_error("Error", &message),
    };

    // The first module is the program: `require.main === module`, and its
    // id is ".", as in Node.
    let main_key = private_key("main");
    let global = ctx.realm.global.clone();
    let main = match global.borrow().props.get(&main_key) {
        Some(Slot::Data(main, _)) => Some(main.clone()),
        _ => None,
    };
    let module = ctx.new_object();
    let exports = ctx.new_object();
    module.put("exports", Value::Obj(exports.clone()));
    module.put("id", Value::str(if main.is_none() { "." } else { path }));
    module.put("filename", Value::str(path));
    module.put("loaded", Value::Bool(false));
    let main = match main {
        Some(main) => main,
        None => {
            global.put_key(main_key, Value::Obj(module.clone()), 0);
            Value::Obj(module.clone())
        }
    };

    // In the cache before it runs: a cycle of requires sees the partial
    // exports, as in Node.
    table.put(path, Value::Obj(module.clone()));
    let outcome = run_module(ctx, path, &source, &module, exports, main);
    if outcome.is_err() {
        // A module that failed is not kept: the next `require` runs it
        // again, as in Node.
        table.borrow_mut().props.remove(&JsStr::from(path));
    }
    outcome
}

/// Runs the source of a module: JSON parsed, CommonJS wrapped in the
/// function Node wraps it in, an ES module linked and run by the engine.
fn run_module(ctx: &mut Ctx, path: &str, source: &str, module: &Obj, exports: Obj, main: Value) -> R<Value> {
    let typescript = is_typescript(path);
    let directory = dirname(path);
    let local_require = ctx.native_with("require", 1, require, Value::str(&directory));
    local_require.put("main", main);
    match module_kind(ctx, path, source) {
        ModuleKind::Json => {
            let value = builtins::json::parse_text(ctx, source)?;
            module.put("exports", value.clone());
            Ok(value)
        }
        ModuleKind::EsModule => {
            // The namespace is the module's exports from now on: a cycle
            // that comes back here finds its getters.
            let namespace = ctx.new_object();
            module.put("exports", Value::Obj(namespace.clone()));
            // Bun gives `require` and the paths to modules too; Node does
            // not.
            let mut bindings: Vec<(&str, Value)> = Vec::new();
            if typescript {
                bindings.push(("require", Value::Obj(local_require)));
                bindings.push(("__filename", Value::str(path)));
                bindings.push(("__dirname", Value::str(&directory)));
            }
            let options = Options { module: true, typescript };
            crate::web::js::module::run_module(ctx, strip_shebang(source), options, path, &namespace, &bindings)?;
            module.put("loaded", Value::Bool(true));
            Ok(Value::Obj(namespace))
        }
        ModuleKind::CommonJs => {
            let wrapper = alloc::format!("(function (exports, require, module, __filename, __dirname) {{\n{}\n}})", strip_shebang(source));
            let function = ctx.eval_global_with(&wrapper, Options { module: false, typescript })?;
            ctx.call(
                &function,
                Value::Obj(exports.clone()),
                &[Value::Obj(exports), Value::Obj(local_require), Value::Obj(module.clone()), Value::str(path), Value::str(&directory)],
            )?;
            module.put("loaded", Value::Bool(true));
            ops::get(ctx, &Value::Obj(module.clone()), "exports")
        }
    }
}

/// The realm's loader for `import`: a built-in module, or a file
/// resolved from the importing module's directory.
fn import_loader(ctx: &mut Ctx, referrer: &str, specifier: &str) -> R<Obj> {
    let name = specifier.strip_prefix("node:").unwrap_or(specifier);
    if let Some(Value::Obj(module)) = node::builtin_module(ctx, name)? {
        return Ok(module);
    }
    if !(name.starts_with("./") || name.starts_with("../") || name.starts_with('/')) {
        let message = alloc::format!("Cannot find package '{}' imported from {} (aucun paquet npm dans Nova)", name, referrer);
        return ctx.throw_error("Error", &message);
    }
    let directory = dirname(referrer);
    let Some(path) = resolve_module(&directory, name) else {
        let message = alloc::format!("Cannot find module '{}' imported from {}", join(&directory, name), referrer);
        return ctx.throw_error("Error", &message);
    };
    match load_module(ctx, &path)? {
        Value::Obj(exports) => Ok(exports),
        // `module.exports = 42`: a namespace whose default is 42.
        other => {
            let namespace = ctx.new_object();
            namespace.put_key(private_key("esm"), Value::Bool(true), 0);
            namespace.put("default", other);
            Ok(namespace)
        }
    }
}

/// `#!/usr/bin/env node` on the first line is not JavaScript; the new
/// line stays, so the line numbers of errors stay right.
fn strip_shebang(source: &str) -> &str {
    if source.starts_with("#!") {
        match source.find('\n') {
            Some(end) => &source[end..],
            None => "",
        }
    } else {
        source
    }
}

// ------------------------------------------------------------
//  Running
// ------------------------------------------------------------

/// Runs `path` here, in the calling task: it must have the stack
/// (`run_file` gives it one).
fn run_here(path: &str, arguments: &[String]) -> JsRun {
    let mut realm = builtins::new_realm();
    let mut host = NodeHost;
    let mut document = crate::web::html::parse("");
    let headroom = || crate::scheduler::stack_headroom();
    let directory = dirname(path);
    let mut exit_code = 0;
    let mut error = None;
    let modules;
    {
        let mut ctx = Ctx::new(&mut realm, &mut host, &mut document, &headroom, FUEL, alloc::format!("file://{}", directory), (80, 25));
        builtins::install(&mut ctx);
        node::install_process(&mut ctx, path, arguments);
        ctx.realm.loader = Some(import_loader);
        let outcome = load_module(&mut ctx, path).and_then(|_| {
            // The jobs, then the timers, the clock jumping to each.
            drain(&mut ctx)
        });
        match outcome {
            Ok(()) => {}
            Err(Ctrl::Throw(value)) => {
                let text = ctx.describe_error(&value);
                builtins::global::console_line(&mut ctx, "erreur", &alloc::format!("exception non rattrapee : {}", text));
                exit_code = 1;
            }
            Err(Ctrl::Halt(reason)) => match reason.strip_prefix("process.exit(").and_then(|r| r.strip_suffix(')')) {
                Some(code) => exit_code = code.parse().unwrap_or(0),
                None => {
                    error = Some(reason);
                    exit_code = 1;
                }
            },
            Err(_) => {}
        }
        node::flush_stdout(&mut ctx);
        if exit_code == 0 && error.is_none() {
            exit_code = node::exit_code(&mut ctx).unwrap_or(0);
        }
        // An error a timer or a rejected promise reported on the way.
        if exit_code == 0 && !ctx.realm.errors.is_empty() {
            exit_code = 1;
        }
        modules = ops::own_keys(&cache(&mut ctx), false, false)
            .iter()
            .filter(|key| !key.as_str().starts_with('\0'))
            .count();
    }
    let output = core::mem::take(&mut realm.console);
    realm.teardown();
    JsRun { output, exit_code, error, modules }
}

/// Runs the queue and the timers until nothing is due any more.
fn drain(ctx: &mut Ctx) -> R<()> {
    for _ in 0..100_000 {
        ctx.run_jobs().map_err(Ctrl::Halt)?;
        let Some(due) = ctx.realm.timers.iter().map(|t| t.due).min() else {
            return Ok(());
        };
        let now = ctx.now();
        if due > now {
            ctx.realm.clock_skew += due - now;
        }
        let now = ctx.now();
        ctx.run_timers(now).map_err(Ctrl::Halt)?;
    }
    Err(Ctrl::Halt(String::from("minuteries sans fin (setInterval jamais arrete ?)")))
}

/// The run handed to the `dev-js` task, and its result.
static JOB: SpinLock<Option<(String, Vec<String>, Option<JsRun>)>> = SpinLock::new(None);
/// One run at a time.
static BUSY: SpinLock<bool> = SpinLock::new(false);

fn worker() -> ! {
    let job = JOB.lock().as_ref().map(|(path, arguments, _)| (path.clone(), arguments.clone()));
    if let Some((path, arguments)) = job {
        let run = run_here(&path, &arguments);
        if let Some(entry) = JOB.lock().as_mut() {
            entry.2 = Some(run);
        }
    }
    crate::scheduler::exit_current_with(0)
}

/// Runs a JavaScript file with the rights `who`, in a task of its own,
/// and waits for it.
pub fn run_file(path: &str, arguments: &[String], who: Credentials) -> Result<JsRun, String> {
    {
        let mut busy = BUSY.lock();
        if *busy {
            return Err(String::from("un autre script tourne deja"));
        }
        *busy = true;
    }
    *RIGHTS.lock() = Some(who);
    *JOB.lock() = Some((String::from(path), arguments.to_vec(), None));
    let pid = crate::scheduler::spawn_with_stack("dev-js", crate::process::BinaryType::Cus, worker, STACK);
    crate::scheduler::wait_for(pid);
    let result = JOB.lock().take().and_then(|(_, _, run)| run);
    *RIGHTS.lock() = None;
    *BUSY.lock() = false;
    result.ok_or_else(|| String::from("le script s'est arrete sans resultat"))
}
