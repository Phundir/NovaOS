//! URLs (a practical subset of the WHATWG URL standard).
//!
//! What a browser does with an address: parse what the user typed or a
//! page linked, resolve it against the page it came from, and write it
//! back in a normal form. Only `http` and `https` are fetched; `about:`
//! pages are the browser's own.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Url {
    pub scheme: String,
    pub host: String,
    pub port: u16,
    /// Always starts with `/`.
    pub path: String,
    /// Without the `?`, empty when absent.
    pub query: String,
    /// Without the `#`, empty when absent.
    pub fragment: String,
}

fn default_port(scheme: &str) -> u16 {
    match scheme {
        "https" => 443,
        _ => 80,
    }
}

/// Removes `.` and `..` segments.
fn normalize_path(path: &str) -> String {
    let mut segments: Vec<&str> = Vec::new();
    let trailing = path.ends_with('/') || path.ends_with("/.") || path.ends_with("/..");
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            other => segments.push(other),
        }
    }
    let mut out = String::from("/");
    out.push_str(&segments.join("/"));
    if trailing && !out.ends_with('/') {
        out.push('/');
    }
    out
}

impl Url {
    /// Parses an absolute URL.
    pub fn parse(text: &str) -> Option<Url> {
        let text = text.trim();
        let (scheme, rest) = text.split_once("://")?;
        let scheme = scheme.to_ascii_lowercase();
        if scheme != "http" && scheme != "https" {
            return None;
        }
        let (rest, fragment) = match rest.split_once('#') {
            Some((head, fragment)) => (head, fragment),
            None => (rest, ""),
        };
        let authority_end = rest.find(['/', '?']).unwrap_or(rest.len());
        let authority = &rest[..authority_end];
        let rest = &rest[authority_end..];
        // User information is dropped: a link that carries a password is
        // a trap more often than a convenience.
        let authority = authority.rsplit_once('@').map(|(_, host)| host).unwrap_or(authority);
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => {
                (host, port.parse().ok()?)
            }
            Some((host, "")) => (host, default_port(&scheme)),
            _ => (authority, default_port(&scheme)),
        };
        if host.is_empty() || host.bytes().any(|byte| byte <= b' ' || byte >= 0x7f) {
            return None;
        }
        let (path, query) = match rest.split_once('?') {
            Some((path, query)) => (path, query),
            None => (rest, ""),
        };
        Some(Url {
            host: host.to_ascii_lowercase(),
            port,
            path: normalize_path(if path.is_empty() { "/" } else { path }),
            query: String::from(query),
            fragment: String::from(fragment),
            scheme,
        })
    }

    /// Resolves a link found on this page.
    pub fn join(&self, link: &str) -> Option<Url> {
        let link = link.trim();
        if link.is_empty() {
            let mut same = self.clone();
            same.fragment.clear();
            return Some(same);
        }
        if let Some((scheme, _)) = link.split_once(':')
            && !scheme.is_empty()
            && scheme.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'-' || b == b'.')
            && !link.starts_with("//")
        {
            // Absolute, in some scheme.
            return Url::parse(link);
        }
        if let Some(rest) = link.strip_prefix("//") {
            return Url::parse(&format!("{}://{}", self.scheme, rest));
        }
        let mut url = self.clone();
        url.fragment.clear();
        if let Some(fragment) = link.strip_prefix('#') {
            url.fragment = String::from(fragment);
            return Some(url);
        }
        let (link, fragment) = match link.split_once('#') {
            Some((head, fragment)) => (head, fragment),
            None => (link, ""),
        };
        url.fragment = String::from(fragment);
        if let Some(query) = link.strip_prefix('?') {
            url.query = String::from(query);
            return Some(url);
        }
        let (path, query) = match link.split_once('?') {
            Some((path, query)) => (path, query),
            None => (link, ""),
        };
        url.query = String::from(query);
        if path.starts_with('/') {
            url.path = normalize_path(path);
        } else {
            let base = match self.path.rfind('/') {
                Some(index) => &self.path[..=index],
                None => "/",
            };
            url.path = normalize_path(&format!("{}{}", base, path));
        }
        Some(url)
    }

    /// `host` or `host:port` when the port is not the default.
    pub fn authority(&self) -> String {
        if self.port == default_port(&self.scheme) {
            self.host.clone()
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }

    /// Path and query, as the request line wants them.
    pub fn target(&self) -> String {
        if self.query.is_empty() {
            self.path.clone()
        } else {
            format!("{}?{}", self.path, self.query)
        }
    }

    pub fn is_secure(&self) -> bool {
        self.scheme == "https"
    }

    /// The URL without its fragment: what is fetched and cached.
    pub fn without_fragment(&self) -> String {
        format!("{}://{}{}", self.scheme, self.authority(), self.target())
    }
}

impl core::fmt::Display for Url {
    fn fmt(&self, formatter: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(formatter, "{}", self.without_fragment())?;
        if !self.fragment.is_empty() {
            write!(formatter, "#{}", self.fragment)?;
        }
        Ok(())
    }
}

/// Percent-encoding for a form field (`application/x-www-form-urlencoded`).
pub fn form_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'*' => out.push(byte as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{:02X}", byte)),
        }
    }
    out
}

/// Decodes `%XX` escapes (and `+` as a space when `plus` is set).
pub fn percent_decode(text: &str, plus: bool) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'%' && index + 2 < bytes.len() {
            let high = (bytes[index + 1] as char).to_digit(16);
            let low = (bytes[index + 2] as char).to_digit(16);
            if let (Some(high), Some(low)) = (high, low) {
                out.push((high * 16 + low) as u8);
                index += 3;
                continue;
            }
        }
        out.push(if plus && byte == b'+' { b' ' } else { byte });
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Percent-encodes what may not appear raw in a path or query: spaces,
/// controls, non-ASCII. Used on links as pages write them.
pub fn escape_unsafe(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte <= b' ' || byte >= 0x7f || byte == b'"' || byte == b'<' || byte == b'>' {
            out.push_str(&format!("%{:02X}", byte));
        } else {
            out.push(byte as char);
        }
    }
    out
}

/// What the address bar makes of what was typed: a URL, or a search.
pub fn from_input(text: &str, search_engine: &str) -> String {
    let text = text.trim();
    if text.starts_with("about:") {
        return text.to_string();
    }
    if text.contains("://") {
        return escape_unsafe(text);
    }
    // A single word with a dot and no space is taken for a site name.
    let looks_like_host = !text.contains(' ')
        && text.contains('.')
        && !text.ends_with('.')
        && text.split('/').next().is_some_and(|host| {
            host.split('.').all(|label| !label.is_empty())
                && host.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-' || b == b':')
        });
    if looks_like_host || text.starts_with("localhost") {
        return format!("https://{}", escape_unsafe(text));
    }
    format!("{}{}", search_engine, form_encode(text))
}
