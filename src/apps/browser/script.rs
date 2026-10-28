//! A page's scripts in the browser (16.2): the realm of a tab, the host
//! it talks to, and the cycle run - lay out again - paint.
//!
//! ## Who owns what
//!
//! A tab whose page has scripts keeps the page itself (`Loaded`: the
//! document and its sheets) next to the realm, for the document is what
//! the scripts change. Every entry into the engine -- the scripts at
//! arrival, a timer, a click, a key in a field -- builds a `Ctx` over the
//! three, runs, and drops it; when the run changed the document
//! (`ctx.dirty`), the window lays the page out again from the live
//! document (`page::render_live`) and hands the new boxes back to the
//! realm.
//!
//! ## The host
//!
//! `BrowserHost` answers what the engine cannot know: the time, the
//! zone, randomness, the network (`net::fetch_now`), cookies and storage
//! (`net`), the heap. What the script asks of the window -- go to an
//! address, show a dialog -- is left in the host and done by the window
//! once the run is over.

use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::web::html::NodeId;
use crate::web::http::{Method, Request};
use crate::web::js::dom;
use crate::web::js::interp::{Ctx, Host, Realm};
use crate::web::layout::Laid;
use crate::web::page::{self, Loaded};
use crate::web::url::Url;

/// Steps one entry into the engine may take: a long script ends there
/// (and the page says so) rather than freezing the window.
const FUEL: u64 = 20_000_000;

pub struct BrowserHost {
    /// Minutes east of UTC, from the session's settings.
    offset_minutes: i32,
    /// Where a script asked to go, and whether in place.
    pub navigate: Option<(String, bool)>,
    /// The last dialog a script opened, for the status bar.
    pub dialog: Option<String>,
}

impl BrowserHost {
    fn new() -> BrowserHost {
        let user = crate::apps::session_credentials().map(|(user, _)| user);
        let settings = crate::i18n::resolve(user.as_deref(), None);
        let zone = crate::i18n::tz::find(&settings.timezone).unwrap_or_else(crate::i18n::tz::default_zone);
        BrowserHost { offset_minutes: zone.offset_minutes as i32, navigate: None, dialog: None }
    }
}

fn method_of(name: &str) -> Option<Method> {
    Some(match name {
        "GET" => Method::Get,
        "HEAD" => Method::Head,
        "POST" => Method::Post,
        "PUT" => Method::Put,
        "PATCH" => Method::Patch,
        "DELETE" => Method::Delete,
        "OPTIONS" => Method::Options,
        _ => return None,
    })
}

impl Host for BrowserHost {
    fn now_ms(&mut self) -> i64 {
        // Without a clock, the ticks: timers must still come due.
        crate::rtc::now_millis().unwrap_or(crate::idt::ticks() * (1000 / crate::TIMER_HZ as u64)) as i64
    }

    fn tz_offset_minutes(&mut self) -> i32 {
        self.offset_minutes
    }

    fn random(&mut self) -> u64 {
        crate::random::u64()
    }

    fn memory_ok(&mut self) -> bool {
        crate::heap::room_for(0)
    }

    fn can_allocate(&mut self, bytes: usize) -> bool {
        crate::heap::room_for(bytes)
    }

    fn fetch(
        &mut self,
        method: &str,
        url: &str,
        headers: &[(String, String)],
        body: Option<Vec<u8>>,
    ) -> Result<(u16, String, Vec<(String, String)>, Vec<u8>, String), String> {
        let method = method_of(method).ok_or_else(|| alloc::format!("methode {} non prise en charge", method))?;
        let target = Url::parse(url).ok_or_else(|| alloc::format!("adresse invalide : {}", url))?;
        if !matches!(target.scheme.as_str(), "http" | "https") {
            return Err(alloc::format!("schema {} non pris en charge", target.scheme));
        }
        let mut request = Request::new(method, target);
        let content_type = headers.iter().find(|(n, _)| n.eq_ignore_ascii_case("content-type")).map(|(_, v)| v.clone());
        request.headers = headers.iter().filter(|(n, _)| !n.eq_ignore_ascii_case("content-type")).cloned().collect();
        if let Some(body) = body {
            request.body = Some((content_type.unwrap_or_else(|| String::from("text/plain;charset=UTF-8")), body));
        }
        match super::net::fetch_now(request) {
            Ok(response) => Ok((response.status, response.reason.clone(), response.headers.clone(), response.body, response.url.to_string())),
            Err(failure) => Err(failure.message),
        }
    }

    fn cookies(&mut self, url: &str) -> String {
        super::net::script_cookies(url)
    }

    fn set_cookie(&mut self, url: &str, line: &str) {
        super::net::script_set_cookie(url, line);
    }

    fn storage(&mut self, origin: &str, session: bool) -> Vec<(String, String)> {
        super::net::storage(origin, session)
    }

    fn set_storage(&mut self, origin: &str, session: bool, items: &[(String, String)]) {
        super::net::set_storage(origin, session, items);
    }

    fn navigate(&mut self, url: &str, replace: bool) {
        self.navigate = Some((String::from(url), replace));
    }

    fn log(&mut self, line: &str) {
        crate::serial_println!("[navigateur] console : {}", crate::web::text::fold(line));
    }

    fn dialog(&mut self, kind: &str, message: &str) -> Option<String> {
        self.dialog = Some(alloc::format!("{} : {}", kind, message));
        None
    }
}

/// What a run leaves for the window to do.
#[derive(Default)]
pub struct Outcome {
    /// The page must be laid out again.
    pub relayout: bool,
    pub navigate: Option<(String, bool)>,
    pub dialog: Option<String>,
    pub scroll: Option<i32>,
    pub history: Option<i32>,
    pub submit: Option<NodeId>,
    /// `history.pushState` moved the address.
    pub address: Option<String>,
}

impl Outcome {
    /// Nothing for the window to do.
    pub fn is_empty(&self) -> bool {
        !self.relayout
            && self.navigate.is_none()
            && self.dialog.is_none()
            && self.scroll.is_none()
            && self.history.is_none()
            && self.submit.is_none()
            && self.address.is_none()
    }
}

/// A tab's live page.
pub struct ScriptPage {
    pub loaded: Loaded,
    pub realm: Box<Realm>,
    pub host: BrowserHost,
    /// The page's address as scripts see it (`location`).
    pub address: String,
    pub viewport: (i32, i32),
}

impl ScriptPage {
    /// Takes a page that has arrived: makes its realm, runs its scripts,
    /// fires its load events.
    pub fn start(loaded: Loaded, width: i32, height: i32) -> (Box<ScriptPage>, Outcome) {
        let address = loaded.url.to_string();
        let mut page = Box::new(ScriptPage {
            loaded,
            realm: Box::new(crate::web::js::builtins::new_realm()),
            host: BrowserHost::new(),
            address,
            viewport: (width, height),
        });
        let scripts = core::mem::take(&mut page.loaded.scripts);
        let skipped = core::mem::take(&mut page.loaded.skipped_scripts);
        let started = crate::idt::ticks();
        let outcome = page.run(|ctx| {
            crate::web::js::builtins::install(ctx);
            dom::install(ctx);
            for line in &skipped {
                crate::web::js::builtins::global::console_line(ctx, "avertissement", line);
            }
            dom::start_page(ctx, &scripts);
        });
        let elapsed = (crate::idt::ticks() - started) * (1000 / crate::TIMER_HZ as u64);
        crate::serial_println!(
            "[navigateur] {} script(s) executes en {} ms - {} ligne(s) de console, {} erreur(s)",
            scripts.len(),
            elapsed,
            page.realm.console.len(),
            page.realm.errors.len()
        );
        (page, outcome)
    }

    /// Runs `work` in a context over the page, and collects what the
    /// window must do after.
    pub fn run(&mut self, work: impl FnOnce(&mut Ctx)) -> Outcome {
        let headroom = || crate::scheduler::stack_headroom();
        let ScriptPage { loaded, realm, host, address, viewport } = self;
        let (dirty, base) = {
            let mut ctx = Ctx::new(realm, host, &mut loaded.document, &headroom, FUEL, address.clone(), *viewport);
            work(&mut ctx);
            (ctx.dirty, ctx.base_url.clone())
        };
        let mut outcome = Outcome { relayout: dirty, ..Outcome::default() };
        if base != *address {
            *address = base.clone();
            outcome.address = Some(base);
        }
        outcome.navigate = self.host.navigate.take();
        outcome.dialog = self.host.dialog.take();
        outcome.scroll = self.realm.dom.scroll_request.take();
        outcome.history = self.realm.dom.history_request.take();
        outcome.submit = self.realm.dom.submit_request.take();
        if let Some(reason) = self.realm.halted.clone() {
            outcome.dialog = Some(alloc::format!("scripts arretes : {}", reason));
        }
        outcome
    }

    /// The timers due now.
    pub fn timers(&mut self) -> Outcome {
        if self.realm.timers.is_empty() || self.realm.halted.is_some() {
            return Outcome::default();
        }
        self.run(|ctx| {
            let now = ctx.now();
            if let Err(reason) = ctx.run_timers(now) {
                ctx.report(reason);
            }
        })
    }

    /// Lays the live document out, and gives the realm the new boxes.
    pub fn relayout(&mut self, width: i32, height: i32) -> Laid {
        let headroom = || crate::scheduler::stack_headroom();
        self.viewport = (width, height);
        let laid = page::render_live(&self.loaded, width, height, &headroom);
        self.realm.dom.boxes = laid.page.boxes.clone();
        laid
    }

    /// The element under page coordinates (x, y), if a box holds it: the
    /// last painted, so the innermost. A text run stands for its parent.
    pub fn element_at(&self, laid: &Laid, x: i32, y: i32) -> Option<NodeId> {
        let (node, ..) = laid.page.boxes.iter().rev().find(|(_, bx, by, bw, bh)| x >= *bx && x < bx + bw && y >= *by && y < by + bh)?;
        let document = &self.loaded.document;
        match document.nodes.get(*node).map(|n| &n.data) {
            Some(crate::web::html::NodeData::Text(_)) => document.nodes[*node].parent,
            Some(_) => Some(*node),
            None => None,
        }
    }

    /// A click at (x, y) on `node`: dispatched, and whether the default
    /// action (follow the link, press the button) may go on.
    pub fn click(&mut self, node: NodeId, x: i32, y: i32) -> (bool, Outcome) {
        let mut allowed = true;
        let outcome = self.run(|ctx| {
            for kind in ["mousedown", "mouseup"] {
                let event = dom::events::mouse_event(ctx, kind, x, y);
                let _ = dom::events::dispatch(ctx, node, &event);
            }
            let event = dom::events::mouse_event(ctx, "click", x, y);
            match dom::events::dispatch(ctx, node, &event) {
                Ok(go_on) => allowed = go_on,
                Err(crate::web::js::interp::Ctrl::Halt(reason)) => ctx.report(reason),
                Err(_) => {}
            }
            let _ = ctx.run_jobs();
        });
        (allowed, outcome)
    }

    /// The user typed in a field: the page sees the value, and an
    /// `input` event.
    pub fn typed(&mut self, node: NodeId, value: &str, event: &str) -> Outcome {
        let value = String::from(value);
        self.run(|ctx| {
            ctx.realm.dom.values.insert(node, value);
            let _ = dom::events::fire(ctx, node, event, true, false);
            let _ = ctx.run_jobs();
        })
    }

    /// A checkbox or radio was clicked.
    pub fn toggled(&mut self, node: NodeId, checked: bool) {
        self.realm.dom.checked.insert(node, checked);
    }

    /// A form is about to be sent: `submit`, cancellable.
    pub fn submitting(&mut self, form: NodeId) -> (bool, Outcome) {
        let mut allowed = true;
        let outcome = self.run(|ctx| {
            if let Ok(go_on) = dom::events::fire(ctx, form, "submit", true, true) {
                allowed = go_on;
            }
            let _ = ctx.run_jobs();
        });
        (allowed, outcome)
    }

    /// The page is left: every cycle of the realm broken.
    pub fn close(&mut self) {
        self.realm.teardown();
    }
}

impl Drop for ScriptPage {
    fn drop(&mut self) {
        self.realm.teardown();
    }
}

// ------------------------------------------------------------
//  Self-test (16.2)
// ------------------------------------------------------------

/// The page of the self-test: a script that changes the document before
/// the first paint, a click listener that cancels, a timer that adds a
/// style sheet.
const TEST_PAGE: &str = "<!doctype html><html><head><title>Avant</title></head><body>\
<p id=\"p\">texte initial</p><button id=\"b\">bouton</button><a id=\"a\" href=\"/suite\">lien</a>\
<script>\
document.title = 'Apres';\
const n = document.createElement('div'); n.id = 'ajout'; n.textContent = 'ajoute par le script'; document.body.appendChild(n);\
document.getElementById('b').addEventListener('click', e => { document.getElementById('p').textContent = 'clique'; });\
document.getElementById('a').addEventListener('click', e => e.preventDefault());\
setTimeout(() => { const s = document.createElement('style'); s.textContent = '#ajout { display: none }'; document.head.appendChild(s); }, 10);\
console.log('pret');\
</script></body></html>";

fn painted(laid: &Laid, needle: &str) -> bool {
    laid.page.paints.iter().any(|paint| matches!(paint, crate::web::layout::Paint::Text { text, .. } if text.contains(needle)))
}

fn centre_of(page: &ScriptPage, laid: &Laid, id: &str) -> Option<(NodeId, i32, i32)> {
    let document = &page.loaded.document;
    let node = document.preorder(0).into_iter().find(|n| document.attribute(*n, "id") == Some(id))?;
    let (_, x, y, w, h) = laid.page.boxes.iter().find(|(n, ..)| *n == node || document.is_inside(*n, node))?;
    Some((node, x + w / 2, y + h / 2))
}

/// A page with scripts through the browser's own path, without a
/// window: arrival, relayout, click dispatch by the layout's boxes,
/// timer, navigation. Returns (passed, failed).
pub fn selftest() -> (usize, usize) {
    let mut passed = 0;
    let mut failed = 0;
    let mut check = |ok: bool, what: &str| {
        if ok {
            passed += 1;
        } else {
            failed += 1;
            crate::serial_println!("[navigateur] ECHEC autotest scripts : {}", what);
        }
    };
    let Some(url) = Url::parse("http://essai.local/dossier/page.html") else {
        return (0, 1);
    };
    let mut loaded = page::from_html(url, TEST_PAGE);
    let document = &loaded.document;
    loaded.scripts = document.elements("script").iter().map(|s| document.text_content_raw(*s)).collect();
    let (mut page, outcome) = ScriptPage::start(loaded, 800, 600);
    check(outcome.relayout, "le script a change le document");
    check(page.loaded.document.title() == "Apres", "document.title");
    check(page.realm.console.iter().any(|l| l == "pret"), "console.log");
    let laid = page.relayout(800, 600);
    check(painted(&laid, "ajoute par le script"), "l'element ajoute est mis en page");
    check(!laid.page.boxes.is_empty() && page.realm.dom.boxes.len() == laid.page.boxes.len(), "boites rendues au royaume");

    // A click lands on the button by its box, and its listener runs.
    match centre_of(&page, &laid, "b") {
        Some((button, x, y)) => {
            check(page.element_at(&laid, x, y) == Some(button), "le clic trouve le bouton");
            let (allowed, outcome) = page.click(button, x, y);
            check(allowed && outcome.relayout, "clic : ecouteur execute, page a refaire");
            let laid = page.relayout(800, 600);
            check(painted(&laid, "clique"), "le texte change par le clic est peint");
        }
        None => check(false, "boite du bouton"),
    }
    // The link's listener cancels: the window would not follow it.
    match centre_of(&page, &laid, "a") {
        Some((link, x, y)) => {
            let (allowed, _) = page.click(link, x, y);
            check(!allowed, "preventDefault sur un lien");
        }
        None => check(false, "boite du lien"),
    }
    // The timer: due 10 ms later, it hides the added element.
    let _ = crate::syscall::invoke(crate::syscall::domain_time::SYS_TIME_SLEEP_MS, [40, 0, 0, 0, 0, 0]);
    let outcome = page.timers();
    check(outcome.relayout, "minuterie executee");
    let laid = page.relayout(800, 600);
    check(!painted(&laid, "ajoute par le script"), "feuille de style ajoutee par script appliquee");
    // A navigation asked by a script is left for the window.
    let outcome = page.run(|ctx| {
        let _ = ctx.run_script("location.href = '../autre.html'");
    });
    check(outcome.navigate.as_ref().map(|(u, _)| u.as_str()) == Some("http://essai.local/autre.html"), "navigation demandee par location.href");
    page.close();
    (passed, failed)
}
