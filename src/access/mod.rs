//! Phase 10.1 -- Accessibility (spec 16).
//!
//! | Module | Covers |
//! |---|---|
//! | `tree` | 10.1.1 -- the accessibility tree parallel to the render tree |
//! | `speech` | 10.1.2 -- formant speech synthesis, in integer arithmetic |
//! | `reader` | 10.1.2 -- `screenreader.mod`, what it announces and when |
//! | `zoom` | 10.1.3 -- global zoom, magnifier, area |
//! | `contrast` | 10.1.4 -- high contrast and the five modes of 16.3 |
//! | `keynav` | 10.1.5 -- the 16.4 key table and focus movement |
//!
//! ## Five modules, five slots
//!
//! 16.2 lists five `.mod` files and marks all five replaceable. Phase 7
//! had one `Accessibility` slot, which would have made four of them share
//! it -- and a slot holds one module at a time, which is what gives
//! "replaceable" its meaning. So four slots are added beside it. The
//! precedent is `Splash`, added in 7.7 for the same reason: 8.2 freezes
//! its own column, it does not forbid slots the rest of the
//! specification asks for.
//!
//! ## Where the state lives, and why it is not in the modules
//!
//! The magnifier and the colour transform act on pixels, and 8.1 says
//! only the compositor writes pixels. If `zoom.mod` painted, 8.1 would be
//! broken; if the compositor decided when to zoom, 16.2 would be -- the
//! module is meant to be replaceable, and a decision taken elsewhere
//! cannot be replaced.
//!
//! The split that satisfies both: **the module owns the policy, the
//! compositor owns the pixels.** `zoom.mod` decides the shape, the factor
//! and where the centre is; the publication stage of `graphics` applies
//! it. What sits between them is the atomic state below, which the
//! compositor reads once per frame without ever waiting for a module.
//!
//! ## Why the state is atomics and not a lock
//!
//! It is read on the render path, once per frame, by a task holding the
//! compositor lock. Putting a second lock there would create an ordering
//! -- COMPOSITOR then ACCESS -- that a module would have to respect in
//! reverse the moment it wanted to read the compositor. Atomics have no
//! order to get wrong.
//!
//! ## 16.4 from the login screen (10.1.7)
//!
//! `start` runs before `login.ss`, and the four toggles are handled by
//! the compositor's routing, which exists as soon as the compositor does.
//! Neither depends on a session, an account or a loaded module. That is
//! what makes 16.4's last sentence true rather than aspirational: a
//! person who needs the reader to log in has it before logging in.

pub mod contrast;
pub mod keynav;
pub mod reader;
pub mod selftest;
pub mod speech;
pub mod tree;
pub mod zoom;

use alloc::string::String;
use core::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, Ordering};

use crate::fs::perm::{Credentials, Mode};
use crate::vga::SpinLock;

use tree::{NodeId, Role, Tree};

/// Channel on which a change of state is published.
///
/// The modules subscribe to it; the coordinator publishes. A module that
/// polled the atomics would work and would burn a core doing it, which
/// `DECISIONS.md` lists among the invariants not to break.
pub const CHANNEL: &str = "sys.access";

/// Settings file, under `/sys/config/`.
pub const CONFIG: &str = "/sys/config/accessibilite.cfg";
/// Per-account settings, relative to `/users/{user}/`.
pub const USER_CONFIG: &str = "settings/accessibilite.cfg";

// ============================================================
//  State
// ============================================================

/// Is the screen reader on? (16.4, Alt+Super+S)
static READER_ON: AtomicBool = AtomicBool::new(false);
/// Is the magnifier on? (16.4, Alt+Super+Z)
static ZOOM_ON: AtomicBool = AtomicBool::new(false);
/// Is a colour transform on? (16.4, Alt+Super+C)
static CONTRAST_ON: AtomicBool = AtomicBool::new(false);
/// Is keyboard navigation on? (16.4, Alt+Super+K)
static KEYBOARD_ON: AtomicBool = AtomicBool::new(false);

/// Colour mode, as `contrast::Mode::code`.
static CONTRAST_MODE: AtomicU32 = AtomicU32::new(0);
/// Magnification in percent, and shape as `zoom::Shape::code`.
static ZOOM_PERCENT: AtomicU32 = AtomicU32::new(200);
static ZOOM_SHAPE: AtomicU32 = AtomicU32::new(0);
/// Centre of the magnification, which follows the pointer.
static ZOOM_X: AtomicI32 = AtomicI32::new(0);
static ZOOM_Y: AtomicI32 = AtomicI32::new(0);

/// Counters, for the state dump and the self-test.
static TOGGLES: AtomicU64 = AtomicU64::new(0);
static ANNOUNCEMENTS: AtomicU64 = AtomicU64::new(0);

/// The custom matrix of 16.3, under a lock: nine values cannot be one
/// atomic, and it is read once per frame, not once per pixel.
static MATRIX: SpinLock<contrast::Matrix> = SpinLock::new(contrast::Matrix([256, 0, 0, 0, 256, 0, 0, 0, 256]));

/// The accessibility tree (16.1).
static TREE: SpinLock<Tree> = SpinLock::new(Tree::new());

/// One of the four features 16.4 toggles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Feature {
    Reader,
    Zoom,
    Contrast,
    Keyboard,
}

impl Feature {
    pub fn as_str(&self) -> &'static str {
        match self {
            Feature::Reader => "lecteur d'ecran",
            Feature::Zoom => "zoom",
            Feature::Contrast => "contraste eleve",
            Feature::Keyboard => "navigation clavier",
        }
    }

    /// Translation key, so the announcement follows the locale (14.3).
    pub fn key(&self) -> &'static str {
        match self {
            Feature::Reader => "access.lecteur",
            Feature::Zoom => "access.zoom",
            Feature::Contrast => "access.contraste",
            Feature::Keyboard => "access.clavier",
        }
    }

    fn cell(&self) -> &'static AtomicBool {
        match self {
            Feature::Reader => &READER_ON,
            Feature::Zoom => &ZOOM_ON,
            Feature::Contrast => &CONTRAST_ON,
            Feature::Keyboard => &KEYBOARD_ON,
        }
    }

    pub fn all() -> [Feature; 4] {
        [
            Feature::Reader,
            Feature::Zoom,
            Feature::Contrast,
            Feature::Keyboard,
        ]
    }
}

/// Is a feature on?
pub fn is_on(feature: Feature) -> bool {
    feature.cell().load(Ordering::Relaxed)
}

/// Sets a feature, returning whether the state actually changed.
///
/// Setting a feature to what it already is publishes nothing: a module
/// woken for a change that did not happen would repaint for nothing, and
/// the reader would announce a toggle nobody pressed.
pub fn set(feature: Feature, on: bool) -> bool {
    if feature.cell().swap(on, Ordering::Relaxed) == on {
        return false;
    }
    TOGGLES.fetch_add(1, Ordering::Relaxed);

    // Turning contrast on with no mode chosen selects the high-contrast
    // one. Leaving it at `None` would flip a flag and change nothing on
    // screen, which reads as a broken shortcut.
    if feature == Feature::Contrast
        && on
        && CONTRAST_MODE.load(Ordering::Relaxed) == contrast::Mode::None.code() as u32
    {
        CONTRAST_MODE.store(contrast::Mode::High.code() as u32, Ordering::Relaxed);
    }

    announce_state(feature, on);
    publish(feature, on);
    true
}

/// Flips a feature. Returns its new state.
pub fn toggle(feature: Feature) -> bool {
    let next = !is_on(feature);
    set(feature, next);
    next
}

/// Tells the subscribers what changed.
///
/// Two bytes: the feature and its state. A text payload would be
/// friendlier to read in a trace and would make every subscriber parse a
/// string on a path that fires on a keystroke.
fn publish(feature: Feature, on: bool) {
    let code = match feature {
        Feature::Reader => 0u8,
        Feature::Zoom => 1,
        Feature::Contrast => 2,
        Feature::Keyboard => 3,
    };
    let payload = [code, u8::from(on)];
    if let Some(id) = crate::ipc::find_channel(CHANNEL) {
        let _ = crate::ipc::publish(
            id,
            crate::ipc::Message::new(
                crate::scheduler::current_pid(),
                crate::ipc::MessageKind::Event,
                0,
                &payload,
            ),
        );
    }
}

/// Says out loud what just changed, when the reader is on.
///
/// The reader announces its **own** activation too: switching it on with
/// no confirmation leaves a person who cannot see the screen unable to
/// tell whether the shortcut worked.
fn announce_state(feature: Feature, on: bool) {
    if feature != Feature::Reader && !is_on(Feature::Reader) {
        return;
    }
    let state = if on { "access.actif" } else { "access.inactif" };
    let text = alloc::format!(
        "{} {}",
        crate::i18n::translate(feature.key(), &[]),
        crate::i18n::translate(state, &[])
    );
    reader::say(&text);
}

// ============================================================
//  Zoom and contrast, as the compositor reads them
// ============================================================

/// Current colour mode, or `None` when the transform is off.
pub fn contrast_mode() -> contrast::Mode {
    if !is_on(Feature::Contrast) {
        return contrast::Mode::None;
    }
    contrast::Mode::from_code(CONTRAST_MODE.load(Ordering::Relaxed) as u8)
        .unwrap_or(contrast::Mode::High)
}

/// Chooses the colour mode. Turning on a mode turns the feature on with
/// it: choosing "deuteranopia" and seeing nothing happen would be a bug
/// report, not a setting.
pub fn set_contrast_mode(mode: contrast::Mode) {
    CONTRAST_MODE.store(mode.code() as u32, Ordering::Relaxed);
    set(Feature::Contrast, mode.is_active());
}

/// Steps through the modes of 16.3, in the order it lists them.
pub fn cycle_contrast_mode() -> contrast::Mode {
    let current = CONTRAST_MODE.load(Ordering::Relaxed) as u8;
    let next = ((current as usize + 1) % contrast::MODE_COUNT) as u8;
    let mode = contrast::Mode::from_code(next).unwrap_or(contrast::Mode::None);
    set_contrast_mode(mode);
    mode
}

/// The custom matrix of 16.3.
pub fn custom_matrix() -> contrast::Matrix {
    *MATRIX.lock()
}

pub fn set_custom_matrix(matrix: contrast::Matrix) {
    *MATRIX.lock() = matrix;
}

/// Current magnifier settings.
pub fn zoom_settings() -> zoom::Settings {
    zoom::Settings {
        shape: zoom::Shape::from_code(ZOOM_SHAPE.load(Ordering::Relaxed) as u8)
            .unwrap_or(zoom::Shape::Global),
        percent: ZOOM_PERCENT.load(Ordering::Relaxed),
        area: (0, 0, 640, 360),
    }
    .sanitised()
}

pub fn set_zoom_settings(settings: zoom::Settings) {
    let settings = settings.sanitised();
    ZOOM_SHAPE.store(settings.shape.code() as u32, Ordering::Relaxed);
    ZOOM_PERCENT.store(settings.percent, Ordering::Relaxed);
}

/// Where the magnification is centred. Followed to the pointer by the
/// compositor, which is the only thing that knows where the pointer is.
pub fn set_zoom_centre(x: i32, y: i32) {
    ZOOM_X.store(x, Ordering::Relaxed);
    ZOOM_Y.store(y, Ordering::Relaxed);
}

pub fn zoom_centre() -> (i32, i32) {
    (
        ZOOM_X.load(Ordering::Relaxed),
        ZOOM_Y.load(Ordering::Relaxed),
    )
}

/// Does anything need to touch the pixels on their way out?
///
/// Read once per frame by the compositor. When it is false the
/// publication takes the plain path of phase 7.1, which is the one that
/// costs nothing -- accessibility must not make the machine slower for
/// somebody who has not turned it on.
pub fn transforms_pixels() -> bool {
    is_on(Feature::Zoom) || contrast_mode().is_active()
}

/// Must the whole screen be republished?
///
/// True while the magnifier is on. Under magnification a rectangle of the
/// back buffer lands on a different rectangle of the screen, and the
/// mapping moves with the pointer: publishing a partial area would leave
/// the rest of the screen showing the previous magnification, torn along
/// the boundary. See `zoom`.
pub fn wants_full_frame() -> bool {
    is_on(Feature::Zoom)
}

/// Applies zoom then colour to one destination pixel.
///
/// The order is not interchangeable. Magnifying a recoloured pixel and
/// recolouring a magnified one give the same answer for a per-pixel
/// transform, and stop doing so the moment the colour transform looks at
/// its neighbours -- which the high-contrast stretch is one step away
/// from doing. Fixing the order now costs nothing and removes a future
/// surprise.
pub fn map_pixel(
    sample: &dyn Fn(usize, usize) -> crate::graphics::framebuffer::Color,
    width: i32,
    height: i32,
    x: usize,
    y: usize,
    zoom_on: bool,
    settings: &zoom::Settings,
    centre: (i32, i32),
    mode: contrast::Mode,
    matrix: &contrast::Matrix,
) -> crate::graphics::framebuffer::Color {
    let colour = if zoom_on {
        match zoom::source_of(
            settings, width, height, centre.0, centre.1, x as i32, y as i32,
        ) {
            Some((sx, sy)) => sample(sx as usize, sy as usize),
            // Outside the magnified region: the pixel is copied as it is,
            // which is what makes the lens and the area shapes work at
            // all.
            None => sample(x, y),
        }
    } else {
        sample(x, y)
    };

    contrast::transform(mode, matrix, colour)
}

// ============================================================
//  16.1 -- The tree
// ============================================================

/// Runs a closure over the accessibility tree.
///
/// Same discipline as `gui::with`: the lock is taken and released by the
/// function, so it cannot be held across a task switch.
pub fn with_tree<R>(action: impl FnOnce(&mut Tree) -> R) -> R {
    action(&mut TREE.lock())
}

/// Declares an element. Returns its identifier.
pub fn declare(parent: NodeId, role: Role, label: &str, owner: crate::process::Pid) -> Option<NodeId> {
    with_tree(|tree| tree.add(parent, role, label, owner))
}

/// Forgets everything a task declared.
pub fn forget_owner(owner: crate::process::Pid) -> usize {
    with_tree(|tree| tree.forget_owner(owner))
}

/// What the reader would say about the focused element.
pub fn focus_announcement() -> Option<String> {
    with_tree(|tree| tree.focused().map(|node| node.announcement()))
}

/// Number of elements declared, and number focusable.
pub fn tree_sizes() -> (usize, usize) {
    with_tree(|tree| (tree.len(), tree.focus_order().len()))
}

/// Handles a key on behalf of the navigation (16.4).
///
/// Returns `true` if the key was consumed. Called by the compositor for
/// the global shortcuts, and by the coordinator module for the rest.
pub fn handle_key(event: &crate::gui::protocol::KeyEvent) -> bool {
    let Some(action) = keynav::action_for(event) else {
        return false;
    };

    // The four toggles fire whatever else is on, keyboard navigation
    // included: 16.4 calls them global, and requiring navigation to be on
    // before one could turn on the reader would be a lock with the key
    // inside.
    match action {
        keynav::Action::ToggleReader => {
            toggle(Feature::Reader);
            return true;
        }
        keynav::Action::ToggleZoom => {
            toggle(Feature::Zoom);
            return true;
        }
        keynav::Action::ToggleContrast => {
            toggle(Feature::Contrast);
            return true;
        }
        keynav::Action::ToggleKeyboard => {
            toggle(Feature::Keyboard);
            return true;
        }
        _ => {}
    }

    // The rest only applies when navigation is on: Tab belongs to the
    // application otherwise, and stealing it would break every form on
    // the system for people who never asked for this.
    if !is_on(Feature::Keyboard) {
        return false;
    }

    let focused_role = with_tree(|tree| tree.focused().map(|node| node.role));
    if !keynav::consumes(action, focused_role) {
        return false;
    }

    let moved = with_tree(|tree| keynav::apply(tree, action));

    if let Some(id) = moved
        && matches!(
            action,
            keynav::Action::FocusNext | keynav::Action::FocusPrevious | keynav::Action::NextPanel
        )
        && let Some(text) = with_tree(|tree| tree.get(id).map(|node| node.announcement()))
    {
        ANNOUNCEMENTS.fetch_add(1, Ordering::Relaxed);
        reader::say(&text);
    }

    true
}

/// Number of announcements requested since boot.
pub fn announcement_count() -> u64 {
    ANNOUNCEMENTS.load(Ordering::Relaxed)
}

/// Number of feature toggles since boot.
pub fn toggle_count() -> u64 {
    TOGGLES.load(Ordering::Relaxed)
}

// ============================================================
//  The five module tasks of 16.2
// ============================================================

/// Waits for a state change on `sys.access`.
///
/// Every accessibility module has the same loop: subscribe, sleep on the
/// mailbox, react. Writing it once is not tidiness -- five copies would
/// diverge on the stop request, and a module that ignores its stop leaves
/// the compositor holding a slot nothing occupies (8.3).
fn module_loop(name: &'static str, mut on_change: impl FnMut(Feature, bool)) -> ! {
    if let Some(id) = crate::ipc::find_channel(CHANNEL)
        && crate::ipc::subscribe(id).is_err()
    {
        crate::serial_println!("[{}] abonnement a \"{}\" refuse", name, CHANNEL);
    }
    crate::serial_println!("[{}] module d'accessibilite pret", name);

    loop {
        let message = crate::ipc::recv();

        if crate::gui::stop_requested(&message) {
            crate::serial_println!("[{}] arret demande", name);
            crate::scheduler::exit_current_with(0);
        }

        // Two bytes: the feature and its state. Anything else is not
        // ours -- a module must not act on a message it did not
        // recognise, which is how a shared channel turns into a source of
        // spurious behaviour.
        if message.payload.len() == 2
            && let Some(feature) = feature_from_code(message.payload[0])
        {
            on_change(feature, message.payload[1] != 0);
        }
    }
}

fn feature_from_code(code: u8) -> Option<Feature> {
    match code {
        0 => Some(Feature::Reader),
        1 => Some(Feature::Zoom),
        2 => Some(Feature::Contrast),
        3 => Some(Feature::Keyboard),
        _ => None,
    }
}

/// `accessibility.mod` -- the coordinator of 16.1.
///
/// It holds no state of its own: the tree and the four flags live in this
/// module's statics, because the compositor reads them on the render path
/// and a coordinator that owned them would have to be asked, by IPC, once
/// per frame.
///
/// What it does own is the **announcement of a change**, and the fact
/// that the tree is emptied of the nodes of dead owners. Both are policy,
/// and policy is what a replaceable module is for.
pub fn coordinator_task() -> ! {
    // The coordinator declares the shell itself, so that a keyboard user
    // has something to reach before any application has started. Without
    // it Tab at the login screen would find an empty tree and do nothing,
    // which is indistinguishable from a broken shortcut.
    let owner = crate::scheduler::current_pid();
    let root = declare(0, Role::Window, "Bureau Nova OS", owner).unwrap_or(0);
    if root != 0 {
        declare(root, Role::Panel, "Barre des taches", owner);
        declare(root, Role::Panel, "Zone de travail", owner);
    }

    module_loop("accessibilite", |feature, on| {
        crate::serial_println!(
            "[accessibilite] {} {}",
            feature.as_str(),
            if on { "actif" } else { "inactif" }
        );
        // A dead application leaves nodes behind, and Tab would land on
        // things that are no longer on screen. The sweep happens on every
        // change rather than on a timer: a timer would be a second clock
        // for something that only matters when somebody is navigating.
        collect_dead_nodes();
    })
}

/// `zoom.mod` -- 16.2, the magnifier.
pub fn zoom_task() -> ! {
    module_loop("zoom", |feature, on| {
        if feature != Feature::Zoom {
            return;
        }
        let settings = zoom_settings();
        crate::serial_println!(
            "[zoom] {} - {} a {}%",
            if on { "actif" } else { "inactif" },
            settings.shape.as_str(),
            settings.percent
        );
    })
}

/// `contrast.mod` -- 16.2 and 16.3.
pub fn contrast_task() -> ! {
    module_loop("contraste", |feature, on| {
        if feature != Feature::Contrast {
            return;
        }
        crate::serial_println!(
            "[contraste] {} - mode {}",
            if on { "actif" } else { "inactif" },
            contrast_mode().as_str()
        );
    })
}

/// `keyboard-nav.mod` -- 16.2 and 16.4.
pub fn keynav_task() -> ! {
    module_loop("clavier", |feature, on| {
        if feature != Feature::Keyboard {
            return;
        }
        let (nodes, focusable) = tree_sizes();
        crate::serial_println!(
            "[clavier] navigation {} - {} element(s), {} focalisable(s)",
            if on { "active" } else { "desactivee" },
            nodes,
            focusable
        );
    })
}

/// Drops the nodes of tasks that are no longer alive.
///
/// Returns how many went. An application that closed its window without
/// withdrawing its nodes is the ordinary case, not the exception: nothing
/// obliges a program to be tidy on the way out, and a tree that trusted
/// them would fill up with the dead.
pub fn collect_dead_nodes() -> usize {
    let owners: alloc::vec::Vec<crate::process::Pid> =
        with_tree(|tree| {
            let mut seen = alloc::vec::Vec::new();
            for node in tree.iter() {
                if node.owner != 0 && !seen.contains(&node.owner) {
                    seen.push(node.owner);
                }
            }
            seen
        });

    let mut removed = 0;
    for owner in owners {
        if crate::scheduler::task_info(owner).is_none() {
            removed += forget_owner(owner);
        }
    }
    removed
}

// ============================================================
//  Settings file
// ============================================================

/// Reads the settings, system layer then user layer.
///
/// Same priority as 14.1, and for the same reason: an account must be
/// able to turn the reader on without turning it on for everybody, and
/// the machine must be able to turn it on for a shared terminal without
/// each account having to.
pub fn load(user: Option<&str>) -> usize {
    let mut applied = 0;
    for path in [
        Some(String::from(CONFIG)),
        user.map(|name| alloc::format!("/users/{}/{}", name, USER_CONFIG)),
    ]
    .into_iter()
    .flatten()
    {
        if let Ok(bytes) = crate::fs::read_file(&path, Credentials::kernel())
            && let Ok(text) = core::str::from_utf8(&bytes)
        {
            applied += apply_config(text);
        }
    }
    applied
}

/// Applies a settings text. Returns how many settings were understood.
pub fn apply_config(text: &str) -> usize {
    let mut applied = 0;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        let understood = match key {
            "lecteur" => {
                parse_bool(value).map(|on| set(Feature::Reader, on)).is_some()
            }
            "zoom" => parse_bool(value).map(|on| set(Feature::Zoom, on)).is_some(),
            "contraste" => parse_bool(value)
                .map(|on| set(Feature::Contrast, on))
                .is_some(),
            "clavier" => parse_bool(value)
                .map(|on| set(Feature::Keyboard, on))
                .is_some(),
            "mode_couleur" => match contrast::Mode::parse(value) {
                Some(mode) => {
                    CONTRAST_MODE.store(mode.code() as u32, Ordering::Relaxed);
                    true
                }
                None => false,
            },
            "matrice" => match contrast::Matrix::parse(value) {
                Some(matrix) => {
                    set_custom_matrix(matrix);
                    true
                }
                None => false,
            },
            "zoom_forme" => match zoom::Shape::parse(value) {
                Some(shape) => {
                    ZOOM_SHAPE.store(shape.code() as u32, Ordering::Relaxed);
                    true
                }
                None => false,
            },
            "zoom_facteur" => match value.parse::<u32>() {
                Ok(percent) => {
                    ZOOM_PERCENT.store(percent.clamp(zoom::MIN_PERCENT, zoom::MAX_PERCENT), Ordering::Relaxed);
                    true
                }
                Err(_) => false,
            },
            "voix_hauteur" => reader::set_pitch(value),
            "voix_debit" => reader::set_rate(value),
            "voix_volume" => reader::set_volume(value),
            _ => false,
        };
        if understood {
            applied += 1;
        }
    }
    applied
}

fn parse_bool(value: &str) -> Option<bool> {
    match value {
        "oui" | "actif" | "1" | "true" => Some(true),
        "non" | "inactif" | "0" | "false" => Some(false),
        _ => None,
    }
}

/// The reference settings file, written on the first boot.
fn default_config() -> String {
    let settings = reader::voice();
    alloc::format!(
        "# Nova OS - accessibilite (spec 16)\n\
         # Raccourcis : Alt+Super+S lecteur, Z zoom, C contraste, K clavier\n\
         lecteur = non\n\
         zoom = non\n\
         contraste = non\n\
         clavier = non\n\
         mode_couleur = contraste_eleve\n\
         matrice = 256,0,0,0,256,0,0,0,256\n\
         zoom_forme = global\n\
         zoom_facteur = 200\n\
         voix_hauteur = {}\n\
         voix_debit = {}\n\
         voix_volume = {}\n",
        settings.pitch_hz,
        settings.rate_percent,
        settings.volume
    )
}

// ============================================================
//  Installation and start
// ============================================================

/// Lays down `/sys/gui/modules/accessibility/` and the settings file.
///
/// Idempotent, like every other layout of the repository: a second boot
/// must find its files, not rewrite them, or a person who turned the
/// reader on would find it off again at every restart -- which is the
/// worst possible failure for this particular subsystem.
pub fn install_layout(now: u64) -> usize {
    if !crate::fs::is_ready() {
        return 0;
    }
    let who = Credentials::kernel();
    let mut created = 0;

    if crate::fs::stat(crate::gui::module::ACCESSIBILITY_DIR, who).is_err()
        && crate::fs::create_directory(
            crate::gui::module::ACCESSIBILITY_DIR,
            who,
            Mode::system_dir(),
            now,
        )
        .is_ok()
    {
        created += 1;
    }

    if crate::fs::stat(CONFIG, who).is_err() {
        let content = default_config();
        if crate::fs::write_file(CONFIG, content.as_bytes(), who, Mode::system_file(), now).is_ok()
        {
            created += 1;
        }
    }

    created
}

/// Starts phase 10.1.
///
/// Called by `init.dsa` **before** the login screen: 16.4 wants the
/// shortcuts working from there, and a subsystem started after it would
/// not be.
pub fn start(now: u64) {
    let created = install_layout(now);

    // The channel is opened here rather than by a module: the coordinator
    // publishes on it as soon as a shortcut fires, which may be before
    // any module has started. Phase 6 paid for the reverse -- "declaring
    // a channel is not opening it".
    if crate::ipc::open_channel(CHANNEL).is_err() && crate::ipc::find_channel(CHANNEL).is_none() {
        crate::serial_println!("[access] canal \"{}\" indisponible", CHANNEL);
    }

    let applied = load(None);

    crate::serial_println!(
        "[access] {} entree(s) creee(s), {} reglage(s) charge(s) - raccourcis 16.4 actifs",
        created,
        applied
    );
    crate::serial_println!(
        "[access] lecteur {} - zoom {} - contraste {} - clavier {}",
        state_word(Feature::Reader),
        state_word(Feature::Zoom),
        state_word(Feature::Contrast),
        state_word(Feature::Keyboard)
    );
}

/// Reloads for an account, at session opening.
pub fn adopt_user(user: &str) {
    let applied = load(Some(user));
    crate::serial_println!(
        "[access] session {} - {} reglage(s) applique(s)",
        user,
        applied
    );
}

fn state_word(feature: Feature) -> &'static str {
    if is_on(feature) { "actif" } else { "inactif" }
}

pub fn dump_state() {
    let (nodes, focusable) = tree_sizes();
    crate::serial_println!(
        "[access] arbre : {} element(s), {} focalisable(s) - focus {}",
        nodes,
        focusable,
        with_tree(|tree| tree.focus())
    );
    crate::serial_println!(
        "[access] lecteur {} - zoom {} ({} {}%) - contraste {} ({}) - clavier {}",
        state_word(Feature::Reader),
        state_word(Feature::Zoom),
        zoom_settings().shape.as_str(),
        zoom_settings().percent,
        state_word(Feature::Contrast),
        contrast_mode().as_str(),
        state_word(Feature::Keyboard)
    );
    crate::serial_println!(
        "[access] {} bascule(s), {} annonce(s)",
        toggle_count(),
        announcement_count()
    );
    reader::dump_state();
}
