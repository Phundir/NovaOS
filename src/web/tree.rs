//! The document as scripts change it: creation, insertion, removal,
//! attributes, serialization (16.2).
//!
//! `html::Document` is an arena: nodes are never freed, a removed node
//! keeps its slot with no parent. That keeps every `NodeId` a script
//! holds valid -- a wrapper object of the engine names its node by index
//! -- at the price of slots that do not come back: creation stays bounded
//! by `html::MAX_NODES`, and a page that creates and drops nodes forever
//! meets that bound.
//!
//! Two consequences the rest of `web` must respect, and does since 16.2:
//!
//! * **document order is the tree's, not the arena's.** A node moved by
//!   a script can sit before its new parent in the arena. Whatever walks
//!   the document in order uses `preorder`, which also leaves out the
//!   nodes no longer connected.
//! * **a detached node still exists**: lookups go from the root down.

use alloc::string::String;
use alloc::vec::Vec;

use super::html::{self, Document, Node, NodeData, NodeId, MAX_NODES};

/// Elements serialized without an end tag.
const VOID: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source", "track", "wbr",
];

impl Document {
    /// The nodes under `root`, root included, in document order.
    pub fn preorder(&self, root: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let mut stack = alloc::vec![root];
        while let Some(node) = stack.pop() {
            out.push(node);
            stack.extend(self.nodes[node].children.iter().rev());
        }
        out
    }

    /// Is the node in the document (its ancestors reach the root)?
    pub fn is_connected(&self, mut node: NodeId) -> bool {
        let mut steps = 0;
        loop {
            if node == 0 {
                return true;
            }
            match self.nodes[node].parent {
                Some(parent) => node = parent,
                None => return false,
            }
            steps += 1;
            if steps > html::MAX_NODES {
                return false;
            }
        }
    }

    fn push(&mut self, data: NodeData) -> Option<NodeId> {
        if self.nodes.len() >= MAX_NODES {
            return None;
        }
        self.nodes.push(Node { parent: None, children: Vec::new(), data });
        Some(self.nodes.len() - 1)
    }

    /// A new element, not yet in the tree; `None` past `MAX_NODES`.
    pub fn create_element(&mut self, tag: &str) -> Option<NodeId> {
        self.push(NodeData::Element { tag: tag.to_ascii_lowercase(), attributes: Vec::new() })
    }

    pub fn create_text(&mut self, text: &str) -> Option<NodeId> {
        self.push(NodeData::Text(String::from(text)))
    }

    /// Takes a node out of its parent; it keeps its own children.
    pub fn detach(&mut self, node: NodeId) {
        if let Some(parent) = self.nodes[node].parent.take() {
            self.nodes[parent].children.retain(|child| *child != node);
        }
    }

    /// Can `child` go under `parent` without making a cycle, and can
    /// `parent` hold children at all?
    pub fn may_insert(&self, parent: NodeId, child: NodeId) -> bool {
        if child == 0 || matches!(self.nodes[parent].data, NodeData::Text(_)) {
            return false;
        }
        child != parent && !self.is_inside(parent, child)
    }

    /// Inserts `child` under `parent`, before `reference` (at the end
    /// when `None` or not a child of `parent`). Moves it if it was
    /// elsewhere. False when the insertion would make a cycle.
    pub fn insert_before(&mut self, parent: NodeId, child: NodeId, reference: Option<NodeId>) -> bool {
        if !self.may_insert(parent, child) {
            return false;
        }
        self.detach(child);
        let children = &mut self.nodes[parent].children;
        let position = reference.and_then(|r| children.iter().position(|c| *c == r)).unwrap_or(children.len());
        children.insert(position, child);
        self.nodes[child].parent = Some(parent);
        true
    }

    pub fn append_child(&mut self, parent: NodeId, child: NodeId) -> bool {
        self.insert_before(parent, child, None)
    }

    /// Detaches every child of `node`.
    pub fn clear_children(&mut self, node: NodeId) {
        let children = core::mem::take(&mut self.nodes[node].children);
        for child in children {
            self.nodes[child].parent = None;
        }
    }

    pub fn set_attribute(&mut self, node: NodeId, name: &str, value: &str) {
        if let NodeData::Element { attributes, .. } = &mut self.nodes[node].data {
            let name = name.to_ascii_lowercase();
            match attributes.iter_mut().find(|(key, _)| *key == name) {
                Some(entry) => entry.1 = String::from(value),
                None => attributes.push((name, String::from(value))),
            }
        }
    }

    pub fn remove_attribute(&mut self, node: NodeId, name: &str) -> bool {
        if let NodeData::Element { attributes, .. } = &mut self.nodes[node].data {
            let name = name.to_ascii_lowercase();
            let before = attributes.len();
            attributes.retain(|(key, _)| *key != name);
            return attributes.len() != before;
        }
        false
    }

    /// `textContent = text`: a text node's data, or an element's children
    /// replaced by one text node (none for the empty text).
    pub fn set_text_content(&mut self, node: NodeId, text: &str) -> bool {
        if let NodeData::Text(content) = &mut self.nodes[node].data {
            *content = String::from(text);
            return true;
        }
        self.clear_children(node);
        if text.is_empty() {
            return true;
        }
        match self.create_text(text) {
            Some(child) => self.append_child(node, child),
            None => false,
        }
    }

    /// Copies `node` (and its subtree when `deep`) into new slots. A loop
    /// rather than a recursion: a script can build a tree deeper than the
    /// parser ever would.
    pub fn clone_node(&mut self, node: NodeId, deep: bool) -> Option<NodeId> {
        let originals = if deep { self.preorder(node) } else { alloc::vec![node] };
        if self.nodes.len() + originals.len() > MAX_NODES {
            return None;
        }
        let mut copies: alloc::collections::BTreeMap<NodeId, NodeId> = alloc::collections::BTreeMap::new();
        for original in &originals {
            let copy = self.push(self.nodes[*original].data.clone())?;
            copies.insert(*original, copy);
            if *original != node {
                let parent = self.nodes[*original].parent.and_then(|p| copies.get(&p).copied());
                if let Some(parent) = parent {
                    self.nodes[parent].children.push(copy);
                    self.nodes[copy].parent = Some(parent);
                }
            }
        }
        copies.get(&node).copied()
    }

    /// Parses `source` as a fragment and copies its nodes in, detached.
    /// Returns the top-level nodes, in order; `None` past `MAX_NODES`.
    pub fn import_fragment(&mut self, source: &str) -> Option<Vec<NodeId>> {
        let fragment = html::parse(source);
        if self.nodes.len() + fragment.nodes.len() > MAX_NODES {
            return None;
        }
        // Fragment ids map to new ids by offset: the fragment's root (0)
        // is not copied.
        let offset = self.nodes.len() - 1;
        for node in fragment.nodes.iter().skip(1) {
            self.nodes.push(Node {
                parent: node.parent.filter(|p| *p != 0).map(|p| p + offset),
                children: node.children.iter().map(|c| c + offset).collect(),
                data: node.data.clone(),
            });
        }
        Some(fragment.nodes[0].children.iter().map(|c| c + offset).collect())
    }

    /// `innerHTML = source`.
    pub fn set_inner_html(&mut self, node: NodeId, source: &str) -> bool {
        let Some(nodes) = self.import_fragment(source) else {
            return false;
        };
        self.clear_children(node);
        for child in nodes {
            self.append_child(node, child);
        }
        true
    }

    /// The markup of a node's children.
    pub fn inner_html(&self, node: NodeId) -> String {
        let mut out = String::new();
        for child in &self.nodes[node].children {
            self.serialize(*child, &mut out, 0);
        }
        out
    }

    /// The markup of a node, itself included.
    pub fn outer_html(&self, node: NodeId) -> String {
        let mut out = String::new();
        self.serialize(node, &mut out, 0);
        out
    }

    fn serialize(&self, node: NodeId, out: &mut String, depth: usize) {
        if depth > html::MAX_DEPTH {
            return;
        }
        match &self.nodes[node].data {
            NodeData::Document => {
                for child in &self.nodes[node].children {
                    self.serialize(*child, out, depth + 1);
                }
            }
            NodeData::Text(text) => {
                let raw = self.nodes[node]
                    .parent
                    .and_then(|p| self.tag(p))
                    .is_some_and(|t| matches!(t, "script" | "style" | "xmp" | "noembed"));
                if raw {
                    out.push_str(text);
                } else {
                    escape_into(text, out, false);
                }
            }
            NodeData::Element { tag, attributes } => {
                out.push('<');
                out.push_str(tag);
                for (name, value) in attributes {
                    out.push(' ');
                    out.push_str(name);
                    out.push_str("=\"");
                    escape_into(value, out, true);
                    out.push('"');
                }
                out.push('>');
                if VOID.contains(&tag.as_str()) {
                    return;
                }
                for child in &self.nodes[node].children {
                    self.serialize(*child, out, depth + 1);
                }
                out.push_str("</");
                out.push_str(tag);
                out.push('>');
            }
        }
    }
}

fn escape_into(text: &str, out: &mut String, attribute: bool) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' if !attribute => out.push_str("&lt;"),
            '>' if !attribute => out.push_str("&gt;"),
            '"' if attribute => out.push_str("&quot;"),
            '\u{a0}' => out.push_str("&nbsp;"),
            _ => out.push(c),
        }
    }
}
