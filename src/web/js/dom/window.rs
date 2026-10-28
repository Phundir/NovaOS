//! The window: `location`, `history`, `navigator`, the storages,
//! `matchMedia`, `getComputedStyle`, animation frames, dialogs, and
//! `document.cookie`.
//!
//! Whatever leaves the page goes through the host: navigation
//! (`Host::navigate`), cookies, storage, dialogs. `history.back()` and
//! `window.scrollTo` cannot be done from inside a script: they are left
//! in `DomState` (`history_request`, `scroll_request`) for the browser to
//! act on once the script returns.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use super::super::interp::{private_key, Ctx, Timer, R};
use super::super::ops;
use super::super::value::*;
use crate::web::url::Url;

fn arg(args: &[Value], index: usize) -> Value {
    args.get(index).cloned().unwrap_or(Value::Undefined)
}

fn string_arg(ctx: &mut Ctx, args: &[Value], index: usize) -> R<String> {
    Ok(String::from(ops::to_string(ctx, &arg(args, index))?.as_str()))
}

fn page_url(ctx: &Ctx) -> Option<Url> {
    Url::parse(&ctx.base_url)
}

/// `scheme://host[:port]`, what storage and cookies are keyed by.
pub fn origin(ctx: &Ctx) -> String {
    match page_url(ctx) {
        Some(url) => alloc::format!("{}://{}", url.scheme, url.authority()),
        None => String::from("null"),
    }
}

/// A link against the page's address.
pub fn resolve(ctx: &Ctx, link: &str) -> Option<String> {
    let base = page_url(ctx)?;
    base.join(link).map(|u| u.to_string())
}

// ------------------------------------------------------------
//  location
// ------------------------------------------------------------

fn location_get(ctx: &mut Ctx, _: &Value, _: &[Value], data: &Value) -> R<Value> {
    let Value::Str(part) = data else {
        return Ok(Value::Undefined);
    };
    let Some(url) = page_url(ctx) else {
        return Ok(Value::str(if part.as_str() == "href" { ctx.base_url.as_str() } else { "" }));
    };
    let text = match part.as_str() {
        "href" => url.to_string(),
        "protocol" => alloc::format!("{}:", url.scheme),
        "host" => url.authority(),
        "hostname" => url.host.clone(),
        "port" => {
            if url.authority().contains(':') {
                alloc::format!("{}", url.port)
            } else {
                String::new()
            }
        }
        "pathname" => url.path.clone(),
        "search" => {
            if url.query.is_empty() {
                String::new()
            } else {
                alloc::format!("?{}", url.query)
            }
        }
        "hash" => {
            if url.fragment.is_empty() {
                String::new()
            } else {
                alloc::format!("#{}", url.fragment)
            }
        }
        "origin" => origin(ctx),
        _ => String::new(),
    };
    Ok(Value::Str(JsStr::from(text)))
}

fn navigate_to(ctx: &mut Ctx, link: &str, replace: bool) {
    if let Some(target) = resolve(ctx, link) {
        ctx.host.navigate(&target, replace);
    }
}

fn location_set(ctx: &mut Ctx, _: &Value, args: &[Value], data: &Value) -> R<Value> {
    let value = string_arg(ctx, args, 0)?;
    let Value::Str(part) = data else {
        return Ok(Value::Undefined);
    };
    let link = match part.as_str() {
        "hash" => alloc::format!("#{}", value.trim_start_matches('#')),
        "search" => alloc::format!("?{}", value.trim_start_matches('?')),
        "pathname" => value,
        _ => value,
    };
    navigate_to(ctx, &link, false);
    Ok(Value::Undefined)
}

/// `assign` (data false) and `replace` (data true).
fn location_go(ctx: &mut Ctx, _: &Value, args: &[Value], data: &Value) -> R<Value> {
    let link = string_arg(ctx, args, 0)?;
    navigate_to(ctx, &link, data.truthy());
    Ok(Value::Undefined)
}

fn location_reload(ctx: &mut Ctx, _: &Value, _: &[Value], _: &Value) -> R<Value> {
    let address = ctx.base_url.clone();
    ctx.host.navigate(&address, true);
    Ok(Value::Undefined)
}

// ------------------------------------------------------------
//  history
// ------------------------------------------------------------

/// `pushState` (data false) / `replaceState` (data true): the address
/// changes, the page stays.
fn history_state(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let state = arg(args, 0);
    let link = arg(args, 2);
    if !link.is_nullish() {
        let link = ops::to_string(ctx, &link)?;
        if let Some(target) = resolve(ctx, &link) {
            ctx.base_url = target;
            ctx.realm.dom.address_changed = true;
        }
    }
    if let Value::Obj(history) = this {
        history.put("state", state);
    }
    Ok(Value::Undefined)
}

/// `back` (-1), `forward` (+1), `go(n)`.
fn history_go(ctx: &mut Ctx, _: &Value, args: &[Value], data: &Value) -> R<Value> {
    let delta = match data {
        Value::Num(n) => n.to_i64_saturating(),
        _ => ops::to_integer(ctx, &arg(args, 0))?,
    };
    if delta != 0 {
        ctx.realm.dom.history_request = Some(delta as i32);
    }
    Ok(Value::Undefined)
}

// ------------------------------------------------------------
//  Storage
// ------------------------------------------------------------

fn storage_items(ctx: &mut Ctx, session: bool) -> Vec<(String, String)> {
    let origin = origin(ctx);
    ctx.host.storage(&origin, session)
}

fn storage_session(this: &Value) -> bool {
    matches!(this, Value::Obj(o) if o.own_value("\0session").is_some_and(|v| v.truthy()))
}

/// The Storage methods; data names the method.
fn storage_method(ctx: &mut Ctx, this: &Value, args: &[Value], data: &Value) -> R<Value> {
    let session = storage_session(this);
    let Value::Str(method) = data else {
        return Ok(Value::Undefined);
    };
    let mut items = storage_items(ctx, session);
    let origin = origin(ctx);
    Ok(match method.as_str() {
        "getItem" => {
            let key = string_arg(ctx, args, 0)?;
            match items.iter().find(|(k, _)| *k == key) {
                Some((_, v)) => Value::str(v),
                None => Value::Null,
            }
        }
        "setItem" => {
            let key = string_arg(ctx, args, 0)?;
            let value = string_arg(ctx, args, 1)?;
            let total: usize = items.iter().map(|(k, v)| k.len() + v.len()).sum::<usize>() + key.len() + value.len();
            if total > 5 * 1024 * 1024 {
                return super::throw_dom(ctx, "QuotaExceededError", "The quota has been exceeded.");
            }
            match items.iter_mut().find(|(k, _)| *k == key) {
                Some(entry) => entry.1 = value,
                None => items.push((key, value)),
            }
            ctx.host.set_storage(&origin, session, &items);
            Value::Undefined
        }
        "removeItem" => {
            let key = string_arg(ctx, args, 0)?;
            items.retain(|(k, _)| *k != key);
            ctx.host.set_storage(&origin, session, &items);
            Value::Undefined
        }
        "clear" => {
            ctx.host.set_storage(&origin, session, &[]);
            Value::Undefined
        }
        "key" => {
            let index = ops::to_integer(ctx, &arg(args, 0))?;
            match items.get(index.max(0) as usize) {
                Some((k, _)) => Value::str(k),
                None => Value::Null,
            }
        }
        "length" => Value::num(items.len() as i64),
        _ => Value::Undefined,
    })
}

fn make_storage(ctx: &mut Ctx, session: bool) -> Obj {
    let storage = ctx.new_object();
    storage.put_hidden("\0session", Value::Bool(session));
    for name in ["getItem", "setItem", "removeItem", "clear", "key"] {
        let native = ctx.native_with(name, 1, storage_method, Value::str(name));
        storage.put_hidden(name, Value::Obj(native));
    }
    let length = ctx.native_with("length", 0, storage_method, Value::str("length"));
    storage.borrow_mut().props.set(JsStr::from("length"), Slot::Accessor(Some(length), None, CONFIGURABLE));
    storage
}

// ------------------------------------------------------------
//  Window functions
// ------------------------------------------------------------

/// `alert` (0), `confirm` (1), `prompt` (2): shown by the host, not
/// waited for -- `confirm` answers true, `prompt` its default.
fn dialog(ctx: &mut Ctx, _: &Value, args: &[Value], data: &Value) -> R<Value> {
    let kind = match data {
        Value::Num(n) => n.to_i64_saturating(),
        _ => 0,
    };
    let message = if args.is_empty() { String::new() } else { string_arg(ctx, args, 0)? };
    let name = ["alert", "confirm", "prompt"][kind.clamp(0, 2) as usize];
    let answer = ctx.host.dialog(name, &message);
    super::super::builtins::global::console_line(ctx, name, &message);
    Ok(match kind {
        0 => Value::Undefined,
        1 => Value::Bool(answer.is_none_or(|a| a != "non")),
        _ => match answer {
            Some(text) => Value::Str(JsStr::from(text)),
            None => {
                if args.len() > 1 {
                    Value::Str(ops::to_string(ctx, &args[1])?)
                } else {
                    Value::Null
                }
            }
        },
    })
}

/// `requestAnimationFrame(callback)`: a timer of one frame (16 ms),
/// whose callback receives the time.
fn request_animation_frame(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let callback = arg(args, 0);
    ops::require_callable(ctx, &callback, "requestAnimationFrame callback")?;
    let id = ctx.realm.next_timer;
    ctx.realm.next_timer = ctx.realm.next_timer.wrapping_add(1).max(1);
    let due = ctx.now() + 16;
    let stamp = due - ctx.realm.dom.started_ms;
    ctx.realm.timers.push(Timer { id, due, interval: None, callback, args: alloc::vec![Value::num(stamp)] });
    Ok(Value::num(id as i64))
}

fn cancel_timer(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let id = ops::to_integer(ctx, &arg(args, 0))?;
    ctx.realm.timers.retain(|t| t.id as i64 != id);
    Ok(Value::Undefined)
}

fn match_media(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let query = string_arg(ctx, args, 0)?;
    let width = ctx.viewport.0;
    let result = ctx.new_object();
    let matches = crate::web::css::media_matches(&query, width) && !query.contains("prefers-color-scheme: dark") && !query.contains("print");
    result.put("matches", Value::Bool(matches));
    result.put("media", Value::Str(JsStr::from(query)));
    for name in ["addListener", "removeListener", "addEventListener", "removeEventListener"] {
        let native = ctx.native(name, 1, noop);
        result.put_hidden(name, Value::Obj(native));
    }
    result.put("onchange", Value::Null);
    Ok(Value::Obj(result))
}

/// `getComputedStyle`: the element's own declarations, with `display`
/// as its tag gives it -- not the cascade's result, which belongs to the
/// layout and is not kept for scripts.
fn computed_style(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let Some(node) = super::node_of(&arg(args, 0)) else {
        return ctx.throw_type("Failed to execute 'getComputedStyle': parameter 1 is not of type 'Element'.");
    };
    let proto = ctx.realm.protos.style.clone();
    Ok(Value::Obj(ctx.alloc(Kind::Node(node), Some(proto), "CSSStyleDeclaration")))
}

fn scroll(ctx: &mut Ctx, _: &Value, args: &[Value], data: &Value) -> R<Value> {
    let relative = data.truthy();
    let y = match arg(args, 0) {
        Value::Obj(_) => ops::get(ctx, &arg(args, 0), "top")?,
        _ => arg(args, 1),
    };
    let y = ops::to_integer(ctx, &y)?.clamp(-1_000_000, 1_000_000) as i32;
    let target = if relative { ctx.realm.dom.scroll_y + y } else { y };
    ctx.realm.dom.scroll_request = Some(target.max(0));
    Ok(Value::Undefined)
}

fn performance_now(ctx: &mut Ctx, _: &Value, _: &[Value], _: &Value) -> R<Value> {
    let elapsed = ctx.now() - ctx.realm.dom.started_ms;
    Ok(Value::num(elapsed))
}

fn window_get(ctx: &mut Ctx, _: &Value, _: &[Value], data: &Value) -> R<Value> {
    let Value::Str(name) = data else {
        return Ok(Value::Undefined);
    };
    let (width, height) = ctx.viewport;
    Ok(match name.as_str() {
        "innerWidth" | "outerWidth" => Value::num(width as i64),
        "innerHeight" | "outerHeight" => Value::num(height as i64),
        "scrollY" | "pageYOffset" => Value::num(ctx.realm.dom.scroll_y as i64),
        "scrollX" | "pageXOffset" => Value::num(0),
        _ => Value::Undefined,
    })
}

fn cookie_get(ctx: &mut Ctx, _: &Value, _: &[Value], _: &Value) -> R<Value> {
    let address = ctx.base_url.clone();
    Ok(Value::Str(JsStr::from(ctx.host.cookies(&address))))
}

fn cookie_set(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let line = string_arg(ctx, args, 0)?;
    let address = ctx.base_url.clone();
    ctx.host.set_cookie(&address, &line);
    Ok(Value::Undefined)
}

fn noop(_: &mut Ctx, _: &Value, _: &[Value], _: &Value) -> R<Value> {
    Ok(Value::Undefined)
}

fn yes(_: &mut Ctx, _: &Value, _: &[Value], _: &Value) -> R<Value> {
    Ok(Value::Bool(true))
}

fn accessor(ctx: &mut Ctx, target: &Obj, name: &str, get: NativeFn, set: Option<NativeFn>, data: Value) {
    let getter = ctx.native_with(name, 0, get, data.clone());
    let setter = set.map(|s| ctx.native_with(name, 1, s, data));
    target.borrow_mut().props.set(JsStr::from(name), Slot::Accessor(Some(getter), setter, CONFIGURABLE | ENUMERABLE));
}

pub fn install(ctx: &mut Ctx) {
    let global = ctx.realm.global.clone();
    for name in ["window", "self", "top", "parent", "frames"] {
        global.put_hidden(name, Value::Obj(global.clone()));
    }
    global.put_hidden("opener", Value::Null);
    global.put_hidden("frameElement", Value::Null);
    global.put_hidden("name", Value::str(""));
    global.put_hidden("closed", Value::Bool(false));
    global.put_hidden("isSecureContext", Value::Bool(ctx.base_url.starts_with("https:")));
    global.put_hidden("origin", Value::Str(JsStr::from(origin(ctx))));
    global.put_hidden("devicePixelRatio", Value::num(1));
    for name in ["innerWidth", "innerHeight", "outerWidth", "outerHeight", "scrollX", "scrollY", "pageXOffset", "pageYOffset"] {
        accessor(ctx, &global, name, window_get, None, Value::str(name));
    }

    let location = ctx.new_object();
    for part in ["href", "protocol", "host", "hostname", "port", "pathname", "search", "hash", "origin"] {
        accessor(ctx, &location, part, location_get, Some(location_set), Value::str(part));
    }
    let assign = ctx.native_with("assign", 1, location_go, Value::Bool(false));
    location.put_hidden("assign", Value::Obj(assign));
    let replace = ctx.native_with("replace", 1, location_go, Value::Bool(true));
    location.put_hidden("replace", Value::Obj(replace));
    ctx.method(&location, "reload", 0, location_reload);
    let to_string = ctx.native_with("toString", 0, location_get, Value::str("href"));
    location.put_hidden("toString", Value::Obj(to_string));
    global.put_hidden("location", Value::Obj(location.clone()));
    let document = ctx.realm.protos.document.clone();
    document.put_hidden("location", Value::Obj(location));
    accessor(ctx, &document, "cookie", cookie_get, Some(cookie_set), Value::Undefined);
    document.put_hidden("defaultView", Value::Obj(global.clone()));

    let history = ctx.new_object();
    history.put("state", Value::Null);
    history.put("length", Value::num(1));
    history.put("scrollRestoration", Value::str("auto"));
    ctx.method(&history, "pushState", 3, history_state);
    ctx.method(&history, "replaceState", 3, history_state);
    let back = ctx.native_with("back", 0, history_go, Value::num(-1));
    history.put_hidden("back", Value::Obj(back));
    let forward = ctx.native_with("forward", 0, history_go, Value::num(1));
    history.put_hidden("forward", Value::Obj(forward));
    let go = ctx.native_with("go", 1, history_go, Value::Undefined);
    history.put_hidden("go", Value::Obj(go));
    global.put_hidden("history", Value::Obj(history));

    let navigator = ctx.new_object();
    navigator.put("userAgent", Value::Str(JsStr::from(alloc::format!("Mozilla/5.0 (Nova OS {}; x86_64) Navigateur/{}", crate::version::VERSION, crate::version::VERSION))));
    navigator.put("appName", Value::str("Netscape"));
    navigator.put("appVersion", Value::str("5.0 (Nova OS)"));
    navigator.put("platform", Value::str("Nova OS x86_64"));
    navigator.put("vendor", Value::str("Nova OS"));
    navigator.put("language", Value::str("fr-FR"));
    let languages = ctx.new_array(alloc::vec![Value::str("fr-FR"), Value::str("fr"), Value::str("en")]);
    navigator.put("languages", Value::Obj(languages));
    navigator.put("onLine", Value::Bool(true));
    navigator.put("cookieEnabled", Value::Bool(true));
    navigator.put("doNotTrack", Value::str("1"));
    navigator.put("hardwareConcurrency", Value::num(1));
    navigator.put("maxTouchPoints", Value::num(0));
    navigator.put("webdriver", Value::Bool(false));
    ctx.method(&navigator, "javaEnabled", 0, noop);
    ctx.method(&navigator, "sendBeacon", 2, yes);
    global.put_hidden("navigator", Value::Obj(navigator));

    let local = make_storage(ctx, false);
    global.put_hidden("localStorage", Value::Obj(local));
    let session = make_storage(ctx, true);
    global.put_hidden("sessionStorage", Value::Obj(session));

    let screen = ctx.new_object();
    let (width, height) = ctx.viewport;
    for (name, value) in [("width", width), ("height", height), ("availWidth", width), ("availHeight", height), ("colorDepth", 24), ("pixelDepth", 24)] {
        screen.put(name, Value::num(value as i64));
    }
    global.put_hidden("screen", Value::Obj(screen));

    let performance = ctx.new_object();
    ctx.method(&performance, "now", 0, performance_now);
    let origin_time = ctx.realm.dom.started_ms;
    performance.put("timeOrigin", Value::num(origin_time));
    ctx.method(&performance, "mark", 1, noop);
    ctx.method(&performance, "measure", 1, noop);
    global.put_hidden("performance", Value::Obj(performance));

    for (name, kind) in [("alert", 0), ("confirm", 1), ("prompt", 2)] {
        let native = ctx.native_with(name, 1, dialog, Value::num(kind));
        global.put_hidden(name, Value::Obj(native));
    }
    ctx.method(&global, "requestAnimationFrame", 1, request_animation_frame);
    ctx.method(&global, "cancelAnimationFrame", 1, cancel_timer);
    ctx.method(&global, "requestIdleCallback", 1, request_animation_frame);
    ctx.method(&global, "cancelIdleCallback", 1, cancel_timer);
    ctx.method(&global, "matchMedia", 1, match_media);
    ctx.method(&global, "getComputedStyle", 1, computed_style);
    for (name, relative) in [("scrollTo", false), ("scroll", false), ("scrollBy", true)] {
        let native = ctx.native_with(name, 2, scroll, Value::Bool(relative));
        global.put_hidden(name, Value::Obj(native));
    }
    for name in ["focus", "blur", "print", "stop", "close", "postMessage", "moveTo", "resizeTo", "open"] {
        let native = ctx.native(name, 0, noop);
        global.put_hidden(name, Value::Obj(native));
    }
    global.put_key(private_key("window"), Value::Bool(true), 0);
}
