//! Cookies (RFC 6265).
//!
//! What a site stores in the browser and gets back with each request --
//! its session, a consent, a preference. The jar keeps them per domain
//! and path, drops them when they expire, sends `Secure` ones over HTTPS
//! only, and writes the persistent ones to disk (`save` / `load`) so a
//! session survives the browser being closed. Session cookies -- no
//! `Expires`, no `Max-Age` -- die with the browser, as the RFC says.
//!
//! ## Limits
//!
//! There is no public suffix list: a site under `example.co.uk` could set
//! a cookie for `co.uk`. Domains of one label (`com`) are refused, which
//! stops the common case and not that one. `SameSite` is recorded and
//! not enforced -- there is no script here to forge a cross-site request
//! from, which is what it defends against.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use super::url::Url;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    /// Sent to subdomains too (a `Domain` attribute was given).
    pub include_subdomains: bool,
    pub path: String,
    /// Seconds since 1970; zero for a session cookie.
    pub expires: u64,
    pub secure: bool,
    pub http_only: bool,
}

/// At most this many cookies, and this many per domain: a page must not
/// be able to fill the heap one `Set-Cookie` at a time.
const MAX_COOKIES: usize = 3000;
const MAX_PER_DOMAIN: usize = 60;

#[derive(Debug, Default, Clone)]
pub struct Jar {
    cookies: Vec<Cookie>,
}

fn default_path(url: &Url) -> String {
    match url.path.rfind('/') {
        Some(0) | None => String::from("/"),
        Some(index) => String::from(&url.path[..index]),
    }
}

fn domain_matches(host: &str, cookie: &Cookie) -> bool {
    if host == cookie.domain {
        return true;
    }
    cookie.include_subdomains && host.ends_with(&format!(".{}", cookie.domain))
}

fn path_matches(path: &str, cookie_path: &str) -> bool {
    path == cookie_path
        || (path.starts_with(cookie_path) && (cookie_path.ends_with('/') || path[cookie_path.len()..].starts_with('/')))
}

/// A date of `Expires`, in the forms servers actually send:
/// `Wed, 21 Oct 2015 07:28:00 GMT`, with `-` or spaces between the parts.
pub fn parse_http_date(text: &str) -> Option<u64> {
    let mut day = None;
    let mut month = None;
    let mut year = None;
    let mut time = None;
    for token in text.split(|c: char| c == ' ' || c == ',' || c == '-').filter(|t| !t.is_empty()) {
        if token.contains(':') {
            let mut parts = token.split(':');
            let hours: u64 = parts.next()?.parse().ok()?;
            let minutes: u64 = parts.next()?.parse().ok()?;
            let seconds: u64 = parts.next().unwrap_or("0").parse().ok()?;
            time = Some(hours * 3600 + minutes * 60 + seconds);
        } else if let Ok(number) = token.parse::<u64>() {
            if day.is_none() && number <= 31 && token.len() <= 2 {
                day = Some(number);
            } else {
                year = Some(if number < 70 { 2000 + number } else if number < 100 { 1900 + number } else { number });
            }
        } else {
            let lower = token.to_ascii_lowercase();
            let months = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
            if let Some(index) = months.iter().position(|name| lower.starts_with(name)) {
                month = Some(index as u64 + 1);
            }
        }
    }
    let days = super::der::days_from_civil(year?, month?, day?);
    Some(days * 86_400 + time.unwrap_or(0))
}

impl Jar {
    pub fn new() -> Jar {
        Jar { cookies: Vec::new() }
    }

    pub fn len(&self) -> usize {
        self.cookies.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cookies.is_empty()
    }

    /// Takes a `Set-Cookie` header received from `url`.
    pub fn store(&mut self, url: &Url, header: &str, now: u64) {
        let mut parts = header.split(';');
        let Some((name, value)) = parts.next().and_then(|pair| pair.split_once('=')) else {
            return;
        };
        let name = name.trim();
        if name.is_empty() || name.len() > 256 {
            return;
        }
        let mut cookie = Cookie {
            name: String::from(name),
            value: String::from(value.trim().trim_matches('"')),
            domain: url.host.clone(),
            include_subdomains: false,
            path: default_path(url),
            expires: 0,
            secure: false,
            http_only: false,
        };
        if cookie.value.len() > 4096 {
            return;
        }
        let mut max_age: Option<i64> = None;
        for attribute in parts {
            let (key, value) = match attribute.split_once('=') {
                Some((key, value)) => (key.trim().to_ascii_lowercase(), value.trim()),
                None => (attribute.trim().to_ascii_lowercase(), ""),
            };
            match key.as_str() {
                "domain" => {
                    let domain = value.trim_start_matches('.').to_ascii_lowercase();
                    // A cookie for another site, or for a whole top-level
                    // domain, is refused.
                    if domain.is_empty()
                        || !domain.contains('.')
                        || !(url.host == domain || url.host.ends_with(&format!(".{}", domain)))
                    {
                        return;
                    }
                    cookie.domain = domain;
                    cookie.include_subdomains = true;
                }
                "path" if value.starts_with('/') => cookie.path = String::from(value),
                "expires" => {
                    if max_age.is_none()
                        && let Some(date) = parse_http_date(value)
                    {
                        cookie.expires = date.max(1);
                    }
                }
                "max-age" => {
                    if let Ok(seconds) = value.parse::<i64>() {
                        max_age = Some(seconds);
                    }
                }
                "secure" => cookie.secure = true,
                "httponly" => cookie.http_only = true,
                _ => {}
            }
        }
        if let Some(seconds) = max_age {
            cookie.expires = if seconds <= 0 { 1 } else { now + seconds as u64 };
        }
        if cookie.secure && !url.is_secure() {
            return;
        }

        self.cookies.retain(|existing| {
            !(existing.name == cookie.name && existing.domain == cookie.domain && existing.path == cookie.path)
        });
        // An expiry in the past is how a site deletes a cookie.
        if cookie.expires != 0 && now != 0 && cookie.expires <= now {
            return;
        }
        let same_domain = self.cookies.iter().filter(|existing| existing.domain == cookie.domain).count();
        if same_domain >= MAX_PER_DOMAIN || self.cookies.len() >= MAX_COOKIES {
            if let Some(index) = self.cookies.iter().position(|existing| existing.domain == cookie.domain) {
                self.cookies.remove(index);
            } else if !self.cookies.is_empty() {
                self.cookies.remove(0);
            }
        }
        self.cookies.push(cookie);
    }

    /// The `Cookie` header for a request to `url`, if any cookie applies.
    pub fn header_for(&mut self, url: &Url, now: u64) -> Option<String> {
        if now != 0 {
            self.cookies.retain(|cookie| cookie.expires == 0 || cookie.expires > now);
        }
        let mut matching: Vec<&Cookie> = self
            .cookies
            .iter()
            .filter(|cookie| {
                domain_matches(&url.host, cookie)
                    && path_matches(&url.path, &cookie.path)
                    && (!cookie.secure || url.is_secure())
            })
            .collect();
        if matching.is_empty() {
            return None;
        }
        // Longer paths first, as the RFC asks.
        matching.sort_by(|a, b| b.path.len().cmp(&a.path.len()));
        let parts: Vec<String> = matching
            .iter()
            .map(|cookie| format!("{}={}", cookie.name, cookie.value))
            .collect();
        Some(parts.join("; "))
    }

    /// `document.cookie` read by a page at `url`: the cookies a request
    /// would carry, but never the `HttpOnly` ones -- that is what the
    /// attribute is for.
    pub fn for_script(&mut self, url: &Url, now: u64) -> String {
        if now != 0 {
            self.cookies.retain(|cookie| cookie.expires == 0 || cookie.expires > now);
        }
        let mut matching: Vec<&Cookie> = self
            .cookies
            .iter()
            .filter(|cookie| {
                !cookie.http_only
                    && domain_matches(&url.host, cookie)
                    && path_matches(&url.path, &cookie.path)
                    && (!cookie.secure || url.is_secure())
            })
            .collect();
        matching.sort_by(|a, b| b.path.len().cmp(&a.path.len()));
        let parts: Vec<String> = matching.iter().map(|cookie| format!("{}={}", cookie.name, cookie.value)).collect();
        parts.join("; ")
    }

    /// `document.cookie = line`: as a `Set-Cookie` header, except that a
    /// page cannot make an `HttpOnly` cookie, nor replace one.
    pub fn store_from_script(&mut self, url: &Url, line: &str, now: u64) {
        if line.split(';').skip(1).any(|attribute| attribute.trim().eq_ignore_ascii_case("httponly")) {
            return;
        }
        let name = line.split(';').next().and_then(|pair| pair.split_once('=')).map(|(n, _)| n.trim()).unwrap_or("");
        if self.cookies.iter().any(|c| c.http_only && c.name == name && domain_matches(&url.host, c)) {
            return;
        }
        self.store(url, line, now);
    }

    /// Forgets every cookie of a domain and its subdomains.
    pub fn clear_domain(&mut self, domain: &str) {
        self.cookies
            .retain(|cookie| cookie.domain != domain && !cookie.domain.ends_with(&format!(".{}", domain)));
    }

    pub fn clear(&mut self) {
        self.cookies.clear();
    }

    /// Drops the session cookies: the browser was closed.
    pub fn end_session(&mut self) {
        self.cookies.retain(|cookie| cookie.expires != 0);
    }

    /// The persistent cookies, one per line, tab-separated.
    pub fn save(&self) -> String {
        let mut out = String::from("# cookies du navigateur - domaine, sous-domaines, chemin, securise, http, expiration, nom, valeur\n");
        for cookie in self.cookies.iter().filter(|cookie| cookie.expires != 0) {
            out.push_str(&format!(
                "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                cookie.domain,
                u8::from(cookie.include_subdomains),
                cookie.path,
                u8::from(cookie.secure),
                u8::from(cookie.http_only),
                cookie.expires,
                cookie.name,
                cookie.value
            ));
        }
        out
    }

    pub fn load(text: &str, now: u64) -> Jar {
        let mut jar = Jar::new();
        for line in text.lines() {
            if line.starts_with('#') {
                continue;
            }
            let fields: Vec<&str> = line.split('\t').collect();
            if fields.len() != 8 {
                continue;
            }
            let Ok(expires) = fields[5].parse::<u64>() else {
                continue;
            };
            if now != 0 && expires <= now {
                continue;
            }
            jar.cookies.push(Cookie {
                domain: String::from(fields[0]),
                include_subdomains: fields[1] == "1",
                path: String::from(fields[2]),
                secure: fields[3] == "1",
                http_only: fields[4] == "1",
                expires,
                name: String::from(fields[6]),
                value: String::from(fields[7]),
            });
        }
        jar
    }

    /// (domain, count) per domain, for the settings page.
    pub fn domains(&self) -> Vec<(String, usize)> {
        let mut out: Vec<(String, usize)> = Vec::new();
        for cookie in &self.cookies {
            match out.iter_mut().find(|(domain, _)| *domain == cookie.domain) {
                Some((_, count)) => *count += 1,
                None => out.push((cookie.domain.clone(), 1)),
            }
        }
        out
    }
}
