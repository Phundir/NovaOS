//! What the web client needs from the kernel: connections, time,
//! randomness, and a place for cookies and the cache.
//!
//! ## Loading happens in a task of its own
//!
//! A page is a DNS query, a TCP connection, a TLS handshake, a transfer,
//! and the same again for each style sheet, then the parsing and the
//! layout: seconds, sometimes. Done in the window's task, the window would
//! freeze for as long. So each load is a **job**: a task is created for
//! it (`worker`), it runs the whole chain, leaves its result in `RESULTS`
//! and ends. The window polls for results every lap, as it polls its
//! mailbox, and a stale result -- the user clicked elsewhere meanwhile --
//! is dropped by its generation number.
//!
//! ## Locks
//!
//! `STORE` (cookies, memory cache) is taken per call and never across
//! the network or the disk: a cookie header is copied out, the cache entry
//! read from disk with the lock released. The worker holds no lock of its
//! own while it waits on TCP.

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::net::tcp;
use crate::vga::SpinLock;
use crate::web::cache::{self, Entry};
use crate::web::cookies::Jar;
use crate::web::http::{Net, Request, Store};
use crate::web::layout::Laid;
use crate::web::page::{self, Loaded, Outcome};
use crate::web::tls::Transport;
use crate::web::url::Url;

/// How long a connection may take to open, and a read to answer.
const CONNECT_MS: u64 = 10_000;
const READ_MS: u64 = 20_000;

/// Names resolved, for five minutes: a page and its style sheets ask for
/// the same host several times in a row.
const DNS_TTL_TICKS: u64 = 300 * crate::TIMER_HZ as u64;
static DNS: SpinLock<BTreeMap<String, (crate::net::Ipv4, u64)>> = SpinLock::new(BTreeMap::new());

pub fn resolve(host: &str) -> Result<crate::net::Ipv4, String> {
    let now = crate::idt::ticks();
    if let Some((address, until)) = DNS.lock().get(host).copied()
        && until > now
    {
        return Ok(address);
    }
    let address = crate::net::dns::resolve(host).map_err(|error| alloc::format!("{} : {}", host, error))?;
    let mut cache = DNS.lock();
    if cache.len() > 256 {
        cache.clear();
    }
    cache.insert(String::from(host), (address, now + DNS_TTL_TICKS));
    Ok(address)
}

/// A TCP connection, closed when dropped.
pub struct Connection {
    handle: tcp::Handle,
}

impl Connection {
    /// Takes over an open TCP connection.
    pub fn from_handle(handle: tcp::Handle) -> Connection {
        Connection { handle }
    }
}

impl Transport for Connection {
    fn send(&mut self, data: &[u8]) -> Result<(), &'static str> {
        tcp::send(self.handle, data).map(|_| ())
    }

    fn recv(&mut self, max: usize) -> Result<Vec<u8>, &'static str> {
        tcp::recv(self.handle, max, READ_MS)
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        tcp::close(self.handle);
    }
}

/// The kernel's side of `Net`, for one job.
pub struct KernelNet {
    pub job: u64,
}

impl Net for KernelNet {
    type Connection = Connection;

    fn connect(&mut self, host: &str, port: u16) -> Result<Connection, String> {
        if cancelled(self.job) {
            return Err(String::from("chargement interrompu"));
        }
        set_progress(self.job, alloc::format!("connexion a {}...", host));
        let address = resolve(host)?;
        let handle = tcp::connect(address, port, CONNECT_MS).map_err(|error| alloc::format!("{} : {}", host, error))?;
        set_progress(self.job, alloc::format!("{} - en attente de reponse...", host));
        Ok(Connection { handle })
    }

    fn now(&self) -> u64 {
        crate::rtc::now().unwrap_or(0)
    }

    fn random(&mut self, buffer: &mut [u8]) {
        crate::random::fill(buffer);
    }

    fn trust(&mut self) -> crate::web::x509::Trust {
        super::certs::trust()
    }

    fn progress(&mut self, received: usize, expected: usize) -> bool {
        let text = if expected > 0 {
            alloc::format!("reception : {} / {} Kio", received / 1024, expected / 1024)
        } else {
            alloc::format!("reception : {} Kio", received / 1024)
        };
        set_progress(self.job, text);
        !cancelled(self.job)
    }
}

// ============================================================
//  Cookies and cache
// ============================================================

/// Files kept in an account's disk cache at most.
const MAX_DISK_ENTRIES: usize = 200;

struct BrowserStore {
    /// The account whose cookies and cache these are.
    user: String,
    jar: Jar,
    memory: cache::Memory,
    /// Cookies changed since the last save.
    dirty: bool,
}

static STORE: SpinLock<Option<BrowserStore>> = SpinLock::new(None);

fn user_directory(user: &str, leaf: &str) -> String {
    alloc::format!("/users/{}/{}", user, leaf)
}

/// Where the cookies and the preferences of an account live.
pub fn settings_directory(user: &str) -> String {
    user_directory(user, "settings/navigateur")
}

fn cache_directory(user: &str) -> String {
    user_directory(user, "cache/navigateur")
}

fn cookie_file(user: &str) -> String {
    alloc::format!("{}/cookies.txt", settings_directory(user))
}

/// Opens the store for the account in session: reads its cookies back.
/// Called by the window at start.
pub fn open_store() {
    let Some((user, credentials)) = crate::apps::session_credentials() else {
        return;
    };
    let now = crate::rtc::now().unwrap_or(0);
    let jar = crate::fs::read_file(&cookie_file(&user), credentials)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .map(|text| Jar::load(&text, now))
        .unwrap_or_default();
    for directory in [settings_directory(&user), cache_directory(&user)] {
        let _ = crate::fs::create_directory(
            &directory,
            credentials,
            crate::fs::perm::Mode::private_dir(),
            crate::idt::ticks(),
        );
    }
    crate::serial_println!("[navigateur] {} cookie(s) relus pour \"{}\"", jar.len(), user);
    *STORE.lock() = Some(BrowserStore {
        user,
        jar,
        memory: cache::Memory::new(8 * 1024 * 1024),
        dirty: false,
    });
}

/// Writes the persistent cookies if they changed. Called now and then by
/// the window, and when it closes.
pub fn save_cookies() {
    let (user, text) = {
        let mut store = STORE.lock();
        let Some(store) = store.as_mut() else {
            return;
        };
        if !store.dirty {
            return;
        }
        store.dirty = false;
        (store.user.clone(), store.jar.save())
    };
    let Some(credentials) = crate::fs::credentials_of(&user) else {
        return;
    };
    let path = cookie_file(&user);
    let now = crate::idt::ticks();
    let _ = crate::fs::delete(&path, credentials, now);
    let _ = crate::fs::write_file(&path, text.as_bytes(), credentials, crate::fs::perm::Mode::private_file(), now);
}

/// The window closed: session cookies die with it (RFC 6265).
pub fn close_store() {
    if let Some(store) = STORE.lock().as_mut() {
        store.jar.end_session();
        store.dirty = true;
    }
    save_cookies();
    *STORE.lock() = None;
}

/// (domains with cookies, cookie count, cache entries, cache bytes).
pub fn store_summary() -> (Vec<(String, usize)>, usize, usize, usize) {
    let store = STORE.lock();
    match store.as_ref() {
        Some(store) => {
            let (entries, bytes) = store.memory.usage();
            (store.jar.domains(), store.jar.len(), entries, bytes)
        }
        None => (Vec::new(), 0, 0, 0),
    }
}

pub fn clear_cookies() {
    if let Some(store) = STORE.lock().as_mut() {
        store.jar.clear();
        store.dirty = true;
    }
    save_cookies();
}

/// Empties the cache, in memory and on disk.
pub fn clear_cache() -> usize {
    let user = {
        let mut store = STORE.lock();
        let Some(store) = store.as_mut() else {
            return 0;
        };
        store.memory.clear();
        store.user.clone()
    };
    let Some(credentials) = crate::fs::credentials_of(&user) else {
        return 0;
    };
    let directory = cache_directory(&user);
    let mut removed = 0;
    if let Ok(links) = crate::fs::list(&directory, credentials) {
        for link in links {
            if crate::fs::delete(&crate::apps::join_path(&directory, &link.name), credentials, crate::idt::ticks()).is_ok() {
                removed += 1;
            }
        }
    }
    removed
}

pub struct KernelStore;

impl Store for KernelStore {
    fn cookie_header(&mut self, url: &Url) -> Option<String> {
        let now = crate::rtc::now().unwrap_or(0);
        STORE.lock().as_mut()?.jar.header_for(url, now)
    }

    fn set_cookie(&mut self, url: &Url, header: &str) {
        let now = crate::rtc::now().unwrap_or(0);
        if let Some(store) = STORE.lock().as_mut() {
            store.jar.store(url, header, now);
            store.dirty = true;
        }
    }

    fn cached(&mut self, url: &str) -> Option<Entry> {
        let user = {
            let mut store = STORE.lock();
            let store = store.as_mut()?;
            if let Some(entry) = store.memory.get(url) {
                return Some(entry);
            }
            store.user.clone()
        };
        // Not in memory: the disk, with no lock held.
        let credentials = crate::fs::credentials_of(&user)?;
        let path = crate::apps::join_path(&cache_directory(&user), &cache::file_name(url));
        let entry = Entry::decode(&crate::fs::read_file(&path, credentials).ok()?)?;
        if entry.url != url {
            return None;
        }
        if let Some(store) = STORE.lock().as_mut() {
            store.memory.put(entry.clone());
        }
        Some(entry)
    }

    fn cache(&mut self, entry: Entry) {
        let user = {
            let mut store = STORE.lock();
            let Some(store) = store.as_mut() else {
                return;
            };
            store.memory.put(entry.clone());
            store.user.clone()
        };
        let encoded = entry.encode();
        if encoded.len() as u64 > crate::fs::format::MAX_FILE_SIZE {
            return;
        }
        let Some(credentials) = crate::fs::credentials_of(&user) else {
            return;
        };
        let directory = cache_directory(&user);
        let path = crate::apps::join_path(&directory, &cache::file_name(&entry.url));
        let now = crate::idt::ticks();
        // The disk cache is bounded by count: past `MAX_DISK_ENTRIES` it
        // starts again empty -- cruder than evicting the oldest, and it
        // keeps a browser from filling the volume one page at a time.
        if let Ok(links) = crate::fs::list(&directory, credentials)
            && links.len() >= MAX_DISK_ENTRIES
        {
            for link in links {
                let _ = crate::fs::delete(&crate::apps::join_path(&directory, &link.name), credentials, now);
            }
        }
        let _ = crate::fs::delete(&path, credentials, now);
        let _ = crate::fs::write_file(&path, &encoded, credentials, crate::fs::perm::Mode::private_file(), now);
    }
}

// ============================================================
//  Jobs
// ============================================================

/// What a job produces.
pub enum Done {
    Page { loaded: Loaded, laid: Laid },
    Download(crate::web::http::Response),
    Failed(crate::web::tls::Failure),
}

struct Job {
    id: u64,
    request: Request,
    width: i32,
    viewport_height: i32,
    cancelled: bool,
    progress: String,
    /// Taken by a worker.
    started: bool,
}

static JOBS: SpinLock<Vec<Job>> = SpinLock::new(Vec::new());
static RESULTS: SpinLock<Vec<(u64, Done)>> = SpinLock::new(Vec::new());
static NEXT_JOB: AtomicU64 = AtomicU64::new(1);

fn cancelled(job: u64) -> bool {
    JOBS.lock().iter().find(|j| j.id == job).is_none_or(|j| j.cancelled)
}

fn set_progress(job: u64, text: String) {
    if let Some(entry) = JOBS.lock().iter_mut().find(|j| j.id == job) {
        entry.progress = text;
    }
}

/// Where a job is, for the status bar.
pub fn progress(job: u64) -> Option<String> {
    JOBS.lock().iter().find(|j| j.id == job).map(|j| j.progress.clone())
}

/// Starts loading `request` for a page `width` wide. Returns the job.
pub fn start(request: Request, width: i32, viewport_height: i32) -> u64 {
    let id = NEXT_JOB.fetch_add(1, Ordering::Relaxed);
    JOBS.lock().push(Job {
        id,
        request,
        width,
        viewport_height,
        cancelled: false,
        progress: String::from("resolution du nom..."),
        started: false,
    });
    crate::scheduler::spawn("navigateur-chargement", crate::process::BinaryType::Cus, worker);
    id
}

pub fn cancel(job: u64) {
    if let Some(entry) = JOBS.lock().iter_mut().find(|j| j.id == job) {
        entry.cancelled = true;
    }
}

/// The finished job `id`, if it is.
pub fn take(id: u64) -> Option<Done> {
    let mut results = RESULTS.lock();
    let index = results.iter().position(|(job, _)| *job == id)?;
    Some(results.remove(index).1)
}

/// Forgets the results nobody will collect: the window closed.
pub fn forget_all() {
    for job in JOBS.lock().iter_mut() {
        job.cancelled = true;
    }
    RESULTS.lock().clear();
}

/// Bytes of stack left, for the layout's recursion.
fn headroom() -> usize {
    crate::scheduler::stack_headroom()
}

/// The task of one job.
///
/// The work is in `run_job`, whose locals are dropped when it returns:
/// `exit_current_with` does not unwind, and what lives in this frame when
/// it is called is never freed.
fn worker() -> ! {
    run_job();
    crate::scheduler::exit_current_with(0)
}

fn run_job() {
    let job = {
        let mut jobs = JOBS.lock();
        jobs.iter_mut().find(|j| !j.started).map(|j| {
            j.started = true;
            (j.id, j.request.clone(), j.width, j.viewport_height)
        })
    };
    let Some((id, request, width, viewport_height)) = job else {
        return;
    };
    let target = request.url.to_string();
    let started = crate::idt::ticks();
    let mut net = KernelNet { job: id };
    let mut store = KernelStore;
    let done = match page::load(&mut net, &mut store, request, width) {
        Ok(Outcome::Page(loaded)) => {
            set_progress(id, String::from("mise en page..."));
            let laid = page::render(&loaded, width, viewport_height, &headroom);
            Done::Page { loaded, laid }
        }
        Ok(Outcome::Download(response)) => Done::Download(response),
        Err(error) => Done::Failed(error),
    };
    let elapsed = (crate::idt::ticks() - started) * (1000 / crate::TIMER_HZ as u64);
    match &done {
        Done::Page { laid, .. } => crate::serial_println!(
            "[navigateur] {} - {} ms, {} elements dessines, {} liens",
            target,
            elapsed,
            laid.page.paints.len(),
            laid.page.links.len()
        ),
        Done::Download(response) => crate::serial_println!(
            "[navigateur] {} - fichier de {} octets ({})",
            target,
            response.body.len(),
            response.content_type()
        ),
        Done::Failed(error) => crate::serial_println!("[navigateur] {} - echec : {}", target, error),
    }
    let keep = !cancelled(id);
    JOBS.lock().retain(|j| j.id != id);
    if keep {
        RESULTS.lock().push((id, done));
    }
}
