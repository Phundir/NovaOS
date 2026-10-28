//! DOM bindings (16.2): what a page's scripts see of its document.
//!
//! ## Nodes and their objects
//!
//! The document is `html::Document`, an arena; a node is a `NodeId`. The
//! object a script holds for a node is a `Kind::Node(id)` made once and
//! kept in `DomState::wrappers`, so that `document.body === document.body`
//! and the properties a script hangs on an element stay there. Every
//! method reads and changes `ctx.doc` directly (`tree.rs` for the tree
//! operations) and sets `ctx.dirty`: the host lays the page out again
//! after the script has run, not at every change.
//!
//! ## What the host gives back
//!
//! The layout belongs to the browser: after each layout it hands the
//! boxes of the elements (`DomState::boxes`, for `getBoundingClientRect`
//! and hit testing) and keeps the state of the form controls in
//! `DomState::values` / `checked`, which `input.value` reads and writes.
//!
//! ## Files
//!
//! `node.rs` Node and Document, `element.rs` Element; `events.rs`
//! EventTarget and the
//! dispatch; `window.rs` window, location, history, navigator, storage;
//! `fetch.rs` fetch, Headers, Response, XMLHttpRequest.

pub mod element;
pub mod events;
pub mod fetch;
pub mod node;
pub mod window;

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use super::interp::{Ctx, R};
use super::value::*;
use crate::web::html::{NodeData, NodeId};

/// The window, as an event target next to the nodes.
pub const WINDOW: NodeId = usize::MAX;

#[derive(Default)]
pub struct DomState {
    /// The object of each node a script has seen.
    pub wrappers: BTreeMap<NodeId, Obj>,
    /// Form controls as the page sees them: typed text, ticked boxes.
    pub values: BTreeMap<NodeId, String>,
    pub checked: BTreeMap<NodeId, bool>,
    /// The element with the focus.
    pub focused: Option<NodeId>,
    /// Boxes of the last layout: (node, x, y, width, height), page
    /// coordinates.
    pub boxes: Vec<(NodeId, i32, i32, i32, i32)>,
    /// Vertical scroll of the page, for `getBoundingClientRect`.
    pub scroll_y: i32,
    /// `performance.now()` counts from here.
    pub started_ms: i64,
    /// "loading", "interactive", "complete".
    pub ready_state: &'static str,
    /// A request to scroll (`window.scrollTo`), for the host.
    pub scroll_request: Option<i32>,
    /// A form the page submitted (`form.submit()`), for the host.
    pub submit_request: Option<NodeId>,
    /// `history.back()` and friends: how far to go, for the host.
    pub history_request: Option<i32>,
    /// `history.pushState` changed the address shown.
    pub address_changed: bool,
    /// Prototypes of `fetch.rs`, made at installation.
    pub headers_proto: Option<Obj>,
    pub response_proto: Option<Obj>,
    pub xhr_proto: Option<Obj>,
}

impl DomState {
    /// Forgets every node object (page teardown).
    pub fn clear(&mut self) {
        self.headers_proto = None;
        self.response_proto = None;
        self.xhr_proto = None;
        self.wrappers.clear();
        self.values.clear();
        self.checked.clear();
        self.boxes.clear();
    }
}

/// The object of `node`, made the first time.
pub fn wrap(ctx: &mut Ctx, node: NodeId) -> Value {
    if node >= ctx.doc.nodes.len() {
        return Value::Null;
    }
    if let Some(object) = ctx.realm.dom.wrappers.get(&node) {
        return Value::Obj(object.clone());
    }
    let (proto, class) = match &ctx.doc.nodes[node].data {
        NodeData::Document => (ctx.realm.protos.document.clone(), "HTMLDocument"),
        NodeData::Text(_) => (ctx.realm.protos.text.clone(), "Text"),
        NodeData::Element { .. } => (ctx.realm.protos.element.clone(), "HTMLElement"),
    };
    let object = ctx.alloc(Kind::Node(node), Some(proto), class);
    ctx.realm.dom.wrappers.insert(node, object.clone());
    Value::Obj(object)
}

/// The node behind an object, if it is one.
pub fn node_of(value: &Value) -> Option<NodeId> {
    match value {
        Value::Obj(object) => object.node(),
        _ => None,
    }
}

/// `this` as a node, or a TypeError naming the method.
pub fn this_node(ctx: &mut Ctx, this: &Value, method: &str) -> R<NodeId> {
    match node_of(this) {
        Some(node) if node < ctx.doc.nodes.len() => Ok(node),
        _ => {
            let message = alloc::format!("{}: 'this' is not a node", method);
            ctx.throw_type(&message)
        }
    }
}

/// A `DOMException`: an Error with its name.
pub fn throw_dom<T>(ctx: &mut Ctx, name: &str, message: &str) -> R<T> {
    let error = ctx.error_object(ctx.realm.protos.error.clone(), name, message);
    error.put_hidden("name", Value::str(name));
    Err(super::interp::Ctrl::Throw(Value::Obj(error)))
}

/// A list of nodes as a `NodeList`: an array whose prototype adds `item`.
pub fn node_list(ctx: &mut Ctx, nodes: &[NodeId]) -> Value {
    let values: Vec<Value> = nodes.iter().map(|n| wrap(ctx, *n)).collect();
    let proto = ctx.realm.protos.node_list.clone();
    Value::Obj(ctx.alloc(Kind::Array(values), Some(proto), "NodeList"))
}

/// Runs a page's scripts in document order, then fires
/// `DOMContentLoaded` (at the document) and `load` (at the window), the
/// job queue drained after each. An error stops its script only.
pub fn start_page(ctx: &mut Ctx, scripts: &[String]) {
    ctx.realm.dom.ready_state = "loading";
    for source in scripts {
        if let Err(error) = ctx.run_script(source) {
            ctx.report(error);
        }
        if let Err(reason) = ctx.run_jobs() {
            ctx.report(reason);
        }
    }
    ctx.realm.dom.ready_state = "interactive";
    if let Err(super::interp::Ctrl::Halt(reason)) = events::fire(ctx, 0, "DOMContentLoaded", true, false) {
        ctx.report(reason);
        return;
    }
    let _ = ctx.run_jobs();
    ctx.realm.dom.ready_state = "complete";
    if let Err(super::interp::Ctrl::Halt(reason)) = events::fire(ctx, WINDOW, "load", false, false) {
        ctx.report(reason);
        return;
    }
    let _ = ctx.run_jobs();
}

/// Installs the DOM on a realm whose library is installed: prototypes,
/// constructors, `window` and `document`.
pub fn install(ctx: &mut Ctx) {
    ctx.realm.dom.started_ms = ctx.now();
    ctx.realm.dom.ready_state = "loading";
    // The window is an event target, and a `NodeList` an array.
    let event_target = ctx.realm.protos.event_target.clone();
    ctx.realm.global.borrow_mut().proto = Some(event_target);
    let array = ctx.realm.protos.array.clone();
    ctx.realm.protos.node_list.borrow_mut().proto = Some(array);
    events::install(ctx);
    node::install(ctx);
    window::install(ctx);
    fetch::install(ctx);
}
