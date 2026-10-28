//! HTML parsing: tokens, then a tree (after the HTML Living Standard,
//! simplified).
//!
//! ## What is kept of the standard
//!
//! The parts that decide what a page looks like in practice:
//!
//! * raw text inside `script` and `style`, escapable raw text inside
//!   `title` and `textarea` -- a `<` in a script must not open a tag;
//! * void elements (`br`, `img`, `input`...) that never take children;
//! * the implied end tags that make sloppy HTML nest the way authors
//!   meant: a `<p>` closes the open paragraph, an `<li>` the open item,
//!   a `<td>` the open cell, a block closes a paragraph;
//! * end tags with no matching start are ignored, and an end tag closes
//!   whatever was left open inside it.
//!
//! What is not: the full insertion modes, foster parenting of text inside
//! tables, the adoption agency for misnested formatting. A page that
//! depends on them renders a little differently here than in a browser
//! that implements them, which is the usual outcome of broken markup.
//!
//! ## Bounds
//!
//! At most `MAX_NODES` nodes and `MAX_DEPTH` open elements: a page must
//! not be able to exhaust the heap or build a tree that later walks
//! recursively past the stack.

use alloc::string::String;
use alloc::vec::Vec;

use super::text;

/// Every task shares the kernel heap, where a failed allocation is a
/// panic of the whole machine: a page's tree is bounded well within it.
/// Sixty thousand nodes is twice a long Wikipedia article.
pub const MAX_NODES: usize = 60_000;
pub const MAX_DEPTH: usize = 256;

pub type NodeId = usize;

#[derive(Debug, Clone)]
pub enum NodeData {
    Document,
    Element { tag: String, attributes: Vec<(String, String)> },
    Text(String),
}

#[derive(Debug, Clone)]
pub struct Node {
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
    pub data: NodeData,
}

#[derive(Debug, Clone)]
pub struct Document {
    pub nodes: Vec<Node>,
}

const VOID: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source", "track", "wbr",
];

/// Elements that close an open `<p>` when they start.
const CLOSES_P: &[&str] = &[
    "address", "article", "aside", "blockquote", "center", "details", "dialog", "dir", "div", "dl", "fieldset",
    "figcaption", "figure", "footer", "form", "h1", "h2", "h3", "h4", "h5", "h6", "header", "hgroup", "hr",
    "main", "menu", "nav", "ol", "p", "pre", "section", "summary", "table", "ul",
];

impl Document {
    pub fn tag(&self, id: NodeId) -> Option<&str> {
        match &self.nodes[id].data {
            NodeData::Element { tag, .. } => Some(tag),
            _ => None,
        }
    }

    pub fn attribute(&self, id: NodeId, name: &str) -> Option<&str> {
        match &self.nodes[id].data {
            NodeData::Element { attributes, .. } => attributes
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.as_str()),
            _ => None,
        }
    }

    /// Every element with this tag in the tree, in document order (the
    /// tree's: a script may have moved nodes, see `tree.rs`).
    pub fn elements(&self, tag: &str) -> Vec<NodeId> {
        self.preorder(0).into_iter().filter(|id| self.tag(*id) == Some(tag)).collect()
    }

    /// The text under a node, whitespace collapsed.
    pub fn text_content(&self, id: NodeId) -> String {
        let mut out = String::new();
        let mut stack = alloc::vec![id];
        while let Some(node) = stack.pop() {
            match &self.nodes[node].data {
                NodeData::Text(content) => out.push_str(content),
                _ => stack.extend(self.nodes[node].children.iter().rev()),
            }
        }
        String::from(text::collapse_whitespace(&out).trim())
    }

    /// The text under a node exactly as written: a `<style>` block.
    pub fn text_content_raw(&self, id: NodeId) -> String {
        let mut out = String::new();
        let mut stack = alloc::vec![id];
        while let Some(node) = stack.pop() {
            match &self.nodes[node].data {
                NodeData::Text(content) => out.push_str(content),
                _ => stack.extend(self.nodes[node].children.iter().rev()),
            }
        }
        out
    }

    pub fn title(&self) -> String {
        self.elements("title")
            .first()
            .map(|id| self.text_content(*id))
            .unwrap_or_default()
    }

    /// The `body` element, or the document itself for a fragment.
    pub fn body(&self) -> NodeId {
        self.elements("body").first().copied().unwrap_or(0)
    }

    /// The base URL a `<base href>` sets, if any.
    pub fn base(&self) -> Option<String> {
        self.elements("base")
            .iter()
            .find_map(|id| self.attribute(*id, "href").map(String::from))
    }

    /// Is `ancestor` above `node`?
    pub fn is_inside(&self, mut node: NodeId, ancestor: NodeId) -> bool {
        while let Some(parent) = self.nodes[node].parent {
            if parent == ancestor {
                return true;
            }
            node = parent;
        }
        false
    }
}

struct Builder {
    document: Document,
    open: Vec<NodeId>,
}

impl Builder {
    fn current(&self) -> NodeId {
        *self.open.last().unwrap_or(&0)
    }

    fn append(&mut self, data: NodeData) -> Option<NodeId> {
        if self.document.nodes.len() >= MAX_NODES {
            return None;
        }
        let parent = self.current();
        let id = self.document.nodes.len();
        self.document.nodes.push(Node {
            parent: Some(parent),
            children: Vec::new(),
            data,
        });
        self.document.nodes[parent].children.push(id);
        Some(id)
    }

    fn text(&mut self, content: String) {
        if content.is_empty() {
            return;
        }
        // Consecutive text merges into one node.
        let parent = self.current();
        if let Some(&last) = self.document.nodes[parent].children.last()
            && let NodeData::Text(existing) = &mut self.document.nodes[last].data
        {
            existing.push_str(&content);
            return;
        }
        self.append(NodeData::Text(content));
    }

    fn open_tags(&self) -> impl Iterator<Item = &str> + '_ {
        self.open.iter().rev().filter_map(|id| self.document.tag(*id))
    }

    /// Closes up to and including the nearest open `tag`, not crossing a
    /// `boundary` element.
    fn close(&mut self, tag: &str, boundaries: &[&str]) -> bool {
        let mut found = None;
        for (depth, open) in self.open.iter().enumerate().rev() {
            let Some(name) = self.document.tag(*open) else {
                continue;
            };
            if name == tag {
                found = Some(depth);
                break;
            }
            if boundaries.contains(&name) {
                break;
            }
        }
        match found {
            Some(depth) => {
                self.open.truncate(depth);
                true
            }
            None => false,
        }
    }

    fn start(&mut self, tag: String, attributes: Vec<(String, String)>, self_closing: bool) {
        let tag_str = tag.as_str();
        // Implied end tags.
        if CLOSES_P.contains(&tag_str) {
            self.close("p", &["button", "table", "td", "th", "li", "div", "section", "article"]);
        }
        match tag_str {
            "li" => {
                self.close("li", &["ul", "ol", "menu"]);
            }
            "dt" | "dd" => {
                if !self.close("dt", &["dl"]) {
                    self.close("dd", &["dl"]);
                }
            }
            "option" => {
                self.close("option", &["select", "datalist"]);
            }
            "tr" => {
                self.close("tr", &["table", "tbody", "thead", "tfoot"]);
            }
            "td" | "th" => {
                if !self.close("td", &["tr", "table"]) {
                    self.close("th", &["tr", "table"]);
                }
            }
            "thead" | "tbody" | "tfoot" => {
                for section in ["thead", "tbody", "tfoot"] {
                    self.close(section, &["table"]);
                }
            }
            "a" => {
                // An anchor never nests: a new one closes the open one.
                self.close("a", &["div", "p", "td", "li", "table"]);
            }
            "body" | "html" | "head" => {
                // Kept once. A second `<body>` merges into the first.
                if let Some(existing) = self.document.elements(tag_str).first().copied() {
                    if let NodeData::Element { attributes: existing_attributes, .. } =
                        &mut self.document.nodes[existing].data
                    {
                        for (key, value) in attributes {
                            if !existing_attributes.iter().any(|(k, _)| *k == key) {
                                existing_attributes.push((key, value));
                            }
                        }
                    }
                    if tag_str == "body" && !self.open.contains(&existing) {
                        self.open.push(existing);
                    }
                    return;
                }
                if tag_str == "body" {
                    self.close("head", &[]);
                }
            }
            _ => {}
        }
        if self.open.len() >= MAX_DEPTH {
            return;
        }
        let is_void = VOID.contains(&tag_str);
        if let Some(id) = self.append(NodeData::Element { tag, attributes })
            && !is_void
            && !self_closing
        {
            self.open.push(id);
        }
    }

    fn end(&mut self, tag: &str) {
        match tag {
            "p" if !self.open_tags().any(|name| name == "p") => {
                // `</p>` with no open paragraph makes an empty one.
                self.append(NodeData::Element {
                    tag: String::from("p"),
                    attributes: Vec::new(),
                });
            }
            "body" | "html" => {}
            "br" => {
                self.append(NodeData::Element {
                    tag: String::from("br"),
                    attributes: Vec::new(),
                });
            }
            _ => {
                self.close(tag, &[]);
            }
        }
    }
}

fn is_name_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_' || byte == b':'
}

/// Where `</tag` starts, case-insensitively, from `from` on. Searched in
/// place: lower-casing the rest of the page for each script made a page
/// with fifty scripts copy itself fifty times.
fn find_end_tag(bytes: &[u8], from: usize, tag: &[u8]) -> Option<usize> {
    let mut index = from;
    while index + 2 + tag.len() <= bytes.len() {
        if bytes[index] == b'<'
            && bytes[index + 1] == b'/'
            && bytes[index + 2..index + 2 + tag.len()].eq_ignore_ascii_case(tag)
        {
            return Some(index);
        }
        index += 1;
    }
    None
}

/// Parses an HTML document.
pub fn parse(source: &str) -> Document {
    let mut builder = Builder {
        document: Document {
            nodes: alloc::vec![Node {
                parent: None,
                children: Vec::new(),
                data: NodeData::Document,
            }],
        },
        open: alloc::vec![0],
    };
    let bytes = source.as_bytes();
    let mut position = 0;
    let mut text_start = 0;

    while position < bytes.len() {
        if bytes[position] != b'<' {
            position += 1;
            continue;
        }
        let next = bytes.get(position + 1).copied().unwrap_or(0);
        let is_markup = next.is_ascii_alphabetic() || next == b'/' || next == b'!' || next == b'?';
        if !is_markup {
            position += 1;
            continue;
        }
        builder.text(text::decode_entities(&source[text_start..position]));

        // Comments, doctype, processing instructions.
        if source[position..].starts_with("<!--") {
            let end = source[position + 4..].find("-->").map(|index| position + 4 + index + 3);
            position = end.unwrap_or(bytes.len());
            text_start = position;
            continue;
        }
        if next == b'!' || next == b'?' {
            let end = source[position..].find('>').map(|index| position + index + 1);
            position = end.unwrap_or(bytes.len());
            text_start = position;
            continue;
        }

        // A tag.
        let closing = next == b'/';
        let mut cursor = position + 1 + usize::from(closing);
        let name_start = cursor;
        while cursor < bytes.len() && is_name_char(bytes[cursor]) {
            cursor += 1;
        }
        let tag = source[name_start..cursor].to_ascii_lowercase();
        if tag.is_empty() {
            // `</ >` or similar: skip to the next `>`.
            position = source[cursor..].find('>').map(|index| cursor + index + 1).unwrap_or(bytes.len());
            text_start = position;
            continue;
        }

        // Attributes.
        let mut attributes: Vec<(String, String)> = Vec::new();
        let mut self_closing = false;
        loop {
            while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
                cursor += 1;
            }
            if cursor >= bytes.len() {
                break;
            }
            match bytes[cursor] {
                b'>' => {
                    cursor += 1;
                    break;
                }
                b'/' => {
                    self_closing = bytes.get(cursor + 1) == Some(&b'>');
                    cursor += 1;
                    continue;
                }
                _ => {}
            }
            let key_start = cursor;
            while cursor < bytes.len()
                && !bytes[cursor].is_ascii_whitespace()
                && !matches!(bytes[cursor], b'=' | b'>' | b'/')
            {
                cursor += 1;
            }
            if cursor == key_start {
                cursor += 1;
                continue;
            }
            let key = source[key_start..cursor].to_ascii_lowercase();
            while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
                cursor += 1;
            }
            let mut value = String::new();
            if bytes.get(cursor) == Some(&b'=') {
                cursor += 1;
                while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
                    cursor += 1;
                }
                match bytes.get(cursor) {
                    Some(&quote) if quote == b'"' || quote == b'\'' => {
                        let value_start = cursor + 1;
                        let end = source[value_start..]
                            .find(quote as char)
                            .map(|index| value_start + index)
                            .unwrap_or(bytes.len());
                        value = text::decode_entities(&source[value_start..end]);
                        cursor = (end + 1).min(bytes.len());
                    }
                    _ => {
                        let value_start = cursor;
                        while cursor < bytes.len() && !bytes[cursor].is_ascii_whitespace() && bytes[cursor] != b'>' {
                            cursor += 1;
                        }
                        value = text::decode_entities(&source[value_start..cursor]);
                    }
                }
            }
            if !attributes.iter().any(|(existing, _)| *existing == key) {
                attributes.push((key, value));
            }
        }
        position = cursor;
        text_start = position;

        if closing {
            builder.end(&tag);
            continue;
        }

        // Raw text elements: everything up to the matching end tag.
        let raw = matches!(tag.as_str(), "script" | "style" | "title" | "textarea" | "xmp" | "noembed");
        builder.start(tag.clone(), attributes, self_closing);
        if raw && !self_closing {
            let end = find_end_tag(bytes, position, tag.as_bytes()).unwrap_or(bytes.len());
            let content = &source[position..end];
            let content = if matches!(tag.as_str(), "title" | "textarea") {
                text::decode_entities(content)
            } else {
                String::from(content)
            };
            builder.text(content);
            builder.close(&tag, &[]);
            position = source[end..].find('>').map(|index| end + index + 1).unwrap_or(bytes.len());
            text_start = position;
        }
    }
    if text_start < bytes.len() {
        builder.text(text::decode_entities(&source[text_start..]));
    }
    builder.document
}
