//! HTTP/1.1 (RFC 9112) over TCP or TLS, with redirects, cookies and the
//! cache.
//!
//! ## One request per connection
//!
//! Every request says `Connection: close` and reads until the server
//! closes. Keeping connections alive would save a handshake per
//! stylesheet, and demands the framing of every response be exactly
//! right, including the ones servers get wrong; a page here fetches its
//! HTML and a few stylesheets, and the saving is not worth the class of
//! bug. Named in the module so it can be revisited.
//!
//! ## What the caller provides
//!
//! `Net` opens connections and gives time and randomness; `Store` holds
//! cookies and cache entries. Both are traits so that the kernel can put
//! locks behind them -- taken per call, never across the network -- and
//! the development machine can put plain values.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use super::cache::{self, Entry, Lookup};
use super::tls::{Failure, Session, Tls, Transport};
use super::x509::Trust;
use super::url::Url;

/// Largest body accepted, compressed or not. The kernel heap is shared by
/// every task and an allocation that fails there stops the machine: a page
/// is its body, its decoded text and its tree, three or four times this.
pub const MAX_BODY: usize = 4 * 1024 * 1024;

pub const USER_AGENT: &str = "Mozilla/5.0 (NovaOS; x86_64) NovaNavigateur/1.0";

/// What the kernel or the host provides.
pub trait Net {
    type Connection: Transport;
    /// A TCP connection to `host:port` (the name is resolved here).
    fn connect(&mut self, host: &str, port: u16) -> Result<Self::Connection, String>;
    /// Seconds since 1970, or zero when unknown.
    fn now(&self) -> u64;
    fn random(&mut self, buffer: &mut [u8]);
    /// Called while a transfer progresses: (bytes so far, bytes expected
    /// or zero). Returns `false` to abandon it.
    fn progress(&mut self, _received: usize, _expected: usize) -> bool {
        true
    }
    /// What the connections trust: the built-in anchors by default; the
    /// browser adds the user's authorities, distrusts and exceptions.
    fn trust(&mut self) -> Trust {
        Trust::builtin(&[])
    }
}

pub trait Store {
    fn cookie_header(&mut self, url: &Url) -> Option<String>;
    fn set_cookie(&mut self, url: &Url, header: &str);
    fn cached(&mut self, url: &str) -> Option<Entry>;
    fn cache(&mut self, entry: Entry);
    /// Drops what is cached for `url`.
    fn forget(&mut self, _url: &str) {}
}

/// The methods of RFC 9110, section 9.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Head,
    Post,
    Put,
    Patch,
    Delete,
    Options,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Head => "HEAD",
            Method::Post => "POST",
            Method::Put => "PUT",
            Method::Patch => "PATCH",
            Method::Delete => "DELETE",
            Method::Options => "OPTIONS",
        }
    }

    /// A method name, as a form or a script writes it. `None` for one
    /// that is not HTTP -- `CONNECT` and `TRACE` are refused on purpose:
    /// a page has no use for either.
    pub fn parse(name: &str) -> Option<Method> {
        match name.trim().to_ascii_uppercase().as_str() {
            "GET" => Some(Method::Get),
            "HEAD" => Some(Method::Head),
            "POST" => Some(Method::Post),
            "PUT" => Some(Method::Put),
            "PATCH" => Some(Method::Patch),
            "DELETE" => Some(Method::Delete),
            "OPTIONS" => Some(Method::Options),
            _ => None,
        }
    }

    /// Safe methods only read: they may be cached and replayed.
    pub fn is_safe(self) -> bool {
        matches!(self, Method::Get | Method::Head | Method::Options)
    }
}

#[derive(Debug, Clone)]
pub struct Request {
    pub method: Method,
    pub url: Url,
    /// Content type and body, for the methods that carry one.
    pub body: Option<(String, Vec<u8>)>,
    pub referer: Option<String>,
    /// Skip the cache's freshness (the reload button).
    pub reload: bool,
    /// Extra headers: those a script sets. The ones the client owns --
    /// `Host`, `Content-Length`, `Cookie`, `Connection` -- are refused
    /// there, as a browser refuses them to a page.
    pub headers: Vec<(String, String)>,
    /// Follow redirects (a script's `redirect: "manual"` does not).
    pub follow: bool,
}

/// Headers a page may not set (the "forbidden request headers" of Fetch).
const FORBIDDEN: &[&str] = &[
    "host",
    "content-length",
    "cookie",
    "cookie2",
    "connection",
    "keep-alive",
    "transfer-encoding",
    "upgrade",
    "te",
    "trailer",
    "via",
    "origin",
    "referer",
    "date",
    "expect",
    "accept-encoding",
];

impl Request {
    pub fn get(url: Url) -> Request {
        Request::new(Method::Get, url)
    }

    pub fn new(method: Method, url: Url) -> Request {
        Request {
            method,
            url,
            body: None,
            referer: None,
            reload: false,
            headers: Vec::new(),
            follow: true,
        }
    }

    /// Adds a header, unless it is one the client alone may set.
    pub fn header(&mut self, name: &str, value: &str) -> bool {
        let lower = name.trim().to_ascii_lowercase();
        let valid_name = !lower.is_empty() && lower.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_!#$%&'*+.^`|~".contains(&b));
        let valid_value = !value.bytes().any(|b| b == b'\r' || b == b'\n' || b == 0);
        if !valid_name || !valid_value || FORBIDDEN.contains(&lower.as_str()) || lower.starts_with("proxy-") || lower.starts_with("sec-") {
            return false;
        }
        self.headers.retain(|(existing, _)| !existing.eq_ignore_ascii_case(&lower));
        self.headers.push((lower, String::from(value)));
        true
    }
}

#[derive(Debug, Clone)]
pub struct Response {
    /// Where the content really came from, after redirects.
    pub url: Url,
    pub status: u16,
    pub reason: String,
    /// Lower-case names.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// Served from the cache without a request.
    pub from_cache: bool,
    /// A line on the security of the connection, for the page info.
    pub security: String,
    /// The TLS session, for the certificate viewer.
    pub tls: Option<Session>,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(key, _)| key == name).map(|(_, value)| value.as_str())
    }

    pub fn content_type(&self) -> String {
        self.header("content-type")
            .map(|value| value.split(';').next().unwrap_or("").trim().to_ascii_lowercase())
            .unwrap_or_default()
    }

    /// The `charset` parameter of the content type, lower-case.
    pub fn charset(&self) -> Option<String> {
        let value = self.header("content-type")?;
        value.split(';').skip(1).find_map(|parameter| {
            let (key, value) = parameter.split_once('=')?;
            (key.trim().eq_ignore_ascii_case("charset")).then(|| value.trim().trim_matches('"').to_ascii_lowercase())
        })
    }
}

/// A connection, plain or encrypted.
enum Stream<C: Transport> {
    Plain(C),
    Secure(Tls<C>),
}

impl<C: Transport> Stream<C> {
    fn write(&mut self, data: &[u8]) -> Result<(), String> {
        match self {
            Stream::Plain(connection) => connection.send(data).map_err(String::from),
            Stream::Secure(tls) => tls.write(data),
        }
    }

    fn read(&mut self) -> Result<Vec<u8>, String> {
        match self {
            Stream::Plain(connection) => connection.recv(32 * 1024).map_err(String::from),
            Stream::Secure(tls) => tls.read(32 * 1024),
        }
    }
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

/// Decodes a `chunked` body. `None` if it is truncated or malformed.
fn dechunk(data: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len());
    let mut offset = 0;
    loop {
        let line_end = offset + find_subsequence(data.get(offset..)?, b"\r\n")?;
        let line = core::str::from_utf8(&data[offset..line_end]).ok()?;
        let size_text = line.split(';').next()?.trim();
        let size = usize::from_str_radix(size_text, 16).ok()?;
        offset = line_end + 2;
        if size == 0 {
            return Some(out);
        }
        out.extend_from_slice(data.get(offset..offset + size)?);
        offset += size + 2;
        if out.len() > MAX_BODY {
            return None;
        }
    }
}

/// Is a chunked body complete? Checked as data arrives, since some
/// servers keep the connection open after the last chunk despite
/// `Connection: close`.
///
/// `offset` remembers where the next chunk header starts: scanning from
/// the beginning at every read made a large chunked body quadratic.
fn chunked_complete(data: &[u8], offset: &mut usize) -> bool {
    loop {
        let Some(relative) = data.get(*offset..).and_then(|rest| find_subsequence(rest, b"\r\n")) else {
            return false;
        };
        let line_end = *offset + relative;
        let Some(size) = core::str::from_utf8(&data[*offset..line_end])
            .ok()
            .and_then(|line| usize::from_str_radix(line.split(';').next().unwrap_or("").trim(), 16).ok())
        else {
            return false;
        };
        if size == 0 {
            return true;
        }
        let next = line_end + 2 + size + 2;
        if next > data.len() {
            return false;
        }
        *offset = next;
    }
}

/// Opens a connection to the server of `url`, through TLS for `https`.
/// Returns the stream, the security line, and the TLS session.
fn open_stream<N: Net>(net: &mut N, url: &Url) -> Result<(Stream<N::Connection>, String, Option<Session>), Failure> {
    let connection = net.connect(&url.host, url.port)?;
    let stream = if url.is_secure() {
        let now = net.now();
        let trust = net.trust();
        let mut random = |buffer: &mut [u8]| net.random(buffer);
        let tls = Tls::connect(connection, &url.host, now, &mut random, &trust).map_err(|mut failure| {
            failure.host = url.host.clone();
            failure
        })?;
        Stream::Secure(tls)
    } else {
        Stream::Plain(connection)
    };
    let (security, session) = match &stream {
        Stream::Secure(tls) => {
            let who = if tls.session.exception {
                format!("certificat de {} accepte par exception", tls.session.subject)
            } else {
                format!("certificat verifie pour {}", tls.session.subject)
            };
            (
                format!("connexion chiffree - {} - {} - {} - {}", tls.session.suite, tls.session.group, who, tls.session.revocation.text()),
                Some(tls.session.clone()),
            )
        }
        Stream::Plain(_) => (String::from("connexion NON chiffree (http)"), None),
    };
    Ok((stream, security, session))
}

/// Reads a response head: status, reason, headers (lower-case names),
/// and the bytes of the body already received with it.
fn read_head<C: Transport>(stream: &mut Stream<C>) -> Result<(u16, String, Vec<(String, String)>, Vec<u8>), Failure> {
    let mut data: Vec<u8> = Vec::new();
    let head_end = loop {
        if let Some(position) = find_subsequence(&data, b"\r\n\r\n") {
            break position;
        }
        if data.len() > 64 * 1024 {
            return Err(Failure::from("en-tetes de reponse trop longs"));
        }
        let chunk = stream.read()?;
        if chunk.is_empty() {
            return Err(Failure::from("le serveur a ferme la connexion sans repondre"));
        }
        data.extend_from_slice(&chunk);
    };
    let head_text = String::from_utf8_lossy(&data[..head_end]).into_owned();
    let mut lines = head_text.split("\r\n");
    let status_line = lines.next().unwrap_or("");
    let mut status_parts = status_line.splitn(3, ' ');
    let version = status_parts.next().unwrap_or("");
    if !version.starts_with("HTTP/1.") {
        return Err(Failure::from("reponse qui n'est pas du HTTP/1.x"));
    }
    let status: u16 = status_parts.next().and_then(|code| code.parse().ok()).ok_or(Failure::from("code de statut illisible"))?;
    let reason = status_parts.next().unwrap_or("").to_string();
    let mut headers: Vec<(String, String)> = Vec::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
        }
    }
    Ok((status, reason, headers, data[head_end + 4..].to_vec()))
}

/// One exchange: no redirect, no cache.
fn exchange<N: Net>(net: &mut N, request: &Request, cookie: Option<String>, validators: Option<&Entry>) -> Result<Response, Failure> {
    let url = &request.url;
    let (mut stream, security, session) = open_stream(net, url)?;

    let method = request.method.as_str();
    let mut head = format!(
        "{} {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: {}\r\nAccept: text/html,application/xhtml+xml,text/css,*/*;q=0.8\r\n\
         Accept-Language: fr-FR,fr;q=0.9,en;q=0.6\r\nAccept-Encoding: gzip, deflate\r\nConnection: close\r\n",
        method,
        url.target(),
        url.authority(),
        USER_AGENT
    );
    if let Some(referer) = &request.referer {
        head.push_str(&format!("Referer: {}\r\n", referer));
    }
    if let Some(cookie) = cookie {
        head.push_str(&format!("Cookie: {}\r\n", cookie));
    }
    if let Some(entry) = validators {
        if !entry.etag.is_empty() {
            head.push_str(&format!("If-None-Match: {}\r\n", entry.etag));
        }
        if !entry.last_modified.is_empty() {
            head.push_str(&format!("If-Modified-Since: {}\r\n", entry.last_modified));
        }
    }
    for (name, value) in &request.headers {
        head.push_str(&format!("{}: {}\r\n", name, value));
    }
    match &request.body {
        Some((content_type, body)) => {
            if !content_type.is_empty() && !request.headers.iter().any(|(name, _)| name == "content-type") {
                head.push_str(&format!("Content-Type: {}\r\n", content_type));
            }
            head.push_str(&format!("Content-Length: {}\r\n", body.len()));
        }
        // A method that usually carries a body says it carries none.
        None if matches!(request.method, Method::Post | Method::Put | Method::Patch) => {
            head.push_str("Content-Length: 0\r\n");
        }
        None => {}
    }
    head.push_str("\r\n");
    let mut out = head.into_bytes();
    if let Some((_, body)) = &request.body {
        out.extend_from_slice(body);
    }
    stream.write(&out)?;

    // --- the response head ---
    let (status, reason, headers, mut body) = read_head(&mut stream)?;
    let header = |name: &str| headers.iter().find(|(key, _)| key == name).map(|(_, value)| value.clone());

    // --- the body ---
    let chunked = header("transfer-encoding").is_some_and(|value| value.to_ascii_lowercase().contains("chunked"));
    let length: Option<usize> = header("content-length").and_then(|value| value.parse().ok());
    let mut chunk_offset = 0usize;
    let bodiless = status == 204 || status == 304 || (100..200).contains(&status) || request.method == Method::Head;
    if !bodiless {
        loop {
            if let Some(length) = length
                && !chunked
                && body.len() >= length
            {
                body.truncate(length);
                break;
            }
            if chunked && chunked_complete(&body, &mut chunk_offset) {
                break;
            }
            if body.len() > MAX_BODY {
                return Err(Failure::from("page trop volumineuse (plus de 4 Mio)"));
            }
            if !net.progress(body.len(), length.unwrap_or(0)) {
                return Err(Failure::from("chargement interrompu"));
            }
            let chunk = stream.read()?;
            if chunk.is_empty() {
                break;
            }
            body.extend_from_slice(&chunk);
        }
    } else {
        body.clear();
    }
    if let Stream::Secure(tls) = &mut stream {
        tls.close();
    }

    if chunked {
        body = dechunk(&body).ok_or(Failure::from("corps de reponse tronque"))?;
    }
    // A 304 repeats the `Content-Encoding` of the stored body and carries
    // none: decoding its empty body failed every revalidation.
    let encoding = if body.is_empty() { None } else { header("content-encoding") };
    match encoding.map(|value| value.to_ascii_lowercase()) {
        Some(encoding) if encoding.contains("gzip") => {
            let received = body.len();
            body = super::inflate::gunzip(&body, MAX_BODY).ok_or_else(|| {
                format!(
                    "decompression gzip impossible ({} octets recus, annonces {}, par blocs {})",
                    received,
                    length.map(|value| value.to_string()).unwrap_or_else(|| String::from("?")),
                    if chunked { "oui" } else { "non" }
                )
            })?;
        }
        Some(encoding) if encoding.contains("deflate") => {
            body = super::inflate::unzlib(&body, MAX_BODY).ok_or(Failure::from("decompression deflate impossible"))?;
        }
        Some(encoding) if encoding.contains("br") => {
            return Err(Failure::from("compression brotli non prise en charge"));
        }
        _ => {}
    }

    Ok(Response {
        url: url.clone(),
        status,
        reason,
        headers,
        body,
        from_cache: false,
        security,
        tls: session,
    })
}

/// Fetches a request, following redirects, sending and storing cookies,
/// and going through the cache.
pub fn fetch<N: Net, S: Store>(net: &mut N, store: &mut S, mut request: Request) -> Result<Response, Failure> {
    for _ in 0..10 {
        let key = request.url.without_fragment();
        let cached = if request.method == Method::Get { store.cached(&key) } else { None };
        // A request that changes something invalidates what the cache
        // holds for its address (RFC 9111, 4.4).
        if !request.method.is_safe() {
            store.forget(&key);
        }
        let now = net.now();
        let mut validators = None;
        if let Some(entry) = cached {
            match entry.lookup(now) {
                Lookup::Fresh(entry) if !request.reload => {
                    return Ok(Response {
                        url: request.url.clone(),
                        status: 200,
                        reason: String::from("OK (cache)"),
                        headers: alloc::vec![(String::from("content-type"), entry.content_type.clone())],
                        body: entry.body,
                        from_cache: true,
                        security: String::from("page servie depuis le cache"),
                        tls: None,
                    });
                }
                Lookup::Fresh(entry) | Lookup::Stale(entry) => validators = Some(entry),
                Lookup::Miss => {}
            }
        }

        let cookie = store.cookie_header(&request.url);
        let mut response = exchange(net, &request, cookie, validators.as_ref())?;
        for (name, value) in &response.headers {
            if name == "set-cookie" {
                store.set_cookie(&request.url, value);
            }
        }

        if response.status == 304
            && let Some(entry) = validators
        {
            let fresh_until = cache::policy(200, &response.headers, entry.body.len(), now).unwrap_or(entry.fresh_until);
            let refreshed = Entry {
                stored: now,
                fresh_until,
                ..entry
            };
            store.cache(refreshed.clone());
            response.status = 200;
            response.reason = String::from("OK (revalide)");
            response.body = refreshed.body;
            response.from_cache = true;
            if response.header("content-type").is_none() {
                response.headers.push((String::from("content-type"), refreshed.content_type));
            }
            return Ok(response);
        }

        if request.follow
            && matches!(response.status, 301 | 302 | 303 | 307 | 308)
            && let Some(location) = response.header("location")
        {
            let next = request.url.join(location).ok_or(Failure::from("redirection vers une adresse invalide"))?;
            let referer = request.url.without_fragment();
            // 303 turns anything but HEAD into a GET without a body, and
            // 301/302 do the same to a POST in practice; 307 and 308 keep
            // the method and the body (RFC 9110, 15.4).
            let to_get = (response.status == 303 && request.method != Method::Head)
                || (request.method == Method::Post && matches!(response.status, 301 | 302));
            if to_get {
                request.method = Method::Get;
                request.body = None;
                request.headers.retain(|(name, _)| !name.starts_with("content-"));
            }
            // Credentials a script set for one origin do not follow it to
            // another.
            if next.host != request.url.host {
                request.headers.retain(|(name, _)| name != "authorization");
            }
            request.url = next;
            request.referer = Some(referer);
            continue;
        }

        if request.method == Method::Get
            && let Some(fresh_until) = cache::policy(response.status, &response.headers, response.body.len(), now)
        {
            store.cache(Entry {
                url: key,
                content_type: response.header("content-type").unwrap_or("").to_string(),
                etag: response.header("etag").unwrap_or("").to_string(),
                last_modified: response.header("last-modified").unwrap_or("").to_string(),
                stored: now,
                fresh_until,
                body: response.body.clone(),
            });
        }
        return Ok(response);
    }
    Err(Failure::from("trop de redirections"))
}

// ============================================================
//  Streamed download (phase 15)
// ============================================================

/// Where a streamed body goes, piece by piece.
///
/// `begin` is called once the final response is known (after the
/// redirects), with the offset the body starts at: the `resume` asked
/// for when the server honoured the range (206), zero when it sent the
/// whole file again (200) -- the sink must then start over.
pub trait Sink {
    fn begin(&mut self, start: u64, total: Option<u64>) -> Result<(), String>;
    fn write(&mut self, data: &[u8]) -> Result<(), String>;
}

/// What a download did.
#[derive(Debug, Clone)]
pub struct Download {
    /// The address the body came from, after the redirects.
    pub url: Url,
    pub status: u16,
    /// Where the body started (non-zero when resumed).
    pub start: u64,
    /// Bytes written to the sink by this call.
    pub received: u64,
    pub security: String,
}

/// Fetches `url` into `sink` without holding the body in memory: the
/// way to bring a package of any size onto the disk (`MAX_BODY` bounds
/// what the browser keeps in the heap, not this).
///
/// - redirects followed (GitHub release links redirect twice);
/// - `resume`: when non-zero, asks for `Range: bytes={resume}-`, so a
///   transfer cut halfway goes on where it stopped;
/// - no content coding is accepted (`identity`): a package is already
///   compressed, and decoding it in a stream would be one more parser;
/// - `limit` bounds the total size, announced or received.
pub fn download<N: Net, K: Sink>(net: &mut N, url: Url, resume: u64, limit: u64, sink: &mut K) -> Result<Download, Failure> {
    let mut url = url;
    for _ in 0..10 {
        let (mut stream, security, _) = open_stream(net, &url)?;
        let mut head = format!(
            "GET {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: {}\r\nAccept: */*\r\nAccept-Encoding: identity\r\nConnection: close\r\n",
            url.target(),
            url.authority(),
            USER_AGENT
        );
        if resume > 0 {
            head.push_str(&format!("Range: bytes={}-\r\n", resume));
        }
        head.push_str("\r\n");
        stream.write(head.as_bytes())?;

        let (status, _reason, headers, first) = read_head(&mut stream)?;
        let header = |name: &str| headers.iter().find(|(key, _)| key == name).map(|(_, value)| value.as_str());

        if matches!(status, 301 | 302 | 303 | 307 | 308) {
            let location = header("location").ok_or(Failure::from("redirection sans adresse"))?;
            url = url.join(location).ok_or(Failure::from("redirection vers une adresse invalide"))?;
            continue;
        }
        let start = match status {
            200 => 0,
            206 if resume > 0 => {
                // `Content-Range: bytes 1000-1999/2000`: the start must be
                // the one asked for, or the pieces would not join.
                let begins = header("content-range")
                    .and_then(|value| value.strip_prefix("bytes "))
                    .and_then(|value| value.split('-').next())
                    .and_then(|value| value.trim().parse::<u64>().ok());
                if begins != Some(resume) {
                    return Err(Failure::from("reprise refusee : plage inattendue"));
                }
                resume
            }
            // The range starts at or beyond the end: nothing is missing.
            416 if resume > 0 => {
                if let Stream::Secure(tls) = &mut stream {
                    tls.close();
                }
                return Ok(Download { url, status, start: resume, received: 0, security });
            }
            _ => return Err(Failure::from(format!("le serveur repond {}", status))),
        };
        if header("content-encoding").is_some_and(|value| !value.eq_ignore_ascii_case("identity")) {
            return Err(Failure::from("codage de contenu non demande"));
        }
        let chunked = header("transfer-encoding").is_some_and(|value| value.to_ascii_lowercase().contains("chunked"));
        let length: Option<u64> = if chunked { None } else { header("content-length").and_then(|value| value.parse().ok()) };
        if length.is_some_and(|length| start + length > limit) {
            return Err(Failure::from("fichier plus grand que la limite"));
        }
        sink.begin(start, length.map(|length| start + length))?;

        let mut decoder = Dechunker::new();
        let mut received = 0u64;
        let mut pending = first;
        loop {
            let piece: Vec<u8> = if chunked {
                let mut out = Vec::new();
                decoder.feed(&pending, &mut out).map_err(Failure::from)?;
                out
            } else if let Some(length) = length {
                let room = (length - received.min(length)) as usize;
                pending.truncate(room);
                core::mem::take(&mut pending)
            } else {
                core::mem::take(&mut pending)
            };
            if !piece.is_empty() {
                if start + received + piece.len() as u64 > limit {
                    return Err(Failure::from("fichier plus grand que la limite"));
                }
                sink.write(&piece)?;
                received += piece.len() as u64;
            }
            let complete = if chunked { decoder.done() } else { length.is_some_and(|length| received >= length) };
            if complete {
                break;
            }
            if !net.progress(received as usize, length.unwrap_or(0) as usize) {
                return Err(Failure::from("telechargement interrompu"));
            }
            pending = stream.read()?;
            if pending.is_empty() {
                // A close before the announced end is a cut: the caller
                // resumes from what it has.
                if chunked || length.is_some() {
                    return Err(Failure::from("connexion coupee avant la fin"));
                }
                break;
            }
        }
        if let Stream::Secure(tls) = &mut stream {
            tls.close();
        }
        return Ok(Download { url, status, start, received, security });
    }
    Err(Failure::from("trop de redirections"))
}

/// Incremental decoder of `Transfer-Encoding: chunked` (RFC 9112, 7.1):
/// fed whatever the network hands over, it keeps its place between calls
/// -- a chunk header split across two reads included.
pub struct Dechunker {
    state: ChunkState,
    line: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChunkState {
    /// Reading a size line.
    Size,
    /// Inside a chunk, this many bytes left.
    Data(u64),
    /// The CRLF after a chunk's data.
    DataEnd,
    /// Trailer lines after the last chunk, until an empty one.
    Trailer,
    Done,
}

impl Default for Dechunker {
    fn default() -> Self {
        Self::new()
    }
}

impl Dechunker {
    pub fn new() -> Dechunker {
        Dechunker { state: ChunkState::Size, line: Vec::new() }
    }

    pub fn done(&self) -> bool {
        self.state == ChunkState::Done
    }

    /// Decodes `input`, appending the data to `out`. Bytes after the end
    /// of the body are ignored.
    pub fn feed(&mut self, input: &[u8], out: &mut Vec<u8>) -> Result<(), &'static str> {
        let mut at = 0;
        while at < input.len() && self.state != ChunkState::Done {
            match self.state {
                ChunkState::Data(left) => {
                    let take = (left.min((input.len() - at) as u64)) as usize;
                    out.extend_from_slice(&input[at..at + take]);
                    at += take;
                    let left = left - take as u64;
                    self.state = if left == 0 { ChunkState::DataEnd } else { ChunkState::Data(left) };
                }
                ChunkState::Size | ChunkState::DataEnd | ChunkState::Trailer => {
                    let byte = input[at];
                    at += 1;
                    if byte != b'\n' {
                        if self.line.len() > 1024 {
                            return Err("ligne de bloc trop longue");
                        }
                        self.line.push(byte);
                        continue;
                    }
                    if self.line.last() == Some(&b'\r') {
                        self.line.pop();
                    }
                    let line = core::mem::take(&mut self.line);
                    self.state = match self.state {
                        ChunkState::DataEnd if line.is_empty() => ChunkState::Size,
                        ChunkState::DataEnd => return Err("bloc mal termine"),
                        ChunkState::Trailer if line.is_empty() => ChunkState::Done,
                        ChunkState::Trailer => ChunkState::Trailer,
                        _ => {
                            let text = core::str::from_utf8(&line).map_err(|_| "taille de bloc illisible")?;
                            let size_text = text.split(';').next().unwrap_or("").trim();
                            let size = u64::from_str_radix(size_text, 16).map_err(|_| "taille de bloc illisible")?;
                            if size == 0 { ChunkState::Trailer } else { ChunkState::Data(size) }
                        }
                    };
                }
                ChunkState::Done => {}
            }
        }
        Ok(())
    }
}
