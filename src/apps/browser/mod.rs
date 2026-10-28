//! `navigateur.cus` -- the web browser.
//!
//! ## What it does
//!
//! Opens addresses and searches (DuckDuckGo by default), in tabs; follows
//! links; fills and sends forms (`GET` and `POST`); goes back and forward;
//! keeps cookies and a cache per account; restores the open tabs at the
//! next start; saves files it cannot show into the account's downloads.
//! Over HTTPS the certificate is verified (`web::x509`) and the status bar
//! says so.
//!
//! ## What it does not
//!
//! **No JavaScript, no images.** A script engine is a project the size
//! of this whole system, and decoding PNG and JPEG a smaller one that has
//! not been done: images show as a frame with their description. Pages
//! that build themselves with scripts show what their HTML carries --
//! often a `<noscript>` notice, sometimes nothing. The sites that keep a
//! page without scripts -- DuckDuckGo's HTML version, Wikipedia, news
//! sites, documentation -- read well. Named here and on the home page, so
//! nobody mistakes the one for the other.
//!
//! ## How it is built
//!
//! The window is a pixel canvas (`gui::canvas`): the chrome -- tabs,
//! address bar, status bar -- and the page are painted by `web::paint` and
//! handed to the compositor as one command. Loading runs in a task per
//! request (`net`), so the window keeps answering while a page arrives.

pub mod certs;
pub mod net;
pub mod pages;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use super::window::{self, App, Press, Ui};
use crate::binfmt::identity::Permissions;
use crate::gui::canvas::Canvas;
use crate::gui::protocol::{CursorShape, KeyEvent};
use crate::web::http::{Method, Request};
use crate::web::form::{self, Field};
use crate::web::layout::{Control, ControlKind, Laid};
use crate::web::paint::{self, Target};
use crate::web::text;
use crate::web::url::{self, Url};

pub const FILE_NAME: &str = "navigateur.cus";
pub const APP_ID: &str = "os.nova.navigateur";
pub const BINARY: &str = "/apps/navigateur.cus";
/// Stack of the browser task: page scripts recurse as deep as their
/// authors wrote them, and a tree-walking interpreter costs a few KiB per
/// level. `scheduler::stack_headroom` still bounds it.
pub const STACK_SIZE: usize = 1024 * 1024;

const WIDTH: i32 = 1200;
const HEIGHT: i32 = 820;

const HOME: &str = "about:accueil";

// --- geometry of the chrome ---
const TAB_H: i32 = 30;
const BAR_H: i32 = 36;
const STATUS_H: i32 = 22;
const SCROLLBAR_W: i32 = 12;
const BUTTON_W: i32 = 32;
const MAX_TABS: usize = 10;

// --- colours of the chrome ---
const CHROME: u32 = 0xdfe3e8;
const TAB_ACTIVE: u32 = 0xf7f8fa;
const TAB_IDLE: u32 = 0xc9ced6;
const INK: u32 = 0x1f2328;
const DIM: u32 = 0x5f6670;
const ACCENT: u32 = 0x2f6fd0;
const SECURE: u32 = 0x1a7f37;
const INSECURE: u32 = 0xb3261e;

// --- keys ---
const SCAN_ESCAPE: u8 = 0x01;
const SCAN_BACKSPACE: u8 = 0x0E;
const SCAN_TAB: u8 = 0x0F;
const SCAN_ENTER: u8 = 0x1C;
const SCAN_SPACE: u8 = 0x39;
const SCAN_F5: u8 = 0x3F;
const SCAN_HOME: u8 = 0x47;
const SCAN_UP: u8 = 0x48;
const SCAN_PAGE_UP: u8 = 0x49;
const SCAN_LEFT: u8 = 0x4B;
const SCAN_RIGHT: u8 = 0x4D;
const SCAN_END: u8 = 0x4F;
const SCAN_DOWN: u8 = 0x50;
const SCAN_PAGE_DOWN: u8 = 0x51;
const SCAN_DELETE: u8 = 0x53;

/// Laps of the window loop between two saves of the cookies.
const SAVE_LAPS: u32 = (30_000 / window::PERIOD_MS) as u32;

/// A page shown in a tab.
struct View {
    url: Url,
    /// What the address bar shows: the URL, or the `about:` name.
    address: String,
    laid: Laid,
    security: String,
    /// The form controls, with what the user typed into them.
    controls: Vec<Control>,
}

struct Tab {
    /// Addresses visited in this tab, and where we are among them.
    history: Vec<String>,
    position: usize,
    title: String,
    view: Option<View>,
    job: Option<u64>,
    /// What loads, for the status bar and the address bar meanwhile.
    loading: String,
    scroll: i32,
    /// A `#fragment` to scroll to once the page arrives.
    fragment: String,
    /// The TLS session of the last real page shown, and its address: what
    /// `about:certificat` shows.
    session: Option<crate::web::tls::Session>,
    session_address: String,
}

impl Tab {
    fn new() -> Tab {
        Tab {
            history: Vec::new(),
            position: 0,
            title: String::from("Nouvel onglet"),
            view: None,
            job: None,
            loading: String::new(),
            scroll: 0,
            fragment: String::new(),
            session: None,
            session_address: String::new(),
        }
    }

    fn address(&self) -> String {
        if !self.loading.is_empty() {
            return self.loading.clone();
        }
        match &self.view {
            Some(view) => view.address.clone(),
            None => self.history.get(self.position).cloned().unwrap_or_default(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    Page,
    Address,
    /// A text control of the page.
    Control(usize),
}

/// What the pointer is over.
#[derive(Clone, PartialEq, Eq)]
enum Hit {
    Tab(usize),
    CloseTab(usize),
    NewTab,
    Back,
    Forward,
    Reload,
    Home,
    Address,
    /// The padlock: the certificate of the page.
    Lock,
    Link(String),
    Control(usize),
    Scrollbar,
    Nothing,
}

struct Browser {
    width: i32,
    height: i32,
    canvas: Option<Canvas>,
    tabs: Vec<Tab>,
    current: usize,
    focus: Focus,
    /// The address bar being edited, and its caret.
    address: String,
    caret: usize,
    hover: Hit,
    /// A repaint is due.
    dirty: bool,
    /// Visited pages: (url, title), oldest first.
    visited: Vec<(String, String)>,
    visited_dirty: bool,
    engine: String,
    /// Access to the network refused by the policy of 12.3.
    network_refused: Option<String>,
    laps: u32,
    /// Ctrl is held: a click on a link opens it in a new tab.
    ctrl_held: bool,
    dragging_scrollbar: bool,
    message: String,
}

fn session_file(user: &str) -> String {
    format!("{}/session.txt", net::settings_directory(user))
}

fn history_file(user: &str) -> String {
    format!("{}/historique.txt", net::settings_directory(user))
}

fn preferences_file(user: &str) -> String {
    format!("{}/preferences.txt", net::settings_directory(user))
}

fn read_text(path: &str) -> Option<String> {
    let (_, credentials) = crate::apps::session_credentials()?;
    String::from_utf8(crate::fs::read_file(path, credentials).ok()?).ok()
}

fn write_text(path: &str, text: &str) {
    let Some((_, credentials)) = crate::apps::session_credentials() else {
        return;
    };
    let now = crate::idt::ticks();
    let _ = crate::fs::delete(path, credentials, now);
    let _ = crate::fs::write_file(path, text.as_bytes(), credentials, crate::fs::perm::Mode::private_file(), now);
}

/// Truncates to `cells` characters, with `...` when cut.
fn fit(text: &str, cells: i32) -> String {
    let cells = cells.max(0) as usize;
    if text.chars().count() <= cells {
        return String::from(text);
    }
    if cells <= 3 {
        return text.chars().take(cells).collect();
    }
    let mut out: String = text.chars().take(cells - 3).collect();
    out.push_str("...");
    out
}

impl Browser {
    fn new() -> Browser {
        Browser {
            width: WIDTH,
            height: HEIGHT,
            canvas: None,
            tabs: Vec::new(),
            current: 0,
            focus: Focus::Page,
            address: String::new(),
            caret: 0,
            hover: Hit::Nothing,
            dirty: true,
            visited: Vec::new(),
            visited_dirty: false,
            engine: String::from("duckduckgo"),
            network_refused: None,
            laps: 0,
            ctrl_held: false,
            dragging_scrollbar: false,
            message: String::new(),
        }
    }

    fn content_top(&self) -> i32 {
        TAB_H + BAR_H
    }

    fn content_height(&self) -> i32 {
        (self.height - self.content_top() - STATUS_H).max(1)
    }

    fn content_width(&self) -> i32 {
        (self.width - SCROLLBAR_W).max(1)
    }

    fn tab(&self) -> &Tab {
        &self.tabs[self.current]
    }

    fn tab_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.current]
    }

    // --------------------------------------------------------
    //  Start and end
    // --------------------------------------------------------

    fn restore(&mut self) {
        net::open_store();
        let user = crate::apps::session_credentials().map(|(user, _)| user);
        if let Some(user) = &user {
            if let Some(text) = read_text(&preferences_file(user)) {
                for line in text.lines() {
                    if let Some(engine) = line.strip_prefix("moteur=") {
                        self.engine = String::from(engine.trim());
                    }
                }
            }
            if let Some(text) = read_text(&history_file(user)) {
                for line in text.lines() {
                    if let Some((url, title)) = line.split_once('\t') {
                        self.visited.push((String::from(url), String::from(title)));
                    }
                }
            }
        }
        if let Err(reason) = crate::apps::authorize_native(BINARY, Permissions::NETWORK_INTERNET) {
            crate::serial_println!("[navigateur] acces a internet refuse : {}", reason);
            self.network_refused = Some(reason);
        }
        // The tabs of the last session, the active one loaded now and the
        // others when they are shown.
        let mut active = 0;
        if let Some(user) = &user
            && let Some(text) = read_text(&session_file(user))
        {
            for line in text.lines() {
                if let Some(index) = line.strip_prefix("actif=") {
                    active = index.trim().parse().unwrap_or(0);
                } else if !line.trim().is_empty() && self.tabs.len() < MAX_TABS {
                    let mut tab = Tab::new();
                    tab.history.push(String::from(line.trim()));
                    tab.title = String::from(line.trim());
                    self.tabs.push(tab);
                }
            }
        }
        if self.tabs.is_empty() {
            self.tabs.push(Tab::new());
            self.tabs[0].history.push(String::from(HOME));
        }
        self.current = active.min(self.tabs.len() - 1);
        self.reload_current(false);
        crate::serial_println!("[navigateur] {} onglet(s) restaure(s)", self.tabs.len());
    }

    fn save_session(&mut self) {
        let Some((user, _)) = crate::apps::session_credentials() else {
            return;
        };
        let mut text = format!("actif={}\n", self.current);
        for tab in &self.tabs {
            let address = tab.address();
            if !address.is_empty() {
                text.push_str(&address);
                text.push('\n');
            }
        }
        write_text(&session_file(&user), &text);
        if self.visited_dirty {
            let start = self.visited.len().saturating_sub(300);
            let mut history = String::new();
            for (url, title) in &self.visited[start..] {
                history.push_str(&format!("{}\t{}\n", url, title));
            }
            write_text(&history_file(&user), &history);
            self.visited_dirty = false;
        }
        write_text(&preferences_file(&user), &format!("moteur={}\n", self.engine));
    }

    // --------------------------------------------------------
    //  Navigation
    // --------------------------------------------------------

    /// Goes to what the address bar or a link names.
    fn open(&mut self, text: &str, new_tab: bool) {
        let target = url::from_input(text, pages::engine_url(&self.engine));
        if new_tab && self.tabs.len() < MAX_TABS {
            self.tabs.push(Tab::new());
            self.current = self.tabs.len() - 1;
        }
        self.navigate(request_for(&target), true);
    }

    fn navigate(&mut self, request: Option<Request>, push: bool) {
        let Some(request) = request else {
            self.message = String::from("adresse invalide");
            self.dirty = true;
            return;
        };
        if request.url.scheme == "about" {
            let address = request.url.path.clone();
            self.push_history(&address, push);
            if let Some(job) = self.tab_mut().job.take() {
                net::cancel(job);
            }
            self.show_internal(&address);
            return;
        }
        let address = request.url.to_string();
        self.push_history(&address, push);
        self.start_load(request);
    }

    /// Opens an internal address, or starts loading a real one.
    fn open_internal_or_load(&mut self, address: &str, reload: bool) {
        if address.starts_with("about:") {
            self.show_internal(address);
            return;
        }
        match Url::parse(address) {
            Some(url) => {
                let mut request = Request::get(url);
                request.reload = reload;
                self.start_load(request);
            }
            None => self.show_internal(&format!("about:erreur?{}", address)),
        }
    }

    fn push_history(&mut self, address: &str, push: bool) {
        let tab = self.tab_mut();
        if push {
            if !tab.history.is_empty() {
                tab.history.truncate(tab.position + 1);
            }
            if tab.history.last().map(|last| last.as_str()) != Some(address) {
                tab.history.push(String::from(address));
            }
            tab.position = tab.history.len() - 1;
        }
    }

    fn start_load(&mut self, request: Request) {
        let (content_width, content_height) = (self.content_width(), self.content_height());
        let refused = self.network_refused.clone();
        let tab = self.tab_mut();
        if let Some(job) = tab.job.take() {
            net::cancel(job);
        }
        tab.fragment = request.url.fragment.clone();
        tab.loading = request.url.to_string();
        if let Some(reason) = refused {
            let address = request.url.to_string();
            self.show_page(
                &address,
                &pages::error(&address, &format!("acces a internet refuse par la politique de securite : {}", reason)),
            );
            return;
        }
        // A link to a fragment of the page shown: no request, a scroll.
        if let Some(view) = &tab.view
            && view.url.without_fragment() == request.url.without_fragment()
            && !request.url.fragment.is_empty()
            && request.method == Method::Get
            && !request.reload
        {
            let fragment = request.url.fragment.clone();
            tab.loading.clear();
            if let Some(view) = tab.view.as_mut() {
                view.url.fragment = fragment.clone();
            }
            self.scroll_to_fragment(&fragment);
            return;
        }
        tab.job = Some(net::start(request, content_width, content_height));
        self.dirty = true;
    }

    fn reload_current(&mut self, reload: bool) {
        let address = {
            let tab = self.tab();
            tab.history.get(tab.position).cloned().unwrap_or_else(|| String::from(HOME))
        };
        self.open_internal_or_load(&address, reload);
    }

    fn back(&mut self) {
        let tab = self.tab_mut();
        if tab.position > 0 {
            tab.position -= 1;
            self.reload_current(false);
        }
    }

    fn forward(&mut self) {
        let tab = self.tab_mut();
        if tab.position + 1 < tab.history.len() {
            tab.position += 1;
            self.reload_current(false);
        }
    }

    /// Shows an HTML page of the browser's own at `address`.
    fn show_page(&mut self, address: &str, source: &str) {
        let url = Url::parse("http://navigateur.local/").expect("static URL");
        let loaded = crate::web::page::from_html(url, source);
        let headroom = || crate::scheduler::stack_headroom();
        let laid = crate::web::page::render(&loaded, self.content_width(), self.content_height(), &headroom);
        let controls = laid.page.controls.clone();
        let tab = self.tab_mut();
        tab.title = text::fold(&loaded.title);
        tab.view = Some(View {
            url: Url::parse("http://navigateur.local/").expect("static URL"),
            address: String::from(address),
            laid,
            security: String::from("page interne du navigateur"),
            controls,
        });
        tab.loading.clear();
        tab.scroll = 0;
        if let Some(entry) = tab.history.get_mut(tab.position) {
            *entry = String::from(address);
        }
        // The address shown is the `about:` one, not the placeholder.
        if let Some(view) = tab.view.as_mut() {
            view.url = Url {
                scheme: String::from("about"),
                host: String::new(),
                port: 0,
                path: String::from(address.trim_start_matches("about:")),
                query: String::new(),
                fragment: String::new(),
            };
        }
        self.focus = Focus::Page;
        self.dirty = true;
    }

    fn show_internal(&mut self, address: &str) {
        let (domains, cookies, entries, bytes) = net::store_summary();
        if let Some(action) = address.strip_prefix("about:action/") {
            let (action, query) = action.split_once('?').unwrap_or((action, ""));
            let parameter = |name: &str| -> String {
                query
                    .split('&')
                    .filter_map(|pair| pair.split_once('='))
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| url::percent_decode(value, true))
                    .unwrap_or_default()
            };
            // Certificate actions show the certificate pages, or go on to
            // the site the user accepted.
            let certificate_message = match action {
                "exception" => {
                    let target = parameter("retour");
                    match certs::add_exception(&parameter("hote"), &parameter("empreinte")) {
                        Ok(()) => {
                            {
                                let tab = self.tab_mut();
                                if let Some(entry) = tab.history.get_mut(tab.position) {
                                    *entry = target.clone();
                                }
                            }
                            self.open_internal_or_load(&target, true);
                            return;
                        }
                        Err(error) => Some(error),
                    }
                }
                "ajouter-racine" => Some(certs::add_root(&parameter("chemin"))),
                "retirer-racine" => Some(certs::remove_root(&parameter("empreinte"))),
                "desapprouver" => Some(certs::distrust(&parameter("cle"), false)),
                "approuver" => Some(certs::distrust(&parameter("cle"), true)),
                "retirer-exception" => Some(certs::remove_exception(&parameter("hote"))),
                _ => None,
            };
            if let Some(message) = certificate_message {
                {
                    let tab = self.tab_mut();
                    if let Some(entry) = tab.history.get_mut(tab.position) {
                        *entry = String::from("about:certificats");
                    }
                }
                self.show_page("about:certificats", &certs::management(&message));
                return;
            }
            self.message = match action {
                "effacer-cookies" => {
                    net::clear_cookies();
                    String::from("Cookies effaces.")
                }
                "effacer-cache" => format!("Cache vide : {} fichier(s) supprime(s).", net::clear_cache()),
                "effacer-historique" => {
                    self.visited.clear();
                    self.visited_dirty = true;
                    String::from("Historique efface.")
                }
                other => match other.strip_prefix("moteur/") {
                    Some(engine) => {
                        self.engine = String::from(engine);
                        self.save_session();
                        format!("Moteur de recherche : {}.", engine)
                    }
                    None => String::from("Action inconnue."),
                },
            };
            let message = core::mem::take(&mut self.message);
            let (domains, cookies, entries, bytes) = net::store_summary();
            let page = pages::preferences(&self.engine, &domains, cookies, entries, bytes, &message);
            {
                let tab = self.tab_mut();
                if let Some(entry) = tab.history.get_mut(tab.position) {
                    *entry = String::from("about:preferences");
                }
            }
            self.show_page("about:preferences", &page);
            return;
        }
        let page = match address {
            "about:accueil" | "about:home" | "about:blank" | "about:" => {
                let recent: Vec<(String, String)> = self.visited.iter().rev().take(8).cloned().collect();
                pages::home(pages::engine_form(&self.engine), pages::engine_field(&self.engine), &recent)
            }
            "about:historique" | "about:history" => pages::history(&self.visited),
            "about:certificat" => {
                let tab = self.tab();
                certs::viewer(&tab.session_address, tab.session.as_ref())
            }
            "about:certificats" => certs::management(""),
            "about:preferences" => pages::preferences(&self.engine, &domains, cookies, entries, bytes, ""),
            other => {
                let reason = other.strip_prefix("about:erreur?").unwrap_or(other);
                pages::error(reason, "adresse inconnue ou invalide")
            }
        };
        self.show_page(address, &page);
    }

    fn scroll_to_fragment(&mut self, fragment: &str) {
        let decoded = url::percent_decode(fragment, false);
        let tab = self.tab_mut();
        if let Some(view) = &tab.view
            && let Some((_, y)) = view.laid.page.anchors.iter().find(|(name, _)| *name == decoded || *name == fragment)
        {
            tab.scroll = (*y - 8).max(0);
        }
        self.clamp_scroll();
        self.dirty = true;
    }

    fn clamp_scroll(&mut self) {
        let visible = self.content_height();
        let tab = self.tab_mut();
        let height = tab.view.as_ref().map(|view| view.laid.page.height).unwrap_or(0);
        let max = (height + 16 - visible).max(0);
        tab.scroll = tab.scroll.clamp(0, max);
    }

    fn scroll_by(&mut self, delta: i32) {
        self.tab_mut().scroll += delta;
        self.clamp_scroll();
        self.dirty = true;
    }

    // --------------------------------------------------------
    //  Results of the jobs
    // --------------------------------------------------------

    fn collect(&mut self) {
        for index in 0..self.tabs.len() {
            let Some(job) = self.tabs[index].job else {
                continue;
            };
            let Some(done) = net::take(job) else {
                continue;
            };
            self.tabs[index].job = None;
            let previous = self.current;
            self.current = index;
            match done {
                net::Done::Page { loaded, laid } => {
                    let title = text::fold(&loaded.title);
                    let address = loaded.url.to_string();
                    let controls = laid.page.controls.clone();
                    let tab = self.tab_mut();
                    tab.title = if title.trim().is_empty() { loaded.url.host.clone() } else { title.clone() };
                    tab.session = loaded.tls.clone();
                    tab.session_address = address.clone();
                    tab.view = Some(View {
                        url: loaded.url.clone(),
                        address: address.clone(),
                        laid,
                        security: loaded.security.clone(),
                        controls,
                    });
                    tab.loading.clear();
                    tab.scroll = 0;
                    // After redirects, the history keeps where we landed.
                    if let Some(entry) = tab.history.get_mut(tab.position) {
                        *entry = address.clone();
                    }
                    let fragment = core::mem::take(&mut tab.fragment);
                    if !fragment.is_empty() {
                        self.scroll_to_fragment(&fragment);
                    }
                    self.visited.retain(|(url, _)| *url != address);
                    self.visited.push((address, title));
                    if self.visited.len() > 500 {
                        self.visited.remove(0);
                    }
                    self.visited_dirty = true;
                }
                net::Done::Download(response) => {
                    let address = response.url.to_string();
                    let (path, failure) = match save_download(&response) {
                        Ok(path) => (Some(path), None),
                        Err(error) => (None, Some(error)),
                    };
                    let page = pages::downloaded(&address, path.as_deref(), response.body.len(), &response.content_type(), failure.as_deref());
                    self.show_page(&address, &page);
                }
                net::Done::Failed(error) => {
                    let address = self.tab().loading.clone();
                    let page = if error.certificates.is_empty() {
                        pages::error(&address, &error.message)
                    } else {
                        certs::error_page(&address, &error.host, &error.message, &error.certificates)
                    };
                    self.show_page(&address, &page);
                }
            }
            self.current = previous;
            self.dirty = true;
        }
    }

    // --------------------------------------------------------
    //  Forms
    // --------------------------------------------------------

    /// Sends the form of control `index`, as if `submitter` was clicked.
    ///
    /// The method, the address and the encoding come from the form, or
    /// from the button's own `formmethod`, `formaction`, `formenctype`.
    /// A file field names a file of the account's home; it is read here
    /// and sent as a part of a multipart body.
    fn submit(&mut self, index: usize, submitter: Option<usize>) {
        let Some(view) = &self.tab().view else {
            return;
        };
        let Some(form_index) = view.controls.get(index).and_then(|control| control.form) else {
            return;
        };
        let Some(form) = view.laid.forms.get(form_index) else {
            return;
        };
        let button = submitter.and_then(|position| view.controls.get(position));
        let pick = |own: Option<&str>, inherited: &str| -> String {
            match own {
                Some(value) if !value.is_empty() => String::from(value),
                _ => String::from(inherited),
            }
        };
        let method_name = pick(button.map(|b| b.form_method.as_str()), &form.method);
        let action_text = pick(button.map(|b| b.form_action.as_str()), &form.action);
        let enctype = pick(button.map(|b| b.form_enctype.as_str()), &form.enctype);
        if method_name == "DIALOG" {
            return;
        }
        let method = if method_name == "POST" { Method::Post } else { Method::Get };

        let home = crate::apps::session_credentials();
        let mut fields: Vec<Field> = Vec::new();
        let mut problem: Option<String> = None;
        for (position, control) in view.controls.iter().enumerate() {
            if control.form != Some(form_index) || control.name.is_empty() {
                continue;
            }
            let value = match control.kind {
                ControlKind::Text | ControlKind::Password | ControlKind::Hidden | ControlKind::TextArea => {
                    Some(control.value.clone())
                }
                ControlKind::Checkbox | ControlKind::Radio => control.checked.then(|| control.value.clone()),
                ControlKind::Select => control.options.get(control.selected).map(|(value, _)| value.clone()),
                ControlKind::Submit => (Some(position) == submitter).then(|| control.value.clone()),
                ControlKind::Button => None,
                ControlKind::File => {
                    let relative = control.value.trim();
                    if relative.is_empty() {
                        fields.push(Field::File {
                            name: control.name.clone(),
                            filename: String::new(),
                            content_type: String::from("application/octet-stream"),
                            data: Vec::new(),
                        });
                    } else if let Some((user, credentials)) = &home {
                        let path = format!("/users/{}/home/{}", user, relative.trim_start_matches('/'));
                        match crate::fs::read_file(&path, *credentials) {
                            Ok(data) => {
                                let filename = String::from(relative.rsplit('/').next().unwrap_or(relative));
                                fields.push(Field::File {
                                    name: control.name.clone(),
                                    content_type: String::from(form::guess_type(&filename)),
                                    filename,
                                    data,
                                });
                            }
                            Err(error) => problem = Some(format!("{} : {}", relative, error.message())),
                        }
                    }
                    None
                }
            };
            if let Some(value) = value {
                fields.push(Field::text(&control.name, &value));
            }
        }
        if let Some(problem) = problem {
            self.message = format!("fichier illisible - {}", problem);
            self.dirty = true;
            return;
        }

        // The browser's own forms post to `about:action/...`.
        if action_text.starts_with("about:") {
            if !self.may_open(&action_text) {
                return;
            }
            let address = format!("{}?{}", action_text, form::urlencoded(&fields));
            self.focus = Focus::Page;
            self.navigate(request_for(&address), true);
            return;
        }
        let base = view.url.clone();
        let action = if action_text.trim().is_empty() {
            base.clone()
        } else if base.scheme == "about" {
            match Url::parse(&action_text) {
                Some(url) => url,
                None => return,
            }
        } else {
            match base.join(&action_text) {
                Some(url) => url,
                None => return,
            }
        };
        let referer = (base.scheme != "about").then(|| base.without_fragment());
        let request = if method == Method::Post {
            let mut random = [0u8; 24];
            crate::random::fill(&mut random);
            let (content_type, body) = form::encode(&fields, form::Encoding::parse(&enctype), &random);
            let mut request = Request::new(Method::Post, action);
            request.body = Some((content_type, body));
            request.referer = referer;
            request
        } else {
            // A GET carries its fields in the address, whatever `enctype`.
            let mut target = action;
            target.query = form::urlencoded(&fields);
            target.fragment.clear();
            let mut request = Request::get(target);
            request.referer = referer;
            request
        };
        crate::serial_println!(
            "[navigateur] formulaire {} ({}) vers {}",
            request.method.as_str(),
            request.body.as_ref().map(|(kind, _)| kind.as_str()).unwrap_or("dans l'adresse"),
            request.url.without_fragment()
        );
        self.focus = Focus::Page;
        self.navigate(Some(request), true);
    }

    // --------------------------------------------------------
    //  Hit testing
    // --------------------------------------------------------

    fn tab_width(&self) -> i32 {
        let count = self.tabs.len().max(1) as i32;
        ((self.width - 44) / count).clamp(60, 220)
    }

    fn hit(&self, x: i32, y: i32) -> Hit {
        if y < TAB_H {
            let width = self.tab_width();
            for index in 0..self.tabs.len() {
                let left = 4 + index as i32 * width;
                if x >= left && x < left + width - 2 {
                    if x >= left + width - 22 {
                        return Hit::CloseTab(index);
                    }
                    return Hit::Tab(index);
                }
            }
            let plus = 4 + self.tabs.len() as i32 * width;
            if x >= plus && x < plus + 28 {
                return Hit::NewTab;
            }
            return Hit::Nothing;
        }
        if y < TAB_H + BAR_H {
            let buttons = [Hit::Back, Hit::Forward, Hit::Reload, Hit::Home];
            for (index, hit) in buttons.into_iter().enumerate() {
                let left = 6 + index as i32 * (BUTTON_W + 4);
                if x >= left && x < left + BUTTON_W {
                    return hit;
                }
            }
            let left = self.address_left();
            if x >= left && x < left + 24 {
                return Hit::Lock;
            }
            if x >= left {
                return Hit::Address;
            }
            return Hit::Nothing;
        }
        if y >= self.height - STATUS_H {
            return Hit::Nothing;
        }
        if x >= self.width - SCROLLBAR_W {
            return Hit::Scrollbar;
        }
        let Some(view) = &self.tab().view else {
            return Hit::Nothing;
        };
        let page_x = x;
        let page_y = y - self.content_top() + self.tab().scroll;
        for (index, control) in view.controls.iter().enumerate() {
            if control.kind != ControlKind::Hidden
                && page_x >= control.x
                && page_x < control.x + control.w
                && page_y >= control.y
                && page_y < control.y + control.h
            {
                return Hit::Control(index);
            }
        }
        for link in view.laid.page.links.iter().rev() {
            if page_x >= link.x && page_x < link.x + link.w && page_y >= link.y && page_y < link.y + link.h {
                return Hit::Link(link.href.clone());
            }
        }
        Hit::Nothing
    }

    fn address_left(&self) -> i32 {
        6 + 4 * (BUTTON_W + 4) + 6
    }

    /// Resolves a link of the page shown.
    fn resolve(&self, href: &str) -> Option<String> {
        if href.starts_with("about:") {
            return Some(String::from(href));
        }
        let view = self.tab().view.as_ref()?;
        if view.url.scheme == "about" {
            return Url::parse(href).map(|url| url.to_string());
        }
        view.url.join(href).map(|url| url.to_string())
    }

    /// May the page shown lead to `target`? The browser's actions --
    /// `about:action/...`, which change trust and delete data -- are only
    /// reachable from the browser's own pages: a site could otherwise
    /// dress a link to "accept this certificate for your bank" as anything.
    fn may_open(&mut self, target: &str) -> bool {
        let internal = self.tab().view.as_ref().is_some_and(|view| view.address.starts_with("about:"));
        if target.starts_with("about:action") && !internal {
            crate::serial_println!("[navigateur] action interne refusee depuis une page web : {}", target);
            self.message = String::from("lien refuse : une page web ne peut pas commander le navigateur");
            self.dirty = true;
            return false;
        }
        true
    }

    fn click_control(&mut self, index: usize) {
        let Some(view) = self.tabs[self.current].view.as_mut() else {
            return;
        };
        let Some(control) = view.controls.get_mut(index) else {
            return;
        };
        match control.kind {
            ControlKind::Text | ControlKind::Password | ControlKind::TextArea | ControlKind::File => {
                self.caret = control.value.chars().count();
                self.focus = Focus::Control(index);
            }
            ControlKind::Checkbox => control.checked = !control.checked,
            ControlKind::Radio => {
                let (form, name) = (control.form, control.name.clone());
                for (position, other) in view.controls.iter_mut().enumerate() {
                    if other.kind == ControlKind::Radio && other.form == form && other.name == name {
                        other.checked = position == index;
                    }
                }
            }
            ControlKind::Select => {
                if !control.options.is_empty() {
                    control.selected = (control.selected + 1) % control.options.len();
                }
            }
            ControlKind::Submit => self.submit(index, Some(index)),
            ControlKind::Button | ControlKind::Hidden => {}
        }
        self.dirty = true;
    }

    // --------------------------------------------------------
    //  Tabs
    // --------------------------------------------------------

    fn new_tab(&mut self) {
        if self.tabs.len() >= MAX_TABS {
            self.message = format!("{} onglets au plus", MAX_TABS);
            return;
        }
        self.tabs.push(Tab::new());
        self.current = self.tabs.len() - 1;
        self.navigate(request_for(HOME), true);
        self.focus = Focus::Address;
        self.address.clear();
        self.caret = 0;
    }

    fn close_tab(&mut self, index: usize) {
        if let Some(job) = self.tabs[index].job {
            net::cancel(job);
        }
        self.tabs.remove(index);
        if self.tabs.is_empty() {
            self.tabs.push(Tab::new());
            self.current = 0;
            self.navigate(request_for(HOME), true);
        } else if self.current >= self.tabs.len() || self.current > index {
            self.current = self.current.saturating_sub(1).min(self.tabs.len() - 1);
        }
        self.focus = Focus::Page;
        self.activate();
    }

    fn switch_to(&mut self, index: usize) {
        self.current = index.min(self.tabs.len() - 1);
        self.focus = Focus::Page;
        self.activate();
    }

    /// A restored tab is loaded the first time it is shown.
    fn activate(&mut self) {
        let tab = self.tab();
        if tab.view.is_none() && tab.job.is_none() {
            self.reload_current(false);
        }
        self.dirty = true;
    }

    // --------------------------------------------------------
    //  Painting
    // --------------------------------------------------------

    fn render(&mut self) {
        let Some(mut canvas) = self.canvas.take() else {
            return;
        };
        let (width, height) = (canvas.width as i32, canvas.height as i32);
        {
            let mut target = Target::new(&mut canvas.pixels, canvas.width, canvas.height);
            target.fill(0, 0, width, height, CHROME);
            self.paint_tabs(&mut target);
            self.paint_toolbar(&mut target);
            self.paint_content(&mut target);
            self.paint_status(&mut target);
        }
        canvas.publish();
        self.canvas = Some(canvas);
    }

    fn paint_tabs(&self, target: &mut Target) {
        let width = self.tab_width();
        for (index, tab) in self.tabs.iter().enumerate() {
            let left = 4 + index as i32 * width;
            let active = index == self.current;
            let color = if active {
                TAB_ACTIVE
            } else if self.hover == Hit::Tab(index) {
                0xd8dce2
            } else {
                TAB_IDLE
            };
            target.fill(left, 4, width - 2, TAB_H - 4, color);
            if active {
                target.fill(left, 4, width - 2, 2, ACCENT);
            }
            let marker = if tab.job.is_some() { "* " } else { "" };
            let label = format!("{}{}", marker, tab.title);
            target.text(left + 8, 11, &fit(&label, (width - 36) / 8), INK, 1, false);
            let close = if self.hover == Hit::CloseTab(index) { INSECURE } else { DIM };
            target.text(left + width - 18, 11, "x", close, 1, false);
        }
        let plus = 4 + self.tabs.len() as i32 * width;
        if self.tabs.len() < MAX_TABS {
            let color = if self.hover == Hit::NewTab { TAB_ACTIVE } else { CHROME };
            target.fill(plus, 6, 26, TAB_H - 8, color);
            target.text(plus + 9, 11, "+", INK, 1, true);
        }
    }

    fn paint_toolbar(&self, target: &mut Target) {
        let top = TAB_H;
        target.fill(0, top, self.width, BAR_H, TAB_ACTIVE);
        target.fill(0, top + BAR_H - 1, self.width, 1, 0xc4c9d0);
        let tab = self.tab();
        let can_back = tab.position > 0;
        let can_forward = tab.position + 1 < tab.history.len();
        let loading = tab.job.is_some();
        let buttons = [
            (Hit::Back, "<", can_back),
            (Hit::Forward, ">", can_forward),
            (Hit::Reload, if loading { "X" } else { "R" }, true),
            (Hit::Home, "H", true),
        ];
        for (index, (hit, label, enabled)) in buttons.into_iter().enumerate() {
            let left = 6 + index as i32 * (BUTTON_W + 4);
            if self.hover == hit && enabled {
                target.fill(left, top + 5, BUTTON_W, BAR_H - 10, 0xe4e8ee);
            }
            target.text(left + 12, top + 10, label, if enabled { INK } else { 0xa0a6ae }, 1, true);
        }

        let left = self.address_left();
        let width = self.width - left - 8;
        let editing = self.focus == Focus::Address;
        target.fill(left, top + 5, width, BAR_H - 10, 0xffffff);
        target.frame(left, top + 5, width, BAR_H - 10, if editing { ACCENT } else { 0xb8bec6 });
        let text_left = left + 26;
        let cells = (width - 34) / 8;
        if editing {
            let shown: String = text::fold(&self.address);
            let start = (self.caret as i32 - cells + 1).max(0) as usize;
            let visible: String = shown.chars().skip(start).take(cells as usize).collect();
            target.text(text_left, top + 10, &visible, INK, 1, false);
            let caret_x = text_left + (self.caret - start) as i32 * 8;
            target.fill(caret_x, top + 9, 1, 18, INK);
        } else {
            let address = text::fold(&tab.address());
            target.text(text_left, top + 10, &fit(&address, cells), INK, 1, false);
        }
        // The lock: green for a verified HTTPS page, red for plain HTTP.
        let secure = tab.view.as_ref().map(|view| view.url.scheme.as_str()) == Some("https") && tab.loading.is_empty();
        let insecure = tab.view.as_ref().map(|view| view.url.scheme.as_str()) == Some("http") && tab.loading.is_empty();
        let color = if secure {
            SECURE
        } else if insecure {
            INSECURE
        } else {
            0xb8bec6
        };
        target.fill(left + 8, top + 12, 10, 10, color);
        target.fill(left + 10, top + 9, 6, 4, color);
    }

    fn paint_content(&self, target: &mut Target) {
        let top = self.content_top();
        let height = self.content_height();
        let width = self.content_width();
        let tab = self.tab();
        let Some(view) = &tab.view else {
            target.fill(0, top, self.width, height, 0xffffff);
            let text = if tab.job.is_some() { "Chargement..." } else { "" };
            target.text(24, top + 24, text, DIM, 1, false);
            return;
        };
        target.set_clip(0, top, width, height);
        target.fill(0, top, width, height, view.laid.background);
        let controls = paint::page(target, &view.laid, 0, top, tab.scroll, height);
        for index in controls {
            if let Some(control) = view.controls.get(index) {
                self.paint_control(target, control, index, top - tab.scroll);
            }
        }
        target.reset_clip();

        // Scroll bar.
        let track_left = self.width - SCROLLBAR_W;
        target.fill(track_left, top, SCROLLBAR_W, height, 0xeceef1);
        let page_height = view.laid.page.height.max(1) + 16;
        if page_height > height {
            let thumb = (height as i64 * height as i64 / page_height as i64).max(20) as i32;
            let position = (tab.scroll as i64 * (height - thumb) as i64 / (page_height - height).max(1) as i64) as i32;
            target.fill(track_left + 2, top + position, SCROLLBAR_W - 4, thumb, 0x9aa1ab);
        }
    }

    fn paint_control(&self, target: &mut Target, control: &Control, index: usize, offset: i32) {
        let (x, y, w, h) = (control.x, control.y + offset, control.w, control.h);
        let focused = self.focus == Focus::Control(index);
        let hovered = self.hover == Hit::Control(index);
        match control.kind {
            ControlKind::Hidden => {}
            ControlKind::Text | ControlKind::Password | ControlKind::TextArea | ControlKind::File => {
                target.fill(x, y, w, h, 0xffffff);
                target.frame(x, y, w, h, if focused { ACCENT } else { 0x8f959e });
                let cells = ((w - 8) / 8).max(1);
                let value = if control.kind == ControlKind::Password {
                    "*".repeat(control.value.chars().count())
                } else {
                    text::fold(&control.value)
                };
                if value.is_empty() && !focused {
                    target.text(x + 4, y + (h - 16) / 2, &fit(&text::fold(&control.placeholder), cells), 0x9aa1ab, 1, false);
                } else {
                    let caret = if focused { self.caret as i32 } else { 0 };
                    let start = (caret - cells + 1).max(0) as usize;
                    let visible: String = value.chars().skip(start).take(cells as usize).collect();
                    target.text(x + 4, y + (h - 16) / 2, &visible, INK, 1, false);
                    if focused {
                        let caret_x = x + 4 + (caret - start as i32) * 8;
                        target.fill(caret_x, y + 3, 1, h - 6, INK);
                    }
                }
            }
            ControlKind::Submit | ControlKind::Button => {
                target.fill(x, y, w, h, if hovered { 0xe2e6ec } else { 0xeef0f3 });
                target.frame(x, y, w, h, 0x8f959e);
                let label = text::fold(&control.value);
                let label = fit(&label, (w - 8) / 8);
                let label_width = label.len() as i32 * 8;
                target.text(x + (w - label_width) / 2, y + (h - 16) / 2, &label, INK, 1, false);
            }
            ControlKind::Checkbox | ControlKind::Radio => {
                target.fill(x, y, w, h, 0xffffff);
                target.frame(x, y, w, h, if hovered { ACCENT } else { 0x8f959e });
                if control.checked {
                    target.fill(x + 4, y + 4, w - 8, h - 8, ACCENT);
                }
            }
            ControlKind::Select => {
                target.fill(x, y, w, h, if hovered { 0xf2f4f7 } else { 0xffffff });
                target.frame(x, y, w, h, 0x8f959e);
                let label = control
                    .options
                    .get(control.selected)
                    .map(|(_, label)| text::fold(label))
                    .unwrap_or_default();
                target.text(x + 4, y + (h - 16) / 2, &fit(&label, (w - 24) / 8), INK, 1, false);
                target.text(x + w - 14, y + (h - 16) / 2, "v", DIM, 1, false);
            }
        }
    }

    fn paint_status(&self, target: &mut Target) {
        let top = self.height - STATUS_H;
        target.fill(0, top, self.width, STATUS_H, CHROME);
        target.fill(0, top, self.width, 1, 0xc4c9d0);
        let tab = self.tab();
        let (text, color) = if let Hit::Link(href) = &self.hover {
            (self.resolve(href).unwrap_or_else(|| href.clone()), INK)
        } else if !self.message.is_empty() {
            (self.message.clone(), INSECURE)
        } else if let Some(job) = tab.job {
            (net::progress(job).unwrap_or_else(|| String::from("chargement...")), DIM)
        } else if let Some(view) = &tab.view {
            let color = if view.url.scheme == "https" { SECURE } else { DIM };
            (view.security.clone(), color)
        } else {
            (String::from("Pret"), DIM)
        };
        let cells = (self.width - 16) / 8;
        target.text(8, top + 4, &fit(&text::fold(&text), cells), color, 1, false);
    }

    // --------------------------------------------------------
    //  Editing
    // --------------------------------------------------------

    /// Applies an editing key to `value` with the caret. Returns `true`
    /// if the key was an editing key.
    fn edit(value: &mut String, caret: &mut usize, key: &KeyEvent) -> bool {
        let length = value.chars().count();
        *caret = (*caret).min(length);
        let byte_at = |value: &String, index: usize| value.char_indices().nth(index).map(|(i, _)| i).unwrap_or(value.len());
        match key.scancode {
            SCAN_BACKSPACE => {
                if *caret > 0 {
                    let at = byte_at(value, *caret - 1);
                    value.remove(at);
                    *caret -= 1;
                }
                true
            }
            SCAN_DELETE => {
                if *caret < length {
                    let at = byte_at(value, *caret);
                    value.remove(at);
                }
                true
            }
            SCAN_LEFT => {
                *caret = caret.saturating_sub(1);
                true
            }
            SCAN_RIGHT => {
                *caret = (*caret + 1).min(length);
                true
            }
            SCAN_HOME => {
                *caret = 0;
                true
            }
            SCAN_END => {
                *caret = length;
                true
            }
            _ => match key.character() {
                Some(character) if !character.is_control() && !key.has(KeyEvent::MOD_CTRL) && length < 2048 => {
                    let at = byte_at(value, *caret);
                    value.insert(at, character);
                    *caret += 1;
                    true
                }
                _ => false,
            },
        }
    }
}

/// A request for an address. An internal `about:` address becomes a
/// placeholder request of scheme `about`, which `navigate` turns into a
/// page of the browser's own instead of a load.
fn request_for(address: &str) -> Option<Request> {
    if address.starts_with("about:") {
        return Some(Request::get(Url {
            scheme: String::from("about"),
            host: String::new(),
            port: 0,
            path: String::from(address),
            query: String::new(),
            fragment: String::new(),
        }));
    }
    Url::parse(address).map(Request::get)
}

impl App for Browser {
    fn size(&mut self, width: i32, height: i32) {
        self.width = width;
        self.height = height;
        self.canvas = Some(Canvas::new(width as usize, height as usize));
        self.restore();
    }

    fn paint(&self, ui: &mut Ui) {
        if let Some(canvas) = &self.canvas {
            ui.client.canvas(ui.surface, canvas.id, 0, 0);
        }
    }

    fn key(&mut self, key: &KeyEvent) -> bool {
        self.ctrl_held = key.has(KeyEvent::MOD_CTRL);
        if !key.pressed {
            return false;
        }
        self.message.clear();
        let ctrl = key.has(KeyEvent::MOD_CTRL);
        let alt = key.has(KeyEvent::MOD_ALT);
        let letter = key.character().map(|c| c.to_ascii_lowercase());

        // Shortcuts that work wherever the focus is.
        if ctrl {
            match (letter, key.scancode) {
                (Some('t'), _) => self.new_tab(),
                (Some('w'), _) => self.close_tab(self.current),
                (Some('l'), _) => {
                    self.focus = Focus::Address;
                    self.address = self.tab().address();
                    self.caret = self.address.chars().count();
                }
                (Some('r'), _) => self.reload_current(true),
                (Some('h'), _) => self.open("about:historique", false),
                (_, SCAN_TAB) => {
                    let count = self.tabs.len();
                    let next = if key.has(KeyEvent::MOD_SHIFT) { (self.current + count - 1) % count } else { (self.current + 1) % count };
                    self.switch_to(next);
                }
                _ => {}
            }
            self.dirty = true;
            return false;
        }
        if alt {
            match key.scancode {
                SCAN_LEFT => self.back(),
                SCAN_RIGHT => self.forward(),
                _ => {}
            }
            self.dirty = true;
            return false;
        }
        if key.scancode == SCAN_F5 {
            self.reload_current(true);
            return false;
        }

        match self.focus {
            Focus::Address => match key.scancode {
                SCAN_ENTER => {
                    let text = self.address.clone();
                    self.focus = Focus::Page;
                    if !text.trim().is_empty() {
                        self.open(&text, false);
                    }
                }
                SCAN_ESCAPE => self.focus = Focus::Page,
                _ => {
                    Self::edit(&mut self.address, &mut self.caret, key);
                }
            },
            Focus::Control(index) => match key.scancode {
                SCAN_ENTER => self.submit(index, None),
                SCAN_ESCAPE | SCAN_TAB => self.focus = Focus::Page,
                _ => {
                    let current = self.current;
                    if let Some(control) = self.tabs[current].view.as_mut().and_then(|view| view.controls.get_mut(index)) {
                        Self::edit(&mut control.value, &mut self.caret, key);
                    }
                }
            },
            Focus::Page => {
                let page = self.content_height() - 40;
                match key.scancode {
                    SCAN_DOWN => self.scroll_by(40),
                    SCAN_UP => self.scroll_by(-40),
                    SCAN_PAGE_DOWN | SCAN_SPACE => self.scroll_by(page),
                    SCAN_PAGE_UP => self.scroll_by(-page),
                    SCAN_HOME => self.scroll_by(-1_000_000),
                    SCAN_END => self.scroll_by(1_000_000),
                    SCAN_BACKSPACE => self.back(),
                    _ => {}
                }
            }
        }
        self.dirty = true;
        false
    }

    fn pointer(&mut self, x: i32, y: i32, press: Press) -> (bool, CursorShape) {
        let hit = self.hit(x, y);
        if hit != self.hover {
            self.hover = hit.clone();
            self.dirty = true;
        }
        if press.released {
            self.dragging_scrollbar = false;
        }
        if press.held && self.dragging_scrollbar {
            let top = self.content_top();
            let height = self.content_height();
            let page_height = self.tab().view.as_ref().map(|view| view.laid.page.height + 16).unwrap_or(0);
            let ratio = ((y - top).clamp(0, height) as i64 * page_height as i64 / height.max(1) as i64) as i32;
            self.tab_mut().scroll = ratio - height / 2;
            self.clamp_scroll();
            self.dirty = true;
        }
        if press.pressed {
            self.message.clear();
            let new_tab = self.ctrl_held;
            match hit.clone() {
                Hit::Tab(index) => self.switch_to(index),
                Hit::CloseTab(index) => self.close_tab(index),
                Hit::NewTab => self.new_tab(),
                Hit::Back => self.back(),
                Hit::Forward => self.forward(),
                Hit::Reload => {
                    if let Some(job) = self.tab_mut().job.take() {
                        net::cancel(job);
                        self.tab_mut().loading.clear();
                    } else {
                        self.reload_current(true);
                    }
                }
                Hit::Home => self.open(HOME, false),
                Hit::Lock => self.open("about:certificat", false),
                Hit::Address => {
                    self.focus = Focus::Address;
                    self.address = self.tab().address();
                    self.caret = self.address.chars().count();
                }
                Hit::Link(href) => {
                    self.focus = Focus::Page;
                    if let Some(target) = self.resolve(&href)
                        && self.may_open(&target)
                    {
                        crate::serial_println!("[navigateur] lien : {}", target);
                        self.open(&target, new_tab);
                    }
                }
                Hit::Control(index) => self.click_control(index),
                Hit::Scrollbar => self.dragging_scrollbar = true,
                Hit::Nothing => self.focus = Focus::Page,
            }
            self.dirty = true;
        }
        let shape = match hit {
            Hit::Address => CursorShape::Text,
            Hit::Control(index) => match self.tab().view.as_ref().and_then(|view| view.controls.get(index)).map(|c| c.kind) {
                Some(ControlKind::Text | ControlKind::Password | ControlKind::TextArea | ControlKind::File) => CursorShape::Text,
                _ => CursorShape::Hand,
            },
            Hit::Nothing | Hit::Scrollbar => CursorShape::Arrow,
            _ => CursorShape::Hand,
        };
        (false, shape)
    }

    fn wheel(&mut self, _x: i32, _y: i32, notches: i32) -> bool {
        self.scroll_by(-notches * 60);
        false
    }

    fn tick(&mut self) -> bool {
        self.collect();
        // The progress text of a load changes as it goes.
        if self.tab().job.is_some() {
            self.laps += 1;
            if self.laps % 10 == 0 {
                self.dirty = true;
            }
        } else {
            self.laps += 1;
        }
        if self.laps % SAVE_LAPS == 0 {
            net::save_cookies();
        }
        if self.dirty {
            self.dirty = false;
            self.render();
            return true;
        }
        false
    }

    fn handles_close_key(&self) -> bool {
        true
    }

    fn title(&self) -> Option<String> {
        let title = &self.tab().title;
        Some(if title.is_empty() { String::from("Navigateur") } else { format!("{} - Navigateur", fit(title, 40)) })
    }

    fn close(&mut self) {
        certs::reset();
        self.save_session();
        net::forget_all();
        net::close_store();
        // The task ends by `exit_current_with`, which does not unwind:
        // nothing on its stack is ever dropped. The pages, the history
        // and the canvas -- megabytes -- are given back here, or never.
        self.tabs.clear();
        self.visited.clear();
        self.canvas = None;
    }
}

/// Saves a file the browser cannot show into the account's downloads.
fn save_download(response: &crate::web::http::Response) -> Result<String, String> {
    let (user, credentials) = crate::apps::session_credentials().ok_or("aucune session ouverte")?;
    let directory = format!("/users/{}/home/downloads", user);
    let from_header = response.header("content-disposition").and_then(|value| {
        let (_, name) = value.split_once("filename=")?;
        Some(String::from(name.trim().trim_matches('"').trim_matches('\'')))
    });
    let from_path = response
        .url
        .path
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .map(|name| url::percent_decode(name, false));
    let name = from_header.or(from_path).unwrap_or_else(|| String::from("telechargement"));
    // Only what a file name may hold on this filesystem.
    let name: String = text::fold(&name)
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') { c } else { '_' })
        .take(48)
        .collect();
    let taken: Vec<String> = crate::fs::list(&directory, credentials)
        .map(|links| links.into_iter().map(|link| link.name).collect())
        .unwrap_or_default();
    let name = crate::apps::explorer::ops::unique_name(&taken, &name);
    let path = crate::apps::join_path(&directory, &name);
    if response.body.len() as u64 > crate::fs::format::MAX_FILE_SIZE {
        return Err(format!(
            "fichier de {} Kio, au-dela des {} Kio qu'un fichier peut contenir",
            response.body.len() / 1024,
            crate::fs::format::MAX_FILE_SIZE / 1024
        ));
    }
    crate::fs::write_file(&path, &response.body, credentials, crate::fs::perm::Mode::private_file(), crate::idt::ticks())
        .map_err(|error| String::from(error.message()))?;
    crate::serial_println!("[navigateur] telechargement enregistre : {} ({} octets)", path, response.body.len());
    Ok(path)
}

pub fn task() -> ! {
    let mut browser = Browser::new();
    window::run("navigateur", "Navigateur", WIDTH, HEIGHT, &mut browser)
}

pub fn install(now: u64) -> bool {
    super::install_binary(
        &super::BinarySpec {
            path: BINARY,
            title: FILE_NAME,
            app_id: APP_ID,
            display_name: "Navigateur",
            format: crate::binfmt::header::BinaryFormat::Cus,
            // Internet (the whitelist of 12.3 asks at first use), and the
            // account's files for downloads, cookies and the cache.
            permissions: Permissions::NETWORK_INTERNET | Permissions::FS_USER_HOME,
        },
        now,
    )
}
