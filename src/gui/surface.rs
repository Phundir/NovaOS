//! 7.2.3 -- Surfaces, z-order and display lists.
//!
//! ## Why a display list and not one buffer per surface
//!
//! The obvious solution gives each surface its own pixel buffer, which
//! the compositor copies. It costs `width * height * 4` bytes per window:
//! three mebibytes for a single full-screen window, and there is no
//! shared memory for the client to write into directly -- every pixel
//! would have to travel through an IPC message capped at 256 bytes.
//!
//! A surface therefore keeps **the instructions received**, not their
//! result. The compositor replays them in z order on every frame. It is
//! exactly what 8.1 describes -- "they send IPC instructions and the
//! compositor performs the rendering" -- and it costs a few tens of bytes
//! per command instead of three mebibytes.
//!
//! Acknowledged price: the render cost is proportional to the number of
//! commands, not to the area. A window emitting ten thousand rectangles
//! would be expensive on every frame; hence the per-surface cap, which
//! refuses rather than let an application slow down the whole screen.
//!
//! ## Z-order
//!
//! The z is an integer, not a position in a list. Two surfaces may share
//! it, in which case the creation order decides -- stable, hence
//! reproducible. The layers are frozen by `Layer`: no module can bring a
//! window in front of the cursor, whatever z value it asks for.

use alloc::string::String;
use alloc::vec::Vec;

use crate::graphics::framebuffer::Color;
use crate::process::Pid;

/// Maximum number of simultaneous surfaces.
pub const MAX_SURFACES: usize = 64;

/// Commands kept per surface, beyond which we refuse.
pub const MAX_COMMANDS: usize = 256;

pub type SurfaceId = u32;

// ============================================================
//  Layers
// ============================================================

/// Composition layers, from the back to the top.
///
/// Frozen by construction: the z a client asks for only plays inside its
/// layer. Without that, a desktop module could come in front of the task
/// bar, and a window in front of the cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Layer {
    /// Wallpaper and desktop.
    Wallpaper = 0,
    /// Application windows.
    Window = 1,
    /// Task bar, launcher.
    Shell = 2,
    /// Notification toasts.
    Notification = 3,
    /// Modal full screen: PRIORITY notification, login screen.
    Modal = 4,
    /// Cursor -- always at the top, by definition.
    Overlay = 5,
}

impl Layer {
    pub fn as_str(&self) -> &'static str {
        match self {
            Layer::Wallpaper => "fond",
            Layer::Window => "fenetre",
            Layer::Shell => "shell",
            Layer::Notification => "notification",
            Layer::Modal => "modal",
            Layer::Overlay => "surimpression",
        }
    }

    pub fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Layer::Wallpaper),
            1 => Some(Layer::Window),
            2 => Some(Layer::Shell),
            3 => Some(Layer::Notification),
            4 => Some(Layer::Modal),
            5 => Some(Layer::Overlay),
            _ => None,
        }
    }

    /// May a surface of this layer receive the keyboard focus?
    ///
    /// The wallpaper and the cursor have no use for a key, and giving
    /// them the focus would take it from the active window.
    pub fn focusable(&self) -> bool {
        matches!(self, Layer::Window | Layer::Shell | Layer::Modal)
    }
}

// ============================================================
//  Rectangle
// ============================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Rect {
    pub const EMPTY: Rect = Rect {
        x: 0,
        y: 0,
        width: 0,
        height: 0,
    };

    pub const fn new(x: i32, y: i32, width: i32, height: i32) -> Self {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.width <= 0 || self.height <= 0
    }

    pub fn right(&self) -> i32 {
        self.x + self.width
    }

    pub fn bottom(&self) -> i32 {
        self.y + self.height
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        !self.is_empty() && x >= self.x && y >= self.y && x < self.right() && y < self.bottom()
    }

    /// Intersection. Returns an empty rectangle when they do not touch.
    pub fn intersect(&self, other: &Rect) -> Rect {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = self.right().min(other.right());
        let bottom = self.bottom().min(other.bottom());
        if right <= x || bottom <= y {
            return Rect::EMPTY;
        }
        Rect::new(x, y, right - x, bottom - y)
    }

    /// Smallest rectangle holding both. The union with an empty rectangle
    /// is the other one: that is what allows accumulating a damaged area
    /// starting from nothing.
    pub fn union(&self, other: &Rect) -> Rect {
        if self.is_empty() {
            return *other;
        }
        if other.is_empty() {
            return *self;
        }
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let right = self.right().max(other.right());
        let bottom = self.bottom().max(other.bottom());
        Rect::new(x, y, right - x, bottom - y)
    }

    /// Moves the rectangle without changing its size.
    pub fn offset(&self, dx: i32, dy: i32) -> Rect {
        Rect::new(self.x + dx, self.y + dy, self.width, self.height)
    }

    /// Shrinks by `amount` on all four sides.
    pub fn shrink(&self, amount: i32) -> Rect {
        let rect = Rect::new(
            self.x + amount,
            self.y + amount,
            self.width - 2 * amount,
            self.height - 2 * amount,
        );
        if rect.is_empty() { Rect::EMPTY } else { rect }
    }
}

// ============================================================
//  Drawing commands
// ============================================================

/// One instruction kept by a surface.
///
/// The coordinates are **relative to the surface**. The compositor
/// translates and clips them at render time: an application overflowing
/// its window sees its stroke cut, not the screen soiled.
#[derive(Debug, Clone)]
pub enum Command {
    /// Fills the whole surface.
    Fill { color: Color },
    Rect { rect: Rect, color: Color },
    /// Hollow frame of a given thickness.
    Frame {
        rect: Rect,
        color: Color,
        thickness: i32,
    },
    Line {
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        color: Color,
    },
    Text {
        x: i32,
        y: i32,
        color: Color,
        background: Option<Color>,
        text: String,
    },
    /// Monochrome 16x16 icon, one bit per pixel, top row first.
    Icon {
        x: i32,
        y: i32,
        bits: [u8; 32],
        color: Color,
        background: Option<Color>,
    },
    /// A pixel canvas painted by the client (`gui::canvas`).
    Canvas {
        x: i32,
        y: i32,
        canvas: super::canvas::CanvasRef,
    },
}

impl Command {
    pub fn name(&self) -> &'static str {
        match self {
            Command::Fill { .. } => "fill",
            Command::Rect { .. } => "rect",
            Command::Frame { .. } => "frame",
            Command::Line { .. } => "line",
            Command::Text { .. } => "text",
            Command::Icon { .. } => "icon",
            Command::Canvas { .. } => "canvas",
        }
    }
}

// ============================================================
//  Surface
// ============================================================

#[derive(Debug, Clone)]
pub struct Surface {
    pub id: SurfaceId,
    pub owner: Pid,
    pub layer: Layer,
    pub rect: Rect,
    /// Order inside the layer. Larger = closer to the eye.
    pub z: i32,
    pub visible: bool,
    pub decorated: bool,
    pub title: String,
    /// List under construction, fed by the commands.
    pending: Vec<Command>,
    /// List published at the last `COMMIT` -- the only one the render reads.
    committed: Vec<Command>,
    /// Creation rank, tie-breaker for equal z.
    pub sequence: u64,
    pub commits: u64,
    pub refused: u64,
}

impl Surface {
    pub fn new(
        id: SurfaceId,
        owner: Pid,
        layer: Layer,
        rect: Rect,
        title: String,
        sequence: u64,
    ) -> Self {
        Surface {
            id,
            owner,
            layer,
            rect,
            z: 0,
            visible: true,
            decorated: layer == Layer::Window,
            title,
            pending: Vec::new(),
            committed: Vec::new(),
            sequence,
            commits: 0,
            refused: 0,
        }
    }

    /// Adds a command to the list under construction.
    ///
    /// The cap protects the render time of the whole screen: a surface
    /// must not be able to make the system unusable by emitting without
    /// end. The refusal is counted and readable in `dump_state`, so that
    /// the cause is visible rather than guessed.
    pub fn push(&mut self, command: Command) -> bool {
        if self.pending.len() >= MAX_COMMANDS {
            self.refused += 1;
            return false;
        }
        self.pending.push(command);
        true
    }

    pub fn reset(&mut self) {
        self.pending.clear();
    }

    /// Publishes the list under construction.
    ///
    /// The double list is not a refinement: without it, the render could
    /// fall in the middle of a series of commands and display a
    /// half-drawn window. It is the double buffer of 7.1, transposed to
    /// the level of the instructions.
    pub fn commit(&mut self) {
        self.committed.clear();
        self.committed.extend(self.pending.iter().cloned());
        self.commits += 1;
    }

    pub fn display_list(&self) -> &[Command] {
        &self.committed
    }

    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// Rectangle occupied on screen, decorations included.
    pub fn outer_rect(&self, title_height: i32, border: i32) -> Rect {
        if !self.decorated {
            return self.rect;
        }
        Rect::new(
            self.rect.x - border,
            self.rect.y - title_height - border,
            self.rect.width + 2 * border,
            self.rect.height + title_height + 2 * border,
        )
    }
}

// ============================================================
//  Surface stack
// ============================================================

/// The set of the surfaces, and the order in which they are painted.
pub struct Stack {
    surfaces: Vec<Surface>,
    next_id: SurfaceId,
    next_sequence: u64,
    focus: Option<SurfaceId>,
}

impl Stack {
    pub const fn new() -> Self {
        Stack {
            surfaces: Vec::new(),
            next_id: 1,
            next_sequence: 0,
            focus: None,
        }
    }

    pub fn len(&self) -> usize {
        self.surfaces.len()
    }

    pub fn is_empty(&self) -> bool {
        self.surfaces.is_empty()
    }

    pub fn focus(&self) -> Option<SurfaceId> {
        self.focus
    }

    pub fn create(
        &mut self,
        owner: Pid,
        layer: Layer,
        rect: Rect,
        title: String,
    ) -> Option<SurfaceId> {
        if self.surfaces.len() >= MAX_SURFACES {
            return None;
        }
        let id = self.next_id;
        self.next_id += 1;
        let sequence = self.next_sequence;
        self.next_sequence += 1;

        let mut surface = Surface::new(id, owner, layer, rect, title, sequence);
        // A surface is born above its peers: it is what one expects of a
        // window that has just opened.
        surface.z = self.top_z(layer) + 1;
        self.surfaces.push(surface);

        if layer.focusable() {
            self.focus = Some(id);
        }
        Some(id)
    }

    fn top_z(&self, layer: Layer) -> i32 {
        self.surfaces
            .iter()
            .filter(|surface| surface.layer == layer)
            .map(|surface| surface.z)
            .max()
            .unwrap_or(0)
    }

    pub fn destroy(&mut self, id: SurfaceId) -> bool {
        let before = self.surfaces.len();
        self.surfaces.retain(|surface| surface.id != id);
        if self.focus == Some(id) {
            self.focus = self.topmost_focusable();
        }
        self.surfaces.len() != before
    }

    /// Removes every surface of a departed process.
    ///
    /// Without that, the death of an application would leave its window
    /// painted on screen forever: the compositor replays a display list,
    /// and a list survives its author very well.
    pub fn destroy_owned_by(&mut self, owner: Pid) -> usize {
        let before = self.surfaces.len();
        self.surfaces.retain(|surface| surface.owner != owner);
        let removed = before - self.surfaces.len();
        if removed != 0
            && let Some(id) = self.focus
            && !self.surfaces.iter().any(|surface| surface.id == id)
        {
            self.focus = self.topmost_focusable();
        }
        removed
    }

    pub fn get(&self, id: SurfaceId) -> Option<&Surface> {
        self.surfaces.iter().find(|surface| surface.id == id)
    }

    pub fn get_mut(&mut self, id: SurfaceId) -> Option<&mut Surface> {
        self.surfaces.iter_mut().find(|surface| surface.id == id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Surface> {
        self.surfaces.iter()
    }

    /// Identifiers in painting order: layer, then z, then creation rank.
    ///
    /// Returns identifiers and not references so that the caller may
    /// mutate the stack while walking -- the render needs to read the
    /// surfaces, event handling to modify them.
    pub fn painting_order(&self) -> Vec<SurfaceId> {
        let mut order: Vec<&Surface> = self.surfaces.iter().filter(|s| s.visible).collect();
        order.sort_by_key(|surface| (surface.layer, surface.z, surface.sequence));
        order.iter().map(|surface| surface.id).collect()
    }

    /// The highest surface holding this point, decorations included.
    pub fn hit(&self, x: i32, y: i32, title_height: i32, border: i32) -> Option<SurfaceId> {
        let mut best: Option<&Surface> = None;
        for surface in self.surfaces.iter().filter(|s| s.visible) {
            // The cursor never lets itself be clicked: it is drawn at the
            // place of the click, it would catch everything.
            if surface.layer == Layer::Overlay {
                continue;
            }
            if !surface.outer_rect(title_height, border).contains(x, y) {
                continue;
            }
            let higher = match best {
                None => true,
                Some(current) => {
                    (surface.layer, surface.z, surface.sequence)
                        > (current.layer, current.z, current.sequence)
                }
            };
            if higher {
                best = Some(surface);
            }
        }
        best.map(|surface| surface.id)
    }

    fn topmost_focusable(&self) -> Option<SurfaceId> {
        self.surfaces
            .iter()
            .filter(|surface| surface.visible && surface.layer.focusable())
            .max_by_key(|surface| (surface.layer, surface.z, surface.sequence))
            .map(|surface| surface.id)
    }

    /// Puts the surface at the top of its layer.
    pub fn raise(&mut self, id: SurfaceId) -> bool {
        let Some(layer) = self.get(id).map(|surface| surface.layer) else {
            return false;
        };
        let top = self.top_z(layer);
        if let Some(surface) = self.get_mut(id) {
            surface.z = top + 1;
            return true;
        }
        false
    }

    pub fn lower(&mut self, id: SurfaceId) -> bool {
        let Some(layer) = self.get(id).map(|surface| surface.layer) else {
            return false;
        };
        let bottom = self
            .surfaces
            .iter()
            .filter(|surface| surface.layer == layer)
            .map(|surface| surface.z)
            .min()
            .unwrap_or(0);
        if let Some(surface) = self.get_mut(id) {
            surface.z = bottom - 1;
            return true;
        }
        false
    }

    /// Gives the focus, refusing the layers that do not want it.
    pub fn set_focus(&mut self, id: SurfaceId) -> bool {
        match self.get(id) {
            Some(surface) if surface.layer.focusable() => {
                self.focus = Some(id);
                true
            }
            _ => false,
        }
    }

    pub fn focus_owner(&self) -> Option<Pid> {
        self.focus
            .and_then(|id| self.get(id))
            .map(|surface| surface.owner)
    }

    /// Owner of the highest overlay surface.
    ///
    /// The `Overlay` layer is excluded from `hit` -- the cursor is always
    /// under itself and would catch every one of its own clicks. It
    /// nonetheless needs the pointer events, since it is what draws the
    /// cursor: without this separate path, the cursor surface never
    /// receives the order to move and stays frozen where it was born.
    /// Rectangle of the highest overlay surface.
    ///
    /// The compositor needs it to dirty the right area when the pointer
    /// moves: the size of the cursor belongs to the module that draws it,
    /// and copying it elsewhere creates two truths that diverge.
    pub fn overlay_rect(&self) -> Option<Rect> {
        self.surfaces
            .iter()
            .filter(|surface| surface.layer == Layer::Overlay)
            .max_by_key(|surface| (surface.z, surface.sequence))
            .map(|surface| surface.rect)
    }

    pub fn overlay_owner(&self) -> Option<Pid> {
        self.surfaces
            .iter()
            .filter(|surface| surface.layer == Layer::Overlay)
            .max_by_key(|surface| (surface.z, surface.sequence))
            .map(|surface| surface.owner)
    }

    /// Distinct owners of the surfaces of a given layer.
    ///
    /// Used to broadcast the global shortcuts of 16.4: they address the
    /// shell modules, not the focused application.
    pub fn owners_in(&self, layer: Layer) -> Vec<Pid> {
        let mut owners: Vec<Pid> = Vec::new();
        for surface in self.surfaces.iter().filter(|s| s.layer == layer) {
            if !owners.contains(&surface.owner) {
                owners.push(surface.owner);
            }
        }
        owners
    }
}

impl Default for Stack {
    fn default() -> Self {
        Stack::new()
    }
}
