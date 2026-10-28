//! CSS parsing and selector matching (CSS 2.1 and the parts of Selectors
//! Level 3 pages use).
//!
//! ## What is understood
//!
//! Rules with selector lists; type, universal, `#id`, `.class` and
//! attribute selectors (`[a]`, `=`, `~=`, `|=`, `^=`, `$=`, `*=`); the
//! four combinators; the structural pseudo-classes (`:first-child`,
//! `:last-child`, `:only-child`, `:nth-child(an+b)`, `:root`, `:empty`,
//! `:link`, `:not(...)`); `@media` evaluated against the page width;
//! `@supports` taken as true; `@import` reported for fetching.
//!
//! ## What is not
//!
//! Pseudo-elements (`::before`, `::after`, `::marker`) and dynamic states
//! (`:hover`, `:focus`, `:checked`...): a rule using them never matches.
//! That is the right failure: generated content and hover effects are
//! decoration, and applying such a rule to the element itself would paint
//! a hover colour permanently.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use super::html::{Document, NodeData, NodeId};

#[derive(Debug, Clone)]
pub struct Declaration {
    pub property: String,
    pub value: String,
    pub important: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Combinator {
    Descendant,
    Child,
    Adjacent,
    Sibling,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum AttributeTest {
    Exists,
    Equals(String),
    Word(String),
    Dash(String),
    Prefix(String),
    Suffix(String),
    Contains(String),
}

#[derive(Debug, Clone)]
enum Pseudo {
    FirstChild,
    LastChild,
    OnlyChild,
    /// `:nth-child(a n + b)`, or from the end.
    Nth { a: i32, b: i32, from_end: bool },
    Root,
    Empty,
    Link,
    Not(Box<Compound>),
}

#[derive(Debug, Clone, Default)]
pub struct Compound {
    tag: Option<String>,
    id: Option<String>,
    classes: Vec<String>,
    attributes: Vec<(String, AttributeTest)>,
    pseudo: Vec<Pseudo>,
    /// A state or pseudo-element this engine never has.
    never: bool,
    /// `::before` (1) or `::after` (2): the rule styles generated content.
    pseudo_element: u8,
}

#[derive(Debug, Clone)]
pub struct Selector {
    /// Left to right; `combinators[i]` joins `compounds[i]` and `[i + 1]`.
    compounds: Vec<Compound>,
    combinators: Vec<Combinator>,
    pub specificity: u32,
}

#[derive(Debug, Clone)]
pub struct Rule {
    pub selectors: Vec<Selector>,
    pub declarations: Vec<Declaration>,
}

#[derive(Debug, Clone, Default)]
pub struct Stylesheet {
    pub rules: Vec<Rule>,
    /// URLs of `@import`, for the loader.
    pub imports: Vec<String>,
}

// ============================================================
//  Parsing
// ============================================================

fn strip_comments(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut rest = source;
    while let Some(start) = rest.find("/*") {
        out.push_str(&rest[..start]);
        match rest[start + 2..].find("*/") {
            Some(end) => rest = &rest[start + 2 + end + 2..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// The index just past the block that starts at `open` (a `{`), with
/// nested braces and strings skipped.
fn block_end(bytes: &[u8], open: usize) -> usize {
    let mut depth = 0usize;
    let mut index = open;
    let mut quote: Option<u8> = None;
    while index < bytes.len() {
        let byte = bytes[index];
        match quote {
            Some(q) => {
                if byte == b'\\' {
                    index += 1;
                } else if byte == q {
                    quote = None;
                }
            }
            None => match byte {
                b'"' | b'\'' => quote = Some(byte),
                b'{' => depth += 1,
                b'}' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return index + 1;
                    }
                }
                _ => {}
            },
        }
        index += 1;
    }
    bytes.len()
}

/// Parses the declarations of a block or a `style` attribute.
pub fn parse_declarations(block: &str) -> Vec<Declaration> {
    let mut out = Vec::new();
    for part in split_top_level(block, ';') {
        let Some((property, value)) = part.split_once(':') else {
            continue;
        };
        let property = property.trim().to_ascii_lowercase();
        if property.is_empty() {
            continue;
        }
        let mut value = value.trim();
        let mut important = false;
        if let Some(position) = value.to_ascii_lowercase().rfind("!important") {
            value = value[..position].trim();
            important = true;
        }
        out.push(Declaration {
            property,
            value: value.to_string(),
            important,
        });
    }
    out
}

/// Splits at `separator` outside parentheses and strings.
pub fn split_top_level(text: &str, separator: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut start = 0;
    for (index, character) in text.char_indices() {
        match quote {
            Some(q) if character == q => quote = None,
            Some(_) => {}
            None => match character {
                '"' | '\'' => quote = Some(character),
                '(' | '[' => depth += 1,
                ')' | ']' => depth -= 1,
                c if c == separator && depth <= 0 => {
                    parts.push(&text[start..index]);
                    start = index + character.len_utf8();
                }
                _ => {}
            },
        }
    }
    parts.push(&text[start..]);
    parts
}

/// Does a media query list apply to a screen `width` pixels wide?
pub fn media_matches(query: &str, width: i32) -> bool {
    let query = query.to_ascii_lowercase();
    if query.trim().is_empty() {
        return true;
    }
    split_top_level(&query, ',').iter().any(|single| {
        let single = single.trim();
        let negated = single.starts_with("not ");
        let single = single.trim_start_matches("not ").trim_start_matches("only ").trim();
        let mut result = true;
        for part in single.split(" and ") {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            let matched = if let Some(feature) = part.strip_prefix('(').and_then(|p| p.strip_suffix(')')) {
                let (name, value) = feature.split_once(':').map(|(n, v)| (n.trim(), v.trim())).unwrap_or((feature.trim(), ""));
                let pixels = parse_length_px(value, 16, 16, width, 800).unwrap_or(0);
                match name {
                    "min-width" => width >= pixels,
                    "max-width" => width <= pixels,
                    "width" => width == pixels,
                    "orientation" => value == "landscape",
                    "prefers-color-scheme" => value == "light",
                    "prefers-reduced-motion" => value == "reduce",
                    "hover" | "any-hover" => value == "hover",
                    "pointer" | "any-pointer" => value == "fine",
                    "min-resolution" | "-webkit-min-device-pixel-ratio" | "min--moz-device-pixel-ratio" => false,
                    _ => true,
                }
            } else {
                matches!(part, "screen" | "all")
            };
            result &= matched;
        }
        result != negated
    })
}

/// Parses a style sheet. `width` evaluates `@media`.
pub fn parse(source: &str, width: i32) -> Stylesheet {
    let mut sheet = Stylesheet::default();
    let text = strip_comments(source);
    parse_into(&text, width, &mut sheet, 0);
    sheet
}

fn parse_into(text: &str, width: i32, sheet: &mut Stylesheet, depth: usize) {
    let bytes = text.as_bytes();
    let mut position = 0;
    while position < bytes.len() {
        while position < bytes.len() && (bytes[position].is_ascii_whitespace() || bytes[position] == b';') {
            position += 1;
        }
        if position >= bytes.len() {
            break;
        }
        if bytes[position] == b'@' {
            let rest = &text[position..];
            let brace = rest.find('{');
            let semicolon = rest.find(';');
            match (brace, semicolon) {
                (Some(brace), semicolon) if semicolon.is_none_or(|s| s > brace) => {
                    let prelude = rest[..brace].to_ascii_lowercase();
                    let end = block_end(bytes, position + brace);
                    let inner = &text[position + brace + 1..end.saturating_sub(1).max(position + brace + 1)];
                    if depth < 8 {
                        if let Some(query) = prelude.strip_prefix("@media") {
                            if media_matches(query, width) {
                                parse_into(inner, width, sheet, depth + 1);
                            }
                        } else if prelude.starts_with("@supports") || prelude.starts_with("@layer") || prelude.starts_with("@container") {
                            parse_into(inner, width, sheet, depth + 1);
                        }
                    }
                    position = end;
                }
                (_, Some(semicolon)) => {
                    let statement = &rest[..semicolon];
                    if let Some(import) = statement.strip_prefix("@import") {
                        let import = import.trim();
                        let url = import
                            .strip_prefix("url(")
                            .map(|inner| inner.split(')').next().unwrap_or(""))
                            .unwrap_or(import.split_whitespace().next().unwrap_or(""));
                        let url = url.trim().trim_matches(|c| c == '"' || c == '\'');
                        let media = import.rsplit(')').next().unwrap_or("");
                        if !url.is_empty() && (import.starts_with("url(") && media_matches(media, width) || !import.starts_with("url(")) {
                            sheet.imports.push(url.to_string());
                        }
                    }
                    position += semicolon + 1;
                }
                _ => break,
            }
            continue;
        }
        let rest = &text[position..];
        let Some(brace) = rest.find('{') else {
            break;
        };
        let end = block_end(bytes, position + brace);
        let prelude = &rest[..brace];
        let block = &text[position + brace + 1..end.saturating_sub(1).max(position + brace + 1)];
        let selectors: Vec<Selector> = split_top_level(prelude, ',')
            .iter()
            .filter_map(|selector| parse_selector(selector.trim()))
            .collect();
        if !selectors.is_empty() {
            let declarations = parse_declarations(block);
            if !declarations.is_empty() {
                sheet.rules.push(Rule { selectors, declarations });
            }
        }
        position = end;
    }
}

fn is_ident(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '-' || character == '_' || !character.is_ascii()
}

fn read_ident(chars: &[char], index: &mut usize) -> String {
    let mut out = String::new();
    while *index < chars.len() {
        let character = chars[*index];
        if character == '\\' && *index + 1 < chars.len() {
            out.push(chars[*index + 1]);
            *index += 2;
            continue;
        }
        if !is_ident(character) {
            break;
        }
        out.push(character);
        *index += 1;
    }
    out
}

fn parse_nth(argument: &str) -> Option<(i32, i32)> {
    let argument: String = argument.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_ascii_lowercase();
    match argument.as_str() {
        "odd" => return Some((2, 1)),
        "even" => return Some((2, 0)),
        _ => {}
    }
    if let Some((a, b)) = argument.split_once('n') {
        let a = match a {
            "" | "+" => 1,
            "-" => -1,
            other => other.parse().ok()?,
        };
        let b = if b.is_empty() { 0 } else { b.trim_start_matches('+').parse().ok()? };
        Some((a, b))
    } else {
        Some((0, argument.parse().ok()?))
    }
}

/// Deepest nesting of `:not(...)` / `:is(...)` accepted, and most
/// compounds in one selector. Both are recursions whose depth the style
/// sheet decides: bounded, a hostile sheet cannot run the parser or the
/// matcher off the stack (DECISIONS.md, "compter des octets").
const MAX_NESTING: usize = 4;
const MAX_COMPOUNDS: usize = 32;

/// Parses one compound selector from `chars[index..]`.
fn parse_compound(chars: &[char], index: &mut usize) -> Option<Compound> {
    parse_compound_at(chars, index, 0)
}

fn parse_compound_at(chars: &[char], index: &mut usize, depth: usize) -> Option<Compound> {
    if depth > MAX_NESTING {
        return None;
    }
    let mut compound = Compound::default();
    let start = *index;
    while *index < chars.len() {
        let character = chars[*index];
        match character {
            '*' => {
                *index += 1;
            }
            '#' => {
                *index += 1;
                compound.id = Some(read_ident(chars, index));
            }
            '.' => {
                *index += 1;
                compound.classes.push(read_ident(chars, index));
            }
            '[' => {
                *index += 1;
                let end = chars[*index..].iter().position(|c| *c == ']')? + *index;
                let inner: String = chars[*index..end].iter().collect();
                *index = end + 1;
                let (name, test) = match inner.find(['=', '~', '|', '^', '$', '*']) {
                    Some(position) => {
                        let name = inner[..position].trim().to_ascii_lowercase();
                        let operator = &inner[position..];
                        let (kind, value) = if let Some(value) = operator.strip_prefix("=") {
                            ('=', value)
                        } else {
                            (operator.chars().next()?, operator.get(2..).unwrap_or(""))
                        };
                        let value = value.trim();
                        // A trailing ` i` flag makes the match case-insensitive;
                        // dropped, and the match stays exact.
                        let value = value.strip_suffix(" i").unwrap_or(value).trim();
                        let value = value.trim_matches(|c| c == '"' || c == '\'').to_string();
                        let test = match kind {
                            '=' => AttributeTest::Equals(value),
                            '~' => AttributeTest::Word(value),
                            '|' => AttributeTest::Dash(value),
                            '^' => AttributeTest::Prefix(value),
                            '$' => AttributeTest::Suffix(value),
                            _ => AttributeTest::Contains(value),
                        };
                        (name, test)
                    }
                    None => (inner.trim().to_ascii_lowercase(), AttributeTest::Exists),
                };
                compound.attributes.push((name, test));
            }
            ':' => {
                *index += 1;
                if chars.get(*index) == Some(&':') {
                    // A pseudo-element: `::before` and `::after` generate
                    // content; the others (`::marker`, `::placeholder`...)
                    // are not generated here.
                    *index += 1;
                    let name = read_ident(chars, index).to_ascii_lowercase();
                    match name.as_str() {
                        "before" => compound.pseudo_element = 1,
                        "after" => compound.pseudo_element = 2,
                        _ => compound.never = true,
                    }
                    if chars.get(*index) == Some(&'(') {
                        skip_parentheses(chars, index);
                    }
                    continue;
                }
                let name = read_ident(chars, index).to_ascii_lowercase();
                // CSS 2 wrote pseudo-elements with one colon.
                if name == "before" || name == "after" {
                    compound.pseudo_element = if name == "before" { 1 } else { 2 };
                    continue;
                }
                let argument = if chars.get(*index) == Some(&'(') {
                    let open = *index;
                    skip_parentheses(chars, index);
                    Some(chars[open + 1..*index - 1].iter().collect::<String>())
                } else {
                    None
                };
                match (name.as_str(), argument) {
                    ("first-child", None) => compound.pseudo.push(Pseudo::FirstChild),
                    ("last-child", None) => compound.pseudo.push(Pseudo::LastChild),
                    ("only-child", None) => compound.pseudo.push(Pseudo::OnlyChild),
                    ("root", None) => compound.pseudo.push(Pseudo::Root),
                    ("empty", None) => compound.pseudo.push(Pseudo::Empty),
                    ("link" | "any-link", None) => compound.pseudo.push(Pseudo::Link),
                    ("nth-child", Some(argument)) => match parse_nth(&argument) {
                        Some((a, b)) => compound.pseudo.push(Pseudo::Nth { a, b, from_end: false }),
                        None => compound.never = true,
                    },
                    ("nth-last-child", Some(argument)) => match parse_nth(&argument) {
                        Some((a, b)) => compound.pseudo.push(Pseudo::Nth { a, b, from_end: true }),
                        None => compound.never = true,
                    },
                    ("not", Some(argument)) => {
                        let inner: Vec<char> = argument.trim().chars().collect();
                        let mut position = 0;
                        match parse_compound_at(&inner, &mut position, depth + 1) {
                            Some(inner_compound) if position == inner.len() => {
                                compound.pseudo.push(Pseudo::Not(Box::new(inner_compound)))
                            }
                            // `:not(a b)` and lists: not understood, so
                            // the rule is dropped rather than misapplied.
                            _ => compound.never = true,
                        }
                    }
                    ("is" | "where" | "matches" | "-webkit-any", Some(argument)) => {
                        // Only the single-compound form is kept.
                        let inner: Vec<char> = argument.trim().chars().collect();
                        let mut position = 0;
                        match parse_compound_at(&inner, &mut position, depth + 1) {
                            Some(inner_compound) if position == inner.len() => merge(&mut compound, inner_compound),
                            _ => compound.never = true,
                        }
                    }
                    _ => compound.never = true,
                }
            }
            c if is_ident(c) && *index == start => {
                compound.tag = Some(read_ident(chars, index).to_ascii_lowercase());
            }
            _ => break,
        }
    }
    (*index > start).then_some(compound)
}

fn merge(into: &mut Compound, other: Compound) {
    if other.tag.is_some() {
        into.tag = other.tag;
    }
    if other.id.is_some() {
        into.id = other.id;
    }
    into.classes.extend(other.classes);
    into.attributes.extend(other.attributes);
    into.pseudo.extend(other.pseudo);
    into.never |= other.never;
}

fn skip_parentheses(chars: &[char], index: &mut usize) {
    let mut depth = 0;
    while *index < chars.len() {
        match chars[*index] {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    *index += 1;
                    return;
                }
            }
            _ => {}
        }
        *index += 1;
    }
}

fn specificity_of(compound: &Compound) -> (u32, u32, u32) {
    let mut ids = u32::from(compound.id.is_some());
    let mut classes = (compound.classes.len() + compound.attributes.len()) as u32;
    let mut tags = u32::from(compound.tag.is_some());
    for pseudo in &compound.pseudo {
        match pseudo {
            Pseudo::Not(inner) => {
                let (a, b, c) = specificity_of(inner);
                ids += a;
                classes += b;
                tags += c;
            }
            _ => classes += 1,
        }
    }
    (ids, classes, tags)
}

pub fn parse_selector(text: &str) -> Option<Selector> {
    let chars: Vec<char> = text.chars().collect();
    let mut index = 0;
    let mut compounds = Vec::new();
    let mut combinators = Vec::new();
    loop {
        while index < chars.len() && chars[index].is_whitespace() {
            index += 1;
        }
        if index >= chars.len() {
            break;
        }
        if !compounds.is_empty() {
            // A combinator: explicit, or the whitespace just skipped.
            let combinator = match chars[index] {
                '>' => Some(Combinator::Child),
                '+' => Some(Combinator::Adjacent),
                '~' => Some(Combinator::Sibling),
                _ => None,
            };
            match combinator {
                Some(combinator) => {
                    index += 1;
                    while index < chars.len() && chars[index].is_whitespace() {
                        index += 1;
                    }
                    combinators.push(combinator);
                }
                None => combinators.push(Combinator::Descendant),
            }
        }
        let compound = parse_compound(&chars, &mut index)?;
        compounds.push(compound);
        if compounds.len() > MAX_COMPOUNDS {
            return None;
        }
    }
    if compounds.is_empty() || combinators.len() + 1 != compounds.len() {
        return None;
    }
    let (mut a, mut b, mut c) = (0, 0, 0);
    for compound in &compounds {
        let (x, y, z) = specificity_of(compound);
        a += x;
        b += y;
        c += z;
    }
    Some(Selector {
        compounds,
        combinators,
        specificity: (a.min(255) << 16) | (b.min(255) << 8) | c.min(255),
    })
}

// ============================================================
//  Matching
// ============================================================

fn element_children(document: &Document, parent: NodeId) -> impl Iterator<Item = NodeId> + '_ {
    document.nodes[parent]
        .children
        .iter()
        .copied()
        .filter(|child| matches!(document.nodes[*child].data, NodeData::Element { .. }))
}

fn previous_element(document: &Document, node: NodeId) -> Option<NodeId> {
    let parent = document.nodes[node].parent?;
    let mut previous = None;
    for child in element_children(document, parent) {
        if child == node {
            return previous;
        }
        previous = Some(child);
    }
    None
}

fn matches_compound(document: &Document, node: NodeId, compound: &Compound) -> bool {
    if compound.never {
        return false;
    }
    let NodeData::Element { tag, attributes } = &document.nodes[node].data else {
        return false;
    };
    if let Some(wanted) = &compound.tag
        && wanted != tag
    {
        return false;
    }
    let attribute = |name: &str| attributes.iter().find(|(key, _)| key == name).map(|(_, value)| value.as_str());
    if let Some(id) = &compound.id
        && attribute("id") != Some(id.as_str())
    {
        return false;
    }
    if !compound.classes.is_empty() {
        let classes = attribute("class").unwrap_or("");
        if !compound
            .classes
            .iter()
            .all(|wanted| classes.split_ascii_whitespace().any(|class| class == wanted))
        {
            return false;
        }
    }
    for (name, test) in &compound.attributes {
        let Some(value) = attribute(name) else {
            return false;
        };
        let ok = match test {
            AttributeTest::Exists => true,
            AttributeTest::Equals(wanted) => value == wanted,
            AttributeTest::Word(wanted) => value.split_ascii_whitespace().any(|word| word == wanted),
            AttributeTest::Dash(wanted) => value == wanted || value.starts_with(&alloc::format!("{}-", wanted)),
            AttributeTest::Prefix(wanted) => !wanted.is_empty() && value.starts_with(wanted.as_str()),
            AttributeTest::Suffix(wanted) => !wanted.is_empty() && value.ends_with(wanted.as_str()),
            AttributeTest::Contains(wanted) => !wanted.is_empty() && value.contains(wanted.as_str()),
        };
        if !ok {
            return false;
        }
    }
    for pseudo in &compound.pseudo {
        let ok = match pseudo {
            Pseudo::Root => tag == "html",
            Pseudo::Link => tag == "a" && attribute("href").is_some(),
            Pseudo::Empty => document.nodes[node].children.is_empty(),
            Pseudo::Not(inner) => !matches_compound(document, node, inner),
            Pseudo::FirstChild | Pseudo::LastChild | Pseudo::OnlyChild | Pseudo::Nth { .. } => {
                let Some(parent) = document.nodes[node].parent else {
                    return false;
                };
                let siblings: Vec<NodeId> = element_children(document, parent).collect();
                let position = siblings.iter().position(|sibling| *sibling == node).unwrap_or(0) as i32;
                let count = siblings.len() as i32;
                match pseudo {
                    Pseudo::FirstChild => position == 0,
                    Pseudo::LastChild => position == count - 1,
                    Pseudo::OnlyChild => count == 1,
                    Pseudo::Nth { a, b, from_end } => {
                        let index = if *from_end { count - position } else { position + 1 };
                        if *a == 0 {
                            index == *b
                        } else {
                            let difference = index - b;
                            difference % a == 0 && difference / a >= 0
                        }
                    }
                    _ => false,
                }
            }
        };
        if !ok {
            return false;
        }
    }
    true
}

/// Does `selector` match `node`, with `compounds[..=last]` still to match?
fn matches_from(document: &Document, node: NodeId, selector: &Selector, last: usize) -> bool {
    if !matches_compound(document, node, &selector.compounds[last]) {
        return false;
    }
    if last == 0 {
        return true;
    }
    match selector.combinators[last - 1] {
        Combinator::Child => document.nodes[node]
            .parent
            .is_some_and(|parent| matches_from(document, parent, selector, last - 1)),
        Combinator::Descendant => {
            let mut ancestor = document.nodes[node].parent;
            while let Some(candidate) = ancestor {
                if matches_from(document, candidate, selector, last - 1) {
                    return true;
                }
                ancestor = document.nodes[candidate].parent;
            }
            false
        }
        Combinator::Adjacent => previous_element(document, node)
            .is_some_and(|previous| matches_from(document, previous, selector, last - 1)),
        Combinator::Sibling => {
            let mut previous = previous_element(document, node);
            while let Some(candidate) = previous {
                if matches_from(document, candidate, selector, last - 1) {
                    return true;
                }
                previous = previous_element(document, candidate);
            }
            false
        }
    }
}

pub fn matches(document: &Document, node: NodeId, selector: &Selector) -> bool {
    matches_from(document, node, selector, selector.compounds.len() - 1)
}

// ============================================================
//  Rule index
// ============================================================

/// Where a rule may apply, by the rightmost compound of each selector:
/// its id, else a class, else its tag, else anywhere. Only the rules in
/// an element's buckets are tried -- a page's two thousand rules against
/// its five thousand elements would otherwise be ten million matches.
#[derive(Default)]
pub struct Index {
    by_id: BTreeMap<String, Vec<(usize, usize, usize)>>,
    by_class: BTreeMap<String, Vec<(usize, usize, usize)>>,
    by_tag: BTreeMap<String, Vec<(usize, usize, usize)>>,
    universal: Vec<(usize, usize, usize)>,
}

/// A rule that matched, for the cascade: (specificity, order, sheet,
/// rule).
pub type Matched = (u32, usize, usize, usize);

impl Index {
    /// Indexes `sheets`. Each entry is (sheet, rule, selector).
    pub fn build(sheets: &[&Stylesheet]) -> Index {
        let mut index = Index::default();
        for (sheet_index, sheet) in sheets.iter().enumerate() {
            for (rule_index, rule) in sheet.rules.iter().enumerate() {
                for (selector_index, selector) in rule.selectors.iter().enumerate() {
                    let entry = (sheet_index, rule_index, selector_index);
                    let key = selector.compounds.last().expect("never empty");
                    if key.never {
                        continue;
                    }
                    if let Some(id) = &key.id {
                        index.by_id.entry(id.clone()).or_default().push(entry);
                    } else if let Some(class) = key.classes.first() {
                        index.by_class.entry(class.clone()).or_default().push(entry);
                    } else if let Some(tag) = &key.tag {
                        index.by_tag.entry(tag.clone()).or_default().push(entry);
                    } else {
                        index.universal.push(entry);
                    }
                }
            }
        }
        index
    }

    /// The rules matching `node`, each rule once with the specificity of
    /// its most specific matching selector. `order` numbers rules across
    /// sheets in the order they were given.
    pub fn matching(&self, document: &Document, sheets: &[&Stylesheet], node: NodeId) -> Vec<Matched> {
        self.matching_for(document, sheets, node, 0)
    }

    /// The rules of the element's `::before` (1) or `::after` (2).
    pub fn matching_pseudo(&self, document: &Document, sheets: &[&Stylesheet], node: NodeId, which: u8) -> Vec<Matched> {
        self.matching_for(document, sheets, node, which)
    }

    fn matching_for(&self, document: &Document, sheets: &[&Stylesheet], node: NodeId, which: u8) -> Vec<Matched> {
        let NodeData::Element { tag, attributes } = &document.nodes[node].data else {
            return Vec::new();
        };
        let mut candidates: Vec<(usize, usize, usize)> = Vec::new();
        if let Some((_, id)) = attributes.iter().find(|(key, _)| key == "id")
            && let Some(entries) = self.by_id.get(id)
        {
            candidates.extend(entries);
        }
        if let Some((_, classes)) = attributes.iter().find(|(key, _)| key == "class") {
            for class in classes.split_ascii_whitespace() {
                if let Some(entries) = self.by_class.get(class) {
                    candidates.extend(entries);
                }
            }
        }
        if let Some(entries) = self.by_tag.get(tag) {
            candidates.extend(entries);
        }
        candidates.extend(&self.universal);

        let mut matched: Vec<Matched> = Vec::new();
        for (sheet, rule, selector) in candidates {
            let selector = &sheets[sheet].rules[rule].selectors[selector];
            if selector.compounds.last().map(|last| last.pseudo_element) != Some(which) {
                continue;
            }
            if !matches(document, node, selector) {
                continue;
            }
            // Rules are ordered by sheet, then by position.
            let order = (sheet << 20) | rule;
            match matched.iter_mut().find(|(_, o, _, _)| *o == order) {
                Some(existing) => existing.0 = existing.0.max(selector.specificity),
                None => matched.push((selector.specificity, order, sheet, rule)),
            }
        }
        matched
    }
}

// ============================================================
//  Values
// ============================================================

/// A number in thousandths: CSS numbers are decimal, and the kernel has
/// no floating point.
pub fn parse_milli(text: &str) -> Option<i64> {
    let text = text.trim();
    let (negative, digits) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    let (whole, fraction) = digits.split_once('.').unwrap_or((digits, ""));
    if whole.is_empty() && fraction.is_empty() {
        return None;
    }
    let whole: i64 = if whole.is_empty() { 0 } else { whole.parse().ok()? };
    let mut milli = whole.checked_mul(1000)?;
    let mut scale = 100;
    for digit in fraction.chars().take(3) {
        milli += digit.to_digit(10)? as i64 * scale;
        scale /= 10;
    }
    Some(if negative { -milli } else { milli })
}

/// Splits a dimension into its number (thousandths) and unit.
pub fn split_unit(text: &str) -> Option<(i64, String)> {
    let text = text.trim();
    let end = text
        .char_indices()
        .find(|(index, c)| !(c.is_ascii_digit() || *c == '.' || ((*c == '-' || *c == '+') && *index == 0)))
        .map(|(index, _)| index)
        .unwrap_or(text.len());
    let number = parse_milli(&text[..end])?;
    Some((number, text[end..].trim().to_ascii_lowercase()))
}

/// A length in pixels. `em` is the element's font size, `rem` the root's,
/// `percent_of` what a percentage refers to, `viewport_height` for `vh`.
pub fn parse_length_px(text: &str, em: i32, rem: i32, percent_of: i32, viewport_height: i32) -> Option<i32> {
    length_px_at(text, em, rem, percent_of, viewport_height, 0)
}

/// `calc()` inside `calc()` recurses; four levels are plenty for real
/// sheets and bound what a hostile one can make the stack do.
fn length_px_at(text: &str, em: i32, rem: i32, percent_of: i32, viewport_height: i32, depth: usize) -> Option<i32> {
    if depth > 4 {
        return None;
    }
    let text = text.trim();
    let lower = text.to_ascii_lowercase();
    // calc(), min(), max(), clamp(): the first argument that parses, or
    // for clamp the preferred value -- an approximation, named as such.
    for function in ["calc(", "min(", "max(", "clamp(", "-webkit-calc("] {
        if let Some(inner) = lower.strip_prefix(function).and_then(|rest| rest.strip_suffix(')')) {
            let arguments = split_top_level(inner, ',');
            let chosen = if function == "clamp(" && arguments.len() == 3 { arguments[1] } else { arguments[0] };
            // Inside calc, the first term.
            let first = chosen.split([' ', '+']).find(|term| !term.trim().is_empty()).unwrap_or("0");
            return length_px_at(first, em, rem, percent_of, viewport_height, depth + 1);
        }
    }
    let (value, unit) = split_unit(&lower)?;
    let pixels = match unit.as_str() {
        "px" | "" => value,
        "em" => value * em as i64,
        "rem" => value * rem as i64,
        "%" => value * percent_of as i64 / 100,
        "pt" => value * 4 / 3,
        "pc" => value * 16,
        "in" => value * 96,
        "cm" => value * 3780 / 100,
        "mm" => value * 378 / 100,
        "vw" => value * percent_of as i64 / 100,
        "vh" => value * viewport_height as i64 / 100,
        "ch" | "ex" => value * 8,
        _ => return None,
    };
    Some((pixels / 1000).clamp(-100_000, 100_000) as i32)
}

const NAMED_COLORS: &[(&str, u32)] = &[
    ("black", 0x000000), ("white", 0xffffff), ("red", 0xff0000), ("green", 0x008000), ("blue", 0x0000ff),
    ("yellow", 0xffff00), ("orange", 0xffa500), ("purple", 0x800080), ("gray", 0x808080), ("grey", 0x808080),
    ("silver", 0xc0c0c0), ("maroon", 0x800000), ("navy", 0x000080), ("teal", 0x008080), ("olive", 0x808000),
    ("lime", 0x00ff00), ("aqua", 0x00ffff), ("cyan", 0x00ffff), ("fuchsia", 0xff00ff), ("magenta", 0xff00ff),
    ("brown", 0xa52a2a), ("pink", 0xffc0cb), ("gold", 0xffd700), ("beige", 0xf5f5dc), ("ivory", 0xfffff0),
    ("khaki", 0xf0e68c), ("coral", 0xff7f50), ("salmon", 0xfa8072), ("tomato", 0xff6347), ("crimson", 0xdc143c),
    ("indigo", 0x4b0082), ("violet", 0xee82ee), ("orchid", 0xda70d6), ("plum", 0xdda0dd), ("tan", 0xd2b48c),
    ("chocolate", 0xd2691e), ("sienna", 0xa0522d), ("peru", 0xcd853f), ("wheat", 0xf5deb3), ("linen", 0xfaf0e6),
    ("lavender", 0xe6e6fa), ("azure", 0xf0ffff), ("honeydew", 0xf0fff0), ("mintcream", 0xf5fffa),
    ("aliceblue", 0xf0f8ff), ("ghostwhite", 0xf8f8ff), ("whitesmoke", 0xf5f5f5), ("snow", 0xfffafa),
    ("seashell", 0xfff5ee), ("oldlace", 0xfdf5e6), ("floralwhite", 0xfffaf0), ("cornsilk", 0xfff8dc),
    ("lightgray", 0xd3d3d3), ("lightgrey", 0xd3d3d3), ("darkgray", 0xa9a9a9), ("darkgrey", 0xa9a9a9),
    ("dimgray", 0x696969), ("dimgrey", 0x696969), ("gainsboro", 0xdcdcdc), ("slategray", 0x708090),
    ("lightslategray", 0x778899), ("darkslategray", 0x2f4f4f), ("lightblue", 0xadd8e6), ("skyblue", 0x87ceeb),
    ("lightskyblue", 0x87cefa), ("deepskyblue", 0x00bfff), ("dodgerblue", 0x1e90ff), ("steelblue", 0x4682b4),
    ("royalblue", 0x4169e1), ("cornflowerblue", 0x6495ed), ("mediumblue", 0x0000cd), ("darkblue", 0x00008b),
    ("midnightblue", 0x191970), ("cadetblue", 0x5f9ea0), ("powderblue", 0xb0e0e6), ("lightcyan", 0xe0ffff),
    ("darkcyan", 0x008b8b), ("turquoise", 0x40e0d0), ("lightgreen", 0x90ee90), ("darkgreen", 0x006400),
    ("forestgreen", 0x228b22), ("seagreen", 0x2e8b57), ("limegreen", 0x32cd32), ("olivedrab", 0x6b8e23),
    ("yellowgreen", 0x9acd32), ("palegreen", 0x98fb98), ("springgreen", 0x00ff7f), ("lightyellow", 0xffffe0),
    ("lightpink", 0xffb6c1), ("hotpink", 0xff69b4), ("deeppink", 0xff1493), ("darkred", 0x8b0000),
    ("firebrick", 0xb22222), ("indianred", 0xcd5c5c), ("lightcoral", 0xf08080), ("darkorange", 0xff8c00),
    ("orangered", 0xff4500), ("goldenrod", 0xdaa520), ("darkgoldenrod", 0xb8860b), ("darkviolet", 0x9400d3),
    ("darkmagenta", 0x8b008b), ("rebeccapurple", 0x663399), ("mediumpurple", 0x9370db), ("thistle", 0xd8bfd8),
    ("lemonchiffon", 0xfffacd), ("papayawhip", 0xffefd5), ("moccasin", 0xffe4b5), ("peachpuff", 0xffdab9),
    ("mistyrose", 0xffe4e1), ("lavenderblush", 0xfff0f5), ("antiquewhite", 0xfaebd7), ("bisque", 0xffe4c4),
    ("blanchedalmond", 0xffebcd), ("burlywood", 0xdeb887), ("rosybrown", 0xbc8f8f), ("sandybrown", 0xf4a460),
    ("darkkhaki", 0xbdb76b), ("palegoldenrod", 0xeee8aa), ("aquamarine", 0x7fffd4), ("mediumseagreen", 0x3cb371),
    ("darkseagreen", 0x8fbc8f), ("lightseagreen", 0x20b2aa), ("darkturquoise", 0x00ced1), ("mediumturquoise", 0x48d1cc),
    ("paleturquoise", 0xafeeee), ("darkolivegreen", 0x556b2f), ("lawngreen", 0x7cfc00), ("chartreuse", 0x7fff00),
    ("greenyellow", 0xadff2f), ("mediumspringgreen", 0x00fa9a), ("darkslateblue", 0x483d8b), ("slateblue", 0x6a5acd),
    ("mediumslateblue", 0x7b68ee), ("blueviolet", 0x8a2be2), ("darkorchid", 0x9932cc), ("mediumorchid", 0xba55d3),
    ("palevioletred", 0xdb7093), ("mediumvioletred", 0xc71585), ("lightsalmon", 0xffa07a), ("darksalmon", 0xe9967a),
    ("navajowhite", 0xffdead), ("lightgoldenrodyellow", 0xfafad2), ("lightsteelblue", 0xb0c4de),
];

/// A colour value. `None` for `transparent` and for what is not a colour;
/// `Some(Err(()))` is never returned -- `currentcolor` is the caller's.
/// Alpha is approximated by blending towards `background` at parse time.
pub fn parse_color(text: &str, background: u32) -> Option<u32> {
    let text = text.trim().to_ascii_lowercase();
    if let Some(hex) = text.strip_prefix('#') {
        let digits: Vec<u32> = hex.chars().map(|c| c.to_digit(16)).collect::<Option<Vec<u32>>>()?;
        let (r, g, b, a) = match digits.len() {
            3 => (digits[0] * 17, digits[1] * 17, digits[2] * 17, 255),
            4 => (digits[0] * 17, digits[1] * 17, digits[2] * 17, digits[3] * 17),
            6 => (digits[0] * 16 + digits[1], digits[2] * 16 + digits[3], digits[4] * 16 + digits[5], 255),
            8 => (
                digits[0] * 16 + digits[1],
                digits[2] * 16 + digits[3],
                digits[4] * 16 + digits[5],
                digits[6] * 16 + digits[7],
            ),
            _ => return None,
        };
        return blend(r, g, b, a, background);
    }
    for (prefix, hsl) in [("rgba(", false), ("rgb(", false), ("hsla(", true), ("hsl(", true)] {
        if let Some(inner) = text.strip_prefix(prefix).and_then(|rest| rest.strip_suffix(')')) {
            let parts: Vec<&str> = inner
                .split(|c: char| c == ',' || c == '/' || c.is_whitespace())
                .filter(|part| !part.is_empty())
                .collect();
            if parts.len() < 3 {
                return None;
            }
            let alpha = parts
                .get(3)
                .and_then(|part| {
                    if let Some(percent) = part.strip_suffix('%') {
                        parse_milli(percent).map(|value| value * 255 / 100_000)
                    } else {
                        parse_milli(part).map(|value| value * 255 / 1000)
                    }
                })
                .unwrap_or(255)
                .clamp(0, 255) as u32;
            if hsl {
                let hue = parse_milli(parts[0].trim_end_matches("deg"))? / 1000;
                let saturation = parse_milli(parts[1].trim_end_matches('%'))? / 1000;
                let lightness = parse_milli(parts[2].trim_end_matches('%'))? / 1000;
                let (r, g, b) = hsl_to_rgb(hue, saturation, lightness);
                return blend(r, g, b, alpha, background);
            }
            let channel = |part: &str| -> Option<u32> {
                if let Some(percent) = part.strip_suffix('%') {
                    Some((parse_milli(percent)? * 255 / 100_000).clamp(0, 255) as u32)
                } else {
                    Some((parse_milli(part)? / 1000).clamp(0, 255) as u32)
                }
            };
            return blend(channel(parts[0])?, channel(parts[1])?, channel(parts[2])?, alpha, background);
        }
    }
    if text == "transparent" {
        return None;
    }
    NAMED_COLORS.iter().find(|(name, _)| *name == text).map(|(_, value)| *value)
}

fn blend(r: u32, g: u32, b: u32, alpha: u32, background: u32) -> Option<u32> {
    if alpha == 0 {
        return None;
    }
    let mix = |channel: u32, shift: u32| -> u32 {
        let under = (background >> shift) & 0xff;
        (channel * alpha + under * (255 - alpha)) / 255
    };
    Some((mix(r, 16) << 16) | (mix(g, 8) << 8) | mix(b, 0))
}

fn hsl_to_rgb(hue: i64, saturation: i64, lightness: i64) -> (u32, u32, u32) {
    // Integer HSL in thousandths.
    let h = hue.rem_euclid(360);
    let s = saturation.clamp(0, 100) * 10;
    let l = lightness.clamp(0, 100) * 10;
    let c = (1000 - (2 * l - 1000).abs()) * s / 1000;
    let h_prime = h * 1000 / 60;
    let x = c * (1000 - ((h_prime % 2000) - 1000).abs()) / 1000;
    let (r1, g1, b1) = match h / 60 {
        0 => (c, x, 0),
        1 => (x, c, 0),
        2 => (0, c, x),
        3 => (0, x, c),
        4 => (x, 0, c),
        _ => (c, 0, x),
    };
    let m = l - c / 2;
    let to = |value: i64| ((value + m) * 255 / 1000).clamp(0, 255) as u32;
    (to(r1), to(g1), to(b1))
}
