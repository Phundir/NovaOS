//! `EventTarget`, `Event`, and the dispatch: capture from the window
//! down, the target, then the bubble back up.
//!
//! Listeners live on the target object itself, in a property no script
//! can name (`interp::private_key`): a node's object, the window (the
//! global object), an `XMLHttpRequest` -- one mechanism for all. An inline
//! handler (`onclick="..."`) is compiled the first time its event fires,
//! as the body of `function (event)`, and kept the same way.
//!
//! An exception in a listener is reported (the console) and the dispatch
//! goes on, as the standard says; only a halt (fuel, stack, memory)
//! stops it.

use alloc::string::String;
use alloc::vec::Vec;

use super::super::interp::{private_key, Ctrl, Ctx, R};
use super::super::ops;
use super::super::value::*;
use super::{wrap, WINDOW};
use crate::web::html::NodeId;

fn arg(args: &[Value], index: usize) -> Value {
    args.get(index).cloned().unwrap_or(Value::Undefined)
}

fn listeners_key() -> JsStr {
    private_key("listeners")
}

/// Where the compiled inline handler of `type` is kept.
pub fn compiled_key(event_type: &str) -> JsStr {
    private_key(&alloc::format!("on{}", event_type))
}

fn hidden(object: &Obj, key: &JsStr) -> Option<Value> {
    match object.borrow().props.get(key) {
        Some(Slot::Data(value, _)) => Some(value.clone()),
        _ => None,
    }
}

/// The target of a listener method: `this`, or the window when called
/// bare (`addEventListener(...)` in a script).
fn target_object(ctx: &mut Ctx, this: &Value) -> Obj {
    match this {
        Value::Obj(object) => object.clone(),
        _ => ctx.realm.global.clone(),
    }
}

/// One listener: (type, callback, capture, once).
fn entries(object: &Obj) -> Vec<(JsStr, Value, bool, bool)> {
    let Some(Value::Obj(list)) = hidden(object, &listeners_key()) else {
        return Vec::new();
    };
    let items = match &list.borrow().kind {
        Kind::Array(items) => items.clone(),
        _ => Vec::new(),
    };
    items
        .into_iter()
        .filter_map(|entry| {
            let Value::Obj(entry) = entry else {
                return None;
            };
            let fields = match &entry.borrow().kind {
                Kind::Array(fields) => fields.clone(),
                _ => return None,
            };
            let Some(Value::Str(event_type)) = fields.first().cloned() else {
                return None;
            };
            Some((event_type, fields.get(1).cloned()?, fields.get(2).is_some_and(Value::truthy), fields.get(3).is_some_and(Value::truthy)))
        })
        .collect()
}

fn store(ctx: &mut Ctx, object: &Obj, list: Vec<(JsStr, Value, bool, bool)>) {
    let values: Vec<Value> = list
        .into_iter()
        .map(|(event_type, callback, capture, once)| Value::Obj(ctx.new_array(alloc::vec![Value::Str(event_type), callback, Value::Bool(capture), Value::Bool(once)])))
        .collect();
    let array = ctx.new_array(values);
    object.put_key(listeners_key(), Value::Obj(array), WRITABLE);
}

/// `capture` and `once` from the third argument: a boolean or options.
fn options(ctx: &mut Ctx, value: &Value) -> R<(bool, bool)> {
    Ok(match value {
        Value::Obj(_) => {
            let capture = ops::get(ctx, value, "capture")?.truthy();
            let once = ops::get(ctx, value, "once")?.truthy();
            (capture, once)
        }
        other => (other.truthy(), false),
    })
}

fn add_event_listener(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let object = target_object(ctx, this);
    let event_type = ops::to_string(ctx, &arg(args, 0))?;
    let callback = arg(args, 1);
    if callback.is_nullish() {
        return Ok(Value::Undefined);
    }
    let (capture, once) = options(ctx, &arg(args, 2))?;
    let mut list = entries(&object);
    if list.iter().any(|(t, c, cap, _)| *t == event_type && c.strict_eq(&callback) && *cap == capture) {
        return Ok(Value::Undefined);
    }
    if list.len() >= 1000 {
        return ctx.throw_range("trop d'ecouteurs sur une meme cible");
    }
    list.push((event_type, callback, capture, once));
    store(ctx, &object, list);
    Ok(Value::Undefined)
}

fn remove_event_listener(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let object = target_object(ctx, this);
    let event_type = ops::to_string(ctx, &arg(args, 0))?;
    let callback = arg(args, 1);
    let (capture, _) = options(ctx, &arg(args, 2))?;
    let mut list = entries(&object);
    let before = list.len();
    list.retain(|(t, c, cap, _)| !(*t == event_type && c.strict_eq(&callback) && *cap == capture));
    if list.len() != before {
        store(ctx, &object, list);
    }
    Ok(Value::Undefined)
}

fn dispatch_event(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let Value::Obj(event) = arg(args, 0) else {
        return ctx.throw_type("Failed to execute 'dispatchEvent': parameter 1 is not of type 'Event'.");
    };
    event.put("isTrusted", Value::Bool(false));
    let object = target_object(ctx, this);
    let target = if object.ptr_eq(&ctx.realm.global) { Some(WINDOW) } else { object.node() };
    match target {
        Some(node) => Ok(Value::Bool(dispatch(ctx, node, &event)?)),
        None => Ok(Value::Bool(dispatch_on_object(ctx, &object, &event)?)),
    }
}

// ------------------------------------------------------------
//  Events
// ------------------------------------------------------------

/// A new event, as the host fires them (trusted).
pub fn new_event(ctx: &mut Ctx, event_type: &str, bubbles: bool, cancelable: bool) -> Obj {
    let proto = ctx.realm.protos.event.clone();
    let event = ctx.alloc(Kind::Ordinary, Some(proto), "Event");
    event.put("type", Value::str(event_type));
    event.put("bubbles", Value::Bool(bubbles));
    event.put("cancelable", Value::Bool(cancelable));
    event.put("defaultPrevented", Value::Bool(false));
    event.put("eventPhase", Value::num(0));
    event.put("target", Value::Null);
    event.put("currentTarget", Value::Null);
    event.put("isTrusted", Value::Bool(true));
    event.put("composed", Value::Bool(false));
    let stamp = ctx.now() - ctx.realm.dom.started_ms;
    event.put("timeStamp", Value::num(stamp));
    event
}

/// A mouse event at page coordinates (x, y).
pub fn mouse_event(ctx: &mut Ctx, event_type: &str, x: i32, y: i32) -> Obj {
    let event = new_event(ctx, event_type, true, true);
    for (name, value) in [("clientX", x), ("clientY", y), ("pageX", x), ("pageY", y), ("offsetX", 0), ("offsetY", 0), ("button", 0), ("detail", 1)] {
        event.put(name, Value::num(value as i64));
    }
    for name in ["ctrlKey", "shiftKey", "altKey", "metaKey"] {
        event.put(name, Value::Bool(false));
    }
    event.put("buttons", Value::num(1));
    event
}

/// A keyboard event: `key` as the standard names it ("a", "Enter").
pub fn key_event(ctx: &mut Ctx, event_type: &str, key: &str, ctrl: bool, shift: bool) -> Obj {
    let event = new_event(ctx, event_type, true, true);
    event.put("key", Value::str(key));
    let code = if key.len() == 1 && key.chars().all(|c| c.is_ascii_alphabetic()) { alloc::format!("Key{}", key.to_ascii_uppercase()) } else { String::from(key) };
    event.put("code", Value::Str(JsStr::from(code)));
    event.put("ctrlKey", Value::Bool(ctrl));
    event.put("shiftKey", Value::Bool(shift));
    event.put("altKey", Value::Bool(false));
    event.put("metaKey", Value::Bool(false));
    event.put("repeat", Value::Bool(false));
    let key_code = match key {
        "Enter" => 13,
        "Escape" => 27,
        "Backspace" => 8,
        "Tab" => 9,
        " " => 32,
        k if k.len() == 1 => k.chars().next().map_or(0, |c| c.to_ascii_uppercase() as i64),
        _ => 0,
    };
    event.put("keyCode", Value::num(key_code));
    event.put("which", Value::num(key_code));
    event
}

fn flag(event: &Obj, name: &str) -> bool {
    hidden(event, &private_key(name)).is_some_and(|v| v.truthy())
}

fn set_flag(event: &Obj, name: &str) {
    event.put_key(private_key(name), Value::Bool(true), WRITABLE);
}

fn prevent_default(_: &mut Ctx, this: &Value, _: &[Value], _: &Value) -> R<Value> {
    if let Value::Obj(event) = this {
        if event.own_value("cancelable").is_some_and(|c| c.truthy()) {
            event.put("defaultPrevented", Value::Bool(true));
        }
    }
    Ok(Value::Undefined)
}

fn stop_propagation(_: &mut Ctx, this: &Value, _: &[Value], data: &Value) -> R<Value> {
    if let Value::Obj(event) = this {
        set_flag(event, "stop");
        if data.truthy() {
            set_flag(event, "stopNow");
        }
    }
    Ok(Value::Undefined)
}

fn composed_path(ctx: &mut Ctx, this: &Value, _: &[Value], _: &Value) -> R<Value> {
    let path = match this {
        Value::Obj(event) => hidden(event, &private_key("path")).unwrap_or(Value::Undefined),
        _ => Value::Undefined,
    };
    Ok(if path.is_undefined() { Value::Obj(ctx.new_array(Vec::new())) } else { path })
}

/// `new Event(type, init)` and the other event constructors: the init
/// dictionary's members become the event's (`detail`, `key`, `clientX`).
fn event_constructor(ctx: &mut Ctx, _: &Value, args: &[Value], data: &Value) -> R<Value> {
    if ctx.new_target.is_none() {
        return ctx.throw_type("Failed to construct 'Event': Please use the 'new' operator.");
    }
    let event_type = ops::to_string(ctx, &arg(args, 0))?;
    let init = arg(args, 1);
    let bubbles = if init.is_nullish() { false } else { ops::get(ctx, &init, "bubbles")?.truthy() };
    let cancelable = if init.is_nullish() { false } else { ops::get(ctx, &init, "cancelable")?.truthy() };
    let event = new_event(ctx, &event_type, bubbles, cancelable);
    event.put("isTrusted", Value::Bool(false));
    if let Value::Obj(init_object) = &init {
        for key in ops::own_keys(init_object, false, true) {
            if matches!(key.as_str(), "bubbles" | "cancelable") {
                continue;
            }
            let value = ops::get_key(ctx, &init, &key)?;
            event.put_key(key, value, DEFAULT);
        }
    }
    if data.truthy() && event.own_value("detail").is_none() {
        event.put("detail", Value::Null);
    }
    let proto = ctx.proto_for_new(ctx.realm.protos.event.clone())?;
    event.borrow_mut().proto = Some(proto);
    Ok(Value::Obj(event))
}

// ------------------------------------------------------------
//  Dispatch
// ------------------------------------------------------------

/// The object standing for a target: the window, or a node's object.
fn object_for(ctx: &mut Ctx, target: NodeId) -> Value {
    if target == WINDOW {
        Value::Obj(ctx.realm.global.clone())
    } else {
        wrap(ctx, target)
    }
}

/// Does this target have anything to run for `type`? Saves making an
/// object for every ancestor of a click.
fn has_handlers(ctx: &Ctx, target: NodeId, event_type: &str) -> bool {
    if target == WINDOW {
        return true;
    }
    if ctx.realm.dom.wrappers.contains_key(&target) {
        return true;
    }
    target < ctx.doc.nodes.len() && ctx.doc.attribute(target, &alloc::format!("on{}", event_type)).is_some()
}

/// The `on<type>` handler of a target: a property set by a script, or
/// the inline attribute, compiled once.
fn handler(ctx: &mut Ctx, target: NodeId, object: &Obj, event_type: &str) -> R<Option<Value>> {
    let property = alloc::format!("on{}", event_type);
    if let Some(value) = object.own_value(&property) {
        return Ok(value.as_obj().is_some_and(|o| o.is_callable()).then_some(value));
    }
    let key = compiled_key(event_type);
    if let Some(compiled) = hidden(object, &key) {
        return Ok(Some(compiled));
    }
    // `<body onload>` is the window's handler.
    let source_node = if target == WINDOW {
        ctx.doc.preorder(0).into_iter().find(|n| ctx.doc.tag(*n) == Some("body"))
    } else if target < ctx.doc.nodes.len() {
        Some(target)
    } else {
        // An object that is no node (an XMLHttpRequest): no attribute.
        None
    };
    let Some(source) = source_node.and_then(|n| ctx.doc.attribute(n, &property)).map(String::from) else {
        return Ok(None);
    };
    let compiled = match ctx.eval_global(&alloc::format!("(function (event) {{\n{}\n}})", source)) {
        Ok(function) => function,
        Err(Ctrl::Throw(error)) => {
            let text = ctx.describe_error(&error);
            ctx.report(text);
            return Ok(None);
        }
        Err(other) => return Err(other),
    };
    object.put_key(key, compiled.clone(), WRITABLE);
    Ok(Some(compiled))
}

/// Runs a target's listeners for one phase: 1 capture, 2 at target,
/// 3 bubble.
fn invoke(ctx: &mut Ctx, target: NodeId, current: &Value, event: &Obj, phase: i64) -> R<()> {
    let Value::Obj(object) = current else {
        return Ok(());
    };
    let event_type = match event.own_value("type") {
        Some(Value::Str(t)) => t,
        _ => return Ok(()),
    };
    event.put("currentTarget", current.clone());
    event.put("eventPhase", Value::num(phase));
    let event_value = Value::Obj(event.clone());
    if phase != 1 {
        if let Some(function) = handler(ctx, target, object, &event_type)? {
            match ctx.call(&function, current.clone(), &[event_value.clone()]) {
                Ok(Value::Bool(false)) => {
                    prevent_default(ctx, &event_value, &[], &Value::Undefined)?;
                }
                Ok(_) => {}
                Err(Ctrl::Throw(error)) => {
                    let text = ctx.describe_error(&error);
                    ctx.report(text);
                }
                Err(other) => return Err(other),
            }
        }
    }
    let list = entries(object);
    for (listener_type, callback, capture, once) in list {
        if listener_type != event_type || flag(event, "stopNow") {
            continue;
        }
        let wanted = match phase {
            1 => capture,
            3 => !capture,
            _ => true,
        };
        if !wanted {
            continue;
        }
        if once {
            let remaining: Vec<_> = entries(object)
                .into_iter()
                .filter(|(t, c, cap, _)| !(*t == listener_type && c.strict_eq(&callback) && *cap == capture))
                .collect();
            store(ctx, object, remaining);
        }
        ctx.tick()?;
        let outcome = if callback.as_obj().is_some_and(|c| c.is_callable()) {
            ctx.call(&callback, current.clone(), &[event_value.clone()])
        } else {
            match ops::get(ctx, &callback, "handleEvent") {
                Ok(method) if method.as_obj().is_some_and(|m| m.is_callable()) => ctx.call(&method, callback.clone(), &[event_value.clone()]),
                Ok(_) => Ok(Value::Undefined),
                Err(error) => Err(error),
            }
        };
        match outcome {
            Ok(_) => {}
            Err(Ctrl::Throw(error)) => {
                let text = ctx.describe_error(&error);
                ctx.report(text);
            }
            Err(other) => return Err(other),
        }
    }
    Ok(())
}

/// Dispatches `event` at `target` (a node, or `WINDOW`). Returns false
/// when a listener cancelled it (`preventDefault`).
pub fn dispatch(ctx: &mut Ctx, target: NodeId, event: &Obj) -> R<bool> {
    let event_type = match event.own_value("type") {
        Some(Value::Str(t)) => String::from(t.as_str()),
        _ => String::new(),
    };
    // The path: the target, its ancestors, the window.
    let mut path = alloc::vec![target];
    if target != WINDOW {
        let mut current = ctx.doc.nodes.get(target).and_then(|n| n.parent);
        while let Some(node) = current {
            path.push(node);
            current = ctx.doc.nodes[node].parent;
            if path.len() > 4096 {
                break;
            }
        }
        // A node out of the document has no window above it.
        if path.last() == Some(&0) {
            path.push(WINDOW);
        }
    }
    let target_object = object_for(ctx, target);
    event.put("target", target_object.clone());
    event.put("srcElement", target_object.clone());
    let objects: Vec<Value> = path.iter().map(|n| if has_handlers(ctx, *n, &event_type) { object_for(ctx, *n) } else { Value::Undefined }).collect();
    let path_array = ctx.new_array(objects.iter().filter(|o| !o.is_undefined()).cloned().collect());
    event.put_key(private_key("path"), Value::Obj(path_array), WRITABLE);
    let bubbles = event.own_value("bubbles").is_some_and(|b| b.truthy());

    // Capture, from the window down.
    for index in (1..path.len()).rev() {
        if flag(event, "stop") {
            break;
        }
        if !objects[index].is_undefined() {
            invoke(ctx, path[index], &objects[index], event, 1)?;
        }
    }
    if !flag(event, "stop") {
        invoke(ctx, target, &target_object, event, 2)?;
    }
    if bubbles {
        for index in 1..path.len() {
            if flag(event, "stop") {
                break;
            }
            if !objects[index].is_undefined() {
                invoke(ctx, path[index], &objects[index], event, 3)?;
            }
        }
    }
    event.put("currentTarget", Value::Null);
    event.put("eventPhase", Value::num(0));
    Ok(!event.own_value("defaultPrevented").is_some_and(|p| p.truthy()))
}

/// Dispatch on an object that is no node: an `XMLHttpRequest`.
pub fn dispatch_on_object(ctx: &mut Ctx, object: &Obj, event: &Obj) -> R<bool> {
    let current = Value::Obj(object.clone());
    event.put("target", current.clone());
    // A sentinel id: no inline attribute to look for.
    invoke(ctx, usize::MAX - 1, &current, event, 2)?;
    event.put("currentTarget", Value::Null);
    Ok(!event.own_value("defaultPrevented").is_some_and(|p| p.truthy()))
}

/// Fires a simple event by name at a target (`load`, `input`...).
pub fn fire(ctx: &mut Ctx, target: NodeId, event_type: &str, bubbles: bool, cancelable: bool) -> R<bool> {
    let event = new_event(ctx, event_type, bubbles, cancelable);
    dispatch(ctx, target, &event)
}

/// Fires at an object (`load` of an XMLHttpRequest).
pub fn fire_on_object(ctx: &mut Ctx, object: &Obj, event_type: &str) -> R<bool> {
    let event = new_event(ctx, event_type, false, false);
    dispatch_on_object(ctx, object, &event)
}

pub fn install(ctx: &mut Ctx) {
    let target = ctx.realm.protos.event_target.clone();
    ctx.method(&target, "addEventListener", 2, add_event_listener);
    ctx.method(&target, "removeEventListener", 2, remove_event_listener);
    ctx.method(&target, "dispatchEvent", 1, dispatch_event);

    let event = ctx.realm.protos.event.clone();
    ctx.method(&event, "preventDefault", 0, prevent_default);
    let stop = ctx.native_with("stopPropagation", 0, stop_propagation, Value::Bool(false));
    event.put_hidden("stopPropagation", Value::Obj(stop));
    let stop_now = ctx.native_with("stopImmediatePropagation", 0, stop_propagation, Value::Bool(true));
    event.put_hidden("stopImmediatePropagation", Value::Obj(stop_now));
    ctx.method(&event, "composedPath", 0, composed_path);
    for (name, value) in [("NONE", 0), ("CAPTURING_PHASE", 1), ("AT_TARGET", 2), ("BUBBLING_PHASE", 3)] {
        event.put_key(JsStr::from(name), Value::num(value), 0);
    }
    let global = ctx.realm.global.clone();
    for (name, custom) in [
        ("Event", false),
        ("CustomEvent", true),
        ("UIEvent", false),
        ("MouseEvent", false),
        ("PointerEvent", false),
        ("KeyboardEvent", false),
        ("InputEvent", false),
        ("FocusEvent", false),
        ("SubmitEvent", false),
        ("ProgressEvent", false),
        ("ErrorEvent", false),
        ("MessageEvent", false),
        ("PopStateEvent", false),
        ("HashChangeEvent", false),
    ] {
        let constructor = ctx.native_with(name, 1, event_constructor, Value::Bool(custom));
        constructor.put_key(JsStr::from("prototype"), Value::Obj(event.clone()), 0);
        if name == "Event" {
            event.put_hidden("constructor", Value::Obj(constructor.clone()));
        }
        global.put_hidden(name, Value::Obj(constructor));
    }
}
