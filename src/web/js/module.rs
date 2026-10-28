//! Modules: linking what a parsed module imports and exports, then
//! running its body in a scope of its own.
//!
//! ## Live bindings without rewriting the program
//!
//! An ES module's import is not a copy: it reads the exporter's binding
//! each time, so a counter incremented over there is seen here, and a
//! cycle of imports works as long as nobody reads a binding before it is
//! made. Both sides are built from the closures the engine already has:
//!
//! * each export is a **getter** on the module's namespace object, a
//!   one-line function `() => local` closed over the module's scope;
//! * each named import is a binding in the importer's scope whose state
//!   is `IMPORTED` and whose value is a function `() => ns[name]`: a read
//!   calls it, a write is the `TypeError` of a constant.
//!
//! The namespace exists before the body runs and before the module's own
//! imports are loaded: a module that imports this one back finds the
//! getters there, and a binding not yet made answers the usual
//! `ReferenceError`.
//!
//! ## Who loads
//!
//! The engine does not know where modules live. Whoever runs modules
//! sets `Realm::loader`: it resolves a specifier from a referrer, loads,
//! caches and runs the module (`run_module`, for an ES module), and
//! returns its namespace. The Node runner (`sdk::js`) has its own, with
//! CommonJS and the built-in modules; `host_loader` is the plain one,
//! ES modules only, their sources asked of the host
//! (`Host::module_source`) -- the engine's self-test uses it.

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

use super::ast::*;
use super::interp::{private_key, Ctrl, Ctx, Frame, Scope, IMPORTED, R};
use super::ops;
use super::parse::Options;
use super::value::*;

/// The realm's module loader: (referrer, specifier) to namespace.
pub type Loader = fn(&mut Ctx, &str, &str) -> R<Obj>;

/// The hidden binding of a module's scope holding its own path or URL.
const REFERRER: &str = "*referrer*";
const BINDING: u8 = 0;
const CONSTANT: u8 = 1;

/// Is `namespace` an ES module's (rather than CommonJS exports)?
pub fn is_module_namespace(namespace: &Obj) -> bool {
    namespace.borrow().props.get(&private_key("esm")).is_some()
}

/// A function of no argument returning `body`, closed over `scope`.
fn thunk(ctx: &mut Ctx, body: Expr, scope: &Scope) -> R<Obj> {
    let func = Rc::new(Func {
        name: None,
        params: Vec::new(),
        rest: None,
        body: alloc::vec![Stmt::Return(Some(body))],
        arrow: true,
        is_async: false,
        generator: false,
        expression: true,
        vars: Vec::new(),
        uses_arguments: false,
        simple_params: true,
        source: (0, 0),
        strict: true,
        is_ctor: false,
    });
    match ctx.make_function(func, scope, None)? {
        Value::Obj(function) => Ok(function),
        _ => ctx.throw_type("module : fonction attendue"),
    }
}

/// `namespace[key]` is read through `getter`.
fn define_getter(namespace: &Obj, key: JsStr, getter: Obj) {
    namespace.borrow_mut().props.set(key, Slot::Accessor(Some(getter), None, ENUMERABLE));
}

fn has_export(namespace: &Obj, key: &JsStr) -> bool {
    namespace.borrow().props.get(key).is_some()
}

/// Runs `source` as a module: `referrer` is its path or URL, `namespace`
/// the object its exports go on (made, and cached, by the loader before
/// this call), `bindings` names the runner adds to its scope (Bun gives
/// `require` and `__dirname` to ES modules too).
pub fn run_module(
    ctx: &mut Ctx,
    source: &str,
    options: Options,
    referrer: &str,
    namespace: &Obj,
    bindings: &[(&str, Value)],
) -> R<()> {
    let headroom = ctx.headroom;
    let options = Options { module: true, ..options };
    let program = match super::parse::parse_with(source, headroom, options) {
        Ok(program) => program,
        Err(error) => {
            let (line, column) = super::lex::position(source, error.at);
            let message = alloc::format!("{} ({} ligne {}, colonne {})", error.message, referrer, line, column);
            return ctx.throw_error("SyntaxError", &message);
        }
    };
    ctx.realm.sources.push(Rc::from(source));
    let info = program.module.clone().unwrap_or_default();
    namespace.put_key(private_key("esm"), Value::Bool(true), 0);

    // The module's scope: below the global one, `this` undefined.
    let frame = Rc::new(Frame {
        this: RefCell::new(Some(Value::Undefined)),
        function: None,
        new_target: Value::Undefined,
        home: None,
        arguments: Vec::new(),
    });
    let global_scope = ctx.realm.global_scope.clone();
    let scope = ctx.new_scope(Some(global_scope), Some(frame));
    scope.declare(JsStr::from(REFERRER), Value::str(referrer), CONSTANT);
    for (name, value) in bindings {
        scope.declare(JsStr::from(*name), value.clone(), BINDING);
    }
    for name in &program.vars {
        if !scope.has_own(name) {
            scope.declare(name.clone(), Value::Undefined, BINDING);
        }
    }
    ctx.declare_lexical(&program.body, &scope);
    ctx.hoist_functions(&program.body, &scope, false)?;

    // Exports first: a module importing this one back finds them.
    for entry in &info.exports {
        if let ExportEntry::Local(exported, local) = entry {
            let getter = thunk(ctx, Expr::Ident(local.clone()), &scope)?;
            define_getter(namespace, exported.clone(), getter);
        }
    }

    // Then the imports, each module loaded (and run) once by the loader.
    for (index, import) in info.imports.iter().enumerate() {
        let source_ns = load(ctx, referrer, import.source.as_str())?;
        let hidden = JsStr::from(alloc::format!("*import{}*", index));
        scope.declare(hidden.clone(), Value::Obj(source_ns.clone()), CONSTANT);
        for (name, local) in &import.bindings {
            let key = match name {
                ImportName::Namespace => {
                    let value = namespace_view(ctx, &source_ns);
                    scope.declare(local.clone(), Value::Obj(value), CONSTANT);
                    continue;
                }
                ImportName::Default => JsStr::from("default"),
                ImportName::Named(imported) => imported.clone(),
            };
            let present = if is_module_namespace(&source_ns) {
                has_export(&source_ns, &key)
            } else {
                key.as_str() == "default" || ops::has_property(ctx, &Value::Obj(source_ns.clone()), &key)?
            };
            if !present && !options.typescript {
                let message = alloc::format!(
                    "The requested module '{}' does not provide an export named '{}'",
                    import.source.as_str(),
                    key.as_str()
                );
                return ctx.throw_error("SyntaxError", &message);
            }
            let read = if is_module_namespace(&source_ns) || key.as_str() != "default" {
                Expr::Index(Box::new(Expr::Ident(hidden.clone())), Box::new(Expr::Str(key)), false)
            } else {
                // The default of CommonJS exports is the exports object.
                Expr::Ident(hidden.clone())
            };
            let getter = thunk(ctx, read, &scope)?;
            scope.declare(local.clone(), Value::Obj(getter), IMPORTED);
        }
    }

    // Re-exports, now that their sources are loaded.
    for (index, entry) in info.exports.iter().enumerate() {
        match entry {
            ExportEntry::Local(..) => {}
            ExportEntry::From(exported, source, imported) => {
                let source_ns = load(ctx, referrer, source.as_str())?;
                let hidden = JsStr::from(alloc::format!("*reexport{}*", index));
                scope.declare(hidden.clone(), Value::Obj(namespace_view(ctx, &source_ns)), CONSTANT);
                let read = match imported {
                    Some(name) => Expr::Index(Box::new(Expr::Ident(hidden)), Box::new(Expr::Str(name.clone())), false),
                    None => Expr::Ident(hidden),
                };
                let getter = thunk(ctx, read, &scope)?;
                define_getter(namespace, exported.clone(), getter);
            }
            ExportEntry::All(source) => {
                let source_ns = load(ctx, referrer, source.as_str())?;
                let hidden = JsStr::from(alloc::format!("*reexport{}*", index));
                scope.declare(hidden.clone(), Value::Obj(source_ns.clone()), CONSTANT);
                for key in ops::own_keys(&source_ns, false, true) {
                    if key.as_str() == "default" || has_export(namespace, &key) {
                        continue;
                    }
                    let read = Expr::Index(Box::new(Expr::Ident(hidden.clone())), Box::new(Expr::Str(key.clone())), false);
                    let getter = thunk(ctx, read, &scope)?;
                    define_getter(namespace, key, getter);
                }
            }
        }
    }

    // TypeScript: `export { T }` of a type exports nothing.
    if options.typescript {
        for entry in &info.exports {
            if let ExportEntry::Local(exported, local) = entry {
                if !ctx.is_declared(local, &scope) && ctx.realm.global.borrow().props.get(local).is_none() {
                    namespace.borrow_mut().props.remove(exported);
                }
            }
        }
    }

    for statement in &program.body {
        ctx.exec(statement, &scope)?;
    }
    Ok(())
}

/// `import * as ns`: an ES module's namespace as it is; CommonJS
/// exports seen as a namespace, `default` being the exports object.
fn namespace_view(ctx: &mut Ctx, exports: &Obj) -> Obj {
    if is_module_namespace(exports) {
        return exports.clone();
    }
    let view = ctx.new_object();
    for key in ops::own_keys(exports, false, true) {
        if let Some(value) = exports.own_value(key.as_str()) {
            view.put_key(key, value, ENUMERABLE);
        }
    }
    view.put("default", Value::Obj(exports.clone()));
    view
}

fn load(ctx: &mut Ctx, referrer: &str, specifier: &str) -> R<Obj> {
    match ctx.realm.loader {
        Some(loader) => loader(ctx, referrer, specifier),
        None => {
            let message = alloc::format!("Cannot find module '{}' (pas de chargeur de modules ici)", specifier);
            ctx.throw_error("Error", &message)
        }
    }
}

/// The path or URL of the module `scope` belongs to: its hidden binding,
/// or `__filename` in a CommonJS module, or the realm's base.
fn referrer_of(ctx: &mut Ctx, scope: &Scope) -> String {
    for name in [REFERRER, "__filename"] {
        let key = JsStr::from(name);
        if ctx.is_declared(&key, scope) {
            if let Ok(Value::Str(path)) = ctx.lookup(&key, scope) {
                return String::from(path.as_str());
            }
        }
    }
    String::from(ctx.base_url.as_str())
}

/// `import(specifier)`: a promise of the namespace.
pub fn dynamic_import(ctx: &mut Ctx, specifier: &str, scope: &Scope) -> R<Value> {
    let referrer = referrer_of(ctx, scope);
    match load(ctx, &referrer, specifier) {
        Ok(namespace) => {
            let view = namespace_view(ctx, &namespace);
            super::builtins::promise::resolved(ctx, Value::Obj(view))
        }
        Err(Ctrl::Throw(error)) => super::builtins::promise::rejected(ctx, error),
        Err(other) => Err(other),
    }
}

/// `import.meta`: `url`, and Node's `filename` and `dirname`.
pub fn import_meta(ctx: &mut Ctx, scope: &Scope) -> R<Value> {
    let referrer = referrer_of(ctx, scope);
    let meta = ctx.new_object();
    let is_path = referrer.starts_with('/');
    let url = if is_path { alloc::format!("file://{}", referrer) } else { referrer.clone() };
    meta.put("url", Value::str(&url));
    if is_path {
        let directory = match referrer.rfind('/') {
            Some(0) => String::from("/"),
            Some(index) => String::from(&referrer[..index]),
            None => String::from("."),
        };
        meta.put("filename", Value::str(&referrer));
        meta.put("dirname", Value::str(&directory));
    }
    Ok(Value::Obj(meta))
}

/// Resolves `specifier` against the module at `referrer`: a path, or a
/// URL when the referrer is one.
pub fn resolve(referrer: &str, specifier: &str) -> Option<String> {
    if !(specifier.starts_with("./") || specifier.starts_with("../") || specifier.starts_with('/')) {
        return if specifier.contains("://") { Some(String::from(specifier)) } else { None };
    }
    if referrer.contains("://") {
        let base = crate::web::url::Url::parse(referrer)?;
        return base.join(specifier).map(|url| alloc::format!("{}", url));
    }
    let directory = match referrer.rfind('/') {
        Some(index) => &referrer[..index],
        None => "",
    };
    let joined = if specifier.starts_with('/') { String::from(specifier) } else { alloc::format!("{}/{}", directory, specifier) };
    let mut parts: Vec<&str> = Vec::new();
    for part in joined.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    Some(alloc::format!("/{}", parts.join("/")))
}

/// The namespaces `host_loader` has made, by resolved name.
fn host_table(ctx: &mut Ctx) -> Obj {
    let key = private_key("hostmodules");
    let global = ctx.realm.global.clone();
    let existing = match global.borrow().props.get(&key) {
        Some(Slot::Data(Value::Obj(table), _)) => Some(table.clone()),
        _ => None,
    };
    match existing {
        Some(table) => table,
        None => {
            let table = ctx.new_object();
            global.put_key(key, Value::Obj(table.clone()), 0);
            table
        }
    }
}

/// A `<script type="module">` of a page: run once, under its address --
/// an imported module that is also a page's script is not run twice.
/// An inline one (whose address is the page's) is run each time.
pub fn run_page_module(ctx: &mut Ctx, url: &str, source: &str) -> R<()> {
    let table = host_table(ctx);
    let inline = url == ctx.base_url.as_str();
    if !inline && table.own_value(url).is_some() {
        return Ok(());
    }
    let namespace = ctx.new_object();
    if !inline {
        table.put(url, Value::Obj(namespace.clone()));
    }
    run_module(ctx, source, Options::SCRIPT, url, &namespace, &[])
}

/// A loader of ES modules whose sources the host gives: each module
/// resolved, run once, cached by its resolved name.
pub fn host_loader(ctx: &mut Ctx, referrer: &str, specifier: &str) -> R<Obj> {
    let Some(path) = resolve(referrer, specifier) else {
        let message = alloc::format!("Cannot find package '{}' imported from {}", specifier, referrer);
        return ctx.throw_error("Error", &message);
    };
    let table = host_table(ctx);
    // As TypeScript writes it: `./lib.js` for `lib.ts`, or no extension.
    let mut candidates = alloc::vec![path.clone()];
    if let Some(stem) = path.strip_suffix(".js") {
        candidates.push(alloc::format!("{}.ts", stem));
    }
    if !path.rsplit('/').next().unwrap_or("").contains('.') {
        candidates.push(alloc::format!("{}.ts", path));
        candidates.push(alloc::format!("{}.js", path));
    }
    let mut found = None;
    for candidate in candidates {
        if let Some(Value::Obj(namespace)) = table.own_value(&candidate) {
            return Ok(namespace);
        }
        if let Some(source) = ctx.host.module_source(&candidate) {
            found = Some((candidate, source));
            break;
        }
    }
    let Some((path, source)) = found else {
        let message = alloc::format!("Cannot find module '{}' imported from {}", path, referrer);
        return ctx.throw_error("Error", &message);
    };
    let namespace = ctx.new_object();
    table.put(&path, Value::Obj(namespace.clone()));
    let typescript = path.ends_with(".ts") || path.ends_with(".mts");
    let outcome = run_module(ctx, &source, Options { module: true, typescript }, &path, &namespace, &[]);
    if outcome.is_err() {
        table.borrow_mut().props.remove(&JsStr::from(path.as_str()));
    }
    outcome.map(|_| namespace)
}
