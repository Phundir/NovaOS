//! `Promise`, on the job queue of the realm.
//!
//! The machinery is the specification's: a promise holds its reactions
//! until it settles; settling queues one job per reaction
//! (`interp::Job::Reaction`), and `Ctx::run_jobs` runs them after the
//! script, in order. Resolving with a thenable queues a
//! `Job::Thenable`, so that `then` is called on a clean stack, as the
//! language requires.
//!
//! The resolving functions share a record (an object no script sees)
//! saying whether one of them already ran: the second call of either is
//! ignored.
//!
//! A rejection nobody handled by the time the queue is empty is reported
//! once, as a browser's console says "Uncaught (in promise)".

use alloc::vec::Vec;

use super::arg;
use crate::web::js::interp::{private_key, Ctrl, Ctx, Job, R};
use crate::web::js::ops;
use crate::web::js::value::*;

fn is_promise(value: &Value) -> bool {
    matches!(value, Value::Obj(o) if matches!(o.borrow().kind, Kind::Promise(_)))
}

pub fn new_promise(ctx: &mut Ctx, proto: Obj) -> Obj {
    ctx.alloc(
        Kind::Promise(PromiseData { state: PromiseState::Pending, value: Value::Undefined, reactions: Vec::new(), handled: false }),
        Some(proto),
        "Promise",
    )
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

/// A record no script can reach: no prototype, private keys only.
fn record(ctx: &mut Ctx) -> Obj {
    ctx.alloc(Kind::Ordinary, None, "Record")
}

/// The pair of resolving functions of `promise`.
pub fn resolving_functions(ctx: &mut Ctx, promise: &Obj) -> (Value, Value) {
    let shared = record(ctx);
    set_hidden(&shared, "promise", Value::Obj(promise.clone()));
    set_hidden(&shared, "done", Value::Bool(false));
    let resolve = ctx.native_with("", 1, resolve_function, Value::Obj(shared.clone()));
    let reject = ctx.native_with("", 1, reject_function, Value::Obj(shared));
    (Value::Obj(resolve), Value::Obj(reject))
}

/// Takes the record's promise, unless a resolving function already ran.
fn claim(data: &Value) -> Option<Obj> {
    let Value::Obj(shared) = data else {
        return None;
    };
    if hidden(shared, "done").truthy() {
        return None;
    }
    set_hidden(shared, "done", Value::Bool(true));
    hidden(shared, "promise").as_obj().cloned()
}

fn resolve_function(ctx: &mut Ctx, _: &Value, args: &[Value], data: &Value) -> R<Value> {
    if let Some(promise) = claim(data) {
        resolve_promise(ctx, &promise, arg(args, 0))?;
    }
    Ok(Value::Undefined)
}

fn reject_function(ctx: &mut Ctx, _: &Value, args: &[Value], data: &Value) -> R<Value> {
    if let Some(promise) = claim(data) {
        reject_promise(ctx, &promise, arg(args, 0));
    }
    Ok(Value::Undefined)
}

/// The steps of a resolve function, once it is known to be the first.
fn resolve_promise(ctx: &mut Ctx, promise: &Obj, resolution: Value) -> R<()> {
    if let Value::Obj(object) = &resolution {
        if object.ptr_eq(promise) {
            let error = ctx.error_object(ctx.realm.protos.type_error.clone(), "TypeError", "Chaining cycle detected for promise");
            reject_promise(ctx, promise, Value::Obj(error));
            return Ok(());
        }
        let then = match ops::get(ctx, &resolution, "then") {
            Ok(then) => then,
            Err(Ctrl::Throw(error)) => {
                reject_promise(ctx, promise, error);
                return Ok(());
            }
            Err(other) => return Err(other),
        };
        if then.as_obj().is_some_and(|t| t.is_callable()) {
            let (resolve, reject) = resolving_functions(ctx, promise);
            ctx.realm.jobs.push_back(Job::Thenable(resolution, then, resolve, reject));
            return Ok(());
        }
    }
    settle(ctx, promise, resolution, PromiseState::Fulfilled);
    Ok(())
}

pub fn reject_promise(ctx: &mut Ctx, promise: &Obj, reason: Value) {
    settle(ctx, promise, reason, PromiseState::Rejected);
}

fn settle(ctx: &mut Ctx, promise: &Obj, value: Value, state: PromiseState) {
    let (reactions, unhandled) = {
        let mut borrowed = promise.borrow_mut();
        let Kind::Promise(data) = &mut borrowed.kind else {
            return;
        };
        if data.state != PromiseState::Pending {
            return;
        }
        data.state = state;
        data.value = value.clone();
        let reactions = core::mem::take(&mut data.reactions);
        let unhandled = state == PromiseState::Rejected && reactions.is_empty() && !data.handled;
        (reactions, unhandled)
    };
    let rejected = state == PromiseState::Rejected;
    for (on_fulfilled, on_rejected, resolve, reject) in reactions {
        let handler = if rejected { on_rejected } else { on_fulfilled };
        ctx.realm.jobs.push_back(Job::Reaction(handler, value.clone(), rejected, resolve, reject));
    }
    if unhandled {
        ctx.realm.unhandled.push(promise.clone());
    }
}

/// Reports the rejections still unhandled; called when the job queue
/// has run dry.
pub fn report_unhandled(ctx: &mut Ctx) {
    let pending = core::mem::take(&mut ctx.realm.unhandled);
    for promise in pending {
        let reason = match &promise.borrow().kind {
            Kind::Promise(data) if !data.handled => Some(data.value.clone()),
            _ => None,
        };
        if let Some(reason) = reason {
            let text = ctx.describe_error(&reason);
            ctx.report(alloc::format!("Uncaught (in promise) {}", text));
        }
    }
}

/// `PerformPromiseThen`: registers the reactions, or queues them at once
/// if the promise has settled. Returns the derived promise.
pub fn perform_then(ctx: &mut Ctx, promise: &Obj, on_fulfilled: Value, on_rejected: Value) -> Obj {
    let proto = ctx.realm.protos.promise.clone();
    let derived = new_promise(ctx, proto);
    let (resolve, reject) = resolving_functions(ctx, &derived);
    let callable = |v: &Value| if v.as_obj().is_some_and(|o| o.is_callable()) { v.clone() } else { Value::Undefined };
    let on_fulfilled = callable(&on_fulfilled);
    let on_rejected = callable(&on_rejected);
    let settled = {
        let mut borrowed = promise.borrow_mut();
        match &mut borrowed.kind {
            Kind::Promise(data) => {
                data.handled = true;
                match data.state {
                    PromiseState::Pending => {
                        data.reactions.push((on_fulfilled.clone(), on_rejected.clone(), resolve.clone(), reject.clone()));
                        None
                    }
                    state => Some((state, data.value.clone())),
                }
            }
            _ => None,
        }
    };
    if let Some((state, value)) = settled {
        let rejected = state == PromiseState::Rejected;
        let handler = if rejected { on_rejected } else { on_fulfilled };
        ctx.realm.jobs.push_back(Job::Reaction(handler, value, rejected, resolve, reject));
    }
    derived
}

/// `PromiseResolve(%Promise%, value)`: a promise is returned as is,
/// anything else wrapped (a thenable adopted).
pub fn resolved(ctx: &mut Ctx, value: Value) -> R<Value> {
    if is_promise(&value) {
        return Ok(value);
    }
    let proto = ctx.realm.protos.promise.clone();
    let promise = new_promise(ctx, proto);
    resolve_promise(ctx, &promise, value)?;
    Ok(Value::Obj(promise))
}

pub fn rejected(ctx: &mut Ctx, reason: Value) -> R<Value> {
    let proto = ctx.realm.protos.promise.clone();
    let promise = new_promise(ctx, proto);
    reject_promise(ctx, &promise, reason);
    Ok(Value::Obj(promise))
}

// ------------------------------------------------------------
//  Constructor and prototype
// ------------------------------------------------------------

fn promise_constructor(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    if ctx.new_target.is_none() {
        return ctx.throw_type("Promise constructor cannot be invoked without 'new'");
    }
    let executor = arg(args, 0);
    ops::require_callable(ctx, &executor, "Promise resolver")?;
    let proto = ctx.proto_for_new(ctx.realm.protos.promise.clone())?;
    let promise = new_promise(ctx, proto);
    let (resolve, reject) = resolving_functions(ctx, &promise);
    match ctx.call(&executor, Value::Undefined, &[resolve, reject.clone()]) {
        Ok(_) => {}
        Err(Ctrl::Throw(error)) => {
            ctx.call(&reject, Value::Undefined, &[error])?;
        }
        Err(other) => return Err(other),
    }
    Ok(Value::Obj(promise))
}

fn this_promise(ctx: &mut Ctx, this: &Value, method: &str) -> R<Obj> {
    match this {
        Value::Obj(object) if matches!(object.borrow().kind, Kind::Promise(_)) => Ok(object.clone()),
        _ => {
            let message = alloc::format!("Method Promise.prototype.{} called on incompatible receiver", method);
            ctx.throw_type(&message)
        }
    }
}

fn then(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let promise = this_promise(ctx, this, "then")?;
    Ok(Value::Obj(perform_then(ctx, &promise, arg(args, 0), arg(args, 1))))
}

/// `catch` calls `this.then`, as the specification says: a subclass or
/// a thenable sees it.
fn catch(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let then = ops::get(ctx, this, "then")?;
    ops::require_callable(ctx, &then, "then")?;
    ctx.call(&then, this.clone(), &[Value::Undefined, arg(args, 0)])
}

fn return_data(_: &mut Ctx, _: &Value, _: &[Value], data: &Value) -> R<Value> {
    Ok(data.clone())
}

fn throw_data(_: &mut Ctx, _: &Value, _: &[Value], data: &Value) -> R<Value> {
    Err(Ctrl::Throw(data.clone()))
}

/// `finally`: the callback runs, its promise is waited for, then the
/// original value (or reason) passes through.
fn finally_step(ctx: &mut Ctx, args: &[Value], data: &Value, rejected: bool) -> R<Value> {
    let outcome = arg(args, 0);
    let result = ctx.call(data, Value::Undefined, &[])?;
    let waited = resolved(ctx, result)?;
    let Value::Obj(waited) = waited else {
        return Ok(outcome);
    };
    let pass = if rejected { ctx.native_with("", 0, throw_data, outcome) } else { ctx.native_with("", 0, return_data, outcome) };
    Ok(Value::Obj(perform_then(ctx, &waited, Value::Obj(pass), Value::Undefined)))
}

fn finally_fulfilled(ctx: &mut Ctx, _: &Value, args: &[Value], data: &Value) -> R<Value> {
    finally_step(ctx, args, data, false)
}

fn finally_rejected(ctx: &mut Ctx, _: &Value, args: &[Value], data: &Value) -> R<Value> {
    finally_step(ctx, args, data, true)
}

fn finally(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let callback = arg(args, 0);
    let (on_fulfilled, on_rejected) = if callback.as_obj().is_some_and(|c| c.is_callable()) {
        let fulfilled = ctx.native_with("", 1, finally_fulfilled, callback.clone());
        let rejected = ctx.native_with("", 1, finally_rejected, callback);
        (Value::Obj(fulfilled), Value::Obj(rejected))
    } else {
        (callback.clone(), callback)
    };
    let then = ops::get(ctx, this, "then")?;
    ops::require_callable(ctx, &then, "then")?;
    ctx.call(&then, this.clone(), &[on_fulfilled, on_rejected])
}

// ------------------------------------------------------------
//  Statics
// ------------------------------------------------------------

fn resolve_static(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    resolved(ctx, arg(args, 0))
}

fn reject_static(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    rejected(ctx, arg(args, 0))
}

fn with_resolvers(ctx: &mut Ctx, _: &Value, _: &[Value], _: &Value) -> R<Value> {
    let proto = ctx.realm.protos.promise.clone();
    let promise = new_promise(ctx, proto);
    let (resolve, reject) = resolving_functions(ctx, &promise);
    let object = ctx.new_object();
    object.put("promise", Value::Obj(promise));
    object.put("resolve", resolve);
    object.put("reject", reject);
    Ok(Value::Obj(object))
}

/// `Promise.try(f, ...args)`.
fn try_static(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let callback = arg(args, 0);
    let rest = if args.len() > 1 { &args[1..] } else { &[][..] };
    match ctx.call(&callback, Value::Undefined, rest) {
        Ok(value) => resolved(ctx, value),
        Err(Ctrl::Throw(error)) => rejected(ctx, error),
        Err(other) => Err(other),
    }
}

/// What a combinator does with each element.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Combinator {
    All,
    AllSettled,
    Any,
}

/// One element's callback of `all`, `allSettled` or `any`: data is a
/// record { shared, index, called, mode, rejected }.
fn element_callback(ctx: &mut Ctx, _: &Value, args: &[Value], data: &Value) -> R<Value> {
    let Value::Obj(element) = data else {
        return Ok(Value::Undefined);
    };
    if hidden(element, "called").truthy() {
        return Ok(Value::Undefined);
    }
    set_hidden(element, "called", Value::Bool(true));
    let Value::Obj(shared) = hidden(element, "shared") else {
        return Ok(Value::Undefined);
    };
    let index = match hidden(element, "index") {
        Value::Num(n) => n.to_i64_saturating() as usize,
        _ => 0,
    };
    let mode = match hidden(element, "mode") {
        Value::Num(n) => n.to_i64_saturating(),
        _ => 0,
    };
    let is_rejection = hidden(element, "rejected").truthy();
    let value = arg(args, 0);
    let stored = if mode == 1 {
        let entry = ctx.new_object();
        if is_rejection {
            entry.put("status", Value::str("rejected"));
            entry.put("reason", value);
        } else {
            entry.put("status", Value::str("fulfilled"));
            entry.put("value", value);
        }
        Value::Obj(entry)
    } else {
        value
    };
    if let Value::Obj(values) = hidden(&shared, "values") {
        if let Kind::Array(items) = &mut values.borrow_mut().kind {
            if index < items.len() {
                items[index] = stored;
            }
        }
    }
    finish_element(ctx, &shared, mode)
}

/// Counts an element down; at zero, settles the combined promise.
fn finish_element(ctx: &mut Ctx, shared: &Obj, mode: i64) -> R<Value> {
    let remaining = match hidden(shared, "remaining") {
        Value::Num(n) => n.to_i64_saturating() - 1,
        _ => 0,
    };
    set_hidden(shared, "remaining", Value::num(remaining));
    if remaining == 0 {
        let values = hidden(shared, "values");
        if mode == 2 {
            // `any`: every element rejected.
            let errors = ops::list_from_array_like(ctx, &values)?;
            let error = aggregate_error(ctx, errors);
            let reject = hidden(shared, "reject");
            ctx.call(&reject, Value::Undefined, &[error])?;
        } else {
            let resolve = hidden(shared, "resolve");
            ctx.call(&resolve, Value::Undefined, &[values])?;
        }
    }
    Ok(Value::Undefined)
}

fn aggregate_error(ctx: &mut Ctx, errors: Vec<Value>) -> Value {
    let proto = match ops::get(ctx, &Value::Obj(ctx.realm.global.clone()), "AggregateError") {
        Ok(Value::Obj(constructor)) => constructor.own_value("prototype").and_then(|p| p.as_obj().cloned()),
        _ => None,
    }
    .unwrap_or_else(|| ctx.realm.protos.error.clone());
    let error = ctx.error_object(proto, "AggregateError", "All promises were rejected");
    let list = ctx.new_array(errors);
    error.put_hidden("errors", Value::Obj(list));
    Value::Obj(error)
}

fn combine(ctx: &mut Ctx, args: &[Value], mode: Combinator) -> R<Value> {
    let proto = ctx.realm.protos.promise.clone();
    let combined = new_promise(ctx, proto);
    let (resolve, reject) = resolving_functions(ctx, &combined);
    let items = match ops::collect(ctx, &arg(args, 0)) {
        Ok(items) => items,
        Err(Ctrl::Throw(error)) => {
            ctx.call(&reject, Value::Undefined, &[error])?;
            return Ok(Value::Obj(combined));
        }
        Err(other) => return Err(other),
    };
    let shared = record(ctx);
    let values = ctx.new_array(alloc::vec![Value::Undefined; items.len()]);
    set_hidden(&shared, "values", Value::Obj(values.clone()));
    // One more than the elements: the last count-down happens after the
    // loop, so that an empty input settles too.
    set_hidden(&shared, "remaining", Value::num(items.len() as i64 + 1));
    set_hidden(&shared, "resolve", resolve.clone());
    set_hidden(&shared, "reject", reject.clone());
    let mode_number = match mode {
        Combinator::All => 0,
        Combinator::AllSettled => 1,
        Combinator::Any => 2,
    };
    for (index, item) in items.into_iter().enumerate() {
        ctx.tick()?;
        let promise = resolved(ctx, item)?;
        let Value::Obj(promise) = promise else {
            continue;
        };
        let element = |ctx: &mut Ctx, rejected: bool| {
            let data = record(ctx);
            set_hidden(&data, "shared", Value::Obj(shared.clone()));
            set_hidden(&data, "index", Value::num(index as i64));
            set_hidden(&data, "called", Value::Bool(false));
            set_hidden(&data, "mode", Value::num(mode_number));
            set_hidden(&data, "rejected", Value::Bool(rejected));
            Value::Obj(ctx.native_with("", 1, element_callback, Value::Obj(data)))
        };
        let (on_fulfilled, on_rejected) = match mode {
            Combinator::All => (element(ctx, false), reject.clone()),
            Combinator::AllSettled => (element(ctx, false), element(ctx, true)),
            Combinator::Any => (resolve.clone(), element(ctx, true)),
        };
        perform_then(ctx, &promise, on_fulfilled, on_rejected);
    }
    finish_element(ctx, &shared, mode_number)?;
    Ok(Value::Obj(combined))
}

fn all(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    combine(ctx, args, Combinator::All)
}

fn all_settled(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    combine(ctx, args, Combinator::AllSettled)
}

fn any(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    combine(ctx, args, Combinator::Any)
}

fn race(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let proto = ctx.realm.protos.promise.clone();
    let combined = new_promise(ctx, proto);
    let (resolve, reject) = resolving_functions(ctx, &combined);
    let items = match ops::collect(ctx, &arg(args, 0)) {
        Ok(items) => items,
        Err(Ctrl::Throw(error)) => {
            ctx.call(&reject, Value::Undefined, &[error])?;
            return Ok(Value::Obj(combined));
        }
        Err(other) => return Err(other),
    };
    for item in items {
        ctx.tick()?;
        if let Value::Obj(promise) = resolved(ctx, item)? {
            perform_then(ctx, &promise, resolve.clone(), reject.clone());
        }
    }
    Ok(Value::Obj(combined))
}

pub fn install(ctx: &mut Ctx) {
    let proto = ctx.realm.protos.promise.clone();
    let constructor = super::constructor(ctx, "Promise", 1, promise_constructor, &proto);
    let statics: &[(&str, usize, NativeFn)] = &[
        ("resolve", 1, resolve_static),
        ("reject", 1, reject_static),
        ("all", 1, all),
        ("allSettled", 1, all_settled),
        ("any", 1, any),
        ("race", 1, race),
        ("withResolvers", 0, with_resolvers),
        ("try", 1, try_static),
    ];
    for (name, length, function) in statics {
        ctx.method(&constructor, name, *length, *function);
    }
    ctx.method(&proto, "then", 2, then);
    ctx.method(&proto, "catch", 1, catch);
    ctx.method(&proto, "finally", 1, finally);
    let tag = ctx.realm.symbols.to_string_tag.clone();
    proto.put_key(tag, Value::str("Promise"), CONFIGURABLE);
}
