//! `Node`, `Text`, `Document`: the tree as scripts read and change it,
//! the queries, and the constructors (`element.rs` has the rest of
//! `Element`).
//!
//! Collections (`childNodes`, `children`, `querySelectorAll`...) are
//! snapshots -- arrays made when asked for -- where the standard has
//! live lists for some of them. A loop that removes the children it
//! walks over works either way; one that expects a list to grow while it
//! walks does not.

use alloc::string::String;
use alloc::vec::Vec;

use super::super::interp::{Ctx, R};
use super::super::ops;
use super::super::value::*;
use super::{node_list, node_of, this_node, throw_dom, wrap};
use crate::web::css;
use crate::web::html::{NodeData, NodeId};

pub(super) fn arg(args: &[Value], index: usize) -> Value {
    args.get(index).cloned().unwrap_or(Value::Undefined)
}

pub(super) fn string_arg(ctx: &mut Ctx, args: &[Value], index: usize) -> R<String> {
    Ok(String::from(ops::to_string(ctx, &arg(args, index))?.as_str()))
}

pub(super) fn is_element(ctx: &Ctx, node: NodeId) -> bool {
    matches!(ctx.doc.nodes[node].data, NodeData::Element { .. })
}

fn is_fragment(ctx: &Ctx, node: NodeId) -> bool {
    ctx.doc.tag(node) == Some("#document-fragment")
}

fn element_children(ctx: &Ctx, node: NodeId) -> Vec<NodeId> {
    ctx.doc.nodes[node].children.iter().copied().filter(|c| is_element(ctx, *c)).collect()
}

fn siblings(ctx: &Ctx, node: NodeId) -> (Option<NodeId>, Option<NodeId>) {
    let Some(parent) = ctx.doc.nodes[node].parent else {
        return (None, None);
    };
    let children = &ctx.doc.nodes[parent].children;
    let position = children.iter().position(|c| *c == node);
    match position {
        Some(i) => (i.checked_sub(1).map(|p| children[p]), children.get(i + 1).copied()),
        None => (None, None),
    }
}

// ------------------------------------------------------------
//  Tree changes
// ------------------------------------------------------------

/// Inserts `child` (a fragment's children, for a fragment) under
/// `parent` before `reference`.
fn insert(ctx: &mut Ctx, parent: NodeId, child: NodeId, reference: Option<NodeId>) -> R<()> {
    let moving: Vec<NodeId> = if is_fragment(ctx, child) { ctx.doc.nodes[child].children.clone() } else { alloc::vec![child] };
    for node in moving {
        if !ctx.doc.insert_before(parent, node, reference) {
            return throw_dom(ctx, "HierarchyRequestError", "The new child element contains the parent.");
        }
    }
    ctx.dirty = true;
    Ok(())
}

/// A node argument, or a text node made from a string (`append("x")`).
fn node_or_text(ctx: &mut Ctx, value: &Value) -> R<NodeId> {
    if let Some(node) = node_of(value) {
        return Ok(node);
    }
    let text = ops::to_string(ctx, value)?;
    match ctx.doc.create_text(&text) {
        Some(node) => Ok(node),
        None => ctx.throw_range("trop de noeuds dans la page"),
    }
}

fn required_node(ctx: &mut Ctx, value: &Value, method: &str) -> R<NodeId> {
    match node_of(value) {
        Some(node) => Ok(node),
        None => {
            let message = alloc::format!("Failed to execute '{}': parameter 1 is not of type 'Node'.", method);
            ctx.throw_type(&message)
        }
    }
}

fn append_child(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let parent = this_node(ctx, this, "appendChild")?;
    let child = required_node(ctx, &arg(args, 0), "appendChild")?;
    insert(ctx, parent, child, None)?;
    Ok(arg(args, 0))
}

fn insert_before(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let parent = this_node(ctx, this, "insertBefore")?;
    let child = required_node(ctx, &arg(args, 0), "insertBefore")?;
    let reference = node_of(&arg(args, 1));
    if let Some(reference) = reference {
        if ctx.doc.nodes[reference].parent != Some(parent) {
            return throw_dom(ctx, "NotFoundError", "The node before which the new node is to be inserted is not a child of this node.");
        }
    }
    insert(ctx, parent, child, reference)?;
    Ok(arg(args, 0))
}

fn remove_child(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let parent = this_node(ctx, this, "removeChild")?;
    let child = required_node(ctx, &arg(args, 0), "removeChild")?;
    if ctx.doc.nodes[child].parent != Some(parent) {
        return throw_dom(ctx, "NotFoundError", "The node to be removed is not a child of this node.");
    }
    ctx.doc.detach(child);
    ctx.dirty = true;
    Ok(arg(args, 0))
}

fn replace_child(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let parent = this_node(ctx, this, "replaceChild")?;
    let new_child = required_node(ctx, &arg(args, 0), "replaceChild")?;
    let old_child = required_node(ctx, &arg(args, 1), "replaceChild")?;
    if ctx.doc.nodes[old_child].parent != Some(parent) {
        return throw_dom(ctx, "NotFoundError", "The node to be replaced is not a child of this node.");
    }
    if new_child != old_child {
        insert(ctx, parent, new_child, Some(old_child))?;
        ctx.doc.detach(old_child);
    }
    Ok(arg(args, 1))
}

pub(super) fn remove(ctx: &mut Ctx, this: &Value, _: &[Value], _: &Value) -> R<Value> {
    let node = this_node(ctx, this, "remove")?;
    ctx.doc.detach(node);
    ctx.dirty = true;
    Ok(Value::Undefined)
}

/// `append`, `prepend`, `before`, `after`, `replaceWith`,
/// `replaceChildren`: data says which.
pub(super) fn insert_many(ctx: &mut Ctx, this: &Value, args: &[Value], data: &Value) -> R<Value> {
    let node = this_node(ctx, this, "append")?;
    let mode = match data {
        Value::Num(n) => n.to_i64_saturating(),
        _ => 0,
    };
    let mut nodes = Vec::new();
    for value in args {
        nodes.push(node_or_text(ctx, value)?);
    }
    let (parent, reference) = match mode {
        // append
        0 => (Some(node), None),
        // prepend
        1 => (Some(node), ctx.doc.nodes[node].children.first().copied()),
        // before
        2 => (ctx.doc.nodes[node].parent, Some(node)),
        // after, replaceWith
        3 | 4 => (ctx.doc.nodes[node].parent, siblings(ctx, node).1),
        // replaceChildren
        _ => {
            ctx.doc.clear_children(node);
            (Some(node), None)
        }
    };
    let Some(parent) = parent else {
        return Ok(Value::Undefined);
    };
    for child in nodes {
        if child == node && mode == 4 {
            continue;
        }
        insert(ctx, parent, child, reference.filter(|r| *r != child))?;
    }
    if mode == 4 && ctx.doc.nodes[node].parent == Some(parent) && !args.iter().any(|a| node_of(a) == Some(node)) {
        ctx.doc.detach(node);
    }
    ctx.dirty = true;
    Ok(Value::Undefined)
}

fn clone_node(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let node = this_node(ctx, this, "cloneNode")?;
    match ctx.doc.clone_node(node, arg(args, 0).truthy()) {
        Some(copy) => Ok(wrap(ctx, copy)),
        None => ctx.throw_range("trop de noeuds dans la page"),
    }
}

fn contains(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let node = this_node(ctx, this, "contains")?;
    Ok(Value::Bool(match node_of(&arg(args, 0)) {
        Some(other) => other == node || ctx.doc.is_inside(other, node),
        None => false,
    }))
}

fn has_child_nodes(ctx: &mut Ctx, this: &Value, _: &[Value], _: &Value) -> R<Value> {
    let node = this_node(ctx, this, "hasChildNodes")?;
    Ok(Value::Bool(!ctx.doc.nodes[node].children.is_empty()))
}

pub(super) fn noop(_: &mut Ctx, _: &Value, _: &[Value], _: &Value) -> R<Value> {
    Ok(Value::Undefined)
}

pub(super) fn insert_adjacent(ctx: &mut Ctx, this: &Value, args: &[Value], data: &Value) -> R<Value> {
    let node = this_node(ctx, this, "insertAdjacentHTML")?;
    let position = string_arg(ctx, args, 0)?.to_ascii_lowercase();
    let kind = match data {
        Value::Num(n) => n.to_i64_saturating(),
        _ => 0,
    };
    let nodes: Vec<NodeId> = match kind {
        // HTML
        0 => {
            let source = string_arg(ctx, args, 1)?;
            match ctx.doc.import_fragment(&source) {
                Some(nodes) => nodes,
                None => return ctx.throw_range("trop de noeuds dans la page"),
            }
        }
        // text
        1 => {
            let text = ops::to_string(ctx, &arg(args, 1))?;
            alloc::vec![node_or_text(ctx, &Value::Str(text))?]
        }
        // element
        _ => alloc::vec![required_node(ctx, &arg(args, 1), "insertAdjacentElement")?],
    };
    let (parent, reference) = match position.as_str() {
        "beforebegin" => (ctx.doc.nodes[node].parent, Some(node)),
        "afterbegin" => (Some(node), ctx.doc.nodes[node].children.first().copied()),
        "beforeend" => (Some(node), None),
        "afterend" => (ctx.doc.nodes[node].parent, siblings(ctx, node).1),
        _ => return throw_dom(ctx, "SyntaxError", "The value provided is not one of 'beforeBegin', 'afterBegin', 'beforeEnd', or 'afterEnd'."),
    };
    let Some(parent) = parent else {
        return Ok(Value::Null);
    };
    for child in &nodes {
        insert(ctx, parent, *child, reference)?;
    }
    Ok(if kind == 2 { arg(args, 1) } else { Value::Undefined })
}

// ------------------------------------------------------------
//  Node properties
// ------------------------------------------------------------

/// A getter on a node; data names the property.
pub(super) fn node_get(ctx: &mut Ctx, this: &Value, _: &[Value], data: &Value) -> R<Value> {
    let node = this_node(ctx, this, "property")?;
    let name = match data {
        Value::Str(s) => s.clone(),
        _ => return Ok(Value::Undefined),
    };
    let doc = &*ctx.doc;
    let value_node = |n: Option<NodeId>| n;
    let result: Option<NodeId> = match name.as_str() {
        "parentNode" => value_node(doc.nodes[node].parent),
        "parentElement" => doc.nodes[node].parent.filter(|p| matches!(doc.nodes[*p].data, NodeData::Element { .. })),
        "firstChild" => doc.nodes[node].children.first().copied(),
        "lastChild" => doc.nodes[node].children.last().copied(),
        "nextSibling" => siblings(ctx, node).1,
        "previousSibling" => siblings(ctx, node).0,
        "firstElementChild" => element_children(ctx, node).first().copied(),
        "lastElementChild" => element_children(ctx, node).last().copied(),
        "nextElementSibling" | "previousElementSibling" => {
            let parent = doc.nodes[node].parent;
            parent.and_then(|p| {
                let elements = element_children(ctx, p);
                let i = elements.iter().position(|e| *e == node)?;
                if name.as_str() == "nextElementSibling" { elements.get(i + 1).copied() } else { i.checked_sub(1).map(|j| elements[j]) }
            })
        }
        "ownerDocument" => Some(0),
        _ => {
            return Ok(match name.as_str() {
                "nodeType" => Value::num(match doc.nodes[node].data {
                    NodeData::Element { .. } => {
                        if is_fragment(ctx, node) {
                            11
                        } else {
                            1
                        }
                    }
                    NodeData::Text(_) => 3,
                    NodeData::Document => 9,
                }),
                "nodeName" => Value::Str(JsStr::from(match &doc.nodes[node].data {
                    NodeData::Element { tag, .. } => tag.to_ascii_uppercase(),
                    NodeData::Text(_) => String::from("#text"),
                    NodeData::Document => String::from("#document"),
                })),
                "nodeValue" | "data" => match &doc.nodes[node].data {
                    NodeData::Text(text) => Value::str(text),
                    _ => Value::Null,
                },
                "isConnected" => Value::Bool(doc.is_connected(node)),
                "childNodes" => {
                    let children = doc.nodes[node].children.clone();
                    return Ok(node_list(ctx, &children));
                }
                "children" => {
                    let children = element_children(ctx, node);
                    return Ok(node_list(ctx, &children));
                }
                "childElementCount" => Value::num(element_children(ctx, node).len() as i64),
                "textContent" => match &doc.nodes[node].data {
                    NodeData::Document => Value::Null,
                    _ => Value::Str(JsStr::from(doc.text_content_raw(node))),
                },
                "length" => match &doc.nodes[node].data {
                    NodeData::Text(text) => Value::num(JsStr::from(text.as_str()).units() as i64),
                    _ => Value::Undefined,
                },
                _ => Value::Undefined,
            });
        }
    };
    Ok(match result {
        Some(n) => wrap(ctx, n),
        None => Value::Null,
    })
}

pub(super) fn text_content_set(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let node = this_node(ctx, this, "textContent")?;
    let value = arg(args, 0);
    let text = if value.is_nullish() { String::new() } else { string_arg(ctx, args, 0)? };
    if !ctx.doc.set_text_content(node, &text) {
        return ctx.throw_range("trop de noeuds dans la page");
    }
    ctx.dirty = true;
    Ok(Value::Undefined)
}

fn data_set(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let node = this_node(ctx, this, "data")?;
    let text = string_arg(ctx, args, 0)?;
    if let NodeData::Text(content) = &mut ctx.doc.nodes[node].data {
        *content = text;
        ctx.dirty = true;
    }
    Ok(Value::Undefined)
}

// ------------------------------------------------------------
//  Queries
// ------------------------------------------------------------

fn selectors(ctx: &mut Ctx, text: &str) -> R<Vec<css::Selector>> {
    let mut out = Vec::new();
    for part in css::split_top_level(text, ',') {
        match css::parse_selector(part.trim()) {
            Some(selector) => out.push(selector),
            None => {
                let message = alloc::format!("'{}' is not a valid selector.", text);
                return throw_dom(ctx, "SyntaxError", &message);
            }
        }
    }
    Ok(out)
}

fn matching(ctx: &mut Ctx, root: NodeId, text: &str, first_only: bool) -> R<Vec<NodeId>> {
    let list = selectors(ctx, text)?;
    let mut out = Vec::new();
    for node in ctx.doc.preorder(root).into_iter().skip(1) {
        if is_element(ctx, node) && list.iter().any(|s| css::matches(ctx.doc, node, s)) {
            out.push(node);
            if first_only {
                break;
            }
        }
    }
    Ok(out)
}

pub(super) fn query_selector(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let root = this_node(ctx, this, "querySelector")?;
    let text = string_arg(ctx, args, 0)?;
    let found = matching(ctx, root, &text, true)?;
    Ok(match found.first() {
        Some(node) => wrap(ctx, *node),
        None => Value::Null,
    })
}

pub(super) fn query_selector_all(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let root = this_node(ctx, this, "querySelectorAll")?;
    let text = string_arg(ctx, args, 0)?;
    let found = matching(ctx, root, &text, false)?;
    Ok(node_list(ctx, &found))
}

pub(super) fn matches(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let node = this_node(ctx, this, "matches")?;
    let text = string_arg(ctx, args, 0)?;
    let list = selectors(ctx, &text)?;
    Ok(Value::Bool(list.iter().any(|s| css::matches(ctx.doc, node, s))))
}

pub(super) fn closest(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let node = this_node(ctx, this, "closest")?;
    let text = string_arg(ctx, args, 0)?;
    let list = selectors(ctx, &text)?;
    let mut current = Some(node);
    while let Some(candidate) = current {
        if is_element(ctx, candidate) && list.iter().any(|s| css::matches(ctx.doc, candidate, s)) {
            return Ok(wrap(ctx, candidate));
        }
        current = ctx.doc.nodes[candidate].parent;
    }
    Ok(Value::Null)
}

pub(super) fn by_tag(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let root = this_node(ctx, this, "getElementsByTagName")?;
    let tag = string_arg(ctx, args, 0)?.to_ascii_lowercase();
    let found: Vec<NodeId> = ctx
        .doc
        .preorder(root)
        .into_iter()
        .skip(1)
        .filter(|n| is_element(ctx, *n) && (tag == "*" || ctx.doc.tag(*n) == Some(tag.as_str())))
        .collect();
    Ok(node_list(ctx, &found))
}

pub(super) fn by_class(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let root = this_node(ctx, this, "getElementsByClassName")?;
    let wanted: Vec<String> = string_arg(ctx, args, 0)?.split_whitespace().map(String::from).collect();
    let found: Vec<NodeId> = ctx
        .doc
        .preorder(root)
        .into_iter()
        .skip(1)
        .filter(|n| {
            let classes = super::element::classes_of(ctx, *n);
            !wanted.is_empty() && wanted.iter().all(|w| classes.contains(w))
        })
        .collect();
    Ok(node_list(ctx, &found))
}

fn by_id(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let id = string_arg(ctx, args, 0)?;
    let found = ctx.doc.preorder(0).into_iter().find(|n| ctx.doc.attribute(*n, "id") == Some(id.as_str()));
    Ok(match found {
        Some(node) => wrap(ctx, node),
        None => Value::Null,
    })
}

fn by_name(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let name = string_arg(ctx, args, 0)?;
    let found: Vec<NodeId> = ctx.doc.preorder(0).into_iter().filter(|n| ctx.doc.attribute(*n, "name") == Some(name.as_str())).collect();
    Ok(node_list(ctx, &found))
}

// ------------------------------------------------------------
//  Document
// ------------------------------------------------------------

fn create_element(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    // createElementNS(namespace, name) passes the name second.
    let name = if args.len() >= 2 && matches!(args[0], Value::Str(_) | Value::Null) && matches!(args[1], Value::Str(_)) && ctx.new_target.is_none() {
        string_arg(ctx, args, 1)?
    } else {
        string_arg(ctx, args, 0)?
    };
    let name = name.rsplit(':').next().unwrap_or("").to_ascii_lowercase();
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return throw_dom(ctx, "InvalidCharacterError", "The tag name provided is not a valid name.");
    }
    match ctx.doc.create_element(&name) {
        Some(node) => Ok(wrap(ctx, node)),
        None => ctx.throw_range("trop de noeuds dans la page"),
    }
}

fn create_element_ns(ctx: &mut Ctx, this: &Value, args: &[Value], data: &Value) -> R<Value> {
    let rest = if args.len() > 1 { &args[1..] } else { args };
    create_element(ctx, this, rest, data)
}

fn create_text_node(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let text = string_arg(ctx, args, 0)?;
    match ctx.doc.create_text(&text) {
        Some(node) => Ok(wrap(ctx, node)),
        None => ctx.throw_range("trop de noeuds dans la page"),
    }
}

/// `createComment`: comments are not kept; an empty text node stands in.
fn create_comment(ctx: &mut Ctx, _: &Value, _: &[Value], _: &Value) -> R<Value> {
    match ctx.doc.create_text("") {
        Some(node) => Ok(wrap(ctx, node)),
        None => ctx.throw_range("trop de noeuds dans la page"),
    }
}

fn create_fragment(ctx: &mut Ctx, _: &Value, _: &[Value], _: &Value) -> R<Value> {
    match ctx.doc.create_element("#document-fragment") {
        Some(node) => Ok(wrap(ctx, node)),
        None => ctx.throw_range("trop de noeuds dans la page"),
    }
}

fn first_tag(ctx: &Ctx, tag: &str) -> Option<NodeId> {
    ctx.doc.preorder(0).into_iter().find(|n| ctx.doc.tag(*n) == Some(tag))
}

fn document_get(ctx: &mut Ctx, _: &Value, _: &[Value], data: &Value) -> R<Value> {
    let Value::Str(name) = data else {
        return Ok(Value::Undefined);
    };
    let node = match name.as_str() {
        "documentElement" => first_tag(ctx, "html"),
        "head" => first_tag(ctx, "head"),
        "body" => first_tag(ctx, "body"),
        "activeElement" => ctx.realm.dom.focused.or_else(|| first_tag(ctx, "body")),
        "scrollingElement" => first_tag(ctx, "html"),
        _ => {
            let tag = match name.as_str() {
                "forms" => "form",
                "images" => "img",
                "links" => "a",
                "scripts" => "script",
                _ => "",
            };
            let found: Vec<NodeId> = ctx.doc.preorder(0).into_iter().filter(|n| ctx.doc.tag(*n) == Some(tag)).collect();
            return Ok(match name.as_str() {
                "forms" | "images" | "links" | "scripts" => node_list(ctx, &found),
                "title" => Value::Str(JsStr::from(ctx.doc.title())),
                "readyState" => Value::str(ctx.realm.dom.ready_state),
                "URL" | "documentURI" => Value::Str(JsStr::from(ctx.base_url.as_str())),
                "domain" => Value::Str(JsStr::from(crate::web::url::Url::parse(&ctx.base_url).map(|u| u.host).unwrap_or_default())),
                "characterSet" | "charset" => Value::str("UTF-8"),
                "compatMode" => Value::str("CSS1Compat"),
                "contentType" => Value::str("text/html"),
                "visibilityState" => Value::str("visible"),
                "hidden" => Value::Bool(false),
                "referrer" => Value::str(""),
                "dir" => Value::str(""),
                _ => Value::Undefined,
            });
        }
    };
    Ok(match node {
        Some(n) => wrap(ctx, n),
        None => Value::Null,
    })
}

fn title_set(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let text = string_arg(ctx, args, 0)?;
    let title = match first_tag(ctx, "title") {
        Some(title) => title,
        None => {
            let Some(title) = ctx.doc.create_element("title") else {
                return Ok(Value::Undefined);
            };
            let parent = first_tag(ctx, "head").or_else(|| first_tag(ctx, "html")).unwrap_or(0);
            ctx.doc.append_child(parent, title);
            title
        }
    };
    ctx.doc.set_text_content(title, &text);
    ctx.dirty = true;
    Ok(Value::Undefined)
}

pub(super) fn has_focus(_: &mut Ctx, _: &Value, _: &[Value], _: &Value) -> R<Value> {
    Ok(Value::Bool(true))
}

/// `document.write`: appended to the body, as after the page has loaded.
fn document_write(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let mut source = String::new();
    for index in 0..args.len() {
        source.push_str(&string_arg(ctx, args, index)?);
    }
    let parent = first_tag(ctx, "body").unwrap_or(0);
    if let Some(nodes) = ctx.doc.import_fragment(&source) {
        for node in nodes {
            ctx.doc.append_child(parent, node);
        }
        ctx.dirty = true;
    }
    Ok(Value::Undefined)
}

// ------------------------------------------------------------
//  Installation
// ------------------------------------------------------------

pub(super) fn getter(ctx: &mut Ctx, target: &Obj, name: &str, function: NativeFn, data: Value) {
    let get = ctx.native_with(name, 0, function, data);
    target.borrow_mut().props.set(JsStr::from(name), Slot::Accessor(Some(get), None, CONFIGURABLE));
}

pub(super) fn accessor(ctx: &mut Ctx, target: &Obj, name: &str, get: NativeFn, set: NativeFn, data: Value) {
    let get = ctx.native_with(name, 0, get, data.clone());
    let set = ctx.native_with(name, 1, set, data);
    target.borrow_mut().props.set(JsStr::from(name), Slot::Accessor(Some(get), Some(set), CONFIGURABLE));
}

pub(super) fn method(ctx: &mut Ctx, target: &Obj, name: &str, length: usize, function: NativeFn, data: Value) {
    let native = ctx.native_with(name, length, function, data);
    target.put_hidden(name, Value::Obj(native));
}

pub fn install(ctx: &mut Ctx) {
    let node = ctx.realm.protos.node.clone();
    for name in [
        "parentNode", "parentElement", "firstChild", "lastChild", "nextSibling", "previousSibling", "ownerDocument",
        "nodeType", "nodeName", "isConnected", "childNodes",
    ] {
        getter(ctx, &node, name, node_get, Value::str(name));
    }
    accessor(ctx, &node, "textContent", node_get, text_content_set, Value::str("textContent"));
    let node_methods: &[(&str, usize, NativeFn)] = &[
        ("appendChild", 1, append_child),
        ("insertBefore", 2, insert_before),
        ("removeChild", 1, remove_child),
        ("replaceChild", 2, replace_child),
        ("cloneNode", 1, clone_node),
        ("contains", 1, contains),
        ("hasChildNodes", 0, has_child_nodes),
        ("normalize", 0, noop),
    ];
    for (name, length, function) in node_methods {
        method(ctx, &node, name, *length, *function, Value::Undefined);
    }
    for (name, code) in [("ELEMENT_NODE", 1), ("TEXT_NODE", 3), ("DOCUMENT_NODE", 9), ("DOCUMENT_FRAGMENT_NODE", 11)] {
        node.put_key(JsStr::from(name), Value::num(code), 0);
    }

    let text = ctx.realm.protos.text.clone();
    accessor(ctx, &text, "data", node_get, data_set, Value::str("data"));
    accessor(ctx, &text, "nodeValue", node_get, data_set, Value::str("nodeValue"));
    getter(ctx, &text, "length", node_get, Value::str("length"));
    method(ctx, &text, "remove", 0, remove, Value::Undefined);

    super::element::install(ctx);

    let list = ctx.realm.protos.node_list.clone();
    method(ctx, &list, "item", 1, node_list_item, Value::Undefined);

    let document = ctx.realm.protos.document.clone();
    for name in [
        "documentElement", "head", "body", "activeElement", "scrollingElement", "forms", "images", "links",
        "scripts", "readyState", "URL", "documentURI", "domain", "characterSet", "charset", "compatMode",
        "contentType", "visibilityState", "hidden", "referrer", "dir",
    ] {
        getter(ctx, &document, name, document_get, Value::str(name));
    }
    accessor(ctx, &document, "title", document_get, title_set, Value::str("title"));
    let document_methods: &[(&str, usize, NativeFn)] = &[
        ("getElementById", 1, by_id),
        ("getElementsByName", 1, by_name),
        ("getElementsByTagName", 1, by_tag),
        ("getElementsByClassName", 1, by_class),
        ("querySelector", 1, query_selector),
        ("querySelectorAll", 1, query_selector_all),
        ("createElement", 1, create_element),
        ("createElementNS", 2, create_element_ns),
        ("createTextNode", 1, create_text_node),
        ("createComment", 1, create_comment),
        ("createDocumentFragment", 0, create_fragment),
        ("hasFocus", 0, has_focus),
        ("write", 1, document_write),
        ("writeln", 1, document_write),
        ("append", 0, insert_many),
    ];
    for (name, length, function) in document_methods {
        method(ctx, &document, name, *length, *function, Value::Undefined);
    }

    // Constructors for `instanceof` and feature tests; none constructs.
    let global = ctx.realm.global.clone();
    let pairs = [
        ("EventTarget", ctx.realm.protos.event_target.clone()),
        ("Node", ctx.realm.protos.node.clone()),
        ("Element", ctx.realm.protos.element.clone()),
        ("HTMLElement", ctx.realm.protos.element.clone()),
        ("Document", ctx.realm.protos.document.clone()),
        ("HTMLDocument", ctx.realm.protos.document.clone()),
        ("Text", ctx.realm.protos.text.clone()),
        ("CharacterData", ctx.realm.protos.text.clone()),
        ("NodeList", ctx.realm.protos.node_list.clone()),
        ("HTMLCollection", ctx.realm.protos.node_list.clone()),
        ("DOMTokenList", ctx.realm.protos.dom_token_list.clone()),
        ("CSSStyleDeclaration", ctx.realm.protos.style.clone()),
    ];
    for (name, proto) in pairs {
        let constructor = ctx.native(name, 0, illegal_constructor);
        constructor.put_key(JsStr::from("prototype"), Value::Obj(proto.clone()), 0);
        if proto.own_value("constructor").is_none() {
            proto.put_hidden("constructor", Value::Obj(constructor.clone()));
        }
        if name == "Node" {
            for (constant, code) in [("ELEMENT_NODE", 1), ("TEXT_NODE", 3), ("DOCUMENT_NODE", 9), ("DOCUMENT_FRAGMENT_NODE", 11)] {
                constructor.put_key(JsStr::from(constant), Value::num(code), 0);
            }
        }
        global.put_hidden(name, Value::Obj(constructor));
    }
    for name in [
        "HTMLDivElement", "HTMLSpanElement", "HTMLAnchorElement", "HTMLInputElement", "HTMLButtonElement",
        "HTMLFormElement", "HTMLImageElement", "HTMLSelectElement", "HTMLTextAreaElement", "HTMLParagraphElement",
        "HTMLScriptElement", "HTMLStyleElement", "HTMLLinkElement", "HTMLTemplateElement", "HTMLCanvasElement",
        "HTMLBodyElement", "HTMLHeadElement", "HTMLIFrameElement", "HTMLLabelElement", "HTMLOptionElement",
        "HTMLTableElement", "HTMLUListElement", "HTMLLIElement", "HTMLVideoElement", "HTMLAudioElement",
        "SVGElement", "DocumentFragment",
    ] {
        let element = ctx.realm.protos.element.clone();
        let constructor = ctx.native(name, 0, illegal_constructor);
        constructor.put_key(JsStr::from("prototype"), Value::Obj(element), 0);
        global.put_hidden(name, Value::Obj(constructor));
    }
    let document = wrap(ctx, 0);
    global.put_hidden("document", document);
}

fn node_list_item(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let index = ops::to_integer(ctx, &arg(args, 0))?;
    Ok(ops::get_value_key(ctx, this, &Value::num(index))?).map(|v| if v.is_undefined() { Value::Null } else { v })
}

fn illegal_constructor(ctx: &mut Ctx, _: &Value, _: &[Value], _: &Value) -> R<Value> {
    ctx.throw_type("Illegal constructor")
}
