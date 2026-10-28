//! A page: fetched, decoded, parsed, styled, laid out.
//!
//! The steps a browser takes between an address and pixels, in one place:
//! the HTML is fetched, decoded in its charset, parsed; its style sheets
//! -- `<style>` blocks and `<link rel="stylesheet">` files, with their
//! `@import`s -- are fetched too; then the cascade and the layout run at
//! the width of the window. The sheets are kept as text, because
//! `@media` depends on the width: a window resized is laid out again from
//! them.
//!
//! The scripts are gathered here too -- inline ones as written, external
//! ones fetched with the sheets, by the loading task -- and handed over
//! in document order; they run in the browser's task (`web::js`), which
//! owns the page once it has arrived. `<noscript>` is still shown: the
//! engine covers a part of what pages expect, and a page that falls back
//! on it says more than one that silently half works.

use alloc::string::String;
use alloc::vec::Vec;

use super::css;
use super::html::{self, Document};
use super::http::{self, Method, Net, Request, Response, Store};
use super::tls::{Failure, Session};
use super::layout::{self, Laid};
use super::style;
use super::text;
use super::url::Url;

/// Style sheets fetched per page at most, and their size.
pub const MAX_SHEETS: usize = 16;
pub const MAX_SHEET_BYTES: usize = 1024 * 1024;
/// External scripts fetched per page at most, and their size.
pub const MAX_SCRIPTS: usize = 32;
pub const MAX_SCRIPT_BYTES: usize = 2 * 1024 * 1024;

pub struct Loaded {
    pub url: Url,
    pub title: String,
    pub document: Document,
    /// CSS sources, in cascade order.
    pub sheets: Vec<String>,
    /// The `<style>` or `<link>` element each sheet came from.
    pub sheet_nodes: Vec<html::NodeId>,
    /// JavaScript sources, in document order.
    pub scripts: Vec<String>,
    /// Scripts left out, and why (modules, sources too large): for the
    /// page's console.
    pub skipped_scripts: Vec<String>,
    pub security: String,
    pub status: u16,
    pub from_cache: bool,
    pub tls: Option<Session>,
}

pub enum Outcome {
    Page(Loaded),
    /// Not a page: a file to save.
    Download(Response),
}

/// Is this content type shown as a page?
fn displayable(content_type: &str) -> bool {
    content_type.is_empty()
        || content_type.starts_with("text/")
        || content_type == "application/xhtml+xml"
        || content_type == "application/xml"
        || content_type == "application/json"
}

/// Escapes text for inclusion in HTML.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(character),
        }
    }
    out
}

/// Builds a page from HTML already in hand (the browser's own pages).
pub fn from_html(url: Url, source: &str) -> Loaded {
    let document = html::parse(source);
    let sheet_nodes = document.elements("style");
    let sheets = sheet_nodes.iter().map(|id| document.text_content_raw(*id)).collect();
    Loaded {
        sheet_nodes,
        title: document.title(),
        url,
        document,
        sheets,
        scripts: Vec::new(),
        skipped_scripts: Vec::new(),
        security: String::from("page interne du navigateur"),
        status: 200,
        from_cache: false,
        tls: None,
    }
}

/// Fetches and parses a page, with its style sheets.
pub fn load<N: Net, S: Store>(net: &mut N, store: &mut S, request: Request, width: i32) -> Result<Outcome, Failure> {
    let response = http::fetch(net, store, request)?;
    let content_type = response.content_type();
    if !displayable(&content_type) {
        return Ok(Outcome::Download(response));
    }
    let charset = response.charset().or_else(|| text::sniff_charset(&response.body));
    let source = text::decode(&response.body, charset.as_deref());

    let is_html = content_type.is_empty() || content_type.contains("html") || content_type.contains("xml");
    let source = if is_html {
        source
    } else {
        alloc::format!("<pre>{}</pre>", escape(&source))
    };
    let document = html::parse(&source);
    let base = document
        .base()
        .and_then(|href| response.url.join(&href))
        .unwrap_or_else(|| response.url.clone());

    // Style sheets, in document order: `<style>` and `<link>` interleave
    // in the cascade exactly as they appear.
    let mut sheets: Vec<String> = Vec::new();
    let mut sheet_nodes: Vec<html::NodeId> = Vec::new();
    let mut fetched = 0;
    for id in 0..document.nodes.len() {
        match document.tag(id) {
            Some("style") => {
                let media = document.attribute(id, "media").unwrap_or("");
                if css::media_matches(media, width) {
                    let content = document.text_content_raw(id);
                    fetched += import_into(net, store, &base, &content, &mut sheets, fetched);
                    sheets.push(content);
                }
            }
            Some("link") => {
                let rel = document.attribute(id, "rel").unwrap_or("").to_ascii_lowercase();
                if !rel.split_whitespace().any(|word| word == "stylesheet") || rel.contains("alternate") {
                    continue;
                }
                let media = document.attribute(id, "media").unwrap_or("");
                if !css::media_matches(media, width) || fetched >= MAX_SHEETS {
                    continue;
                }
                let Some(href) = document.attribute(id, "href") else {
                    continue;
                };
                let Some(url) = base.join(href) else {
                    continue;
                };
                fetched += 1;
                if let Some(content) = fetch_sheet(net, store, url.clone()) {
                    fetched += import_into(net, store, &url, &content, &mut sheets, fetched);
                    sheets.push(content);
                }
            }
            _ => {}
        }
        sheet_nodes.resize(sheets.len(), id);
    }

    let (scripts, skipped_scripts) = gather_scripts(net, store, &document, &base);

    Ok(Outcome::Page(Loaded {
        title: document.title(),
        url: response.url.clone(),
        document,
        sheets,
        sheet_nodes,
        scripts,
        skipped_scripts,
        security: response.security.clone(),
        status: response.status,
        from_cache: response.from_cache,
        tls: response.tls.clone(),
    }))
}

/// Is this `type` of `<script>` JavaScript? The empty type and the
/// historical names are; `module`, JSON and templates are not run.
fn is_classic_script(kind: &str) -> bool {
    let kind = kind.trim().to_ascii_lowercase();
    kind.is_empty()
        || matches!(
            kind.as_str(),
            "text/javascript" | "application/javascript" | "application/x-javascript" | "text/ecmascript" | "application/ecmascript" | "text/jscript"
        )
}

/// The page's scripts, in document order: inline text, or the fetched
/// file of `src`. Returns them, and what was left out.
fn gather_scripts<N: Net, S: Store>(net: &mut N, store: &mut S, document: &Document, base: &Url) -> (Vec<String>, Vec<String>) {
    let mut scripts = Vec::new();
    let mut skipped = Vec::new();
    let mut fetched = 0;
    for id in document.elements("script") {
        let kind = document.attribute(id, "type").unwrap_or("");
        if !is_classic_script(kind) {
            if kind.eq_ignore_ascii_case("module") {
                skipped.push(String::from("script de type module ignore (modules ES non pris en charge)"));
            }
            continue;
        }
        match document.attribute(id, "src") {
            None => scripts.push(document.text_content_raw(id)),
            Some(src) => {
                let Some(url) = base.join(src) else {
                    continue;
                };
                if fetched >= MAX_SCRIPTS {
                    skipped.push(alloc::format!("{} : trop de scripts", url));
                    continue;
                }
                fetched += 1;
                match fetch_text(net, store, url.clone(), MAX_SCRIPT_BYTES) {
                    Some(source) => scripts.push(source),
                    None => skipped.push(alloc::format!("{} : script non charge", url)),
                }
            }
        }
    }
    (scripts, skipped)
}

/// A text file, if it comes back 200 and within `limit` bytes.
fn fetch_text<N: Net, S: Store>(net: &mut N, store: &mut S, url: Url, limit: usize) -> Option<String> {
    let request = Request::new(Method::Get, url);
    let response = http::fetch(net, store, request).ok()?;
    if response.status != 200 || response.body.len() > limit {
        return None;
    }
    Some(text::decode(&response.body, response.charset().as_deref()))
}

fn fetch_sheet<N: Net, S: Store>(net: &mut N, store: &mut S, url: Url) -> Option<String> {
    let request = Request::new(Method::Get, url);
    let response = http::fetch(net, store, request).ok()?;
    if response.status != 200 || response.body.len() > MAX_SHEET_BYTES {
        return None;
    }
    Some(text::decode(&response.body, response.charset().as_deref()))
}

/// Fetches the `@import`s of a sheet (one level), before the sheet
/// itself in the cascade, as CSS orders them. Returns how many were
/// fetched.
fn import_into<N: Net, S: Store>(net: &mut N, store: &mut S, base: &Url, content: &str, sheets: &mut Vec<String>, already: usize) -> usize {
    let imports = css::parse(content, 1024).imports;
    let mut count = 0;
    for import in imports {
        if already + count >= MAX_SHEETS {
            break;
        }
        if let Some(url) = base.join(&import) {
            count += 1;
            if let Some(imported) = fetch_sheet(net, store, url) {
                sheets.push(imported);
            }
        }
    }
    count
}

/// The sheets of a page whose scripts changed it: in tree order, a
/// `<style>`'s current text, and what a `<link>` brought at loading.
/// A `<style>` added by a script counts; a removed one no longer does.
pub fn current_sheets(loaded: &Loaded) -> Vec<String> {
    let mut out = Vec::new();
    for node in loaded.document.preorder(0) {
        match loaded.document.tag(node) {
            Some("style") => {
                // Its `@import`s, fetched at loading, then its text now.
                let own: Vec<usize> = (0..loaded.sheets.len()).filter(|i| loaded.sheet_nodes.get(*i) == Some(&node)).collect();
                for index in own.iter().take(own.len().saturating_sub(1)) {
                    out.push(loaded.sheets[*index].clone());
                }
                out.push(loaded.document.text_content_raw(node));
            }
            Some("link") => {
                for (index, origin) in loaded.sheet_nodes.iter().enumerate() {
                    if *origin == node {
                        out.push(loaded.sheets[index].clone());
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Lays out a page its scripts have changed.
pub fn render_live(loaded: &Loaded, width: i32, viewport_height: i32, headroom: &dyn Fn() -> usize) -> Laid {
    let sheets: Vec<css::Stylesheet> = current_sheets(loaded).iter().map(|source| css::parse(source, width)).collect();
    let styles = style::compute(&loaded.document, &sheets, (width, viewport_height));
    layout::layout(&loaded.document, &styles, width, viewport_height, headroom)
}

/// Lays a loaded page out at `width`.
pub fn render(loaded: &Loaded, width: i32, viewport_height: i32, headroom: &dyn Fn() -> usize) -> Laid {
    let sheets: Vec<css::Stylesheet> = loaded.sheets.iter().map(|source| css::parse(source, width)).collect();
    let styles = style::compute(&loaded.document, &sheets, (width, viewport_height));
    layout::layout(&loaded.document, &styles, width, viewport_height, headroom)
}
