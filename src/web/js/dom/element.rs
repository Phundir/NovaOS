//! `Element`: attributes, `classList`, `style`, `dataset`, form
//! controls and geometry -- what an element adds to a node.
//!
//! Split from `node.rs` (the tree, queries, the document) for size; the
//! two share their small helpers.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use super::super::interp::{Ctx, R};
use super::super::ops;
use super::super::value::*;
use super::node::{
    accessor, arg, by_class, by_tag, closest, getter, has_focus, insert_adjacent, insert_many, matches, method, node_get, noop,
    query_selector, query_selector_all, remove, string_arg, text_content_set,
};
use super::{node_list, this_node, throw_dom, wrap};
use crate::web::css;
use crate::web::html::{NodeData, NodeId};

fn get_attribute(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let node = this_node(ctx, this, "getAttribute")?;
    let name = string_arg(ctx, args, 0)?.to_ascii_lowercase();
    Ok(match ctx.doc.attribute(node, &name) {
        Some(value) => Value::str(value),
        None => Value::Null,
    })
}

fn set_attribute(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let node = this_node(ctx, this, "setAttribute")?;
    let name = string_arg(ctx, args, 0)?;
    let value = string_arg(ctx, args, 1)?;
    write_attribute(ctx, node, &name, &value);
    Ok(Value::Undefined)
}

pub fn write_attribute(ctx: &mut Ctx, node: NodeId, name: &str, value: &str) {
    let name = name.to_ascii_lowercase();
    ctx.doc.set_attribute(node, &name, value);
    // An inline handler set again is compiled again.
    if name.starts_with("on") {
        if let Some(wrapper) = ctx.realm.dom.wrappers.get(&node) {
            wrapper.borrow_mut().props.remove(&super::events::compiled_key(&name[2..]));
        }
    }
    if name == "value" {
        ctx.realm.dom.values.remove(&node);
    }
    if name == "checked" {
        ctx.realm.dom.checked.remove(&node);
    }
    ctx.dirty = true;
}

fn remove_attribute(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let node = this_node(ctx, this, "removeAttribute")?;
    let name = string_arg(ctx, args, 0)?;
    if ctx.doc.remove_attribute(node, &name) {
        ctx.dirty = true;
    }
    Ok(Value::Undefined)
}

fn has_attribute(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let node = this_node(ctx, this, "hasAttribute")?;
    let name = string_arg(ctx, args, 0)?.to_ascii_lowercase();
    Ok(Value::Bool(ctx.doc.attribute(node, &name).is_some()))
}

fn toggle_attribute(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let node = this_node(ctx, this, "toggleAttribute")?;
    let name = string_arg(ctx, args, 0)?.to_ascii_lowercase();
    let present = ctx.doc.attribute(node, &name).is_some();
    let wanted = match arg(args, 1) {
        Value::Undefined => !present,
        force => force.truthy(),
    };
    if wanted && !present {
        write_attribute(ctx, node, &name, "");
    } else if !wanted && present {
        ctx.doc.remove_attribute(node, &name);
        ctx.dirty = true;
    }
    Ok(Value::Bool(wanted))
}

fn attribute_pairs(ctx: &Ctx, node: NodeId) -> Vec<(String, String)> {
    match &ctx.doc.nodes[node].data {
        NodeData::Element { attributes, .. } => attributes.clone(),
        _ => Vec::new(),
    }
}

fn get_attribute_names(ctx: &mut Ctx, this: &Value, _: &[Value], _: &Value) -> R<Value> {
    let node = this_node(ctx, this, "getAttributeNames")?;
    let names: Vec<Value> = attribute_pairs(ctx, node).into_iter().map(|(n, _)| Value::str(&n)).collect();
    Ok(Value::Obj(ctx.new_array(names)))
}

fn attributes_get(ctx: &mut Ctx, this: &Value, _: &[Value], _: &Value) -> R<Value> {
    let node = this_node(ctx, this, "attributes")?;
    let mut list = Vec::new();
    for (name, value) in attribute_pairs(ctx, node) {
        let attribute = ctx.new_object();
        attribute.put("name", Value::str(&name));
        attribute.put("value", Value::str(&value));
        list.push(Value::Obj(attribute));
    }
    Ok(Value::Obj(ctx.new_array(list)))
}

/// A string attribute reflected as a property: data is the attribute.
fn reflect_get(ctx: &mut Ctx, this: &Value, _: &[Value], data: &Value) -> R<Value> {
    let node = this_node(ctx, this, "attribute")?;
    let Value::Str(name) = data else {
        return Ok(Value::Undefined);
    };
    let value = ctx.doc.attribute(node, name).unwrap_or("");
    // URLs read back absolute, as browsers give them.
    if matches!(name.as_str(), "href" | "src" | "action") && !value.is_empty() {
        let base = crate::web::url::Url::parse(&ctx.base_url);
        if let Some(absolute) = base.and_then(|b| b.join(value)) {
            return Ok(Value::Str(JsStr::from(absolute.to_string())));
        }
    }
    Ok(Value::str(value))
}

fn reflect_set(ctx: &mut Ctx, this: &Value, args: &[Value], data: &Value) -> R<Value> {
    let node = this_node(ctx, this, "attribute")?;
    let Value::Str(name) = data else {
        return Ok(Value::Undefined);
    };
    let value = string_arg(ctx, args, 0)?;
    write_attribute(ctx, node, &name.clone(), &value);
    Ok(Value::Undefined)
}

/// A boolean attribute reflected as a property (`hidden`, `disabled`).
fn flag_get(ctx: &mut Ctx, this: &Value, _: &[Value], data: &Value) -> R<Value> {
    let node = this_node(ctx, this, "attribute")?;
    let Value::Str(name) = data else {
        return Ok(Value::Undefined);
    };
    Ok(Value::Bool(ctx.doc.attribute(node, name).is_some()))
}

fn flag_set(ctx: &mut Ctx, this: &Value, args: &[Value], data: &Value) -> R<Value> {
    let node = this_node(ctx, this, "attribute")?;
    let Value::Str(name) = data else {
        return Ok(Value::Undefined);
    };
    let name = name.clone();
    if arg(args, 0).truthy() {
        write_attribute(ctx, node, &name, "");
    } else if ctx.doc.remove_attribute(node, &name) {
        ctx.dirty = true;
    }
    Ok(Value::Undefined)
}

fn tag_name(ctx: &mut Ctx, this: &Value, _: &[Value], data: &Value) -> R<Value> {
    let node = this_node(ctx, this, "tagName")?;
    let tag = ctx.doc.tag(node).unwrap_or("");
    Ok(Value::Str(JsStr::from(if data.truthy() { String::from(tag) } else { tag.to_ascii_uppercase() })))
}

fn inner_html_get(ctx: &mut Ctx, this: &Value, _: &[Value], data: &Value) -> R<Value> {
    let node = this_node(ctx, this, "innerHTML")?;
    Ok(Value::Str(JsStr::from(if data.truthy() { ctx.doc.outer_html(node) } else { ctx.doc.inner_html(node) })))
}

fn inner_html_set(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let node = this_node(ctx, this, "innerHTML")?;
    let source = string_arg(ctx, args, 0)?;
    if !ctx.doc.set_inner_html(node, &source) {
        return ctx.throw_range("trop de noeuds dans la page");
    }
    ctx.dirty = true;
    Ok(Value::Undefined)
}

fn outer_html_set(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let node = this_node(ctx, this, "outerHTML")?;
    let source = string_arg(ctx, args, 0)?;
    let Some(parent) = ctx.doc.nodes[node].parent else {
        return throw_dom(ctx, "NoModificationAllowedError", "This element has no parent node.");
    };
    let Some(nodes) = ctx.doc.import_fragment(&source) else {
        return ctx.throw_range("trop de noeuds dans la page");
    };
    for child in nodes {
        ctx.doc.insert_before(parent, child, Some(node));
    }
    ctx.doc.detach(node);
    ctx.dirty = true;
    Ok(Value::Undefined)
}

fn inner_text_get(ctx: &mut Ctx, this: &Value, _: &[Value], _: &Value) -> R<Value> {
    let node = this_node(ctx, this, "innerText")?;
    Ok(Value::Str(JsStr::from(ctx.doc.text_content(node))))
}

// --- form controls ---

fn is_control(ctx: &Ctx, node: NodeId) -> bool {
    matches!(ctx.doc.tag(node), Some("input" | "textarea" | "select" | "option" | "button"))
}

/// The value a control holds: what was typed, else its markup.
pub fn control_value(ctx: &Ctx, node: NodeId) -> String {
    if let Some(value) = ctx.realm.dom.values.get(&node) {
        return value.clone();
    }
    match ctx.doc.tag(node) {
        Some("textarea") => ctx.doc.text_content_raw(node),
        Some("select") => {
            let options: Vec<NodeId> = ctx.doc.preorder(node).into_iter().filter(|n| ctx.doc.tag(*n) == Some("option")).collect();
            let chosen = options.iter().find(|o| ctx.doc.attribute(**o, "selected").is_some()).or(options.first());
            chosen.map(|o| option_value(ctx, *o)).unwrap_or_default()
        }
        Some("option") => option_value(ctx, node),
        _ => String::from(ctx.doc.attribute(node, "value").unwrap_or("")),
    }
}

fn option_value(ctx: &Ctx, node: NodeId) -> String {
    match ctx.doc.attribute(node, "value") {
        Some(value) => String::from(value),
        None => ctx.doc.text_content(node),
    }
}

fn value_get(ctx: &mut Ctx, this: &Value, _: &[Value], _: &Value) -> R<Value> {
    let node = this_node(ctx, this, "value")?;
    if !is_control(ctx, node) {
        return Ok(match ctx.doc.attribute(node, "value") {
            Some(value) => Value::str(value),
            None => Value::Undefined,
        });
    }
    Ok(Value::Str(JsStr::from(control_value(ctx, node))))
}

fn value_set(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let node = this_node(ctx, this, "value")?;
    let value = if arg(args, 0).is_nullish() { String::new() } else { string_arg(ctx, args, 0)? };
    if ctx.doc.tag(node) == Some("select") {
        // The option with this value becomes the selected one.
        let options: Vec<NodeId> = ctx.doc.preorder(node).into_iter().filter(|n| ctx.doc.tag(*n) == Some("option")).collect();
        for option in options {
            let selected = option_value(ctx, option) == value;
            if selected {
                ctx.doc.set_attribute(option, "selected", "");
            } else {
                ctx.doc.remove_attribute(option, "selected");
            }
        }
    }
    ctx.realm.dom.values.insert(node, value);
    ctx.dirty = true;
    Ok(Value::Undefined)
}

fn checked_get(ctx: &mut Ctx, this: &Value, _: &[Value], _: &Value) -> R<Value> {
    let node = this_node(ctx, this, "checked")?;
    let checked = match ctx.realm.dom.checked.get(&node) {
        Some(checked) => *checked,
        None => ctx.doc.attribute(node, "checked").is_some(),
    };
    Ok(Value::Bool(checked))
}

fn checked_set(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let node = this_node(ctx, this, "checked")?;
    let checked = arg(args, 0).truthy();
    ctx.realm.dom.checked.insert(node, checked);
    // A radio ticked unticks its group.
    if checked && ctx.doc.attribute(node, "type").is_some_and(|t| t.eq_ignore_ascii_case("radio")) {
        if let Some(name) = ctx.doc.attribute(node, "name").map(String::from) {
            let group: Vec<NodeId> = ctx
                .doc
                .preorder(0)
                .into_iter()
                .filter(|n| *n != node && ctx.doc.tag(*n) == Some("input") && ctx.doc.attribute(*n, "name") == Some(name.as_str()))
                .collect();
            for other in group {
                ctx.realm.dom.checked.insert(other, false);
            }
        }
    }
    ctx.dirty = true;
    Ok(Value::Undefined)
}

fn selected_index_get(ctx: &mut Ctx, this: &Value, _: &[Value], _: &Value) -> R<Value> {
    let node = this_node(ctx, this, "selectedIndex")?;
    let value = control_value(ctx, node);
    let options: Vec<NodeId> = ctx.doc.preorder(node).into_iter().filter(|n| ctx.doc.tag(*n) == Some("option")).collect();
    Ok(Value::num(options.iter().position(|o| option_value(ctx, *o) == value).map_or(-1, |i| i as i64)))
}

fn options_get(ctx: &mut Ctx, this: &Value, _: &[Value], _: &Value) -> R<Value> {
    let node = this_node(ctx, this, "options")?;
    let options: Vec<NodeId> = ctx.doc.preorder(node).into_iter().filter(|n| ctx.doc.tag(*n) == Some("option")).collect();
    Ok(node_list(ctx, &options))
}

fn form_get(ctx: &mut Ctx, this: &Value, _: &[Value], _: &Value) -> R<Value> {
    let node = this_node(ctx, this, "form")?;
    let mut current = ctx.doc.nodes[node].parent;
    while let Some(parent) = current {
        if ctx.doc.tag(parent) == Some("form") {
            return Ok(wrap(ctx, parent));
        }
        current = ctx.doc.nodes[parent].parent;
    }
    Ok(Value::Null)
}

/// `form.elements`: its controls, in order.
fn elements_get(ctx: &mut Ctx, this: &Value, _: &[Value], _: &Value) -> R<Value> {
    let node = this_node(ctx, this, "elements")?;
    let controls: Vec<NodeId> = ctx
        .doc
        .preorder(node)
        .into_iter()
        .filter(|n| matches!(ctx.doc.tag(*n), Some("input" | "textarea" | "select" | "button")))
        .collect();
    let list = node_list(ctx, &controls);
    // By name too: `form.elements.email`.
    if let Value::Obj(object) = &list {
        for control in &controls {
            if let Some(name) = ctx.doc.attribute(*control, "name").map(String::from) {
                let wrapper = wrap(ctx, *control);
                object.put(&name, wrapper);
            }
        }
    }
    Ok(list)
}

/// `form.submit()` / `requestSubmit()`: asks the host.
fn form_submit(ctx: &mut Ctx, this: &Value, _: &[Value], data: &Value) -> R<Value> {
    let node = this_node(ctx, this, "submit")?;
    if data.truthy() {
        let event = super::events::new_event(ctx, "submit", true, true);
        if !super::events::dispatch(ctx, node, &event)? {
            return Ok(Value::Undefined);
        }
    }
    ctx.realm.dom.submit_request = Some(node);
    Ok(Value::Undefined)
}

// --- classList ---

pub(super) fn classes_of(ctx: &Ctx, node: NodeId) -> Vec<String> {
    ctx.doc.attribute(node, "class").unwrap_or("").split_whitespace().map(String::from).collect()
}

fn set_classes(ctx: &mut Ctx, node: NodeId, classes: &[String]) {
    let joined = classes.join(" ");
    ctx.doc.set_attribute(node, "class", &joined);
    ctx.dirty = true;
}

fn class_list_get(ctx: &mut Ctx, this: &Value, _: &[Value], _: &Value) -> R<Value> {
    let node = this_node(ctx, this, "classList")?;
    let proto = ctx.realm.protos.dom_token_list.clone();
    Ok(Value::Obj(ctx.alloc(Kind::Node(node), Some(proto), "DOMTokenList")))
}

/// The DOMTokenList methods: data says which.
fn token_list(ctx: &mut Ctx, this: &Value, args: &[Value], data: &Value) -> R<Value> {
    let node = this_node(ctx, this, "classList")?;
    let which = match data {
        Value::Str(s) => s.clone(),
        _ => return Ok(Value::Undefined),
    };
    let mut classes = classes_of(ctx, node);
    let mut names = Vec::new();
    for value in args {
        names.push(String::from(ops::to_string(ctx, value)?.as_str()));
    }
    Ok(match which.as_str() {
        "add" => {
            for name in names {
                if !classes.contains(&name) {
                    classes.push(name);
                }
            }
            set_classes(ctx, node, &classes);
            Value::Undefined
        }
        "remove" => {
            classes.retain(|c| !names.contains(c));
            set_classes(ctx, node, &classes);
            Value::Undefined
        }
        "toggle" => {
            let name = names.first().cloned().unwrap_or_default();
            let present = classes.contains(&name);
            let wanted = match args.get(1) {
                Some(force) if !force.is_undefined() => force.truthy(),
                _ => !present,
            };
            if wanted && !present {
                classes.push(name);
            } else if !wanted {
                classes.retain(|c| *c != name);
            }
            set_classes(ctx, node, &classes);
            Value::Bool(wanted)
        }
        "contains" => Value::Bool(names.first().is_some_and(|n| classes.contains(n))),
        "replace" => {
            let (old, new) = (names.first().cloned().unwrap_or_default(), names.get(1).cloned().unwrap_or_default());
            match classes.iter().position(|c| *c == old) {
                Some(i) => {
                    classes[i] = new;
                    classes.dedup();
                    set_classes(ctx, node, &classes);
                    Value::Bool(true)
                }
                None => Value::Bool(false),
            }
        }
        "item" => {
            let index = ops::to_integer(ctx, &arg(args, 0))?;
            match classes.get(index.max(0) as usize) {
                Some(c) => Value::str(c),
                None => Value::Null,
            }
        }
        "length" => Value::num(classes.len() as i64),
        "value" | "toString" => Value::Str(JsStr::from(classes.join(" "))),
        "forEach" => {
            let callback = arg(args, 0);
            for (i, class) in classes.iter().enumerate() {
                ctx.call(&callback, arg(args, 1), &[Value::str(class), Value::num(i as i64), this.clone()])?;
            }
            Value::Undefined
        }
        _ => Value::Undefined,
    })
}

fn class_list_values(ctx: &mut Ctx, this: &Value, _: &[Value], _: &Value) -> R<Value> {
    let node = this_node(ctx, this, "classList")?;
    let classes: Vec<Value> = classes_of(ctx, node).iter().map(|c| Value::str(c)).collect();
    let proto = ctx.realm.protos.iterator.clone();
    Ok(Value::Obj(ctx.alloc(Kind::Iterator(IterState::List(classes, 0)), Some(proto), "Array Iterator")))
}

// --- style ---

/// CSS properties a script sets through `element.style.x`: camelCase
/// name, CSS name. Without `Proxy`, each needs an accessor.
const STYLE_PROPERTIES: &[&str] = &[
    "display", "position", "top", "right", "bottom", "left", "width", "height", "min-width", "min-height",
    "max-width", "max-height", "margin", "margin-top", "margin-right", "margin-bottom", "margin-left", "padding",
    "padding-top", "padding-right", "padding-bottom", "padding-left", "border", "border-top", "border-right",
    "border-bottom", "border-left", "border-color", "border-width", "border-style", "border-radius", "color",
    "background", "background-color", "background-image", "font", "font-size", "font-weight", "font-family",
    "font-style", "line-height", "text-align", "text-decoration", "text-transform", "white-space", "overflow",
    "overflow-x", "overflow-y", "visibility", "opacity", "z-index", "float", "clear", "cursor", "transform",
    "transition", "animation", "flex", "flex-direction", "flex-wrap", "flex-grow", "flex-shrink", "flex-basis",
    "justify-content", "align-items", "align-self", "gap", "grid-template-columns", "grid-template-rows",
    "box-sizing", "box-shadow", "outline", "vertical-align", "list-style", "content", "pointer-events",
    "user-select", "letter-spacing", "word-break", "word-wrap", "text-overflow", "inset",
];

fn camel(name: &str) -> String {
    let mut out = String::new();
    let mut upper = false;
    for c in name.chars() {
        if c == '-' {
            upper = true;
        } else if upper {
            out.push(c.to_ascii_uppercase());
            upper = false;
        } else {
            out.push(c);
        }
    }
    out
}

fn declarations(ctx: &Ctx, node: NodeId) -> Vec<(String, String)> {
    let text = ctx.doc.attribute(node, "style").unwrap_or("");
    css::parse_declarations(text).into_iter().map(|d| (d.property, d.value)).collect()
}

fn write_declarations(ctx: &mut Ctx, node: NodeId, list: &[(String, String)]) {
    // Serialized as the CSSOM does: "a: b; c: d;".
    let text: Vec<String> = list.iter().map(|(p, v)| alloc::format!("{}: {};", p, v)).collect();
    let text = text.join(" ");
    if text.is_empty() {
        ctx.doc.remove_attribute(node, "style");
    } else {
        ctx.doc.set_attribute(node, "style", &text);
    }
    ctx.dirty = true;
}

fn set_declaration(ctx: &mut Ctx, node: NodeId, property: &str, value: &str) {
    let mut list = declarations(ctx, node);
    list.retain(|(p, _)| p != property);
    if !value.trim().is_empty() {
        list.push((String::from(property), String::from(value.trim())));
    }
    write_declarations(ctx, node, &list);
}

fn style_get_object(ctx: &mut Ctx, this: &Value, _: &[Value], _: &Value) -> R<Value> {
    let node = this_node(ctx, this, "style")?;
    let proto = ctx.realm.protos.style.clone();
    Ok(Value::Obj(ctx.alloc(Kind::Node(node), Some(proto), "CSSStyleDeclaration")))
}

fn style_property_get(ctx: &mut Ctx, this: &Value, _: &[Value], data: &Value) -> R<Value> {
    let node = this_node(ctx, this, "style")?;
    let Value::Str(property) = data else {
        return Ok(Value::str(""));
    };
    let value = declarations(ctx, node).into_iter().rev().find(|(p, _)| p == property.as_str()).map(|(_, v)| v).unwrap_or_default();
    Ok(Value::Str(JsStr::from(value)))
}

fn style_property_set(ctx: &mut Ctx, this: &Value, args: &[Value], data: &Value) -> R<Value> {
    let node = this_node(ctx, this, "style")?;
    let Value::Str(property) = data else {
        return Ok(Value::Undefined);
    };
    let value = match arg(args, 0) {
        Value::Null | Value::Undefined => String::new(),
        Value::Num(n) if !n.is_zero() && !matches!(property.as_str(), "opacity" | "z-index" | "flex-grow" | "flex-shrink" | "line-height" | "font-weight") => {
            // A bare number is only valid for unitless properties.
            String::from(ops::number_string(n).as_str())
        }
        other => String::from(ops::to_string(ctx, &other)?.as_str()),
    };
    let property = String::from(property.as_str());
    set_declaration(ctx, node, &property, &value);
    Ok(Value::Undefined)
}

fn style_method(ctx: &mut Ctx, this: &Value, args: &[Value], data: &Value) -> R<Value> {
    let node = this_node(ctx, this, "style")?;
    let which = match data {
        Value::Str(s) => s.clone(),
        _ => return Ok(Value::Undefined),
    };
    let property = string_arg(ctx, args, 0)?.to_ascii_lowercase();
    Ok(match which.as_str() {
        "setProperty" => {
            let value = string_arg(ctx, args, 1)?;
            set_declaration(ctx, node, &property, &value);
            Value::Undefined
        }
        "removeProperty" => {
            let old = declarations(ctx, node).into_iter().find(|(p, _)| *p == property).map(|(_, v)| v).unwrap_or_default();
            set_declaration(ctx, node, &property, "");
            Value::Str(JsStr::from(old))
        }
        _ => Value::Str(JsStr::from(declarations(ctx, node).into_iter().rev().find(|(p, _)| *p == property).map(|(_, v)| v).unwrap_or_default())),
    })
}

fn css_text_get(ctx: &mut Ctx, this: &Value, _: &[Value], _: &Value) -> R<Value> {
    let node = this_node(ctx, this, "cssText")?;
    Ok(Value::str(ctx.doc.attribute(node, "style").unwrap_or("")))
}

fn css_text_set(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let node = this_node(ctx, this, "cssText")?;
    let text = string_arg(ctx, args, 0)?;
    ctx.doc.set_attribute(node, "style", &text);
    ctx.dirty = true;
    Ok(Value::Undefined)
}

// --- dataset ---

/// `dataset`: an object of the `data-*` attributes, whose accessors
/// write back. A key added to it that no attribute had is not seen
/// (that would take a `Proxy`).
fn dataset_get(ctx: &mut Ctx, this: &Value, _: &[Value], _: &Value) -> R<Value> {
    let node = this_node(ctx, this, "dataset")?;
    let object = ctx.new_object();
    object.put_key(crate::web::js::interp::private_key("node"), Value::num(node as i64), 0);
    for (name, _) in attribute_pairs(ctx, node) {
        if let Some(key) = name.strip_prefix("data-") {
            let attribute = Value::str(&name);
            let getter = ctx.native_with(key, 0, dataset_value, attribute.clone());
            let setter = ctx.native_with(key, 1, dataset_store, attribute);
            object.borrow_mut().props.set(JsStr::from(camel(key)), Slot::Accessor(Some(getter), Some(setter), ENUMERABLE | CONFIGURABLE));
        }
    }
    Ok(Value::Obj(object))
}

fn dataset_node(this: &Value) -> Option<NodeId> {
    let Value::Obj(object) = this else {
        return None;
    };
    match object.borrow().props.get(&crate::web::js::interp::private_key("node")) {
        Some(Slot::Data(Value::Num(n), _)) => Some(n.to_i64_saturating() as usize),
        _ => None,
    }
}

fn dataset_value(ctx: &mut Ctx, this: &Value, _: &[Value], data: &Value) -> R<Value> {
    let (Some(node), Value::Str(attribute)) = (dataset_node(this), data) else {
        return Ok(Value::Undefined);
    };
    Ok(match ctx.doc.attribute(node, attribute) {
        Some(value) => Value::str(value),
        None => Value::Undefined,
    })
}

fn dataset_store(ctx: &mut Ctx, this: &Value, args: &[Value], data: &Value) -> R<Value> {
    let (Some(node), Value::Str(attribute)) = (dataset_node(this), data) else {
        return Ok(Value::Undefined);
    };
    let value = string_arg(ctx, args, 0)?;
    write_attribute(ctx, node, &attribute.clone(), &value);
    Ok(Value::Undefined)
}

// --- geometry ---

fn rect_of(ctx: &Ctx, node: NodeId) -> (i32, i32, i32, i32) {
    let mut found: Option<(i32, i32, i32, i32)> = None;
    for (n, x, y, w, h) in &ctx.realm.dom.boxes {
        if *n == node || ctx.doc.is_inside(*n, node) {
            found = Some(match found {
                None => (*x, *y, *w, *h),
                Some((fx, fy, fw, fh)) => {
                    let left = fx.min(*x);
                    let top = fy.min(*y);
                    let right = (fx + fw).max(x + w);
                    let bottom = (fy + fh).max(y + h);
                    (left, top, right - left, bottom - top)
                }
            });
        }
    }
    found.unwrap_or((0, 0, 0, 0))
}

fn bounding_rect(ctx: &mut Ctx, this: &Value, _: &[Value], _: &Value) -> R<Value> {
    let node = this_node(ctx, this, "getBoundingClientRect")?;
    let (x, y, w, h) = rect_of(ctx, node);
    let y = y - ctx.realm.dom.scroll_y;
    let rect = ctx.new_object();
    for (name, value) in [("x", x), ("y", y), ("left", x), ("top", y), ("width", w), ("height", h), ("right", x + w), ("bottom", y + h)] {
        rect.put(name, Value::num(value as i64));
    }
    Ok(Value::Obj(rect))
}

fn geometry(ctx: &mut Ctx, this: &Value, _: &[Value], data: &Value) -> R<Value> {
    let node = this_node(ctx, this, "offsetWidth")?;
    let (x, y, w, h) = rect_of(ctx, node);
    let which = match data {
        Value::Num(n) => n.to_i64_saturating(),
        _ => 0,
    };
    Ok(Value::num(match which {
        0 => w,
        1 => h,
        2 => x,
        _ => y,
    } as i64))
}

fn focus(ctx: &mut Ctx, this: &Value, _: &[Value], data: &Value) -> R<Value> {
    let node = this_node(ctx, this, "focus")?;
    if data.truthy() {
        if ctx.realm.dom.focused == Some(node) {
            ctx.realm.dom.focused = None;
        }
    } else {
        ctx.realm.dom.focused = Some(node);
    }
    Ok(Value::Undefined)
}

fn click(ctx: &mut Ctx, this: &Value, _: &[Value], _: &Value) -> R<Value> {
    let node = this_node(ctx, this, "click")?;
    let event = super::events::new_event(ctx, "click", true, true);
    super::events::dispatch(ctx, node, &event)?;
    Ok(Value::Undefined)
}


pub fn install(ctx: &mut Ctx) {
    let element = ctx.realm.protos.element.clone();
    for name in [
        "children", "childElementCount", "firstElementChild", "lastElementChild", "nextElementSibling",
        "previousElementSibling",
    ] {
        getter(ctx, &element, name, node_get, Value::str(name));
    }
    getter(ctx, &element, "tagName", tag_name, Value::Bool(false));
    getter(ctx, &element, "localName", tag_name, Value::Bool(true));
    for (property, attribute) in [
        ("id", "id"), ("className", "class"), ("title", "title"), ("lang", "lang"), ("href", "href"), ("src", "src"),
        ("alt", "alt"), ("name", "name"), ("type", "type"), ("placeholder", "placeholder"), ("action", "action"),
        ("method", "method"), ("target", "target"), ("rel", "rel"), ("htmlFor", "for"), ("role", "role"),
        ("autocomplete", "autocomplete"), ("min", "min"), ("max", "max"), ("step", "step"), ("pattern", "pattern"),
        ("dir", "dir"), ("download", "download"), ("defaultValue", "value"),
    ] {
        accessor(ctx, &element, property, reflect_get, reflect_set, Value::str(attribute));
    }
    for name in ["hidden", "disabled", "required", "readOnly", "multiple", "autofocus", "selected", "open", "defer", "async"] {
        let attribute = name.to_ascii_lowercase();
        accessor(ctx, &element, name, flag_get, flag_set, Value::str(&attribute));
    }
    accessor(ctx, &element, "innerHTML", inner_html_get, inner_html_set, Value::Bool(false));
    accessor(ctx, &element, "outerHTML", inner_html_get, outer_html_set, Value::Bool(true));
    accessor(ctx, &element, "innerText", inner_text_get, text_content_set, Value::Undefined);
    accessor(ctx, &element, "value", value_get, value_set, Value::Undefined);
    accessor(ctx, &element, "checked", checked_get, checked_set, Value::Undefined);
    getter(ctx, &element, "selectedIndex", selected_index_get, Value::Undefined);
    getter(ctx, &element, "options", options_get, Value::Undefined);
    getter(ctx, &element, "form", form_get, Value::Undefined);
    getter(ctx, &element, "elements", elements_get, Value::Undefined);
    getter(ctx, &element, "attributes", attributes_get, Value::Undefined);
    getter(ctx, &element, "classList", class_list_get, Value::Undefined);
    getter(ctx, &element, "style", style_get_object, Value::Undefined);
    getter(ctx, &element, "dataset", dataset_get, Value::Undefined);
    for (name, which) in [("offsetWidth", 0), ("offsetHeight", 1), ("offsetLeft", 2), ("offsetTop", 3), ("clientWidth", 0), ("clientHeight", 1), ("scrollWidth", 0), ("scrollHeight", 1)] {
        getter(ctx, &element, name, geometry, Value::num(which));
    }
    let element_methods: &[(&str, usize, NativeFn, Value)] = &[
        ("getAttribute", 1, get_attribute, Value::Undefined),
        ("setAttribute", 2, set_attribute, Value::Undefined),
        ("removeAttribute", 1, remove_attribute, Value::Undefined),
        ("hasAttribute", 1, has_attribute, Value::Undefined),
        ("toggleAttribute", 1, toggle_attribute, Value::Undefined),
        ("getAttributeNames", 0, get_attribute_names, Value::Undefined),
        ("remove", 0, remove, Value::Undefined),
        ("append", 0, insert_many, Value::num(0)),
        ("prepend", 0, insert_many, Value::num(1)),
        ("before", 0, insert_many, Value::num(2)),
        ("after", 0, insert_many, Value::num(3)),
        ("replaceWith", 0, insert_many, Value::num(4)),
        ("replaceChildren", 0, insert_many, Value::num(5)),
        ("insertAdjacentHTML", 2, insert_adjacent, Value::num(0)),
        ("insertAdjacentText", 2, insert_adjacent, Value::num(1)),
        ("insertAdjacentElement", 2, insert_adjacent, Value::num(2)),
        ("querySelector", 1, query_selector, Value::Undefined),
        ("querySelectorAll", 1, query_selector_all, Value::Undefined),
        ("matches", 1, matches, Value::Undefined),
        ("closest", 1, closest, Value::Undefined),
        ("getElementsByTagName", 1, by_tag, Value::Undefined),
        ("getElementsByClassName", 1, by_class, Value::Undefined),
        ("getBoundingClientRect", 0, bounding_rect, Value::Undefined),
        ("scrollIntoView", 0, noop, Value::Undefined),
        ("focus", 0, focus, Value::Bool(false)),
        ("blur", 0, focus, Value::Bool(true)),
        ("click", 0, click, Value::Undefined),
        ("submit", 0, form_submit, Value::Bool(false)),
        ("requestSubmit", 0, form_submit, Value::Bool(true)),
        ("reset", 0, noop, Value::Undefined),
        ("select", 0, noop, Value::Undefined),
        ("setSelectionRange", 2, noop, Value::Undefined),
        ("checkValidity", 0, has_focus, Value::Undefined),
        ("reportValidity", 0, has_focus, Value::Undefined),
    ];
    for (name, length, function, data) in element_methods {
        method(ctx, &element, name, *length, *function, data.clone());
    }
    // `matches` has older names.
    method(ctx, &element, "webkitMatchesSelector", 1, matches, Value::Undefined);

    let tokens = ctx.realm.protos.dom_token_list.clone();
    for name in ["add", "remove", "toggle", "contains", "replace", "item", "forEach", "toString"] {
        method(ctx, &tokens, name, 1, token_list, Value::str(name));
    }
    getter(ctx, &tokens, "length", token_list, Value::str("length"));
    accessor(ctx, &tokens, "value", token_list, noop, Value::str("value"));
    let iterator = ctx.realm.symbols.iterator.clone();
    let values = ctx.native("values", 0, class_list_values);
    tokens.put_key(iterator, Value::Obj(values), HIDDEN);

    let style = ctx.realm.protos.style.clone();
    for property in STYLE_PROPERTIES {
        let name = camel(property);
        accessor(ctx, &style, &name, style_property_get, style_property_set, Value::str(property));
        if name != *property {
            // `style["background-color"]` works too.
            accessor(ctx, &style, property, style_property_get, style_property_set, Value::str(property));
        }
    }
    accessor(ctx, &style, "cssFloat", style_property_get, style_property_set, Value::str("float"));
    accessor(ctx, &style, "cssText", css_text_get, css_text_set, Value::Undefined);
    for name in ["setProperty", "removeProperty", "getPropertyValue"] {
        method(ctx, &style, name, 2, style_method, Value::str(name));
    }

}
