//! The HTTP cache (RFC 9111, the parts a single-user browser needs).
//!
//! A response is kept when it may be: `GET`, status 200, not
//! `no-store`, not too large. It is **fresh** for its `max-age` -- or
//! until its `Expires` -- and served with no request at all. Once stale,
//! it is **revalidated**: the request carries `If-None-Match` or
//! `If-Modified-Since`, and a `304` gives the stored body back. That is
//! what makes the back button and a stylesheet shared by every page of a
//! site cost nothing.
//!
//! This module is the policy and the encoding. Where the entries live --
//! memory, then the account's `cache/` directory -- is the kernel's side
//! (`apps::browser`), so this stays checkable on the development machine.

use alloc::string::String;
use alloc::vec::Vec;

/// Largest body kept.
pub const MAX_ENTRY: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub url: String,
    pub content_type: String,
    pub etag: String,
    pub last_modified: String,
    /// When it was stored, seconds since 1970.
    pub stored: u64,
    /// Until when it is fresh; zero when it must always be revalidated.
    pub fresh_until: u64,
    pub body: Vec<u8>,
}

/// What the cache says about a request.
pub enum Lookup {
    /// Serve as is.
    Fresh(Entry),
    /// Ask the server whether it changed.
    Stale(Entry),
    Miss,
}

/// The `Cache-Control` directives this cache follows.
#[derive(Debug, Default, Clone, Copy)]
pub struct Control {
    pub no_store: bool,
    pub no_cache: bool,
    pub max_age: Option<u64>,
}

pub fn parse_control(value: &str) -> Control {
    let mut control = Control::default();
    for directive in value.split(',') {
        let directive = directive.trim().to_ascii_lowercase();
        if directive == "no-store" {
            control.no_store = true;
        } else if directive == "no-cache" {
            control.no_cache = true;
        } else if let Some(age) = directive.strip_prefix("max-age=") {
            control.max_age = age.trim_matches('"').parse().ok();
        } else if let Some(age) = directive.strip_prefix("s-maxage=") {
            // For shared caches; a private cache ignores it.
            let _ = age;
        }
    }
    control
}

/// Decides, for a fresh response, whether and how long it may be kept.
///
/// `headers` are the response headers with lower-case names. Returns the
/// `fresh_until` date, or `None` if the response must not be stored.
pub fn policy(status: u16, headers: &[(String, String)], body_len: usize, now: u64) -> Option<u64> {
    if status != 200 || body_len > MAX_ENTRY {
        return None;
    }
    let get = |name: &str| headers.iter().find(|(key, _)| key == name).map(|(_, value)| value.as_str());
    let control = parse_control(get("cache-control").unwrap_or(""));
    if control.no_store {
        return None;
    }
    let has_validator = get("etag").is_some() || get("last-modified").is_some();
    if control.no_cache {
        return has_validator.then_some(0);
    }
    if let Some(age) = control.max_age {
        return Some(now + age);
    }
    if let Some(expires) = get("expires").and_then(super::cookies::parse_http_date) {
        return Some(expires);
    }
    // No freshness given: kept only if it can be revalidated, and then
    // revalidated every time.
    has_validator.then_some(0)
}

impl Entry {
    pub fn lookup(self, now: u64) -> Lookup {
        if now != 0 && self.fresh_until > now {
            Lookup::Fresh(self)
        } else {
            Lookup::Stale(self)
        }
    }

    /// A compact binary form, for the disk.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.body.len() + 256);
        out.extend_from_slice(b"NVC1");
        for text in [&self.url, &self.content_type, &self.etag, &self.last_modified] {
            out.extend_from_slice(&(text.len() as u32).to_le_bytes());
            out.extend_from_slice(text.as_bytes());
        }
        out.extend_from_slice(&self.stored.to_le_bytes());
        out.extend_from_slice(&self.fresh_until.to_le_bytes());
        out.extend_from_slice(&(self.body.len() as u32).to_le_bytes());
        out.extend_from_slice(&self.body);
        out
    }

    pub fn decode(data: &[u8]) -> Option<Entry> {
        if data.get(..4)? != b"NVC1" {
            return None;
        }
        let mut offset = 4;
        let mut read = |count: usize| -> Option<&[u8]> {
            let slice = data.get(offset..offset + count)?;
            offset += count;
            Some(slice)
        };
        let mut texts: Vec<String> = Vec::new();
        for _ in 0..4 {
            let length = u32::from_le_bytes(read(4)?.try_into().ok()?) as usize;
            texts.push(String::from_utf8(read(length)?.to_vec()).ok()?);
        }
        let stored = u64::from_le_bytes(read(8)?.try_into().ok()?);
        let fresh_until = u64::from_le_bytes(read(8)?.try_into().ok()?);
        let length = u32::from_le_bytes(read(4)?.try_into().ok()?) as usize;
        let body = read(length)?.to_vec();
        let last_modified = texts.pop()?;
        let etag = texts.pop()?;
        let content_type = texts.pop()?;
        let url = texts.pop()?;
        Some(Entry {
            url,
            content_type,
            etag,
            last_modified,
            stored,
            fresh_until,
            body,
        })
    }
}

/// The in-memory part: the most recently used entries, bounded in bytes.
#[derive(Debug, Default)]
pub struct Memory {
    entries: Vec<Entry>,
    bytes: usize,
    limit: usize,
}

impl Memory {
    pub fn new(limit: usize) -> Memory {
        Memory {
            entries: Vec::new(),
            bytes: 0,
            limit,
        }
    }

    pub fn get(&mut self, url: &str) -> Option<Entry> {
        let index = self.entries.iter().position(|entry| entry.url == url)?;
        // Most recent at the end.
        let entry = self.entries.remove(index);
        self.entries.push(entry.clone());
        Some(entry)
    }

    pub fn put(&mut self, entry: Entry) {
        if let Some(index) = self.entries.iter().position(|existing| existing.url == entry.url) {
            let old = self.entries.remove(index);
            self.bytes -= old.body.len();
        }
        self.bytes += entry.body.len();
        self.entries.push(entry);
        while self.bytes > self.limit && !self.entries.is_empty() {
            let old = self.entries.remove(0);
            self.bytes -= old.body.len();
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }

    pub fn usage(&self) -> (usize, usize) {
        (self.entries.len(), self.bytes)
    }
}

/// A short, stable file name for a URL: FNV-1a in hex.
pub fn file_name(url: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in url.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    alloc::format!("{:016x}.cache", hash)
}
