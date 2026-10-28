//! Fetching for SPM: `file:`, `http://` and `https://`, into memory or
//! straight onto the disk (phase 15.0.4).
//!
//! ## Why a second client next to the browser's
//!
//! The browser loads pages: it caches, keeps cookies, decompresses, and
//! holds a body of at most `web::http::MAX_BODY` (4 MiB) in the heap. A
//! package manager wants none of that and one thing more: a file of any
//! size written to the disk **as it arrives**, resumed where a cut left
//! it. Both sit on the same HTTP and TLS code (`web::http::download`);
//! this module is only the kernel's side of it -- connections, trust,
//! and a sink that writes into a file of the volume.
//!
//! Before phase 15 a repository was reached with a hand-made HTTP/1.0
//! request, in clear: GitHub, which the registry of the proposal names,
//! answers only over HTTPS and redirects its release links twice.
//!
//! ## Trust
//!
//! The built-in anchors of `web::roots` (the same as the browser's
//! defaults), never the exceptions an account added in its browser: a
//! user who accepted a certificate to read a page did not agree to have
//! system software installed from that host.

use alloc::string::String;
use alloc::vec::Vec;

use crate::fs::perm::{Credentials, Mode};
use crate::web::http::{self, Net, Sink};
use crate::web::url::Url;

/// How long a connection may take to open, and a read to answer.
const CONNECT_MS: u64 = 10_000;
const READ_MS: u64 = 30_000;

/// The kernel's connections, for system services.
pub struct SystemNet;

/// A TCP connection, closed when dropped.
pub struct Connection(crate::net::tcp::Handle);

impl crate::web::tls::Transport for Connection {
    fn send(&mut self, data: &[u8]) -> Result<(), &'static str> {
        crate::net::tcp::send(self.0, data).map(|_| ())
    }

    fn recv(&mut self, max: usize) -> Result<Vec<u8>, &'static str> {
        crate::net::tcp::recv(self.0, max, READ_MS)
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        crate::net::tcp::close(self.0);
    }
}

impl Net for SystemNet {
    type Connection = Connection;

    fn connect(&mut self, host: &str, port: u16) -> Result<Connection, String> {
        if !crate::net::interface().configured() {
            return Err(String::from("aucune interface reseau configuree"));
        }
        let address = match crate::net::Ipv4::parse(host) {
            Some(address) => address,
            None => crate::apps::browser::net::resolve(host)?,
        };
        let handle = crate::net::tcp::connect(address, port, CONNECT_MS)
            .map_err(|error| alloc::format!("{} : {}", host, error))?;
        Ok(Connection(handle))
    }

    fn now(&self) -> u64 {
        crate::rtc::now().unwrap_or(0)
    }

    fn random(&mut self, buffer: &mut [u8]) {
        crate::random::fill(buffer);
    }
}

/// A body kept in memory, bounded.
pub struct MemorySink {
    pub data: Vec<u8>,
    limit: usize,
}

impl MemorySink {
    pub fn new(limit: usize) -> MemorySink {
        MemorySink { data: Vec::new(), limit }
    }
}

impl Sink for MemorySink {
    fn begin(&mut self, start: u64, total: Option<u64>) -> Result<(), String> {
        if total.is_some_and(|total| total > self.limit as u64) {
            return Err(String::from("ressource plus volumineuse que la limite"));
        }
        self.data.truncate(start as usize);
        Ok(())
    }

    fn write(&mut self, data: &[u8]) -> Result<(), String> {
        if self.data.len() + data.len() > self.limit {
            return Err(String::from("ressource plus volumineuse que la limite"));
        }
        self.data.extend_from_slice(data);
        Ok(())
    }
}

/// A body written into a file of the volume as it arrives.
pub struct FileSink<'a> {
    path: &'a str,
    who: Credentials,
    offset: u64,
    now: u64,
}

impl<'a> FileSink<'a> {
    pub fn new(path: &'a str, who: Credentials, now: u64) -> FileSink<'a> {
        FileSink { path, who, offset: 0, now }
    }
}

impl Sink for FileSink<'_> {
    fn begin(&mut self, start: u64, _total: Option<u64>) -> Result<(), String> {
        if crate::fs::stat(self.path, self.who).is_err() {
            crate::fs::write_file(self.path, &[], self.who, Mode::system_file(), self.now)
                .map_err(|_| alloc::format!("creation impossible : {}", self.path))?;
        }
        // Whatever lies past the start goes: a resumed transfer continues
        // at `start`, a restarted one (start 0) begins from nothing.
        crate::fs::truncate(self.path, start, self.who, self.now)
            .map_err(|_| alloc::format!("ecriture impossible : {}", self.path))?;
        self.offset = start;
        Ok(())
    }

    fn write(&mut self, data: &[u8]) -> Result<(), String> {
        let written = crate::fs::write_range(self.path, self.offset, data, self.who, self.now)
            .map_err(|_| alloc::format!("ecriture impossible : {}", self.path))?;
        if written != data.len() {
            return Err(alloc::format!("disque plein : {}", self.path));
        }
        self.offset += data.len() as u64;
        Ok(())
    }
}

/// Fetches a resource into memory, at most `limit` bytes.
pub fn to_memory(url: &str, limit: usize) -> Result<Vec<u8>, String> {
    if let Some(path) = url.strip_prefix("file:") {
        let bytes = crate::fs::read_file(path, Credentials::kernel())
            .map_err(|_| alloc::format!("introuvable : {}", path))?;
        if bytes.len() > limit {
            return Err(String::from("ressource plus volumineuse que la limite"));
        }
        return Ok(bytes);
    }
    let parsed = parse_remote(url)?;
    let mut sink = MemorySink::new(limit);
    http::download(&mut SystemNet, parsed, 0, limit as u64, &mut sink).map_err(|failure| failure.message)?;
    Ok(sink.data)
}

/// Fetches a resource into the file `path`, at most `limit` bytes.
///
/// A file already there is a transfer cut earlier: it is **resumed** from
/// its size (`Range`), or restarted if the server does not resume.
/// Returns the size of the complete file.
pub fn to_file(url: &str, path: &str, limit: u64, who: Credentials, now: u64) -> Result<u64, String> {
    if let Some(source) = url.strip_prefix("file:") {
        return copy_local(source, path, limit, who, now);
    }
    let parsed = parse_remote(url)?;
    let resume = crate::fs::stat(path, who).map(|node| node.size).unwrap_or(0);
    let mut sink = FileSink::new(path, who, now);
    let done = http::download(&mut SystemNet, parsed, resume, limit, &mut sink).map_err(|failure| failure.message)?;
    Ok(done.start + done.received)
}

fn parse_remote(url: &str) -> Result<Url, String> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(String::from("schema d'URL non gere - file:, http:// ou https:// attendus"));
    }
    Url::parse(url).ok_or_else(|| alloc::format!("adresse invalide : {}", url))
}

/// `file:` into a file, by pieces of 64 KiB: a package on a USB stick
/// is copied as a downloaded one would be, without holding it whole.
fn copy_local(source: &str, path: &str, limit: u64, who: Credentials, now: u64) -> Result<u64, String> {
    let size = crate::fs::stat(source, Credentials::kernel())
        .map_err(|_| alloc::format!("introuvable : {}", source))?
        .size;
    if size > limit {
        return Err(String::from("ressource plus volumineuse que la limite"));
    }
    let mut sink = FileSink::new(path, who, now);
    sink.begin(0, Some(size))?;
    let mut buffer = alloc::vec![0u8; 64 * 1024];
    let mut offset = 0u64;
    while offset < size {
        let read = crate::fs::read_range(source, offset, &mut buffer, Credentials::kernel())
            .map_err(|_| alloc::format!("lecture impossible : {}", source))?;
        if read == 0 {
            break;
        }
        sink.write(&buffer[..read])?;
        offset += read as u64;
    }
    Ok(offset)
}
