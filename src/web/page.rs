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
//! No script runs. `<noscript>` content is therefore shown, as it would be
//! in a browser with scripts disabled -- which is what this is.

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

pub struct Loaded {
    pub url: Url,
    pub title: String,
    pub document: Document,
    /// CSS sources, in cascade order.
    pub sheets: Vec<String>,
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
    let sheets = document
        .elements("style")
        .iter()
        .map(|id| document.text_content_raw(*id))
        .collect();
    Loaded {
        title: document.title(),
        url,
        document,
        sheets,
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
    }

    Ok(Outcome::Page(Loaded {
        title: document.title(),
        url: response.url.clone(),
        document,
        sheets,
        security: response.security.clone(),
        status: response.status,
        from_cache: response.from_cache,
        tls: response.tls.clone(),
    }))
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

/// Lays a loaded page out at `width`.
pub fn render(loaded: &Loaded, width: i32, viewport_height: i32, headroom: &dyn Fn() -> usize) -> Laid {
    let sheets: Vec<css::Stylesheet> = loaded.sheets.iter().map(|source| css::parse(source, width)).collect();
    let styles = style::compute(&loaded.document, &sheets, (width, viewport_height));
    layout::layout(&loaded.document, &styles, width, viewport_height, headroom)
}
