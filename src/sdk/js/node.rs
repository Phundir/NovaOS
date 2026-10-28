//! The Node.js side of `dev interpret` (16.3): the built-in modules a
//! script `require`s by name -- `fs`, `path`, `os`, `util`, `events` --
//! and the `process` object.
//!
//! Each is the small part of Node's that scripts lean on, written over
//! Nova's own services: `fs` calls `SYS_FS_*` synchronously with the rights
//! of the run, `path` is POSIX only (Nova has no other), `events` is
//! written in JavaScript because it is short and lives in the language it
//! serves. What is missing (streams, `child_process`, `net`, the callback
//! and promise forms of `fs`) is missing by name: `require` of it fails
//! with a message saying what exists.

use alloc::string::String;
use alloc::vec::Vec;

use super::{cache, dirname, join, normalize, rights, string_arg};
use crate::web::js::builtins::{self, arg};
use crate::web::js::interp::{private_key, Ctrl, Ctx, R};
use crate::web::js::ops;
use crate::web::js::value::*;

// ------------------------------------------------------------
//  Built-in modules
// ------------------------------------------------------------

/// `fs`, `path`, `os`, `util`, `events`: made on first use, then cached.
pub(super) fn builtin_module(ctx: &mut Ctx, name: &str) -> R<Option<Value>> {
    if !matches!(name, "fs" | "path" | "os" | "util" | "events" | "process") {
        return Ok(None);
    }
    let table = cache(ctx);
    let key = alloc::format!("\0builtin:{}", name);
    if let Some(module) = table.own_value(&key) {
        return Ok(Some(module));
    }
    let module = match name {
        "fs" => fs_module(ctx),
        "path" => path_module(ctx),
        "os" => os_module(ctx),
        "util" => util_module(ctx),
        "events" => events_module(ctx)?,
        _ => match ctx.realm.global.own_value("process") {
            Some(Value::Obj(process)) => process,
            _ => ctx.new_object(),
        },
    };
    table.put(&key, Value::Obj(module.clone()));
    Ok(Some(Value::Obj(module)))
}

fn fs_error<T>(ctx: &mut Ctx, call: &str, path: &str, error: crate::fs::FsError) -> R<T> {
    let code = match error {
        crate::fs::FsError::NotFound => "ENOENT",
        crate::fs::FsError::PermissionDenied(_) => "EACCES",
        crate::fs::FsError::AlreadyExists => "EEXIST",
        crate::fs::FsError::DirectoryNotEmpty => "ENOTEMPTY",
        _ => "EIO",
    };
    let message = alloc::format!("{}: {}, {} '{}'", code, error.message(), call, path);
    let failure = ctx.error_object(ctx.realm.protos.error.clone(), "Error", &message);
    failure.put("code", Value::str(code));
    failure.put("path", Value::str(path));
    Err(Ctrl::Throw(Value::Obj(failure)))
}

/// The directory relative paths start from: the script's.
fn working_directory(ctx: &Ctx) -> String {
    String::from(ctx.base_url.trim_start_matches("file://"))
}

fn fs_path(ctx: &mut Ctx, args: &[Value]) -> R<String> {
    let path = string_arg(ctx, args, 0)?;
    Ok(join(&working_directory(ctx), &path))
}

/// The synchronous calls of `fs`: data names the call.
fn fs_call(ctx: &mut Ctx, _: &Value, args: &[Value], data: &Value) -> R<Value> {
    let Value::Str(call) = data else {
        return Ok(Value::Undefined);
    };
    let call = String::from(call.as_str());
    let path = fs_path(ctx, args)?;
    let who = rights();
    let now = crate::idt::ticks();
    match call.as_str() {
        "readFileSync" => {
            let bytes = match crate::fs::read_file(&path, who) {
                Ok(bytes) => bytes,
                Err(error) => return fs_error(ctx, "open", &path, error),
            };
            Ok(Value::Str(JsStr::from(String::from_utf8_lossy(&bytes).into_owned())))
        }
        "writeFileSync" | "appendFileSync" => {
            let text = string_arg(ctx, args, 1)?;
            let mut bytes = Vec::new();
            if call == "appendFileSync" {
                if let Ok(existing) = crate::fs::read_file(&path, who) {
                    bytes = existing;
                }
            }
            bytes.extend_from_slice(text.as_bytes());
            match crate::fs::write_file(&path, &bytes, who, crate::fs::perm::Mode::private_file(), now) {
                Ok(_) => Ok(Value::Undefined),
                Err(error) => fs_error(ctx, "open", &path, error),
            }
        }
        "existsSync" => Ok(Value::Bool(crate::fs::stat(&path, who).is_ok())),
        "readdirSync" => match crate::fs::list(&path, who) {
            Ok(entries) => {
                let mut names: Vec<String> = entries.into_iter().map(|e| e.name).collect();
                names.sort();
                let values = names.iter().map(|n| Value::str(n)).collect();
                Ok(Value::Obj(ctx.new_array(values)))
            }
            Err(error) => fs_error(ctx, "scandir", &path, error),
        },
        "mkdirSync" => match crate::fs::create_directory(&path, who, crate::fs::perm::Mode::private_dir(), now) {
            Ok(_) => Ok(Value::Undefined),
            Err(error) => fs_error(ctx, "mkdir", &path, error),
        },
        "unlinkSync" | "rmSync" | "rmdirSync" => match crate::fs::delete(&path, who, now) {
            Ok(()) => Ok(Value::Undefined),
            Err(error) => fs_error(ctx, "unlink", &path, error),
        },
        "statSync" => match crate::fs::stat(&path, who) {
            Ok(node) => {
                let directory = node.kind == crate::fs::format::NodeKind::Directory;
                let stats = ctx.new_object();
                stats.put("size", Value::num(node.size as i64));
                let is_dir = ctx.native_with("isDirectory", 0, constant, Value::Bool(directory));
                stats.put_hidden("isDirectory", Value::Obj(is_dir));
                let is_file = ctx.native_with("isFile", 0, constant, Value::Bool(!directory));
                stats.put_hidden("isFile", Value::Obj(is_file));
                Ok(Value::Obj(stats))
            }
            Err(error) => fs_error(ctx, "stat", &path, error),
        },
        _ => Ok(Value::Undefined),
    }
}

fn constant(_: &mut Ctx, _: &Value, _: &[Value], data: &Value) -> R<Value> {
    Ok(data.clone())
}

fn fs_module(ctx: &mut Ctx) -> Obj {
    let module = ctx.new_object();
    for name in ["readFileSync", "writeFileSync", "appendFileSync", "existsSync", "readdirSync", "mkdirSync", "unlinkSync", "rmSync", "rmdirSync", "statSync"] {
        let native = ctx.native_with(name, 2, fs_call, Value::str(name));
        module.put(name, Value::Obj(native));
    }
    module
}

/// `path`: data names the function.
fn path_call(ctx: &mut Ctx, _: &Value, args: &[Value], data: &Value) -> R<Value> {
    let Value::Str(call) = data else {
        return Ok(Value::Undefined);
    };
    let mut parts = Vec::new();
    for index in 0..args.len() {
        parts.push(string_arg(ctx, args, index)?);
    }
    let first = parts.first().cloned().unwrap_or_default();
    let text = match call.as_str() {
        "join" => {
            let joined = parts.join("/");
            if joined.is_empty() {
                String::from(".")
            } else {
                let absolute = joined.starts_with('/');
                let normalized = normalize(&joined);
                if absolute { normalized } else { String::from(normalized.trim_start_matches('/')) }
            }
        }
        "resolve" => {
            let mut current = working_directory(ctx);
            for part in &parts {
                current = join(&current, part);
            }
            current
        }
        "dirname" => dirname(&first),
        "basename" => {
            let base = String::from(first.trim_end_matches('/').rsplit('/').next().unwrap_or(""));
            match parts.get(1) {
                Some(suffix) if base.ends_with(suffix.as_str()) => String::from(&base[..base.len() - suffix.len()]),
                _ => base,
            }
        }
        "extname" => {
            let base = first.rsplit('/').next().unwrap_or("");
            match base.rfind('.') {
                Some(index) if index > 0 => String::from(&base[index..]),
                _ => String::new(),
            }
        }
        "normalize" => normalize(&first),
        _ => String::new(),
    };
    if call.as_str() == "isAbsolute" {
        return Ok(Value::Bool(first.starts_with('/')));
    }
    Ok(Value::Str(JsStr::from(text)))
}

fn path_module(ctx: &mut Ctx) -> Obj {
    let module = ctx.new_object();
    for name in ["join", "resolve", "dirname", "basename", "extname", "normalize", "isAbsolute"] {
        let native = ctx.native_with(name, 2, path_call, Value::str(name));
        module.put(name, Value::Obj(native));
    }
    module.put("sep", Value::str("/"));
    module.put("delimiter", Value::str(":"));
    let posix = module.clone();
    module.put("posix", Value::Obj(posix));
    module
}

fn os_module(ctx: &mut Ctx) -> Obj {
    let module = ctx.new_object();
    for (name, value) in [("platform", "nova"), ("type", "Nova OS"), ("arch", "x64")] {
        let native = ctx.native_with(name, 0, constant, Value::str(value));
        module.put(name, Value::Obj(native));
    }
    module.put("EOL", Value::str("\n"));
    let cpus = ctx.native_with("cpus", 0, constant, Value::Undefined);
    module.put("cpus", Value::Obj(cpus));
    let release = ctx.native_with("release", 0, constant, Value::str(crate::version::VERSION));
    module.put("release", Value::Obj(release));
    module
}

fn util_format(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    Ok(Value::Str(JsStr::from(builtins::global::format_args(ctx, args)?)))
}

fn util_inspect(_: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    Ok(Value::Str(JsStr::from(ops::to_display(&arg(args, 0)))))
}

fn util_module(ctx: &mut Ctx) -> Obj {
    let module = ctx.new_object();
    ctx.method(&module, "format", 1, util_format);
    ctx.method(&module, "inspect", 1, util_inspect);
    module
}

/// `events`: `EventEmitter`, in JavaScript -- it is short, and written
/// once in the language it serves.
const EVENT_EMITTER: &str = "(function () {
class EventEmitter {
  constructor() { this._events = new Map(); }
  on(name, listener) { if (!this._events.has(name)) this._events.set(name, []); this._events.get(name).push(listener); return this; }
  addListener(name, listener) { return this.on(name, listener); }
  once(name, listener) { const wrapper = (...args) => { this.off(name, wrapper); listener.apply(this, args); }; return this.on(name, wrapper); }
  off(name, listener) { const list = this._events.get(name); if (list) { const i = list.indexOf(listener); if (i >= 0) list.splice(i, 1); } return this; }
  removeListener(name, listener) { return this.off(name, listener); }
  removeAllListeners(name) { if (name === undefined) this._events.clear(); else this._events.delete(name); return this; }
  emit(name, ...args) { const list = this._events.get(name); if (!list || list.length === 0) { if (name === 'error') throw args[0]; return false; } for (const listener of [...list]) listener.apply(this, args); return true; }
  listenerCount(name) { const list = this._events.get(name); return list ? list.length : 0; }
}
EventEmitter.EventEmitter = EventEmitter;
return EventEmitter;
})()";

fn events_module(ctx: &mut Ctx) -> R<Obj> {
    match ctx.eval_global(EVENT_EMITTER)? {
        Value::Obj(constructor) => Ok(constructor),
        _ => Ok(ctx.new_object()),
    }
}

// ------------------------------------------------------------
//  process
// ------------------------------------------------------------

fn exit(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let code = match arg(args, 0) {
        Value::Undefined => 0,
        other => ops::to_integer(ctx, &other)?,
    };
    Err(Ctrl::Halt(alloc::format!("process.exit({})", code)))
}

fn cwd(ctx: &mut Ctx, _: &Value, _: &[Value], _: &Value) -> R<Value> {
    Ok(Value::Str(JsStr::from(ctx.base_url.trim_start_matches("file://"))))
}

fn next_tick(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let callback = arg(args, 0);
    ops::require_callable(ctx, &callback, "process.nextTick callback")?;
    let rest = if args.len() > 1 { args[1..].to_vec() } else { Vec::new() };
    ctx.realm.jobs.push_back(crate::web::js::interp::Job::Call(callback, rest));
    Ok(Value::Undefined)
}

/// `process.stdout.write`: text without a new line, gathered until one.
fn stdout_write(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let text = string_arg(ctx, args, 0)?;
    let global = ctx.realm.global.clone();
    let key = private_key("stdout");
    let mut pending = match global.borrow().props.get(&key) {
        Some(Slot::Data(Value::Str(s), _)) => String::from(s.as_str()),
        _ => String::new(),
    };
    pending.push_str(&text);
    while let Some(end) = pending.find('\n') {
        let line = String::from(&pending[..end]);
        builtins::global::console_line(ctx, "", &line);
        pending = String::from(&pending[end + 1..]);
    }
    global.put_key(key, Value::Str(JsStr::from(pending)), WRITABLE);
    Ok(Value::Bool(true))
}

pub(super) fn install_process(ctx: &mut Ctx, script: &str, arguments: &[String]) {
    let process = ctx.new_object();
    let mut argv = alloc::vec![Value::str("/sys/runtimes/node"), Value::str(script)];
    argv.extend(arguments.iter().map(|a| Value::str(a)));
    let argv = ctx.new_array(argv);
    process.put("argv", Value::Obj(argv));
    let env = ctx.new_object();
    env.put("NOVA_OS", Value::str(crate::version::VERSION));
    if let Some((user, _)) = crate::apps::session_credentials() {
        env.put("USER", Value::Str(JsStr::from(user.clone())));
        env.put("HOME", Value::Str(JsStr::from(alloc::format!("/users/{}/home", user))));
    }
    process.put("env", Value::Obj(env));
    process.put("platform", Value::str("nova"));
    process.put("arch", Value::str("x64"));
    process.put("version", Value::str("v0.0.0-nova"));
    let versions = ctx.new_object();
    versions.put("nova", Value::str(crate::version::VERSION));
    process.put("versions", Value::Obj(versions));
    process.put("exitCode", Value::Undefined);
    ctx.method(&process, "exit", 1, exit);
    ctx.method(&process, "cwd", 0, cwd);
    ctx.method(&process, "nextTick", 1, next_tick);
    let stdout = ctx.new_object();
    ctx.method(&stdout, "write", 1, stdout_write);
    process.put("stdout", Value::Obj(stdout.clone()));
    process.put("stderr", Value::Obj(stdout));
    let global = ctx.realm.global.clone();
    global.put_hidden("process", Value::Obj(process));
    global.put_hidden("global", Value::Obj(global.clone()));
}

/// The end of a run: what `process.stdout.write` left without a new line
/// goes out as the last line.
pub(super) fn flush_stdout(ctx: &mut Ctx) {
    let pending = match ctx.realm.global.borrow().props.get(&private_key("stdout")) {
        Some(Slot::Data(Value::Str(text), _)) if !text.as_str().is_empty() => Some(String::from(text.as_str())),
        _ => None,
    };
    if let Some(text) = pending {
        builtins::global::console_line(ctx, "", &text);
    }
}

/// `process.exitCode`, which a program that ends on its own exits with.
pub(super) fn exit_code(ctx: &mut Ctx) -> Option<i64> {
    let Some(Value::Obj(process)) = ctx.realm.global.own_value("process") else {
        return None;
    };
    match process.own_value("exitCode") {
        Some(Value::Num(code)) => Some(code.to_i64_saturating()),
        _ => None,
    }
}
