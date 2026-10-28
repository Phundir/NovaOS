//! `fetch`, `Headers`, `Response`, `XMLHttpRequest`, on the host's
//! client (`Host::fetch`: the HTTP(S) client of 15.0.4 in the browser).
//!
//! **The request is made synchronously**, inside the call: `fetch`
//! returns a promise already settled, and an asynchronous
//! `XMLHttpRequest` fires its events from the job queue, after the
//! script. What a page observes is the order of the language -- its
//! `then` runs after the current script -- but the window waits for the
//! transfer. A transfer in a task of its own, the promise settled when it
//! ends, is the next step; named here rather than discovered.

use alloc::string::String;
use alloc::vec::Vec;

use super::super::builtins::promise;
use super::super::interp::{private_key, Ctrl, Ctx, Job, R};
use super::super::ops;
use super::super::value::*;

fn arg(args: &[Value], index: usize) -> Value {
    args.get(index).cloned().unwrap_or(Value::Undefined)
}

fn hidden(object: &Obj, name: &str) -> Value {
    match object.borrow().props.get(&private_key(name)) {
        Some(Slot::Data(value, _)) => value.clone(),
        _ => Value::Undefined,
    }
}

fn set_hidden(object: &Obj, name: &str, value: Value) {
    object.put_key(private_key(name), value, WRITABLE);
}

type Answer = (u16, String, Vec<(String, String)>, Vec<u8>, String);

// ------------------------------------------------------------
//  Headers
// ------------------------------------------------------------

fn header_list(object: &Obj) -> Vec<(String, String)> {
    let Value::Obj(list) = hidden(object, "headers") else {
        return Vec::new();
    };
    let items = match &list.borrow().kind {
        Kind::Array(items) => items.clone(),
        _ => Vec::new(),
    };
    items
        .into_iter()
        .filter_map(|pair| {
            let Value::Obj(pair) = pair else {
                return None;
            };
            let fields = match &pair.borrow().kind {
                Kind::Array(fields) => fields.clone(),
                _ => return None,
            };
            match (fields.first(), fields.get(1)) {
                (Some(Value::Str(name)), Some(Value::Str(value))) => Some((String::from(name.as_str()), String::from(value.as_str()))),
                _ => None,
            }
        })
        .collect()
}

fn store_headers(ctx: &mut Ctx, object: &Obj, list: &[(String, String)]) {
    let values: Vec<Value> = list.iter().map(|(n, v)| Value::Obj(ctx.new_array(alloc::vec![Value::str(n), Value::str(v)]))).collect();
    let array = ctx.new_array(values);
    set_hidden(object, "headers", Value::Obj(array));
}

fn new_headers(ctx: &mut Ctx, list: &[(String, String)]) -> Obj {
    let proto = ctx.realm.dom.headers_proto.clone().unwrap_or_else(|| ctx.realm.protos.object.clone());
    let object = ctx.alloc(Kind::Ordinary, Some(proto), "Headers");
    store_headers(ctx, &object, list);
    object
}

/// Headers from what a script passes: a `Headers`, an object, pairs.
fn headers_from(ctx: &mut Ctx, value: &Value) -> R<Vec<(String, String)>> {
    let Value::Obj(object) = value else {
        return Ok(Vec::new());
    };
    if !hidden(object, "headers").is_undefined() {
        return Ok(header_list(object));
    }
    let mut list = Vec::new();
    if object.is_array() {
        for pair in ops::list_from_array_like(ctx, value)? {
            let name = ops::get_value_key(ctx, &pair, &Value::num(0))?;
            let item = ops::get_value_key(ctx, &pair, &Value::num(1))?;
            list.push((String::from(ops::to_string(ctx, &name)?.as_str()).to_ascii_lowercase(), String::from(ops::to_string(ctx, &item)?.as_str())));
        }
        return Ok(list);
    }
    for key in ops::own_keys(object, false, true) {
        let item = ops::get_key(ctx, value, &key)?;
        list.push((key.as_str().to_ascii_lowercase(), String::from(ops::to_string(ctx, &item)?.as_str())));
    }
    Ok(list)
}

fn headers_constructor(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let list = headers_from(ctx, &arg(args, 0))?;
    Ok(Value::Obj(new_headers(ctx, &list)))
}

fn headers_method(ctx: &mut Ctx, this: &Value, args: &[Value], data: &Value) -> R<Value> {
    let (Value::Obj(object), Value::Str(method)) = (this, data) else {
        return ctx.throw_type("Illegal invocation");
    };
    let mut list = header_list(object);
    let name = if args.is_empty() { String::new() } else { String::from(ops::to_string(ctx, &args[0])?.as_str()).to_ascii_lowercase() };
    Ok(match method.as_str() {
        "get" => {
            let values: Vec<&str> = list.iter().filter(|(n, _)| *n == name).map(|(_, v)| v.as_str()).collect();
            if values.is_empty() { Value::Null } else { Value::Str(JsStr::from(values.join(", "))) }
        }
        "has" => Value::Bool(list.iter().any(|(n, _)| *n == name)),
        "set" | "append" => {
            let value = String::from(ops::to_string(ctx, &arg(args, 1))?.as_str());
            if method.as_str() == "set" {
                list.retain(|(n, _)| *n != name);
            }
            list.push((name, value));
            store_headers(ctx, object, &list);
            Value::Undefined
        }
        "delete" => {
            list.retain(|(n, _)| *n != name);
            store_headers(ctx, object, &list);
            Value::Undefined
        }
        "forEach" => {
            let callback = arg(args, 0);
            for (n, v) in list {
                ctx.call(&callback, arg(args, 1), &[Value::str(&v), Value::str(&n), this.clone()])?;
            }
            Value::Undefined
        }
        "entries" | "keys" | "values" => {
            let items: Vec<Value> = list
                .iter()
                .map(|(n, v)| match method.as_str() {
                    "keys" => Value::str(n),
                    "values" => Value::str(v),
                    _ => Value::Obj(ctx.new_array(alloc::vec![Value::str(n), Value::str(v)])),
                })
                .collect();
            let proto = ctx.realm.protos.iterator.clone();
            Value::Obj(ctx.alloc(Kind::Iterator(IterState::List(items, 0)), Some(proto), "Headers Iterator"))
        }
        _ => Value::Undefined,
    })
}

// ------------------------------------------------------------
//  Response
// ------------------------------------------------------------

fn new_response(ctx: &mut Ctx, answer: &Answer) -> Obj {
    let (status, status_text, headers, body, url) = answer;
    let proto = ctx.realm.dom.response_proto.clone().unwrap_or_else(|| ctx.realm.protos.object.clone());
    let response = ctx.alloc(Kind::Ordinary, Some(proto), "Response");
    response.put("status", Value::num(*status as i64));
    response.put("statusText", Value::str(status_text));
    response.put("ok", Value::Bool((200..300).contains(status)));
    response.put("url", Value::str(url));
    response.put("redirected", Value::Bool(false));
    response.put("type", Value::str("basic"));
    response.put("bodyUsed", Value::Bool(false));
    let lowered: Vec<(String, String)> = headers.iter().map(|(n, v)| (n.to_ascii_lowercase(), v.clone())).collect();
    let charset = lowered
        .iter()
        .find(|(n, _)| n == "content-type")
        .and_then(|(_, v)| v.split(';').find_map(|p| p.trim().strip_prefix("charset=").map(|c| String::from(c.trim_matches('"')))));
    let headers_object = new_headers(ctx, &lowered);
    response.put("headers", Value::Obj(headers_object));
    let text = crate::web::text::decode(body, charset.as_deref());
    set_hidden(&response, "body", Value::Str(JsStr::from(text)));
    response
}

/// `text()` (data 0), `json()` (1), `arrayBuffer()`/`blob()` (2).
fn response_body(ctx: &mut Ctx, this: &Value, _: &[Value], data: &Value) -> R<Value> {
    let Value::Obj(response) = this else {
        return ctx.throw_type("Illegal invocation");
    };
    if response.own_value("bodyUsed").is_some_and(|u| u.truthy()) {
        let error = ctx.error_object(ctx.realm.protos.type_error.clone(), "TypeError", "Body has already been consumed.");
        return promise::rejected(ctx, Value::Obj(error));
    }
    response.put("bodyUsed", Value::Bool(true));
    let body = hidden(response, "body");
    let which = match data {
        Value::Num(n) => n.to_i64_saturating(),
        _ => 0,
    };
    match which {
        0 => promise::resolved(ctx, body),
        1 => {
            let text = ops::to_string(ctx, &body)?;
            match super::super::builtins::json::parse_text(ctx, &text) {
                Ok(value) => promise::resolved(ctx, value),
                Err(Ctrl::Throw(error)) => promise::rejected(ctx, error),
                Err(other) => Err(other),
            }
        }
        _ => {
            let error = ctx.error_object(ctx.realm.protos.type_error.clone(), "TypeError", "arrayBuffer et blob ne sont pas pris en charge");
            promise::rejected(ctx, Value::Obj(error))
        }
    }
}

fn response_clone(ctx: &mut Ctx, this: &Value, _: &[Value], _: &Value) -> R<Value> {
    let Value::Obj(response) = this else {
        return ctx.throw_type("Illegal invocation");
    };
    let proto = response.proto();
    let copy = ctx.alloc(Kind::Ordinary, proto, "Response");
    for key in ops::own_keys(response, false, false) {
        if let Some(value) = response.own_value(&key) {
            copy.put_key(key, value, DEFAULT);
        }
    }
    set_hidden(&copy, "body", hidden(response, "body"));
    Ok(Value::Obj(copy))
}

/// `new Response(body, init)`, for scripts that build one.
fn response_constructor(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let body = match arg(args, 0) {
        Value::Undefined | Value::Null => String::new(),
        other => String::from(ops::to_string(ctx, &other)?.as_str()),
    };
    let init = arg(args, 1);
    let status = if init.is_nullish() {
        200
    } else {
        let given = ops::get(ctx, &init, "status")?;
        ops::to_integer(ctx, &given)?.clamp(0, 999)
    };
    let status = if status == 0 { 200 } else { status as u16 };
    let headers = if init.is_nullish() {
        Vec::new()
    } else {
        let given = ops::get(ctx, &init, "headers")?;
        headers_from(ctx, &given)?
    };
    let answer = (status, String::new(), headers, body.into_bytes(), String::new());
    Ok(Value::Obj(new_response(ctx, &answer)))
}

// ------------------------------------------------------------
//  fetch
// ------------------------------------------------------------

/// The request a script describes: method, absolute URL, headers, body.
fn request_from(ctx: &mut Ctx, input: &Value, init: &Value) -> R<(String, String, Vec<(String, String)>, Option<Vec<u8>>)> {
    let link = match input {
        Value::Obj(_) => {
            let url = ops::get(ctx, input, "url")?;
            let url = if url.is_undefined() { ops::get(ctx, input, "href")? } else { url };
            ops::to_string(ctx, &url)?
        }
        other => ops::to_string(ctx, other)?,
    };
    let Some(url) = super::window::resolve(ctx, &link) else {
        let message = alloc::format!("Failed to fetch: adresse invalide '{}'", link.as_str());
        return ctx.throw_type(&message);
    };
    let mut method = String::from("GET");
    let mut headers = Vec::new();
    let mut body = None;
    if let Value::Obj(_) = init {
        let given = ops::get(ctx, init, "method")?;
        if !given.is_undefined() {
            method = String::from(ops::to_string(ctx, &given)?.as_str()).to_ascii_uppercase();
        }
        let given = ops::get(ctx, init, "headers")?;
        headers = headers_from(ctx, &given)?;
        let given = ops::get(ctx, init, "body")?;
        if !given.is_nullish() {
            body = Some(String::from(ops::to_string(ctx, &given)?.as_str()).into_bytes());
        }
    }
    Ok((method, url, headers, body))
}

fn fetch(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let (method, url, headers, body) = match request_from(ctx, &arg(args, 0), &arg(args, 1)) {
        Ok(request) => request,
        Err(Ctrl::Throw(error)) => return promise::rejected(ctx, error),
        Err(other) => return Err(other),
    };
    match ctx.host.fetch(&method, &url, &headers, body) {
        Ok(answer) => {
            let response = new_response(ctx, &answer);
            promise::resolved(ctx, Value::Obj(response))
        }
        Err(reason) => {
            let message = alloc::format!("Failed to fetch: {}", reason);
            let error = ctx.error_object(ctx.realm.protos.type_error.clone(), "TypeError", &message);
            promise::rejected(ctx, Value::Obj(error))
        }
    }
}

// ------------------------------------------------------------
//  XMLHttpRequest
// ------------------------------------------------------------

fn this_xhr(ctx: &mut Ctx, this: &Value) -> R<Obj> {
    match this {
        Value::Obj(object) if object.class_name() == "XMLHttpRequest" => Ok(object.clone()),
        _ => ctx.throw_type("Illegal invocation"),
    }
}

fn xhr_constructor(ctx: &mut Ctx, _: &Value, _: &[Value], _: &Value) -> R<Value> {
    let proto = ctx.realm.dom.xhr_proto.clone().unwrap_or_else(|| ctx.realm.protos.object.clone());
    let xhr = ctx.alloc(Kind::Ordinary, Some(proto), "XMLHttpRequest");
    xhr.put("readyState", Value::num(0));
    xhr.put("status", Value::num(0));
    xhr.put("statusText", Value::str(""));
    xhr.put("responseText", Value::str(""));
    xhr.put("response", Value::str(""));
    xhr.put("responseType", Value::str(""));
    xhr.put("responseURL", Value::str(""));
    xhr.put("timeout", Value::num(0));
    xhr.put("withCredentials", Value::Bool(false));
    Ok(Value::Obj(xhr))
}

fn xhr_open(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let xhr = this_xhr(ctx, this)?;
    let method = String::from(ops::to_string(ctx, &arg(args, 0))?.as_str()).to_ascii_uppercase();
    let link = ops::to_string(ctx, &arg(args, 1))?;
    let asynchronous = !matches!(arg(args, 2), Value::Bool(false));
    set_hidden(&xhr, "method", Value::Str(JsStr::from(method)));
    set_hidden(&xhr, "url", Value::Str(link));
    set_hidden(&xhr, "async", Value::Bool(asynchronous));
    let empty = new_headers(ctx, &[]);
    set_hidden(&xhr, "request", Value::Obj(empty));
    xhr.put("readyState", Value::num(1));
    Ok(Value::Undefined)
}

fn xhr_set_header(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let xhr = this_xhr(ctx, this)?;
    if let Value::Obj(headers) = hidden(&xhr, "request") {
        let mut list = header_list(&headers);
        let name = String::from(ops::to_string(ctx, &arg(args, 0))?.as_str()).to_ascii_lowercase();
        let value = String::from(ops::to_string(ctx, &arg(args, 1))?.as_str());
        list.push((name, value));
        store_headers(ctx, &headers, &list);
    }
    Ok(Value::Undefined)
}

/// The events of a finished request; as a job for an asynchronous one.
fn xhr_finish(ctx: &mut Ctx, _: &Value, _: &[Value], data: &Value) -> R<Value> {
    let Value::Obj(xhr) = data else {
        return Ok(Value::Undefined);
    };
    let failed = hidden(xhr, "failed").truthy();
    super::events::fire_on_object(ctx, xhr, "readystatechange")?;
    super::events::fire_on_object(ctx, xhr, if failed { "error" } else { "load" })?;
    super::events::fire_on_object(ctx, xhr, "loadend")?;
    Ok(Value::Undefined)
}

fn xhr_send(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let xhr = this_xhr(ctx, this)?;
    let method = ops::to_string(ctx, &hidden(&xhr, "method"))?;
    let link = ops::to_string(ctx, &hidden(&xhr, "url"))?;
    let headers = match hidden(&xhr, "request") {
        Value::Obj(headers) => header_list(&headers),
        _ => Vec::new(),
    };
    let body = match arg(args, 0) {
        Value::Undefined | Value::Null => None,
        other => Some(String::from(ops::to_string(ctx, &other)?.as_str()).into_bytes()),
    };
    let outcome = match super::window::resolve(ctx, &link) {
        Some(url) => ctx.host.fetch(&method, &url, &headers, body),
        None => Err(String::from("adresse invalide")),
    };
    match outcome {
        Ok(answer) => {
            let response = new_response(ctx, &answer);
            let text = hidden(&response, "body");
            xhr.put("status", Value::num(answer.0 as i64));
            xhr.put("statusText", Value::str(&answer.1));
            xhr.put("responseURL", Value::str(&answer.4));
            xhr.put("responseText", text.clone());
            let wanted = ops::get(ctx, &Value::Obj(xhr.clone()), "responseType")?;
            let response_value = if matches!(&wanted, Value::Str(s) if s.as_str() == "json") {
                let text = ops::to_string(ctx, &text)?;
                super::super::builtins::json::parse_text(ctx, &text).unwrap_or(Value::Null)
            } else {
                text
            };
            xhr.put("response", response_value);
            set_hidden(&xhr, "answer", Value::Obj(response));
        }
        Err(reason) => {
            set_hidden(&xhr, "failed", Value::Bool(true));
            crate::web::js::builtins::global::console_line(ctx, "erreur", &alloc::format!("XMLHttpRequest : {}", reason));
        }
    }
    xhr.put("readyState", Value::num(4));
    if hidden(&xhr, "async").truthy() {
        let finish = ctx.native_with("", 0, xhr_finish, Value::Obj(xhr.clone()));
        ctx.realm.jobs.push_back(Job::Call(Value::Obj(finish), Vec::new()));
    } else {
        xhr_finish(ctx, &Value::Undefined, &[], &Value::Obj(xhr))?;
    }
    Ok(Value::Undefined)
}

fn xhr_response_header(ctx: &mut Ctx, this: &Value, args: &[Value], data: &Value) -> R<Value> {
    let xhr = this_xhr(ctx, this)?;
    let Value::Obj(response) = hidden(&xhr, "answer") else {
        return Ok(if data.truthy() { Value::str("") } else { Value::Null });
    };
    let Value::Obj(headers) = response.own_value("headers").unwrap_or(Value::Undefined) else {
        return Ok(Value::Null);
    };
    let list = header_list(&headers);
    if data.truthy() {
        let all: Vec<String> = list.iter().map(|(n, v)| alloc::format!("{}: {}\r\n", n, v)).collect();
        return Ok(Value::Str(JsStr::from(all.concat())));
    }
    let name = String::from(ops::to_string(ctx, &arg(args, 0))?.as_str()).to_ascii_lowercase();
    let values: Vec<&str> = list.iter().filter(|(n, _)| *n == name).map(|(_, v)| v.as_str()).collect();
    Ok(if values.is_empty() { Value::Null } else { Value::Str(JsStr::from(values.join(", "))) })
}

fn noop(_: &mut Ctx, _: &Value, _: &[Value], _: &Value) -> R<Value> {
    Ok(Value::Undefined)
}

pub fn install(ctx: &mut Ctx) {
    let global = ctx.realm.global.clone();
    ctx.method(&global, "fetch", 1, fetch);

    let headers_proto = ctx.new_object();
    for name in ["get", "has", "set", "append", "delete", "forEach", "entries", "keys", "values"] {
        let native = ctx.native_with(name, 1, headers_method, Value::str(name));
        headers_proto.put_hidden(name, Value::Obj(native));
    }
    let iterator = ctx.realm.symbols.iterator.clone();
    let entries = headers_proto.own_value("entries").unwrap_or(Value::Undefined);
    headers_proto.put_key(iterator, entries, HIDDEN);
    super::super::builtins::constructor(ctx, "Headers", 0, headers_constructor, &headers_proto);
    ctx.realm.dom.headers_proto = Some(headers_proto);

    let response_proto = ctx.new_object();
    for (name, which) in [("text", 0), ("json", 1), ("arrayBuffer", 2), ("blob", 2), ("formData", 2)] {
        let native = ctx.native_with(name, 0, response_body, Value::num(which));
        response_proto.put_hidden(name, Value::Obj(native));
    }
    ctx.method(&response_proto, "clone", 0, response_clone);
    super::super::builtins::constructor(ctx, "Response", 1, response_constructor, &response_proto);
    ctx.realm.dom.response_proto = Some(response_proto);

    let xhr_proto = ctx.alloc(Kind::Ordinary, Some(ctx.realm.protos.event_target.clone()), "XMLHttpRequest");
    ctx.method(&xhr_proto, "open", 3, xhr_open);
    ctx.method(&xhr_proto, "send", 1, xhr_send);
    ctx.method(&xhr_proto, "setRequestHeader", 2, xhr_set_header);
    let one = ctx.native_with("getResponseHeader", 1, xhr_response_header, Value::Bool(false));
    xhr_proto.put_hidden("getResponseHeader", Value::Obj(one));
    let all = ctx.native_with("getAllResponseHeaders", 0, xhr_response_header, Value::Bool(true));
    xhr_proto.put_hidden("getAllResponseHeaders", Value::Obj(all));
    ctx.method(&xhr_proto, "abort", 0, noop);
    ctx.method(&xhr_proto, "overrideMimeType", 1, noop);
    let constructor = super::super::builtins::constructor(ctx, "XMLHttpRequest", 0, xhr_constructor, &xhr_proto);
    for (name, value) in [("UNSENT", 0), ("OPENED", 1), ("HEADERS_RECEIVED", 2), ("LOADING", 3), ("DONE", 4)] {
        constructor.put_key(JsStr::from(name), Value::num(value), 0);
        xhr_proto.put_key(JsStr::from(name), Value::num(value), 0);
    }
    ctx.realm.dom.xhr_proto = Some(xhr_proto);
}
