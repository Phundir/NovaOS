//! 7.7.1 and 7.7.2 -- Login screen (spec 2.4).
//!
//! ## What 2.4 imposes
//!
//! "Display of every user account. Selection then password entry.
//! Accessibility button always visible. Anti-bruteforce: 3 failures ->
//! 30 s, 5 failures -> 5 min, 10 failures -> account locked."
//!
//! ## Password storage
//!
//! The spec says nothing of the format. The choice kept: SHA-256 over
//! `salt || password`, the salt being drawn per account. What that brings
//! and what it does not must be said clearly.
//!
//! * What it brings: two accounts with the same password do not have the
//!   same digest, and a precomputed table is useless.
//! * What it does **not** bring: SHA-256 is designed to be fast. An
//!   offline attack on the file would try millions of candidates per
//!   second. Real storage demands a slow, parameterised function --
//!   Argon2, scrypt, failing that PBKDF2 with a high iteration count.
//!   **So this is not safe storage**, and that must stay written as long
//!   as it is not.
//!
//! The salt comes from the tick counter and the time counter of the RTC.
//! Nor is that a cryptographic source: there is none in the kernel today.
//! An `RDRAND` or an entropy collection will come with 12.
//!
//! ## Anti-bruteforce
//!
//! The failure count and the delays live in memory. A reboot erases them
//! -- which is a real weakness, and the reason the lock at ten failures
//! is **persisted** in the account file. A delay lost at reboot costs the
//! attacker thirty seconds; a lock lost would give them the account back.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::binfmt::sha256;
use crate::fs::perm::{Credentials, Mode, Role};
use crate::vga::SpinLock;

use crate::graphics::framebuffer::mix;

use super::client::{Client, Event};
use super::protocol::KeyEvent;
use super::surface::Layer;

/// Pure white, only ever used as the far end of a `mix`.
///
/// Lightening a colour towards white is the one hover tint that cannot
/// collide with a theme: a theme chooses its own palette, and any *other*
/// colour picked as a hover shade could be declared equal to the one it is
/// supposed to stand out from.
const WHITE: u32 = 0xFF_FF_FF;

/// File of the password digests.
pub const CREDENTIALS_PATH: &str = "/sys/config/accounts.cfg";

/// Thresholds of 2.4.
pub const DELAY_AFTER: u32 = 3;
pub const LONG_DELAY_AFTER: u32 = 5;
pub const LOCK_AFTER: u32 = 10;

/// Associated durations, in milliseconds.
pub const DELAY_MS: u64 = 30_000;
pub const LONG_DELAY_MS: u64 = 5 * 60_000;

const SCAN_ENTER: u8 = 0x1C;
const SCAN_BACKSPACE: u8 = 0x0E;
const SCAN_TAB: u8 = 0x0F;
const SCAN_ESCAPE: u8 = 0x01;
const SCAN_F1: u8 = 0x3B;

/// Maximum length of a password entered.
const MAX_PASSWORD: usize = 64;

// ============================================================
//  Authentication state
// ============================================================

/// Verdict of an attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Accepted,
    WrongPassword,
    UnknownAccount,
    /// The account is delayed; the value is the remainder in milliseconds.
    Throttled(u64),
    /// Locked until an administrator intervenes.
    Locked,
}

impl Verdict {
    pub fn as_str(&self) -> &'static str {
        match self {
            Verdict::Accepted => "accepte",
            Verdict::WrongPassword => "mot de passe incorrect",
            Verdict::UnknownAccount => "compte inconnu",
            Verdict::Throttled(_) => "temporise",
            Verdict::Locked => "verrouille",
        }
    }
}

#[derive(Debug, Clone)]
struct Attempts {
    user: String,
    failures: u32,
    /// Tick before which no attempt is accepted.
    blocked_until: u64,
    locked: bool,
}

struct Guard {
    accounts: Vec<Attempts>,
    successes: u64,
    failures: u64,
    throttles: u64,
    locks: u64,
}

static GUARD: SpinLock<Guard> = SpinLock::new(Guard {
    accounts: Vec::new(),
    successes: 0,
    failures: 0,
    throttles: 0,
    locks: 0,
});

fn ms_to_ticks(ms: u64) -> u64 {
    let per_tick = 1000 / crate::TIMER_HZ as u64;
    ms.div_ceil(per_tick).max(1)
}

fn ticks_to_ms(ticks: u64) -> u64 {
    ticks * (1000 / crate::TIMER_HZ as u64)
}

impl Guard {
    fn entry(&mut self, user: &str) -> &mut Attempts {
        if let Some(index) = self.accounts.iter().position(|item| item.user == user) {
            return &mut self.accounts[index];
        }
        self.accounts.push(Attempts {
            user: String::from(user),
            failures: 0,
            blocked_until: 0,
            locked: false,
        });
        self.accounts.last_mut().expect("entree juste inseree")
    }
}

/// 7.7.2 -- Current state of an account, without trying anything.
pub fn status(user: &str) -> Verdict {
    let now = crate::idt::ticks();
    let mut guard = GUARD.lock();
    let entry = guard.entry(user);
    if entry.locked {
        return Verdict::Locked;
    }
    if now < entry.blocked_until {
        return Verdict::Throttled(ticks_to_ms(entry.blocked_until - now));
    }
    Verdict::Accepted
}

/// Records a failure and applies the scale of 2.4.
///
/// The scale is **cumulative and increasing**: three failures give thirty
/// seconds, five give five minutes, ten lock. The thresholds only reset
/// on a successful login -- failing which waiting out a delay would be
/// enough to start again from zero, and the attacker would gain a free
/// attempt every thirty seconds.
pub fn record_failure(user: &str) -> Verdict {
    let now = crate::idt::ticks();
    let mut guard = GUARD.lock();
    guard.failures += 1;
    let entry = guard.entry(user);
    entry.failures += 1;
    let failures = entry.failures;

    if failures >= LOCK_AFTER {
        entry.locked = true;
        guard.locks += 1;
        persist_lock(user, true);
        return Verdict::Locked;
    }
    if failures >= LONG_DELAY_AFTER {
        entry.blocked_until = now + ms_to_ticks(LONG_DELAY_MS);
        guard.throttles += 1;
        return Verdict::Throttled(LONG_DELAY_MS);
    }
    if failures >= DELAY_AFTER {
        entry.blocked_until = now + ms_to_ticks(DELAY_MS);
        guard.throttles += 1;
        return Verdict::Throttled(DELAY_MS);
    }
    Verdict::WrongPassword
}

fn record_success(user: &str) {
    let mut guard = GUARD.lock();
    guard.successes += 1;
    let entry = guard.entry(user);
    entry.failures = 0;
    entry.blocked_until = 0;
}

/// Unlocks an account: reserved to an administrator.
pub fn unlock(user: &str) -> bool {
    let mut guard = GUARD.lock();
    let entry = guard.entry(user);
    let was_locked = entry.locked;
    entry.locked = false;
    entry.failures = 0;
    entry.blocked_until = 0;
    drop(guard);
    persist_lock(user, false);
    was_locked
}

/// Resets the counter without touching the lock - for the self-test.
pub fn reset(user: &str) {
    let mut guard = GUARD.lock();
    let entry = guard.entry(user);
    entry.failures = 0;
    entry.blocked_until = 0;
    entry.locked = false;
}

pub fn counters() -> (u64, u64, u64, u64) {
    let guard = GUARD.lock();
    (
        guard.successes,
        guard.failures,
        guard.throttles,
        guard.locks,
    )
}

// ============================================================
//  Account file
// ============================================================

/// One line of the file: `name:salt_hex:digest_hex:locked`.
#[derive(Debug, Clone)]
pub struct Record {
    pub user: String,
    pub salt: [u8; 16],
    pub digest: [u8; 32],
    pub locked: bool,
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from_digit((byte >> 4) as u32, 16).unwrap_or('0'));
        out.push(char::from_digit((byte & 0xF) as u32, 16).unwrap_or('0'));
    }
    out
}

fn hex_decode(text: &str, out: &mut [u8]) -> bool {
    if text.len() != out.len() * 2 {
        return false;
    }
    for (index, slot) in out.iter_mut().enumerate() {
        let high = text.as_bytes()[index * 2] as char;
        let low = text.as_bytes()[index * 2 + 1] as char;
        let (Some(high), Some(low)) = (high.to_digit(16), low.to_digit(16)) else {
            return false;
        };
        *slot = (high * 16 + low) as u8;
    }
    true
}

/// Digest of a password for a given salt.
///
/// The salt precedes the password: suffixing it would allow, on some
/// constructions, extending an already hashed message. SHA-256 does not
/// have that defect, but the sound order costs nothing.
pub fn hash_password(salt: &[u8; 16], password: &str) -> [u8; 32] {
    let mut buffer = Vec::with_capacity(salt.len() + password.len());
    buffer.extend_from_slice(salt);
    buffer.extend_from_slice(password.as_bytes());
    sha256::digest(&buffer)
}

/// Pseudo-random salt.
///
/// **This is not a cryptographic source.** The tick counter and the wall
/// clock are predictable to whoever watches the machine. The salt fills
/// its main role -- two accounts with the same password have different
/// digests -- but not that of being unpredictable.
fn make_salt(user: &str) -> [u8; 16] {
    let mut seed = Vec::with_capacity(user.len() + 16);
    seed.extend_from_slice(user.as_bytes());
    seed.extend_from_slice(&crate::idt::ticks().to_le_bytes());
    seed.extend_from_slice(&crate::rtc::now().unwrap_or(0).to_le_bytes());
    let digest = sha256::digest(&seed);
    let mut salt = [0u8; 16];
    salt.copy_from_slice(&digest[..16]);
    salt
}

/// Reads the account file.
pub fn load_records() -> Vec<Record> {
    let mut records = Vec::new();
    if !crate::fs::is_ready() {
        return records;
    }
    let Ok(bytes) = crate::fs::read_file(CREDENTIALS_PATH, Credentials::kernel()) else {
        return records;
    };
    let Ok(text) = core::str::from_utf8(&bytes) else {
        return records;
    };

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fields = line.split(':');
        let (Some(user), Some(salt_hex), Some(digest_hex), Some(locked)) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let mut salt = [0u8; 16];
        let mut digest = [0u8; 32];
        if !hex_decode(salt_hex, &mut salt) || !hex_decode(digest_hex, &mut digest) {
            continue;
        }
        records.push(Record {
            user: String::from(user),
            salt,
            digest,
            locked: locked == "1",
        });
    }
    records
}

fn write_records(records: &[Record], now: u64) -> bool {
    if !crate::fs::is_ready() {
        return false;
    }
    let mut text = String::from("# Nova OS - empreintes de mots de passe (spec 2.4)\n");
    text.push_str("# nom:sel:sha256(sel||motdepasse):verrouille\n");
    for record in records {
        text.push_str(&format!(
            "{}:{}:{}:{}\n",
            record.user,
            hex_encode(&record.salt),
            hex_encode(&record.digest),
            u8::from(record.locked)
        ));
    }

    let who = Credentials::kernel();
    // The file is rewritten entirely: modifying it in place would demand
    // reading it back, replacing a line and writing it, for the same
    // result on a file of a few hundred bytes.
    let _ = crate::fs::delete(CREDENTIALS_PATH, who, now);
    // `private_file` and not `system_file`: the file belongs to ROOT and
    // nobody else must be able to read it. A digest readable by all is
    // broken offline at the speed of SHA-256.
    crate::fs::write_file(
        CREDENTIALS_PATH,
        text.as_bytes(),
        who,
        Mode::private_file(),
        now,
    )
    .is_ok()
}

fn persist_lock(user: &str, locked: bool) {
    let mut records = load_records();
    let Some(record) = records.iter_mut().find(|record| record.user == user) else {
        return;
    };
    record.locked = locked;
    write_records(&records, crate::idt::ticks());
}

/// Sets or replaces the password of an account.
pub fn set_password(user: &str, password: &str, now: u64) -> bool {
    let mut records = load_records();
    let salt = make_salt(user);
    let digest = hash_password(&salt, password);

    match records.iter_mut().find(|record| record.user == user) {
        Some(record) => {
            record.salt = salt;
            record.digest = digest;
        }
        None => records.push(Record {
            user: String::from(user),
            salt,
            digest,
            locked: false,
        }),
    }
    write_records(&records, now)
}

/// 7.7.1 -- Checks an account / password pair.
///
/// The order of the checks is not indifferent: the lock and the delay are
/// evaluated **before** the password. Checking the password first would
/// give, through the duration of the answer, a way to tell a delayed
/// account from a wrong password.
pub fn authenticate(user: &str, password: &str) -> Verdict {
    match status(user) {
        Verdict::Locked => return Verdict::Locked,
        Verdict::Throttled(remaining) => return Verdict::Throttled(remaining),
        _ => {}
    }

    let records = load_records();
    let Some(record) = records.iter().find(|record| record.user == user) else {
        // An unknown account counts as a failure: without that,
        // enumerating the accounts would be free.
        record_failure(user);
        return Verdict::UnknownAccount;
    };
    if record.locked {
        let mut guard = GUARD.lock();
        guard.entry(user).locked = true;
        return Verdict::Locked;
    }

    let candidate = hash_password(&record.salt, password);
    if sha256::equal(&candidate, &record.digest) {
        record_success(user);
        Verdict::Accepted
    } else {
        record_failure(user)
    }
}

/// Removes an account from the digest file.
///
/// Exists so that the self-test does not leave its own behind: the file
/// survives the reboot, and a test entry settling in permanently is
/// exactly the kind of residue phase 5 has already paid for once.
pub fn remove_record(user: &str, now: u64) -> bool {
    let mut records = load_records();
    let before = records.len();
    records.retain(|record| record.user != user);
    if records.len() == before {
        return false;
    }
    write_records(&records, now)
}

/// Creates the missing passwords.
///
/// The passwords are **equal to the account name**, which is
/// deliberately bad and deliberately said: with no SDK and no installer,
/// there is nobody to ask the user for one, and an account with no
/// password at all would be worse -- it would test nothing.
///
/// The check bears on **each account** and not on the existence of the
/// file. The difference is not theoretical: the first version stopped as
/// soon as the file held something, and it was enough for another
/// component to write in it first for no real account to get a password
/// -- hence for nobody to be able to open a session. A global existence
/// test says nothing of what is missing.
pub fn install_default_credentials(now: u64) -> usize {
    if !crate::fs::is_ready() {
        return 0;
    }
    let existing = load_records();
    let mut created = 0;

    for (name, _role, _uid) in crate::fs::account_names() {
        if existing.iter().any(|record| record.user == name) {
            continue;
        }
        if set_password(&name, &name, now) {
            created += 1;
        }
    }

    if created != 0 {
        crate::serial_println!(
            "[login] {} mot(s) de passe par defaut installes - egaux au nom du compte",
            created
        );
    }
    created
}

// ============================================================
//  7.7.1 -- Geometry of the panel
// ============================================================

/// Where the pointer landed.
///
/// `Elsewhere` is a case in its own right and not an absence: a click
/// beside the field must take the focus away from it, which is a decision
/// as real as taking it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// Row of an account, by its rank in the list.
    Account(usize),
    /// The password field.
    Password,
    /// The arrow that validates, beside the field.
    Submit,
    /// The Accessibility button of 2.4.
    Accessibility,
    /// Anywhere else on the screen.
    Elsewhere,
}

impl Target {
    /// Shape the pointer takes over this target.
    ///
    /// This is the whole of the rule asked of the cursor: "you may click
    /// here", "you may type here", nothing otherwise. The mapping lives
    /// here and not in the compositor because the compositor knows which
    /// surface the pointer is over and nothing of what is inside it.
    pub fn cursor(&self) -> super::protocol::CursorShape {
        use super::protocol::CursorShape;
        match self {
            Target::Account(_) | Target::Submit | Target::Accessibility => CursorShape::Hand,
            Target::Password => CursorShape::Text,
            Target::Elsewhere => CursorShape::Arrow,
        }
    }
}

/// Geometry of the login panel, computed once.
///
/// ## Why this exists rather than the arithmetic inlined in `draw`
///
/// The hit test and the painting must agree on the pixel. The first
/// version of the cursor damage rectangle was written "14 by 20" while
/// `cursor.mod` creates a 16 by 16 surface, and the three right-hand
/// columns were never dirtied -- a defect invisible for as long as the
/// arrow stayed narrow. Two places computing the same rectangle is that
/// defect waiting to happen. Here it would read as "the click does not
/// land where the row is drawn", and nothing would point at the cause.
///
/// So `draw` reads its rectangles here, and `hit` reads the same ones.
///
/// ## Coordinate space
///
/// The fields are in **surface-local** coordinates. The login surface is
/// created at the origin and at screen size, so they coincide with the
/// screen coordinates carried by a `PointerEvent`; `hit` relies on that
/// and says so. Should the panel ever get its own smaller surface, the
/// translation would have to be subtracted there and nowhere else.
#[derive(Debug, Clone, Copy)]
pub struct Layout {
    pub panel_x: i32,
    pub panel_y: i32,
    pub panel_width: i32,
    pub panel_height: i32,
    pub padding: i32,
    pub row_height: i32,
    pub glyph_width: i32,
    pub glyph_height: i32,
    /// Text baseline of the first account row.
    pub first_row_y: i32,
    pub accounts: usize,
    /// Password field: x, y, width, height.
    pub field: (i32, i32, i32, i32),
    /// Validation arrow, to the right of the field: x, y, width, height.
    pub submit: (i32, i32, i32, i32),
    /// Accessibility button: x, y, width, height.
    pub button: (i32, i32, i32, i32),
    /// Baseline of the "Mot de passe" label.
    pub label_y: i32,
    /// Baseline of the message line.
    pub message_y: i32,
}

/// The two labels of the Accessibility button.
///
/// The rectangle is sized on the **longer** of the two, whatever the
/// current state. A button that shrinks when it is pressed moves out from
/// under the pointer that pressed it, and the second click misses.
const ACCESS_OFF: &str = "[F1] Accessibilite";
const ACCESS_ON: &str = "[F1] Accessibilite : active";

/// Side of the validation button, in pixels.
///
/// Square and as tall as the field, so the two read as one control. The
/// field gives up exactly this much width plus a gap -- taken from the
/// field and not added to the panel, because the panel width is what
/// centres it on screen and a panel that widens with a button would move
/// everything else.
const SUBMIT_SIDE: i32 = 26;

impl Layout {
    pub fn new(width: i32, height: i32, theme: &super::theme::Theme, accounts: usize) -> Self {
        let glyph_width = crate::graphics::font::GLYPH_WIDTH as i32;
        let glyph_height = crate::graphics::font::GLYPH_HEIGHT as i32;
        let padding = theme.padding as i32;

        let panel_width = 420;
        let row_height = 26;
        let panel_height = (accounts as i32 + 6) * row_height + 4 * padding;
        let panel_x = (width - panel_width) / 2;
        let panel_y = (height - panel_height) / 2;

        let first_row_y = panel_y + padding + row_height;

        // Same walk down the panel as the painting does, kept in one
        // place: label, then field, then message line.
        let label_y = first_row_y + accounts as i32 * row_height + padding;
        let field_y = label_y + glyph_height + 4;
        let field_x = panel_x + padding * 3;
        // The field and the arrow share the width the field used to have
        // on its own: `padding * 6` of margins, the gap, and the button.
        let field_width = panel_width - 6 * padding - SUBMIT_SIDE - padding;
        let field = (field_x, field_y, field_width, row_height);
        let submit = (
            field_x + field_width + padding,
            field_y,
            SUBMIT_SIDE,
            row_height,
        );
        let message_y = field_y + row_height + padding;

        let longest = ACCESS_OFF.len().max(ACCESS_ON.len()) as i32;
        let button = (
            panel_x + padding,
            panel_y + panel_height - row_height - padding,
            glyph_width * longest + 2 * padding,
            row_height,
        );

        Layout {
            panel_x,
            panel_y,
            panel_width,
            panel_height,
            padding,
            row_height,
            glyph_width,
            glyph_height,
            first_row_y,
            accounts,
            field,
            submit,
            button,
            label_y,
            message_y,
        }
    }

    /// Highlight rectangle of an account row: x, y, width, height.
    ///
    /// This is also its clickable area. Drawing the selection over one
    /// rectangle and testing the click against another would put the two
    /// out of step by the three pixels of the vertical offset.
    pub fn row(&self, index: usize) -> (i32, i32, i32, i32) {
        (
            self.panel_x + self.padding,
            self.first_row_y + index as i32 * self.row_height - 3,
            self.panel_width - 2 * self.padding,
            self.row_height - 2,
        )
    }

    fn inside(rect: (i32, i32, i32, i32), x: i32, y: i32) -> bool {
        x >= rect.0 && x < rect.0 + rect.2 && y >= rect.1 && y < rect.1 + rect.3
    }

    /// What is under a point, in screen coordinates.
    ///
    /// The order of the tests matters where the rectangles could touch:
    /// the button is tested before the rows, because it is the one the
    /// panel height is measured against and an off-by-one in the account
    /// count must not make it unreachable.
    pub fn hit(&self, x: i32, y: i32) -> Target {
        if Self::inside(self.button, x, y) {
            return Target::Accessibility;
        }
        // The arrow before the field: they are adjacent, and testing the
        // field first would be correct only for as long as the gap between
        // them stays positive.
        if Self::inside(self.submit, x, y) {
            return Target::Submit;
        }
        if Self::inside(self.field, x, y) {
            return Target::Password;
        }
        for index in 0..self.accounts {
            if Self::inside(self.row(index), x, y) {
                return Target::Account(index);
            }
        }
        Target::Elsewhere
    }
}

// ============================================================
//  7.7.1 -- The login surface
// ============================================================

/// Where the printable keystrokes go.
///
/// ## Why the state is needed at all
///
/// Before the pointer there was nothing to choose: every printable key
/// went into the password. A click in the field that only painted a
/// brighter border would be the F1 button of before step 10.1 -- it said
/// accessibility was on while nothing was on. A focus that changes the
/// colour and nothing else is the same lie.
///
/// So the focus commands the keyboard: `Password` types, `Accounts` does
/// not. The initial state is `Password`, which leaves the documented
/// keyboard path untouched -- `admin` is preselected, one types, one
/// presses Enter, and no click is needed anywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// The account list has the keyboard: printable keys do not type.
    Accounts,
    /// The password field has the keyboard.
    Password,
}

/// What the screen is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScreenMode {
    /// No session: every account is offered, and success opens one.
    Login,
    /// The session of this account is locked: only it is offered, and
    /// success lifts the panel without reopening anything -- the windows,
    /// the theme and the modules never went away.
    Unlock(String),
}

/// State of the entry, shared between the task and the keyboard interception.
struct Session {
    mode: ScreenMode,
    users: Vec<(String, Role)>,
    selected: usize,
    password: String,
    message: String,
    accessibility: bool,
    focus: Focus,
    /// Target under the pointer, for the hover effect.
    hover: Target,
    /// Pointer events received, and repaints the hover cost.
    ///
    /// Counted because the claim "the hover repaints only when the target
    /// changes" is otherwise unverifiable, and an unverifiable claim about
    /// a cost is how this feature was refused in the first place. The two
    /// numbers are printed when the session opens; their ratio is the
    /// measurement.
    pointer_events: u64,
    hover_repaints: u64,
    authenticated: Option<String>,
    dirty: bool,
    active: bool,
}

static SESSION: SpinLock<Option<Session>> = SpinLock::new(None);

/// The logged-in account, once the session is open.
///
/// A locked session is still open: the account behind the lock screen is
/// the current user, even though nobody has typed its password yet.
pub fn current_user() -> Option<String> {
    SESSION.lock().as_ref().and_then(|session| {
        session.authenticated.clone().or(match &session.mode {
            ScreenMode::Unlock(user) => Some(user.clone()),
            ScreenMode::Login => None,
        })
    })
}

pub fn is_showing() -> bool {
    SESSION
        .lock()
        .as_ref()
        .map(|session| session.active)
        .unwrap_or(false)
}

/// Runs an attempt and writes the verdict into the session.
///
/// Extracted because two paths reach it now -- Enter and, one day, a
/// validation button -- and because the ordering constraint it carries is
/// easy to lose: `authenticate` reads the filesystem, so the session lock
/// must be **released** before calling it. Holding a lock across I/O is
/// what DECISIONS.md forbids, and the rule is invisible when the call is
/// buried in a `match` arm.
fn submit() {
    // Extracted under the lock, used outside it.
    let (user, password) = {
        let guard = SESSION.lock();
        let Some(session) = guard.as_ref() else {
            return;
        };
        let Some((user, _)) = session.users.get(session.selected) else {
            return;
        };
        (user.clone(), session.password.clone())
    };

    let verdict = authenticate(&user, &password);

    let mut guard = SESSION.lock();
    let Some(session) = guard.as_mut() else {
        return;
    };
    session.password.clear();
    session.dirty = true;
    session.message = match verdict {
        Verdict::Accepted => {
            session.authenticated = Some(user.clone());
            session.active = false;
            format!("bienvenue, {}", user)
        }
        Verdict::WrongPassword => String::from("mot de passe incorrect"),
        Verdict::UnknownAccount => String::from("compte inconnu"),
        Verdict::Throttled(ms) => format!("trop d'echecs - reessayez dans {} s", ms / 1000),
        Verdict::Locked => String::from("compte verrouille - voir un administrateur"),
    };
    drop(guard);
    crate::serial_println!("[login] \"{}\" : {}", user, verdict.as_str());
}

/// Toggles the Accessibility feature of 2.4.
///
/// Shared by F1 and by the click on the button: two entry points for one
/// behaviour, and the reader announcement must not depend on which one
/// was used.
fn toggle_accessibility(session: &mut Session) {
    // Until step 10.1 this flipped a flag and painted a different colour,
    // and nothing else: a button that says "accessibility is on" while
    // nothing is on is worse than no button. It now switches on what a
    // person who cannot see the screen needs to get through this screen
    // -- the reader and keyboard navigation -- and announces the account
    // under the cursor, which is the only thing worth hearing here.
    session.accessibility = !session.accessibility;
    crate::access::set(crate::access::Feature::Reader, session.accessibility);
    crate::access::set(crate::access::Feature::Keyboard, session.accessibility);
    if session.accessibility
        && let Some((user, _)) = session.users.get(session.selected)
    {
        crate::access::reader::say(&alloc::format!(
            "{} {}",
            crate::i18n::translate("login.titre", &[]),
            user
        ));
    }
    session.dirty = true;
}

/// Selects an account and gets ready for the entry.
///
/// 2.4 reads "selection then password entry": having picked an account,
/// the next thing anybody does is type, so the focus follows. The
/// password and the message are cleared for the same reason Tab clears
/// them -- what was typed was meant for the previous account.
fn select_account(session: &mut Session, index: usize) {
    if index >= session.users.len() {
        return;
    }
    session.selected = index;
    session.password.clear();
    session.message.clear();
    session.focus = Focus::Password;
    session.dirty = true;
    if session.accessibility
        && let Some((user, _)) = session.users.get(index)
    {
        crate::access::reader::say(user);
    }
}

/// 7.7.2 and 16.4 -- Keyboard interception by the login screen.
///
/// Returns `true` when the key has been consumed. The login screen
/// **must** intercept before the normal routing: it is modal, and a
/// keystroke slipping to a background window during the entry of a
/// password would be exactly the leak we do not want.
pub fn intercept(key: KeyEvent) -> bool {
    let mut guard = SESSION.lock();
    let Some(session) = guard.as_mut() else {
        return false;
    };
    if !session.active {
        return false;
    }
    if !key.pressed {
        return true;
    }

    match key.scancode {
        SCAN_TAB => {
            if session.users.is_empty() {
                return true;
            }
            let next = (session.selected + 1) % session.users.len();
            // Tab also brings the focus back to the field. Without that,
            // a click beside the panel would leave a keyboard-only user
            // with no key that returns to the entry -- which would be a
            // pointer making the keyboard worse.
            select_account(session, next);
        }
        SCAN_BACKSPACE => {
            if session.focus == Focus::Password {
                session.password.pop();
                session.dirty = true;
            }
        }
        SCAN_ESCAPE => {
            // Escape is the way out of any state: it empties the entry and
            // hands the keyboard back to the field.
            session.password.clear();
            session.focus = Focus::Password;
            session.dirty = true;
        }
        SCAN_F1 => {
            // The Accessibility button of 2.4, reachable by keyboard as
            // 16.4 demands -- and by click since the pointer arrived.
            toggle_accessibility(session);
        }
        SCAN_ENTER => {
            if session.focus == Focus::Accounts {
                // The account list has the keyboard: Enter moves on to
                // the entry rather than submitting an empty password,
                // which is the "selection then entry" order of 2.4.
                session.focus = Focus::Password;
                session.message.clear();
                session.dirty = true;
                return true;
            }
            // The lock is released before the authentication: that one
            // reads the filesystem, and holding a lock across I/O is what
            // DECISIONS.md forbids.
            drop(guard);
            submit();
            return true;
        }
        _ => {
            if session.focus != Focus::Password {
                // A printable key with the field unfocused says where it
                // should have gone. Swallowing it silently would leave
                // somebody typing a whole password into nothing.
                if key.character().is_some() {
                    session.message =
                        String::from("cliquez dans le champ - ou Entree - pour saisir");
                    session.dirty = true;
                }
                return true;
            }
            if let Some(character) = key.character()
                && session.password.len() < MAX_PASSWORD
                && (0x20..=0x7e).contains(&(character as u32))
            {
                session.password.push(character);
                session.dirty = true;
            }
        }
    }
    true
}

/// 7.7.1 -- Click on the login screen.
///
/// ## Why this is not symmetric with `intercept`
///
/// The keyboard is intercepted in `gui::route_input`, **before**
/// `compositor::route_key`, because the login screen is modal and a
/// keystroke must not reach a window behind it. The pointer needs nothing
/// of the kind: the login surface is `Layer::Modal` at screen size, so
/// `Stack::hit` already designates it for every click and `route_pointer`
/// posts `EV_POINTER` to this task. Intercepting upstream would also have
/// starved the cursor -- the overlay must keep receiving the event, which
/// is decision 33 of DECISIONS.md.
///
/// ## Why only the pressing edge
///
/// A pointer publishes an event per movement. Acting on the state rather
/// than on the transition would repaint the panel a score of times per
/// millimetre of mouse, for a display that does not depend on the
/// position -- the same reason `taskbar.mod` ignores a hover.
///
/// `was_down` is the button state at the previous event. Returns `true`
/// when the panel must be repainted.
pub fn on_pointer(layout: &Layout, event: super::protocol::PointerEvent, was_down: bool) -> bool {
    let target = layout.hit(event.x as i32, event.y as i32);
    let down = event.pressed(super::protocol::PointerEvent::BUTTON_LEFT);

    let mut guard = SESSION.lock();
    let Some(session) = guard.as_mut() else {
        return false;
    };
    if !session.active {
        return false;
    }

    // --- hover ---
    //
    // Repainted only when the target **changes**, never on every movement.
    // That is the whole answer to the cost this used to be refused for: a
    // pointer publishes an event per movement, so acting on the position
    // would repaint the panel a score of times per millimetre of mouse.
    // Acting on the boundary makes it one repaint per boundary crossed,
    // which is a handful a second at most and is what anybody can see.
    session.pointer_events += 1;
    let entered = session.hover != target;
    if entered {
        session.hover = target;
        session.hover_repaints += 1;
        session.dirty = true;
    }

    if !down || was_down {
        return entered;
    }

    match target {
        Target::Account(index) => {
            select_account(session, index);
            let name = session
                .users
                .get(index)
                .map(|(user, _)| user.clone())
                .unwrap_or_default();
            drop(guard);
            crate::serial_println!("[login] clic : compte \"{}\" selectionne", name);
            true
        }
        Target::Submit => {
            // The arrow is the pointer's Enter, and nothing more: the same
            // `submit`, hence the same 7.7.2 scale. An empty field
            // therefore counts a failure here exactly as it does on Enter.
            // Refusing it on the arrow alone would give two rules for one
            // behaviour, and greying out a button that still acts is the
            // lie of entry 37 of DECISIONS.md.
            //
            // The focus follows: whoever clicked the arrow was entering a
            // password, and a refused attempt must leave the keyboard
            // where it can be retyped.
            session.focus = Focus::Password;
            drop(guard);
            crate::serial_println!("[login] clic : validation par la fleche");
            submit();
            true
        }
        Target::Password => {
            let changed = session.focus != Focus::Password;
            session.focus = Focus::Password;
            session.message.clear();
            session.dirty = true;
            drop(guard);
            if changed {
                crate::serial_println!("[login] clic : focus au champ de mot de passe");
            }
            true
        }
        Target::Accessibility => {
            toggle_accessibility(session);
            let state = session.accessibility;
            drop(guard);
            crate::serial_println!(
                "[login] clic : accessibilite {}",
                if state { "activee" } else { "desactivee" }
            );
            true
        }
        Target::Elsewhere => {
            // Taking the focus away is a decision, not the absence of one:
            // it is what makes the click in the field mean something. The
            // password is **kept** -- losing an entry because the pointer
            // slipped beside the panel would be a punishment, not a
            // behaviour.
            if session.focus == Focus::Accounts {
                return entered;
            }
            session.focus = Focus::Accounts;
            session.dirty = true;
            true
        }
    }
}

/// Installs the state of the screen, which takes the keyboard at once.
///
/// Separate from the task because a lock must take the keyboard **before**
/// the task that paints the panel is even elected: `intercept` swallows
/// every key from the moment `active` is set, while the surface only
/// appears once the new task has connected to the compositor.
///
/// Returns `false` when there is no account to offer -- for `Unlock`, when
/// the account in session no longer exists.
pub fn begin(mode: ScreenMode) -> bool {
    let users: Vec<(String, Role)> = crate::fs::account_names()
        .into_iter()
        // ROOT is never used directly (5.2): offering it on the screen
        // would be offering an account that must not be used.
        .filter(|(_, role, _)| *role != Role::Root && *role != Role::Service)
        .filter(|(name, _, _)| match &mode {
            ScreenMode::Login => true,
            ScreenMode::Unlock(owner) => name == owner,
        })
        .map(|(name, role, _)| (name, role))
        .collect();

    if matches!(mode, ScreenMode::Unlock(_)) && users.is_empty() {
        return false;
    }

    *SESSION.lock() = Some(Session {
        mode,
        users,
        selected: 0,
        password: String::new(),
        message: String::new(),
        accessibility: false,
        focus: Focus::Password,
        hover: Target::Elsewhere,
        pointer_events: 0,
        hover_repaints: 0,
        authenticated: None,
        dirty: true,
        active: true,
    });
    true
}

/// The `login.ss` task, at boot.
pub fn task() -> ! {
    begin(ScreenMode::Login);
    resume_task()
}

/// The `login.ss` task, on a state `begin` already installed.
///
/// Spawned by `session` after a lock or a log out.
pub fn resume_task() -> ! {
    let Some((users, mode)) = SESSION
        .lock()
        .as_ref()
        .map(|session| (session.users.clone(), session.mode.clone()))
    else {
        crate::serial_println!("[login] aucun etat installe - ecran non affiche");
        crate::scheduler::exit_current_with(-1);
    };

    let Some(mut client) = Client::connect(5_000) else {
        crate::serial_println!("[login] compositeur injoignable");
        crate::scheduler::exit_current_with(-1);
    };
    let Some((width, height, _, _)) = client.screen_info() else {
        crate::scheduler::exit_current_with(-1);
    };
    let (mut width, mut height) = (width as i32, height as i32);
    let theme = client.theme().unwrap_or_default();
    let title = match &mode {
        ScreenMode::Login => "Connexion a Nova OS",
        ScreenMode::Unlock(_) => "Session verrouillee",
    };

    let Some(surface) = client.create_surface(Layer::Modal, 0, 0, width, height, "login") else {
        crate::serial_println!("[login] creation de surface refusee");
        crate::scheduler::exit_current_with(-1);
    };

    // Computed once, and again only when the screen changes size: the
    // geometry depends on the screen, the theme and the number of
    // accounts, and only the first can change while the panel is up.
    let mut layout = Layout::new(width, height, &theme, users.len());

    crate::serial_println!(
        "[login] ecran affiche ({}) - {} compte(s), surface {}",
        match mode {
            ScreenMode::Login => "connexion",
            ScreenMode::Unlock(_) => "deverrouillage",
        },
        users.len(),
        surface
    );

    // Button state at the previous event, held by the task and not by the
    // session: only this loop reads the pointer, and the keyboard
    // interception has no use for it.
    let mut pointer_down = false;

    loop {
        while let Some(event) = client.poll_event() {
            match &event {
                Event::Other(message) if super::stop_requested(message) => {
                    client.destroy_surface(surface);
                    crate::scheduler::exit_current_with(0);
                }
                // The panel is centred and the surface covers the screen:
                // both are redone, and the panel repainted at once -- a
                // modal screen half off the display cannot be used.
                Event::ScreenResized {
                    width: new_width,
                    height: new_height,
                } => {
                    width = *new_width;
                    height = *new_height;
                    layout = Layout::new(width, height, &theme, users.len());
                    client.resize_surface(surface, width, height);
                    if let Some(session) = SESSION.lock().as_mut() {
                        session.dirty = true;
                    }
                }
                Event::Pointer(pointer) => {
                    let was_down = pointer_down;
                    pointer_down = pointer.pressed(super::protocol::PointerEvent::BUTTON_LEFT);
                    on_pointer(&layout, *pointer, was_down);

                    // Compared against the shape the **event carries**, not
                    // against a shape remembered here. The event holds what
                    // the compositor currently has, so there is nothing to
                    // remember -- and a memory would go stale the moment
                    // the compositor resets the shape on its own, which it
                    // does whenever the pointer leaves a surface so that no
                    // client can strand its cursor on the desktop. The
                    // comparison is therefore self-correcting: whatever
                    // happened in between, the shape converges within one
                    // movement.
                    let wanted = layout.hit(pointer.x as i32, pointer.y as i32).cursor();
                    if pointer.cursor_shape() != wanted {
                        client.set_cursor(wanted);
                    }
                }
                _ => {}
            }
        }

        let snapshot = {
            let mut guard = SESSION.lock();
            let Some(session) = guard.as_mut() else {
                crate::scheduler::exit_current_with(0);
            };
            let dirty = session.dirty;
            session.dirty = false;
            (
                dirty,
                session.active,
                session.selected,
                session.password.len(),
                session.message.clone(),
                session.accessibility,
                session.authenticated.clone(),
                session.focus,
                session.hover,
            )
        };

        if let Some(user) = snapshot.6.clone() {
            client.destroy_surface(surface);
            let (events, repaints) = {
                let guard = SESSION.lock();
                guard
                    .as_ref()
                    .map(|session| (session.pointer_events, session.hover_repaints))
                    .unwrap_or((0, 0))
            };
            crate::serial_println!(
                "[login] pointeur : {} evenement(s), {} repeint(s) de survol",
                events,
                repaints
            );
            match mode {
                ScreenMode::Login => {
                    crate::serial_println!("[login] session ouverte pour \"{}\"", user);
                    open_session(&user);
                }
                ScreenMode::Unlock(_) => {
                    // Nothing to reopen: lifting the lock is lifting the
                    // flag that makes the compositor refuse session
                    // actions, and repainting what the panel covered.
                    super::with(|comp| {
                        comp.session_locked = false;
                        comp.damage_all();
                    });
                    crate::serial_println!("[login] session deverrouillee pour \"{}\"", user);
                }
            }
            crate::scheduler::exit_current_with(0);
        }

        if snapshot.0 {
            draw(
                &mut client,
                surface,
                &layout,
                width,
                &theme,
                title,
                &users,
                snapshot.2,
                snapshot.3,
                &snapshot.4,
                snapshot.5,
                snapshot.7,
                snapshot.8,
            );
        }

        let _ = crate::syscall::invoke(
            crate::syscall::domain_time::SYS_TIME_SLEEP_MS,
            [50, 0, 0, 0, 0, 0],
        );
    }
}

/// Applies the theme of the account and relaunches the modules with its priority.
fn open_session(user: &str) {
    super::with(|comp| {
        comp.session_user = Some(String::from(user));
    });

    // 14.1 and 16 -- The account's own locale and accessibility settings.
    //
    // Before the theme and before the module sweep: the modules read
    // their strings when they start, and relaunching them ahead of the
    // reload would give them the previous account's language. It is the
    // same ordering argument the theme reload below makes, and it applies
    // one step earlier.
    crate::i18n::adopt_user(user);
    crate::access::adopt_user(user);
    // Every tab of `parametres.ss` -- the audio, the display, and whatever
    // registers after them -- takes the account's values.
    crate::config::adopt_user(user);

    use super::protocol::{self as proto};
    // Reload the theme and re-sweep the modules in that order: the
    // modules read the theme when they start, relaunching them before the
    // reload would give them the old one.
    let (theme, origin) = super::theme::load_for(Some(user));
    super::with(|comp| {
        comp.theme = theme;
        comp.center.session_active = true;
        comp.damage_all();
    });
    crate::serial_println!(
        "[login] theme de \"{}\" : {} cles systeme, {} cles utilisateur ({})",
        user,
        origin.system_keys,
        origin.user_keys,
        if origin.user_present {
            "theme personnel present"
        } else {
            "aucun theme personnel"
        }
    );

    let active = super::rescan_modules(Some(user));
    crate::serial_println!(
        "[login] {} module(s) actif(s) apres ouverture de session",
        active
    );
    let _ = proto::OP_THEME_RELOAD;

    // 9.1: a NORMAL, which makes a toast while the session is active and
    // disappears after five seconds. It is also the only path of 9 that
    // is exercised end to end at boot -- emission, display by
    // `notifs.mod`, expiry.
    super::with(|comp| {
        super::compositor::post_notification(
            comp,
            super::notify::Level::Normale,
            "Session ouverte",
            &alloc::format!("Bienvenue, {}", user),
        )
    });
}

#[allow(clippy::too_many_arguments)]
fn draw(
    client: &mut Client,
    surface: super::surface::SurfaceId,
    layout: &Layout,
    width: i32,
    theme: &super::theme::Theme,
    title: &str,
    users: &[(String, Role)],
    selected: usize,
    password_len: usize,
    message: &str,
    accessibility: bool,
    focus: Focus,
    hover: Target,
) {
    let padding = layout.padding;
    let glyph_width = layout.glyph_width;
    let glyph_height = layout.glyph_height;
    let row_height = layout.row_height;

    client.reset(surface);
    client.fill(surface, theme.background);

    client.rect(
        surface,
        layout.panel_x,
        layout.panel_y,
        layout.panel_width,
        layout.panel_height,
        theme.surface,
    );
    client.frame(
        surface,
        layout.panel_x,
        layout.panel_y,
        layout.panel_width,
        layout.panel_height,
        theme.border,
        theme.border_width.max(1) as u8,
    );

    client.text(
        surface,
        layout.panel_x + (layout.panel_width - glyph_width * title.len() as i32) / 2,
        layout.panel_y + padding,
        title,
        theme.accent,
        None,
    );

    // --- accounts ---
    for (index, (name, role)) in users.iter().enumerate() {
        let chosen = index == selected;
        let hovered = hover == Target::Account(index);
        let (row_x, row_y, row_w, row_h) = layout.row(index);
        let baseline = layout.first_row_y + index as i32 * row_height;
        // Three states and three shades, and the middle one is the reason
        // `mix` exists: a hovered row painted in `surface_alt` would look
        // exactly like the selected one, so the pointer would appear to
        // select whatever it passes over. It gets a third of the way there
        // instead -- enough to follow the mouse, not enough to be mistaken
        // for a choice. A selected row that is also hovered goes the other
        // way, towards the accent, so that hovering the current account
        // still answers.
        let background = match (chosen, hovered) {
            (true, true) => mix(theme.surface_alt, theme.accent, 56),
            (true, false) => theme.surface_alt,
            (false, true) => mix(theme.surface, theme.surface_alt, 85),
            (false, false) => theme.surface,
        };
        if chosen || hovered {
            client.rect(surface, row_x, row_y, row_w, row_h, background);
        }
        if chosen {
            client.rect(surface, row_x, row_y, 3, row_h, theme.accent);
        } else if hovered {
            // A thinner marker than the selected one: it says "here",
            // where three pixels of accent say "this one".
            client.rect(surface, row_x, row_y, 1, row_h, theme.text_dim);
        }
        let label = format!("{}  ({})", name, role.as_str());
        client.text(
            surface,
            layout.panel_x + padding * 3,
            baseline,
            &label,
            if chosen || hovered {
                theme.text
            } else {
                theme.text_dim
            },
            None,
        );

        // 7.7.2: the anti-bruteforce state is visible before the entry,
        // not only after a failure.
        let hint = match status(name) {
            Verdict::Locked => String::from("verrouille"),
            Verdict::Throttled(ms) => format!("{} s", ms.div_ceil(1000)),
            _ => String::new(),
        };
        if !hint.is_empty() {
            client.text(
                surface,
                layout.panel_x + layout.panel_width
                    - padding * 2
                    - glyph_width * hint.len() as i32,
                baseline,
                &hint,
                theme.danger,
                None,
            );
        }
    }

    // --- password field ---
    client.text(
        surface,
        layout.panel_x + padding * 3,
        layout.label_y,
        "Mot de passe",
        theme.text_dim,
        None,
    );

    let (field_x, field_y, field_w, field_h) = layout.field;
    let field_hovered = hover == Target::Password;
    client.rect(
        surface,
        field_x,
        field_y,
        field_w,
        field_h,
        if field_hovered {
            mix(theme.background, theme.surface, 110)
        } else {
            theme.background
        },
    );
    // The focused field is framed in the accent colour and two pixels
    // thick. The colour alone would not do: a theme is free to give
    // `accent` and `border` a close pair, and the only cue telling where
    // the keyboard goes must not be able to vanish into a palette.
    // Hover and focus are two different things and must not be drawn the
    // same: the hover says where the mouse is, the focus says where the
    // keyboard goes. An unfocused field under the pointer therefore gets a
    // brighter border, not the accent one -- which is reserved for the
    // focus and would otherwise claim the keyboard is here when it is not.
    let focused = focus == Focus::Password;
    client.frame(
        surface,
        field_x,
        field_y,
        field_w,
        field_h,
        match (focused, field_hovered) {
            (true, _) => theme.accent,
            (false, true) => theme.text_dim,
            (false, false) => theme.border,
        },
        if focused { 2 } else { 1 },
    );

    // The password is never displayed, even masked character by character
    // with its exact length: that is already information. A fixed number
    // of dots beyond eight characters blurs it.
    let dots = password_len.min(8);
    let shown: String = core::iter::repeat_n('*', dots).collect();
    let suffix = if password_len > 8 { "..." } else { "" };
    let text_y = field_y + (field_h - glyph_height) / 2;
    client.text(
        surface,
        field_x + padding,
        text_y,
        &format!("{}{}", shown, suffix),
        theme.text,
        None,
    );

    // A caret after the last star, drawn only when the field has the
    // keyboard. It is what makes the focus readable at a glance, and it is
    // steady rather than blinking: a blink would demand a repaint twice a
    // second for the whole panel.
    if focused {
        let caret_x = field_x + padding + glyph_width * (shown.len() + suffix.len()) as i32;
        client.rect(surface, caret_x, text_y, 1, glyph_height, theme.accent);
    }

    // --- validation arrow, beside the field ---
    //
    // Drawn and not written: the embedded font is ASCII, and the rule of
    // section 4 forbids a byte outside 0x20..0x7E in a displayed string.
    // A "->" in text would have been legible but would have read as two
    // characters in a field of stars; an arrow glyph does not exist in the
    // font and the VGA writer would have painted a solid square.
    let (submit_x, submit_y, submit_w, submit_h) = layout.submit;
    // Towards white rather than towards another theme colour: the button is
    // already the accent, and a theme is free to give `accent` and every
    // other colour whatever pair it likes. Lightening what is there cannot
    // collide with anything.
    client.rect(
        surface,
        submit_x,
        submit_y,
        submit_w,
        submit_h,
        if hover == Target::Submit {
            mix(theme.accent, WHITE, 60)
        } else {
            theme.accent
        },
    );
    client.frame(
        surface,
        submit_x,
        submit_y,
        submit_w,
        submit_h,
        theme.border,
        1,
    );

    // Shaft then head, from the centre of the button. The head is built
    // from vertical segments of growing length rather than from a filled
    // triangle primitive: the protocol has no triangle, and six `rect`
    // commands cost less than teaching it one.
    let centre_x = submit_x + submit_w / 2;
    let centre_y = submit_y + submit_h / 2;
    client.rect(
        surface,
        centre_x - 7,
        centre_y - 1,
        9,
        3,
        theme.background,
    );
    for step in 0..6i32 {
        client.rect(
            surface,
            centre_x + 5 - step,
            centre_y - step,
            1,
            2 * step + 1,
            theme.background,
        );
    }

    if !message.is_empty() {
        client.text(
            surface,
            layout.panel_x + padding * 3,
            layout.message_y,
            message,
            theme.danger,
            None,
        );
    }

    // --- Accessibility button, always visible (2.4) ---
    let button = if accessibility { ACCESS_ON } else { ACCESS_OFF };
    let (button_x, button_y, button_w, button_h) = layout.button;
    let button_base = if accessibility {
        theme.accent
    } else {
        theme.surface_alt
    };
    client.rect(
        surface,
        button_x,
        button_y,
        button_w,
        button_h,
        if hover == Target::Accessibility {
            mix(button_base, WHITE, 45)
        } else {
            button_base
        },
    );
    client.text(
        surface,
        button_x + padding,
        button_y + (button_h - glyph_height) / 2,
        button,
        if accessibility {
            theme.background
        } else {
            theme.text
        },
        None,
    );

    let help = "Clic : choisir un compte    Tab : suivant    Fleche ou Entree : valider";
    client.text(
        surface,
        (width - glyph_width * help.len() as i32) / 2,
        layout.panel_y + layout.panel_height + padding * 2,
        help,
        theme.text_dim,
        None,
    );

    client.commit(surface);
}

pub fn dump_state() {
    let (successes, failures, throttles, locks) = counters();
    crate::serial_println!(
        "[login] {} connexion(s), {} echec(s), {} temporisation(s), {} verrouillage(s)",
        successes,
        failures,
        throttles,
        locks
    );
    let records = load_records();
    crate::serial_println!("[login] {} compte(s) avec mot de passe", records.len());
    for record in records.iter() {
        crate::serial_println!(
            "[login]   {:<10} sel {} - {}",
            record.user,
            &hex_encode(&record.salt)[..8],
            if record.locked { "VERROUILLE" } else { "ouvert" }
        );
    }
}
