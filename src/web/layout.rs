//! Layout: from a styled tree to positioned boxes (CSS 2.1 visual
//! formatting model, simplified).
//!
//! ## The model
//!
//! * **Block flow.** Block boxes stack vertically inside their parent's
//!   content width; vertical margins between siblings collapse to the
//!   larger of the two; `margin: auto` centres a box of fixed width.
//! * **Inline flow.** Runs of text and inline boxes are cut into words
//!   and packed into lines, left to right, broken where a word would not
//!   fit; `text-align` shifts each line; `white-space: pre` keeps lines
//!   and spaces.
//! * **Tables.** Columns are sized from their cells' minimum and preferred
//!   widths, rows from their tallest cell; `colspan` is honoured.
//! * **Flex and grid.** Items are sized to their content and placed in
//!   rows that wrap -- the common case of navigation bars and card grids.
//!   The rest of flexbox (grow, shrink, order, alignment on both axes) is
//!   not modelled.
//!
//! Floats are laid out in the flow, and positioned boxes where they would
//! have been without their offset -- except the "visually hidden" ones,
//! which are dropped.
//!
//! ## One font
//!
//! Every glyph is 8x16. Text of 22 px and above is drawn at twice the
//! size; smaller text keeps the one size there is. Bold is a second pass
//! one pixel to the right.
//!
//! ## Recursion
//!
//! Boxes nest as deep as the page's markup. Every descent asks
//! `headroom` -- the kernel's `stack_headroom` -- and stops laying out
//! below a floor, as DECISIONS.md requires of any recursion whose depth
//! comes from outside data.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use super::html::{Document, NodeData, NodeId};
use super::style::{Align, Display, Float, Length, ListStyle, Position, Style, Track};
use super::text;

/// Width of a glyph, height of a line of text.
pub const GLYPH_W: i32 = 8;
pub const LINE_H: i32 = 20;

/// Below this many bytes of stack, the layout stops descending.
pub const STACK_FLOOR: usize = 40 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Paint {
    Rect {
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        color: u32,
    },
    Text {
        x: i32,
        y: i32,
        text: String,
        color: u32,
        bold: bool,
        scale: i32,
        underline: bool,
        strike: bool,
    },
    /// A form control, drawn by the browser from `Page::controls`.
    Control(usize),
    /// A background with rounded corners (`border-radius`).
    RoundRect {
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        color: u32,
        radius: i32,
    },
    /// What follows is clipped to this rectangle (`overflow: hidden`),
    /// until the matching `Unclip`.
    Clip {
        x: i32,
        y: i32,
        w: i32,
        h: i32,
    },
    Unclip,
    /// An image, not decoded: its box and its description.
    Image {
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        alt: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub href: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlKind {
    Text,
    Password,
    Submit,
    Button,
    Checkbox,
    Radio,
    Select,
    TextArea,
    Hidden,
    /// `<input type="file">`: a path in the account's home, typed in.
    File,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Control {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub kind: ControlKind,
    pub form: Option<usize>,
    pub name: String,
    pub value: String,
    pub checked: bool,
    /// (value, label) of a `select`.
    pub options: Vec<(String, String)>,
    pub selected: usize,
    pub placeholder: String,
    /// The node, to keep the state of a control across a new layout.
    pub node: NodeId,
    /// A submit button's own `formaction`, `formmethod`, `formenctype`:
    /// empty when the form's apply.
    pub form_action: String,
    pub form_method: String,
    pub form_enctype: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Form {
    pub action: String,
    /// The `method` attribute, upper-case: GET, POST, or DIALOG.
    pub method: String,
    pub enctype: String,
    pub node: NodeId,
}

#[derive(Debug, Clone, Default)]
pub struct Page {
    pub paints: Vec<Paint>,
    pub links: Vec<Link>,
    pub controls: Vec<Control>,
    /// `id` and `name` targets, for `#fragment` links.
    pub anchors: Vec<(String, i32)>,
    pub height: i32,
}

impl Page {
    /// Appends `other`, moved by (dx, dy).
    fn place(&mut self, other: Page, dx: i32, dy: i32) {
        let base = self.controls.len();
        for paint in other.paints {
            self.paints.push(match paint {
                Paint::Rect { x, y, w, h, color } => Paint::Rect { x: x + dx, y: y + dy, w, h, color },
                Paint::Text { x, y, text, color, bold, scale, underline, strike } => Paint::Text {
                    x: x + dx,
                    y: y + dy,
                    text,
                    color,
                    bold,
                    scale,
                    underline,
                    strike,
                },
                Paint::Control(index) => Paint::Control(index + base),
                Paint::RoundRect { x, y, w, h, color, radius } => Paint::RoundRect { x: x + dx, y: y + dy, w, h, color, radius },
                Paint::Clip { x, y, w, h } => Paint::Clip { x: x + dx, y: y + dy, w, h },
                Paint::Unclip => Paint::Unclip,
                Paint::Image { x, y, w, h, alt } => Paint::Image { x: x + dx, y: y + dy, w, h, alt },
            });
        }
        for mut link in other.links {
            link.x += dx;
            link.y += dy;
            self.links.push(link);
        }
        for mut control in other.controls {
            control.x += dx;
            control.y += dy;
            self.controls.push(control);
        }
        for (name, y) in other.anchors {
            self.anchors.push((name, y + dy));
        }
    }
}

/// The result of laying out a document.
#[derive(Debug, Clone, Default)]
pub struct Laid {
    pub page: Page,
    pub forms: Vec<Form>,
    pub background: u32,
    pub width: i32,
}

/// One piece of inline content.
enum Atom {
    Word {
        text: String,
        node: NodeId,
        link: Option<usize>,
        /// A space separates it from the previous atom.
        space: bool,
    },
    Break,
    Box {
        node: NodeId,
        link: Option<usize>,
        space: bool,
    },
    /// A floated box met inside a line.
    Float(NodeId),
    /// An absolutely positioned box met inside a line.
    Absolute(NodeId),
}

/// A floated box, in page coordinates.
#[derive(Debug, Clone, Copy)]
struct FloatBox {
    left: bool,
    x0: i32,
    x1: i32,
    y0: i32,
    y1: i32,
}

/// What a fragment of its own saves of the engine around it.
struct Saved {
    page: Page,
    floats: Vec<FloatBox>,
}

struct Engine<'a> {
    document: &'a Document,
    styles: &'a [Style],
    page: Page,
    forms: Vec<Form>,
    /// Hrefs of the links, indexed by `Atom::Word::link`.
    hrefs: Vec<String>,
    viewport_height: i32,
    headroom: &'a dyn Fn() -> usize,
    preferred: Vec<Option<i32>>,
    minimum: Vec<Option<i32>>,
    /// Laying out stopped somewhere for want of stack.
    truncated: bool,
    /// The floats of the current block formatting context.
    floats: Vec<FloatBox>,
    /// Absolutely positioned boxes waiting for their containing block to
    /// be sized: (node, static x, static y), one list per positioned
    /// ancestor, the viewport first.
    containers: Vec<Vec<(NodeId, i32, i32)>>,
}

fn is_blank(text: &str) -> bool {
    text.chars().all(|c| c.is_whitespace() && c != '\u{a0}')
}

impl<'a> Engine<'a> {
    fn style(&self, node: NodeId) -> &'a Style {
        &self.styles[node]
    }

    fn deep_enough(&mut self) -> bool {
        if (self.headroom)() < STACK_FLOOR {
            self.truncated = true;
            return true;
        }
        false
    }

    /// Children as the layout sees them: `display: contents` replaced by
    /// its own children.
    fn children(&self, node: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        for child in &self.document.nodes[node].children {
            if let NodeData::Element { .. } = self.document.nodes[*child].data
                && self.styles[*child].display == Display::Contents
            {
                out.extend(self.children(*child));
            } else {
                out.push(*child);
            }
        }
        out
    }

    fn is_inline_level(&self, node: NodeId) -> bool {
        match &self.document.nodes[node].data {
            NodeData::Text(_) => true,
            NodeData::Element { .. } => {
                let style = self.style(node);
                matches!(style.display, Display::Inline | Display::InlineBlock)
                    && style.float == Float::None
                    && !style.out_of_flow
            }
            NodeData::Document => false,
        }
    }

    /// Not laid out at all.
    ///
    /// Positioned boxes (`absolute`, `fixed`) that are invisible are among
    /// these. A browser takes a positioned box out of the flow and places
    /// it by its offsets, over the rest. The invisible ones are menus,
    /// tooltips and pop-ups waiting for a hover or a script; laid out in
    /// the flow they pushed a Wikipedia article five thousand pixels down
    /// under a hundred and fifty hidden language links. The visible ones
    /// -- a whole centred home page, a badge -- stay, at the place they
    /// would have had in the flow: approximate, and present.
    fn hidden(&self, node: NodeId) -> bool {
        let style = self.style(node);
        style.display == Display::None
            || style.hidden_offscreen
            || (style.out_of_flow && (!style.visible || style.transparent || style.slid_out))
    }

    // --------------------------------------------------------
    //  Intrinsic widths
    // --------------------------------------------------------

    fn text_width(text: &str, style: &Style) -> i32 {
        text.len() as i32 * GLYPH_W * style.scale()
    }

    fn horizontal_extra(style: &Style, base: i32) -> i32 {
        style.margin[1].or_zero(base)
            + style.margin[3].or_zero(base)
            + style.padding[1].or_zero(base)
            + style.padding[3].or_zero(base)
            + style.border[1]
            + style.border[3]
    }

    /// Preferred (max-content) width: the content on one line.
    fn preferred(&mut self, node: NodeId) -> i32 {
        if let Some(width) = self.preferred[node] {
            return width;
        }
        if self.deep_enough() {
            return 0;
        }
        let width = match &self.document.nodes[node].data {
            NodeData::Text(content) => {
                let style = self.style(node);
                let folded = text::fold(content);
                if style.pre {
                    folded.lines().map(|line| Self::text_width(line, style)).max().unwrap_or(0)
                } else {
                    Self::text_width(text::collapse_whitespace(&folded).trim(), style)
                }
            }
            NodeData::Element { tag, .. } => {
                if self.hidden(node) {
                    0
                } else if let Some(size) = self.replaced_size(node, tag) {
                    size.0
                } else {
                    let style = self.style(node);
                    let own = match style.width {
                        Length::Px(pixels) => Some(pixels),
                        _ => None,
                    };
                    let content = match own {
                        Some(pixels) => pixels,
                        None => {
                            let mut run = 0;
                            let mut widest = 0;
                            for child in self.children(node) {
                                if self.is_inline_level(child) {
                                    run += self.preferred(child) + GLYPH_W;
                                } else {
                                    widest = widest.max(run);
                                    run = 0;
                                    widest = widest.max(self.preferred(child));
                                }
                            }
                            widest.max(run)
                        }
                    };
                    content + Self::horizontal_extra(style, 0)
                }
            }
            NodeData::Document => 0,
        }
        .clamp(0, 20_000);
        self.preferred[node] = Some(width);
        width
    }

    /// Minimum (min-content) width: the widest word or control.
    fn minimum(&mut self, node: NodeId) -> i32 {
        if let Some(width) = self.minimum[node] {
            return width;
        }
        if self.deep_enough() {
            return 0;
        }
        let width = match &self.document.nodes[node].data {
            NodeData::Text(content) => {
                let style = self.style(node);
                let folded = text::fold(content);
                if style.pre || style.nowrap {
                    self.preferred(node)
                } else {
                    folded
                        .split_whitespace()
                        .map(|word| Self::text_width(word, style))
                        .max()
                        .unwrap_or(0)
                }
            }
            NodeData::Element { tag, .. } => {
                if self.hidden(node) {
                    0
                } else if let Some(size) = self.replaced_size(node, tag) {
                    size.0
                } else {
                    let style = self.style(node);
                    // A row that does not wrap is as narrow as all its
                    // items side by side at their narrowest.
                    let row = (style.display == Display::Flex || style.inline_flex)
                        && !style.flex_column
                        && !style.flex_wrap
                        && !style.grid;
                    let gap = style.gap;
                    let children = self.children(node);
                    let mut widest = 0;
                    let mut count = 0;
                    for child in children {
                        let minimum = self.minimum(child);
                        if row {
                            if minimum > 0 {
                                widest += minimum;
                                count += 1;
                            }
                        } else {
                            widest = widest.max(minimum);
                        }
                    }
                    if count > 1 {
                        widest += gap * (count - 1);
                    }
                    if let Length::Px(pixels) = style.width {
                        widest = widest.max(pixels);
                    }
                    widest + Self::horizontal_extra(style, 0)
                }
            }
            NodeData::Document => 0,
        }
        .clamp(0, 20_000);
        self.minimum[node] = Some(width);
        width
    }

    /// Size of a replaced element: images and form controls.
    fn replaced_size(&self, node: NodeId, tag: &str) -> Option<(i32, i32)> {
        let style = self.style(node);
        let attribute_px = |name: &str| {
            self.document
                .attribute(node, name)
                .and_then(|value| value.trim().trim_end_matches("px").parse::<i32>().ok())
        };
        match tag {
            "img" => {
                let width = match style.width {
                    Length::Px(pixels) => Some(pixels),
                    _ => attribute_px("width"),
                };
                let height = match style.height {
                    Length::Px(pixels) => Some(pixels),
                    _ => attribute_px("height"),
                };
                match (width, height) {
                    (Some(w), Some(h)) => Some((w.clamp(1, 2000), h.clamp(1, 2000))),
                    (Some(w), None) => Some((w.clamp(1, 2000), (w * 3 / 4).clamp(1, 1500))),
                    (None, Some(h)) => Some(((h * 4 / 3).clamp(1, 2000), h.clamp(1, 1500))),
                    // No size given: a line high, as wide as its alt text.
                    (None, None) => {
                        let alt = self.document.attribute(node, "alt").unwrap_or("");
                        let alt = text::fold(alt);
                        if alt.trim().is_empty() {
                            Some((0, 0))
                        } else {
                            Some(((alt.len() as i32 + 2) * GLYPH_W, LINE_H))
                        }
                    }
                }
            }
            "input" => {
                let kind = self.document.attribute(node, "type").unwrap_or("text").to_ascii_lowercase();
                let width_attr = match style.width {
                    Length::Px(pixels) => Some(pixels),
                    _ => None,
                };
                match kind.as_str() {
                    "hidden" => Some((0, 0)),
                    "checkbox" | "radio" => Some((16, 16)),
                    "file" => Some((width_attr.unwrap_or(36 * GLYPH_W + 10), 24)),
                    "submit" | "button" | "reset" | "image" => {
                        let label = self.document.attribute(node, "value").unwrap_or(match kind.as_str() {
                            "reset" => "Effacer",
                            _ => "Envoyer",
                        });
                        let label = text::fold(label);
                        Some((width_attr.unwrap_or((label.len() as i32 + 2) * GLYPH_W + 4), 24))
                    }
                    _ => {
                        let size = self
                            .document
                            .attribute(node, "size")
                            .and_then(|value| value.trim().parse::<i32>().ok())
                            .unwrap_or(20)
                            .clamp(2, 100);
                        Some((width_attr.unwrap_or(size * GLYPH_W + 10), 24))
                    }
                }
            }
            "button" => {
                let label = text::fold(&self.document.text_content(node));
                let width = match style.width {
                    Length::Px(pixels) => pixels,
                    _ => (label.len() as i32 + 2) * GLYPH_W + 4,
                };
                Some((width.max(24), 24))
            }
            "select" => {
                let widest = self
                    .document
                    .elements("option")
                    .into_iter()
                    .filter(|option| self.document.is_inside(*option, node))
                    .map(|option| text::fold(&self.document.text_content(option)).len() as i32)
                    .max()
                    .unwrap_or(4)
                    .min(40);
                Some(((widest + 4) * GLYPH_W, 24))
            }
            "textarea" => {
                let columns = self
                    .document
                    .attribute(node, "cols")
                    .and_then(|value| value.parse::<i32>().ok())
                    .unwrap_or(40)
                    .clamp(4, 120);
                let rows = self
                    .document
                    .attribute(node, "rows")
                    .and_then(|value| value.parse::<i32>().ok())
                    .unwrap_or(3)
                    .clamp(1, 20);
                Some((columns * GLYPH_W + 10, rows * LINE_H + 8))
            }
            _ => None,
        }
    }

    // --------------------------------------------------------
    //  Fragments, floats, positioned boxes
    // --------------------------------------------------------

    /// Starts laying out a box on its own, at the origin: a fresh page, a
    /// block formatting context of its own (no outer float leaks in), and
    /// a frame for positioned boxes that find no containing block inside.
    fn begin_fragment(&mut self) -> Saved {
        self.containers.push(Vec::new());
        Saved {
            page: core::mem::take(&mut self.page),
            floats: core::mem::take(&mut self.floats),
        }
    }

    /// Ends it: its positioned leftovers are placed in its own box, and
    /// the fragment is handed back for the caller to place.
    fn end_fragment(&mut self, saved: Saved, width: i32, height: i32) -> Page {
        let pending = self.containers.pop().unwrap_or_default();
        self.layout_absolutes(pending, 0, 0, width, height);
        self.floats = saved.floats;
        core::mem::replace(&mut self.page, saved.page)
    }

    /// Records a positioned box for its containing block: the nearest
    /// positioned ancestor, the viewport for `fixed`.
    fn defer_absolute(&mut self, node: NodeId, x: i32, y: i32) {
        let fixed = self.style(node).position == Position::Fixed;
        let frame = if fixed { self.containers.first_mut() } else { self.containers.last_mut() };
        if let Some(frame) = frame {
            frame.push((node, x, y));
        }
    }

    /// Places positioned boxes in a containing block (its padding box).
    fn layout_absolutes(&mut self, pending: Vec<(NodeId, i32, i32)>, cx: i32, cy: i32, cw: i32, ch: i32) {
        for (node, static_x, static_y) in pending {
            if self.deep_enough() {
                return;
            }
            let style = self.style(node);
            let left = style.offsets[3].resolve(cw);
            let right = style.offsets[1].resolve(cw);
            let top = style.offsets[0].resolve(ch);
            let bottom = style.offsets[2].resolve(ch);
            let extra = Self::horizontal_extra(style, cw);
            let width = match style.width.resolve(cw) {
                Some(given) => {
                    if style.border_box {
                        given
                    } else {
                        given + extra
                    }
                }
                None => match (left, right) {
                    (Some(l), Some(r)) => (cw - l - r).max(0),
                    _ => self.preferred(node).min(cw).max(self.minimum(node).min(cw)),
                },
            }
            .max(1);
            let saved = self.begin_fragment();
            let (height, _) = self.block(node, 0, 0, width, 0);
            let fragment = self.end_fragment(saved, width, height);
            let x = left.map(|l| cx + l).or(right.map(|r| cx + cw - r - width)).unwrap_or(static_x);
            let y = top.map(|t| cy + t).or(bottom.map(|b| cy + ch - b - height)).unwrap_or(static_y);
            self.page.place(fragment, x, y);
        }
    }

    /// (left inset, right inset) the floats make at `y` over `height`
    /// inside `[x, x + width)`.
    fn insets(&self, y: i32, height: i32, x: i32, width: i32) -> (i32, i32) {
        let mut left = 0;
        let mut right = 0;
        for float in &self.floats {
            if float.y1 <= y || float.y0 >= y + height || float.x1 <= x || float.x0 >= x + width {
                continue;
            }
            if float.left {
                left = left.max(float.x1 - x);
            } else {
                right = right.max(x + width - float.x0);
            }
        }
        (left, right)
    }

    /// The span a line starting at `y` can use, moved down past floats
    /// that leave it too narrow. Returns (y, left, width).
    fn line_span(&self, x: i32, mut y: i32, width: i32, height: i32) -> (i32, i32, i32) {
        for _ in 0..32 {
            let (left, right) = self.insets(y, height, x, width);
            let available = width - left - right;
            if available >= (4 * GLYPH_W).min(width) || (left == 0 && right == 0) {
                return (y, x + left, available.max(GLYPH_W));
            }
            // Below the float that ends first among those in the way.
            let next = self
                .floats
                .iter()
                .filter(|f| f.y1 > y && f.y0 < y + height)
                .map(|f| f.y1)
                .min()
                .unwrap_or(y + height);
            y = next.max(y + 1);
        }
        (y, x, width)
    }

    /// The bottom of the floats `clear` asks to go below.
    fn cleared(&self, clear: u8, y: i32) -> i32 {
        self.floats
            .iter()
            .filter(|f| (f.left && clear & 1 != 0) || (!f.left && clear & 2 != 0))
            .map(|f| f.y1)
            .fold(y, i32::max)
    }

    /// Lays out a float and puts it at the left or right edge, at `y` or
    /// lower, beside the floats already there.
    fn place_float(&mut self, node: NodeId, x: i32, y: i32, width: i32) {
        if self.hidden(node) || self.deep_enough() {
            return;
        }
        let style = self.style(node);
        let left = style.float != Float::Right;
        let extra = Self::horizontal_extra(style, width);
        let float_width = match style.width.resolve(width) {
            Some(given) => {
                if style.border_box {
                    given
                } else {
                    given + extra
                }
            }
            None => self.preferred(node).min(width).max(self.minimum(node).min(width)),
        }
        .clamp(1, width.max(1));
        let saved = self.begin_fragment();
        let (height, bottom) = self.block(node, 0, 0, float_width, 0);
        let height = (height + bottom).max(1);
        let fragment = self.end_fragment(saved, float_width, height);
        let y = self.cleared(style.clear, y);
        let mut top = y;
        for _ in 0..32 {
            let (inset_left, inset_right) = self.insets(top, height, x, width);
            if float_width <= width - inset_left - inset_right || (inset_left == 0 && inset_right == 0) {
                let fx = if left { x + inset_left } else { x + width - inset_right - float_width };
                self.page.place(fragment, fx, top);
                self.floats.push(FloatBox {
                    left,
                    x0: fx,
                    x1: fx + float_width,
                    y0: top,
                    y1: top + height,
                });
                return;
            }
            top = self
                .floats
                .iter()
                .filter(|f| f.y1 > top && f.y0 < top + height)
                .map(|f| f.y1)
                .min()
                .unwrap_or(top + height);
        }
        self.page.place(fragment, x, top);
    }

    /// Shifts what was painted since the marks: `position: relative`.
    fn shift_since(&mut self, marks: (usize, usize, usize, usize), dx: i32, dy: i32) {
        if dx == 0 && dy == 0 {
            return;
        }
        let mut paints = self.page.paints.split_off(marks.0);
        // `place` numbers controls from where they will land: make the
        // ones split off relative to the split first.
        for paint in paints.iter_mut() {
            if let Paint::Control(index) = paint {
                *index -= marks.2;
            }
        }
        let tail = Page {
            paints,
            links: self.page.links.split_off(marks.1),
            controls: self.page.controls.split_off(marks.2),
            anchors: self.page.anchors.split_off(marks.3),
            height: 0,
        };
        self.page.place(tail, dx, dy);
    }

    // --------------------------------------------------------
    //  Block flow
    // --------------------------------------------------------

    /// Lays out a block-level box at `(x, y)` in a containing block
    /// `width` wide. `collapse` is the bottom margin of the previous
    /// sibling, to collapse with this top margin. Returns (height used,
    /// bottom margin left to collapse).
    fn block(&mut self, node: NodeId, x: i32, y: i32, width: i32, collapse: i32) -> (i32, i32) {
        if self.hidden(node) || self.deep_enough() {
            return (0, collapse);
        }
        let style = self.style(node);
        let tag = self.document.tag(node).unwrap_or("");
        let marks = (
            self.page.paints.len(),
            self.page.links.len(),
            self.page.controls.len(),
            self.page.anchors.len(),
        );
        let floats_before = self.floats.len();
        let positioned = style.position != Position::Static;
        if positioned {
            self.containers.push(Vec::new());
        }

        let margin_top = style.margin[0].or_zero(width);
        let margin_bottom = style.margin[2].or_zero(width);
        let mut margin_left = style.margin[3].or_zero(width);
        let mut margin_right = style.margin[1].or_zero(width);
        let padding: [i32; 4] = [
            style.padding[0].or_zero(width),
            style.padding[1].or_zero(width),
            style.padding[2].or_zero(width),
            style.padding[3].or_zero(width),
        ];
        let border = style.border;
        let frame = padding[1] + padding[3] + border[1] + border[3];

        // Width: given, or what is left.
        let available = (width - margin_left - margin_right - frame).max(0);
        let mut content_width = match style.width.resolve(width) {
            // `box-sizing: border-box` counts padding and border in.
            Some(given) if style.border_box => (given - frame).max(0).min(available.max(0)),
            Some(given) => given.min(available.max(given.min(width))).max(0),
            None => available,
        };
        if let Some(max) = style.max_width.resolve(width) {
            content_width = content_width.min(max.max(0));
        }
        if let Some(min) = style.min_width.resolve(width) {
            content_width = content_width.max(min.min(width));
        }
        if tag == "table" && style.width == Length::Auto {
            // A table is as wide as its content needs, up to the space.
            content_width = content_width.min(self.preferred(node).max(self.minimum(node)).max(0));
        }
        // `margin: auto` on both sides centres a box narrower than its
        // container.
        let leftover = width - content_width - frame - margin_left - margin_right;
        if leftover > 0 {
            match (style.margin[3], style.margin[1]) {
                (Length::Auto, Length::Auto) => {
                    margin_left += leftover / 2;
                    margin_right += leftover - leftover / 2;
                }
                (Length::Auto, _) => margin_left += leftover,
                _ => {}
            }
        }
        let _ = margin_right;

        let top_gap = margin_top.max(collapse) - collapse;
        let box_x = x + margin_left;
        let box_y = y + top_gap;
        let content_x = box_x + border[3] + padding[3];
        let content_y = box_y + border[0] + padding[0];

        // The background goes under everything: its slot is taken now and
        // filled once the height is known.
        let background_slot = self.page.paints.len();
        if style.background.is_some() || border.iter().any(|b| *b > 0) {
            self.page.paints.push(Paint::Rect { x: 0, y: 0, w: 0, h: 0, color: 0 });
        }
        // `overflow: hidden`: the content is clipped to the padding box,
        // known once the height is.
        let clip_slot = self.page.paints.len();
        if style.clip {
            self.page.paints.push(Paint::Clip { x: 0, y: 0, w: 0, h: 0 });
        }

        if let Some(id) = self.document.attribute(node, "id") {
            self.page.anchors.push((String::from(id), box_y));
        }

        let replaced = self.replaced_size(node, tag);
        let content_height = match style.display {
            // An image or a control made a block: its own size, unless the
            // style gives a width.
            _ if replaced.is_some() => {
                let (natural_width, height) = replaced.unwrap_or((0, 0));
                let width = if style.width == Length::Auto { natural_width.min(content_width) } else { content_width };
                if tag == "img" {
                    if width > 0 && height > 0 {
                        let alt = text::fold(self.document.attribute(node, "alt").unwrap_or(""));
                        self.page.paints.push(Paint::Image { x: content_x, y: content_y, w: width, h: height, alt });
                    }
                } else if let Some(control) = self.control(node, tag, content_x, content_y, width, height) {
                    let index = self.page.controls.len();
                    self.page.controls.push(control);
                    self.page.paints.push(Paint::Control(index));
                }
                height
            }
            Display::Table => self.table(node, content_x, content_y, content_width),
            _ if (style.display == Display::Flex || style.inline_flex) && !style.flex_column => {
                self.flex(node, content_x, content_y, content_width)
            }
            _ => {
                let height = self.flow(node, content_x, content_y, content_width);
                // No marker for an item with nothing to show.
                if style.display == Display::ListItem && style.list_style != ListStyle::None && height > 0 {
                    self.marker(node, content_x, content_y);
                }
                height
            }
        };
        // A block holds the floats started inside it: without this, a
        // container of floats alone -- the "clearfix" pattern -- would
        // have no height, and what follows would climb under them.
        let float_bottom = self.floats[floats_before.min(self.floats.len())..].iter().map(|f| f.y1).max();
        let content_height = match float_bottom {
            Some(bottom) => content_height.max(bottom - content_y),
            None => content_height,
        };
        self.floats.truncate(floats_before);
        let content_height = match style.height {
            // A clipped box is exactly as tall as asked: what overflows is
            // cut. Elsewhere a height only grows the box -- content drawn
            // past a box that stays short would overlap what follows.
            Length::Px(pixels) if style.clip => {
                let given = if style.border_box { pixels - padding[0] - padding[2] - border[0] - border[2] } else { pixels };
                given.clamp(0, 4000)
            }
            Length::Px(pixels) => content_height.max(pixels.min(4000)),
            _ => content_height,
        };
        let content_height = match style.min_height {
            Length::Px(pixels) => content_height.max(pixels.min(4000)),
            _ => content_height,
        };
        let box_width = content_width + frame;
        let box_height = content_height + padding[0] + padding[2] + border[0] + border[2];
        if style.clip && clip_slot < self.page.paints.len() {
            self.page.paints[clip_slot] = Paint::Clip {
                x: box_x + border[3],
                y: box_y + border[0],
                w: box_width - border[1] - border[3],
                h: box_height - border[0] - border[2],
            };
            self.page.paints.push(Paint::Unclip);
        }

        if background_slot < self.page.paints.len()
            && matches!(self.page.paints[background_slot], Paint::Rect { w: 0, h: 0, .. })
        {
            let color = style.background.unwrap_or(0xffffff);
            if style.background.is_some() && style.radius > 0 {
                self.page.paints[background_slot] =
                    Paint::RoundRect { x: box_x, y: box_y, w: box_width, h: box_height, color, radius: style.radius };
            } else if style.background.is_some() {
                self.page.paints[background_slot] = Paint::Rect { x: box_x, y: box_y, w: box_width, h: box_height, color };
            } else {
                self.page.paints[background_slot] = Paint::Rect { x: 0, y: 0, w: 0, h: 0, color: 0 };
            }
            let c = style.border_color;
            if border[0] > 0 {
                self.page.paints.push(Paint::Rect { x: box_x, y: box_y, w: box_width, h: border[0], color: c });
            }
            if border[2] > 0 {
                self.page.paints.push(Paint::Rect {
                    x: box_x,
                    y: box_y + box_height - border[2],
                    w: box_width,
                    h: border[2],
                    color: c,
                });
            }
            if border[3] > 0 {
                self.page.paints.push(Paint::Rect { x: box_x, y: box_y, w: border[3], h: box_height, color: c });
            }
            if border[1] > 0 {
                self.page.paints.push(Paint::Rect {
                    x: box_x + box_width - border[1],
                    y: box_y,
                    w: border[1],
                    h: box_height,
                    color: c,
                });
            }
        }
        if positioned {
            // Its absolutely positioned descendants, in its padding box.
            let pending = self.containers.pop().unwrap_or_default();
            self.layout_absolutes(
                pending,
                box_x + border[3],
                box_y + border[0],
                box_width - border[1] - border[3],
                box_height - border[0] - border[2],
            );
            if style.position == Position::Relative {
                let dx = style.offsets[3].resolve(width).or(style.offsets[1].resolve(width).map(|r| -r)).unwrap_or(0);
                let dy = style.offsets[0].resolve(width).or(style.offsets[2].resolve(width).map(|b| -b)).unwrap_or(0);
                self.shift_since(marks, dx.clamp(-2000, 2000), dy.clamp(-2000, 2000));
            }
        }
        if tag == "hr" && box_height == 0 {
            self.page.paints.push(Paint::Rect { x: box_x, y: box_y, w: box_width, h: 1, color: 0xaaaaaa });
            return (top_gap + 2, margin_bottom);
        }
        (top_gap + box_height, margin_bottom)
    }

    fn marker(&mut self, node: NodeId, x: i32, y: i32) {
        let style = self.style(node);
        let text = match style.list_style {
            ListStyle::Decimal => {
                let parent = self.document.nodes[node].parent.unwrap_or(0);
                let start = self
                    .document
                    .attribute(parent, "start")
                    .and_then(|value| value.parse::<i32>().ok())
                    .unwrap_or(1);
                let position = self.document.nodes[parent]
                    .children
                    .iter()
                    .filter(|child| self.document.tag(**child) == Some("li"))
                    .position(|child| *child == node)
                    .unwrap_or(0) as i32;
                alloc::format!("{}.", start + position)
            }
            _ => String::from("*"),
        };
        let width = text.len() as i32 * GLYPH_W;
        self.page.paints.push(Paint::Text {
            x: x - width - 6,
            y: y + 2,
            text,
            color: style.color,
            bold: false,
            scale: 1,
            underline: false,
            strike: false,
        });
    }

    /// Lays out the children of `node`: blocks one under the other, and
    /// runs of inline content between them as paragraphs of lines.
    /// Floats go to the edges, positioned boxes to their containing block,
    /// and `::before` / `::after` content opens and closes the flow.
    fn flow(&mut self, node: NodeId, x: i32, y: i32, width: i32) -> i32 {
        let style = self.style(node);
        let mut lead = style.before.clone();
        let trail = style.after.clone();
        let mut cursor = y;
        let mut collapse = 0;
        let mut run: Vec<NodeId> = Vec::new();
        let children = self.children(node);
        for child in children {
            if self.is_inline_level(child) {
                run.push(child);
                continue;
            }
            if let NodeData::Element { .. } = self.document.nodes[child].data {
                if self.hidden(child) {
                    continue;
                }
                let child_style = self.style(child);
                if child_style.out_of_flow {
                    self.defer_absolute(child, x, cursor);
                    continue;
                }
                if !run.is_empty() || lead.is_some() {
                    let height = self.inline(node, &run, x, cursor, width, lead.take().as_deref().map(|t| t.as_str()), None);
                    if height > 0 {
                        cursor += height;
                        collapse = 0;
                    }
                    run.clear();
                }
                if child_style.float != Float::None {
                    self.place_float(child, x, cursor, width);
                    continue;
                }
                if child_style.clear != 0 {
                    let below = self.cleared(child_style.clear, cursor);
                    if below > cursor {
                        cursor = below;
                        collapse = 0;
                    }
                }
                let (height, bottom) = self.block(child, x, cursor, width, collapse);
                cursor += height;
                collapse = bottom;
            }
        }
        if !run.is_empty() || lead.is_some() || trail.is_some() {
            let height = self.inline(
                node,
                &run,
                x,
                cursor,
                width,
                lead.take().as_deref().map(|t| t.as_str()),
                trail.as_deref().map(|t| t.as_str()),
            );
            if height > 0 {
                cursor += height;
                collapse = 0;
            }
        }
        cursor - y + collapse
    }

    /// Words of generated content, in the style of `node`.
    fn generated(&mut self, node: NodeId, content: &str, link: Option<usize>, atoms: &mut Vec<Atom>, space: &mut bool) {
        let folded = text::fold(content);
        let mut first = true;
        for word in folded.split_whitespace() {
            atoms.push(Atom::Word {
                text: String::from(word),
                node,
                link,
                space: if first { *space || folded.starts_with(' ') } else { true },
            });
            first = false;
        }
        if !first {
            *space = folded.ends_with(' ');
        }
    }

    // --------------------------------------------------------
    //  Inline flow
    // --------------------------------------------------------

    fn collect_atoms(&mut self, node: NodeId, link: Option<usize>, atoms: &mut Vec<Atom>, space: &mut bool) {
        if self.deep_enough() {
            return;
        }
        match &self.document.nodes[node].data {
            NodeData::Text(content) => {
                let style = self.style(node);
                if !style.visible && is_blank(content) {
                    return;
                }
                let folded = text::fold(content);
                let folded = if style.uppercase { folded.to_ascii_uppercase() } else { folded };
                if style.pre {
                    for (index, line) in folded.split('\n').enumerate() {
                        if index > 0 {
                            atoms.push(Atom::Break);
                        }
                        if !line.is_empty() {
                            atoms.push(Atom::Word {
                                text: line.replace('\t', "    "),
                                node,
                                link,
                                space: false,
                            });
                        }
                    }
                    *space = false;
                    return;
                }
                // A space between two atoms comes from either side: the
                // end of the previous text or the start of this one.
                let leading = folded.starts_with(|c: char| c.is_whitespace());
                let trailing = folded.ends_with(|c: char| c.is_whitespace());
                let mut first = true;
                for word in folded.split_whitespace() {
                    atoms.push(Atom::Word {
                        text: String::from(word),
                        node,
                        link,
                        space: if first { *space || leading } else { true },
                    });
                    first = false;
                }
                if first {
                    // Only white space: it separates what surrounds it.
                    *space |= !folded.is_empty();
                } else {
                    *space = trailing;
                }
            }
            NodeData::Element { tag, .. } => {
                if self.hidden(node) {
                    return;
                }
                let style = self.style(node);
                if style.out_of_flow {
                    atoms.push(Atom::Absolute(node));
                    return;
                }
                if style.float != Float::None {
                    atoms.push(Atom::Float(node));
                    return;
                }
                match tag.as_str() {
                    "br" => {
                        atoms.push(Atom::Break);
                        *space = false;
                        return;
                    }
                    "img" | "input" | "button" | "select" | "textarea" => {
                        atoms.push(Atom::Box { node, link, space: *space });
                        *space = false;
                        return;
                    }
                    _ => {}
                }
                if style.display == Display::InlineBlock {
                    atoms.push(Atom::Box { node, link, space: *space });
                    *space = false;
                    return;
                }
                let link = if tag == "a" {
                    match self.document.attribute(node, "href") {
                        Some(href) if !href.trim().is_empty() && !href.trim_start().starts_with("javascript:") => {
                            self.hrefs.push(String::from(href.trim()));
                            Some(self.hrefs.len() - 1)
                        }
                        _ => link,
                    }
                } else {
                    link
                };
                if tag == "a" && (self.document.attribute(node, "name").is_some() || self.document.attribute(node, "id").is_some()) {
                    // An anchor target: an empty word, recorded at the
                    // line it lands on.
                    atoms.push(Atom::Word {
                        text: String::new(),
                        node,
                        link: None,
                        space: false,
                    });
                }
                if let Some(before) = style.before.clone() {
                    self.generated(node, &before, link, atoms, space);
                }
                for child in self.children(node) {
                    self.collect_atoms(child, link, atoms, space);
                }
                if let Some(after) = style.after.clone() {
                    self.generated(node, &after, link, atoms, space);
                }
            }
            NodeData::Document => {}
        }
    }

    /// Lays out a run of inline nodes as lines. Returns the height.
    ///
    /// Each line takes the span the floats leave at its height; `lead` and
    /// `trail` are the container's generated content.
    #[allow(clippy::too_many_arguments)]
    fn inline(&mut self, container: NodeId, run: &[NodeId], x: i32, y: i32, width: i32, lead: Option<&str>, trail: Option<&str>) -> i32 {
        let mut atoms = Vec::new();
        let mut space = false;
        if let Some(lead) = lead {
            self.generated(container, lead, None, &mut atoms, &mut space);
        }
        for node in run {
            self.collect_atoms(*node, None, &mut atoms, &mut space);
        }
        if let Some(trail) = trail {
            self.generated(container, trail, None, &mut atoms, &mut space);
        }
        if !atoms.iter().any(|atom| match atom {
            Atom::Word { text, .. } => !text.is_empty(),
            _ => true,
        }) {
            return 0;
        }
        let container_style = self.style(container);
        let align = container_style.align;
        let nowrap = container_style.nowrap;
        let width = width.max(GLYPH_W);
        let base_height = if container_style.line_height > 0 { container_style.line_height.max(16) } else { LINE_H };

        // A line under construction: (x offset, fragment) pairs.
        let mut line: Vec<(i32, Placed)> = Vec::new();
        let mut line_x = 0;
        let mut line_height = 0;
        let (mut cursor_y, mut line_left, mut line_width) = self.line_span(x, y, width, base_height);

        for atom in atoms {
            match atom {
                Atom::Break => {
                    let height = if line.is_empty() { base_height } else { line_height };
                    self.finish_line(&mut line, line_left, cursor_y, line_width, line_x, height, align);
                    (cursor_y, line_left, line_width) = self.line_span(x, cursor_y + height, width, base_height);
                    line_x = 0;
                    line_height = 0;
                }
                Atom::Float(node) => {
                    self.place_float(node, x, cursor_y, width);
                    if line.is_empty() {
                        (cursor_y, line_left, line_width) = self.line_span(x, cursor_y, width, base_height);
                    }
                }
                Atom::Absolute(node) => self.defer_absolute(node, line_left + line_x, cursor_y),
                Atom::Word { text, node, link, space } => {
                    let style = self.style(node);
                    if text.is_empty() {
                        // An anchor target.
                        if let Some(name) = self.document.attribute(node, "name").or(self.document.attribute(node, "id")) {
                            self.page.anchors.push((String::from(name), cursor_y));
                        }
                        continue;
                    }
                    let scale = style.scale();
                    let glyph = GLYPH_W * scale;
                    let height = if scale > 1 {
                        (LINE_H * scale - 4).max(style.line_height)
                    } else if style.line_height > 0 {
                        style.line_height.max(16)
                    } else {
                        base_height
                    };
                    let gap = if space && !line.is_empty() { glyph } else { 0 };
                    let mut word = text;
                    let mut word_width = word.len() as i32 * glyph;
                    if !nowrap && !style.pre && line_x + gap + word_width > line_width && !line.is_empty() {
                        self.finish_line(&mut line, line_left, cursor_y, line_width, line_x, line_height, align);
                        (cursor_y, line_left, line_width) = self.line_span(x, cursor_y + line_height, width, base_height);
                        line_x = 0;
                        line_height = 0;
                    }
                    let gap = if line.is_empty() { 0 } else { gap };
                    // A word wider than the line is cut -- unless the line
                    // is narrower than a few letters, where cutting would
                    // stack the word one letter per line; it overflows.
                    while !nowrap && !style.pre && word_width > line_width && word.len() > 1 && line_width >= 4 * glyph {
                        let fit = ((line_width - line_x) / glyph).max(1) as usize;
                        let fit = fit.min(word.len());
                        let (head, tail) = word.split_at(fit);
                        let head = String::from(head);
                        let tail = String::from(tail);
                        line.push((line_x + gap, Placed::Text { text: head.clone(), node, link, width: head.len() as i32 * glyph, height }));
                        line_height = line_height.max(height);
                        line_x += gap + head.len() as i32 * glyph;
                        self.finish_line(&mut line, line_left, cursor_y, line_width, line_x, line_height, align);
                        (cursor_y, line_left, line_width) = self.line_span(x, cursor_y + line_height, width, base_height);
                        line_x = 0;
                        line_height = 0;
                        word = tail;
                        word_width = word.len() as i32 * glyph;
                    }
                    let gap = if line.is_empty() { 0 } else { gap };
                    line.push((line_x + gap, Placed::Text { text: word, node, link, width: word_width, height }));
                    line_x += gap + word_width;
                    line_height = line_height.max(height);
                }
                Atom::Box { node, link, space } => {
                    let (fragment, box_width, box_height) = self.inline_box(node, line_width);
                    let gap = if space && !line.is_empty() { GLYPH_W } else { 0 };
                    if !nowrap && line_x + gap + box_width > line_width && !line.is_empty() {
                        self.finish_line(&mut line, line_left, cursor_y, line_width, line_x, line_height, align);
                        (cursor_y, line_left, line_width) = self.line_span(x, cursor_y + line_height, width, base_height);
                        line_x = 0;
                        line_height = 0;
                    }
                    let gap = if line.is_empty() { 0 } else { gap };
                    line.push((line_x + gap, Placed::Box { fragment, link, width: box_width, height: box_height }));
                    line_x += gap + box_width;
                    line_height = line_height.max(box_height);
                }
            }
        }
        if !line.is_empty() {
            self.finish_line(&mut line, line_left, cursor_y, line_width, line_x, line_height, align);
            cursor_y += line_height;
        }
        cursor_y - y
    }

    /// Lays out an inline-level box (image, control, inline-block) on its
    /// own, at the origin; the line places it.
    fn inline_box(&mut self, node: NodeId, available: i32) -> (Page, i32, i32) {
        let tag = self.document.tag(node).unwrap_or("");
        let style = self.style(node);
        let margin_left = style.margin[3].or_zero(available).max(0);
        let margin_right = style.margin[1].or_zero(available).max(0);
        if let Some((width, height)) = self.replaced_size(node, tag) {
            let width = width.min(available.max(8));
            let mut fragment = Page::default();
            match tag {
                "img" => {
                    if width > 0 && height > 0 {
                        let alt = text::fold(self.document.attribute(node, "alt").unwrap_or(""));
                        fragment.paints.push(Paint::Image { x: margin_left, y: 0, w: width, h: height, alt });
                    }
                }
                _ => {
                    if let Some(control) = self.control(node, tag, margin_left, 0, width, height) {
                        let index = fragment.controls.len();
                        fragment.controls.push(control);
                        fragment.paints.push(Paint::Control(index));
                    }
                }
            }
            let height = if width == 0 { 0 } else { height };
            return (fragment, width + margin_left + margin_right, height);
        }
        // An inline-block: shrink to fit, laid out as a block.
        let preferred = self.preferred(node);
        let width = match style.width.resolve(available) {
            Some(given) => given + Self::horizontal_extra(style, available),
            None => preferred.min(available).max(self.minimum(node).min(available)),
        };
        let saved = self.begin_fragment();
        let (height, _) = self.block(node, 0, 0, width, 0);
        let fragment = self.end_fragment(saved, width, height);
        (fragment, width, height)
    }

    fn finish_line(
        &mut self,
        line: &mut Vec<(i32, Placed)>,
        x: i32,
        y: i32,
        width: i32,
        used: i32,
        height: i32,
        align: Align,
    ) {
        let shift = match align {
            Align::Left => 0,
            Align::Center => ((width - used) / 2).max(0),
            Align::Right => (width - used).max(0),
        };
        // Words of one style and one link merge into one run of text:
        // fewer paints, and a link underlined in one stroke.
        let mut pending: Option<(i32, String, NodeId, Option<usize>, i32, i32)> = None;
        for (offset, placed) in line.drain(..) {
            match placed {
                Placed::Text { text, node, link, width: word_width, height: word_height } => {
                    if let Some((start, ref mut joined, run_node, run_link, run_width, run_height)) = pending
                        && run_link == link
                        && self.same_look(run_node, node)
                        && start + run_width <= offset
                    {
                        let glyph = GLYPH_W * self.style(node).scale();
                        let spaces = ((offset - start - run_width) / glyph).max(0) as usize;
                        for _ in 0..spaces {
                            joined.push(' ');
                        }
                        joined.push_str(&text);
                        pending = Some((start, joined.clone(), run_node, run_link, offset + word_width - start, run_height.max(word_height)));
                        continue;
                    }
                    if let Some(run) = pending.take() {
                        self.emit_text(run, x + shift, y, height);
                    }
                    pending = Some((offset, text, node, link, word_width, word_height));
                }
                Placed::Box { fragment, link, width: box_width, height: box_height } => {
                    if let Some(run) = pending.take() {
                        self.emit_text(run, x + shift, y, height);
                    }
                    let box_x = x + shift + offset;
                    let box_y = y + height - box_height;
                    if let Some(link) = link {
                        self.page.links.push(Link {
                            x: box_x,
                            y: box_y,
                            w: box_width,
                            h: box_height,
                            href: self.hrefs[link].clone(),
                        });
                    }
                    self.page.place(fragment, box_x, box_y);
                }
            }
        }
        if let Some(run) = pending.take() {
            self.emit_text(run, x + shift, y, height);
        }
    }

    fn same_look(&self, a: NodeId, b: NodeId) -> bool {
        if a == b {
            return true;
        }
        let (a, b) = (self.style(a), self.style(b));
        a.color == b.color
            && a.bold == b.bold
            && a.underline == b.underline
            && a.strike == b.strike
            && a.scale() == b.scale()
            && a.visible == b.visible
            && a.background == b.background
    }

    fn emit_text(&mut self, run: (i32, String, NodeId, Option<usize>, i32, i32), x: i32, y: i32, line_height: i32) {
        let (offset, text, node, link, width, height) = run;
        let style = self.style(node);
        let top = y + line_height - height + (height - 16 * style.scale()) / 2;
        let left = x + offset;
        if let Some(link) = link {
            self.page.links.push(Link {
                x: left,
                y: y + line_height - height,
                w: width,
                h: height,
                href: self.hrefs[link].clone(),
            });
        }
        if !style.visible {
            return;
        }
        // Text inside an inline element with a background -- a
        // highlighted word, a badge, a `<mark>` -- has it painted under.
        if let Some(parent) = self.document.nodes[node].parent {
            let parent_style = self.style(parent);
            if parent_style.display == Display::Inline
                && let Some(background) = parent_style.background
            {
                self.page.paints.push(Paint::Rect {
                    x: left,
                    y: top - 1,
                    w: width,
                    h: 16 * style.scale() + 2,
                    color: background,
                });
            }
        }
        self.page.paints.push(Paint::Text {
            x: left,
            y: top,
            text,
            color: style.color,
            bold: style.bold,
            scale: style.scale(),
            underline: style.underline,
            strike: style.strike,
        });
    }

    // --------------------------------------------------------
    //  Forms
    // --------------------------------------------------------

    fn form_of(&mut self, node: NodeId) -> Option<usize> {
        let mut current = self.document.nodes[node].parent;
        while let Some(ancestor) = current {
            if self.document.tag(ancestor) == Some("form") {
                if let Some(index) = self.forms.iter().position(|form| form.node == ancestor) {
                    return Some(index);
                }
                let action = self.document.attribute(ancestor, "action").unwrap_or("").to_string();
                let method = self.document.attribute(ancestor, "method").unwrap_or("get").trim().to_ascii_uppercase();
                let enctype = self.document.attribute(ancestor, "enctype").unwrap_or("").to_string();
                self.forms.push(Form { action, method, enctype, node: ancestor });
                return Some(self.forms.len() - 1);
            }
            current = self.document.nodes[ancestor].parent;
        }
        None
    }

    fn control(&mut self, node: NodeId, tag: &str, x: i32, y: i32, w: i32, h: i32) -> Option<Control> {
        let attribute = |name: &str| self.document.attribute(node, name).unwrap_or("").to_string();
        let (kind, value) = match tag {
            "input" => {
                let kind = attribute("type").to_ascii_lowercase();
                let kind = match kind.as_str() {
                    "hidden" => ControlKind::Hidden,
                    "password" => ControlKind::Password,
                    "submit" | "image" => ControlKind::Submit,
                    "button" | "reset" => ControlKind::Button,
                    "checkbox" => ControlKind::Checkbox,
                    "radio" => ControlKind::Radio,
                    "file" => ControlKind::File,
                    _ => ControlKind::Text,
                };
                let value = match kind {
                    ControlKind::Submit if self.document.attribute(node, "value").is_none() => String::from("Envoyer"),
                    ControlKind::Checkbox | ControlKind::Radio if self.document.attribute(node, "value").is_none() => {
                        String::from("on")
                    }
                    _ => attribute("value"),
                };
                (kind, value)
            }
            "button" => {
                let kind = attribute("type").to_ascii_lowercase();
                let kind = if kind == "button" || kind == "reset" { ControlKind::Button } else { ControlKind::Submit };
                let label = self.document.text_content(node);
                let value = self.document.attribute(node, "value").map(String::from).unwrap_or(label);
                (kind, value)
            }
            "select" => (ControlKind::Select, String::new()),
            "textarea" => (ControlKind::TextArea, self.document.text_content(node)),
            _ => return None,
        };
        let mut options = Vec::new();
        let mut selected = 0;
        if kind == ControlKind::Select {
            for option in self.document.elements("option") {
                if !self.document.is_inside(option, node) {
                    continue;
                }
                let label = self.document.text_content(option);
                let value = self.document.attribute(option, "value").map(String::from).unwrap_or_else(|| label.clone());
                if self.document.attribute(option, "selected").is_some() {
                    selected = options.len();
                }
                options.push((value, label));
            }
        }
        Some(Control {
            x,
            y,
            w,
            h,
            kind,
            form: self.form_of(node),
            name: attribute("name"),
            value,
            checked: self.document.attribute(node, "checked").is_some(),
            options,
            selected,
            placeholder: if kind == ControlKind::File && self.document.attribute(node, "placeholder").is_none() {
                String::from("chemin dans votre dossier, ex. documents/photo.png")
            } else {
                attribute("placeholder")
            },
            node,
            form_action: attribute("formaction"),
            form_method: attribute("formmethod").to_ascii_uppercase(),
            form_enctype: attribute("formenctype"),
        })
    }

    // --------------------------------------------------------
    //  Tables
    // --------------------------------------------------------

    fn rows(&self, table: NodeId) -> Vec<NodeId> {
        let mut rows = Vec::new();
        for child in self.children(table) {
            match self.document.tag(child) {
                Some("tr") => rows.push(child),
                Some("thead" | "tbody" | "tfoot") => {
                    for row in self.children(child) {
                        if self.document.tag(row) == Some("tr") {
                            rows.push(row);
                        }
                    }
                }
                _ => {}
            }
        }
        rows.retain(|row| !self.hidden(*row));
        rows
    }

    fn cells(&self, row: NodeId) -> Vec<(NodeId, usize)> {
        self.children(row)
            .into_iter()
            .filter(|cell| matches!(self.document.tag(*cell), Some("td" | "th")) && !self.hidden(*cell))
            .map(|cell| {
                let span = self
                    .document
                    .attribute(cell, "colspan")
                    .and_then(|value| value.trim().parse::<usize>().ok())
                    .unwrap_or(1)
                    .clamp(1, 50);
                (cell, span)
            })
            .collect()
    }

    fn table(&mut self, node: NodeId, x: i32, y: i32, width: i32) -> i32 {
        let rows = self.rows(node);
        let mut cursor = y;
        // A caption first.
        for child in self.children(node) {
            if self.document.tag(child) == Some("caption") {
                let (height, _) = self.block(child, x, cursor, width, 0);
                cursor += height;
            }
        }
        if rows.is_empty() {
            return cursor - y;
        }
        let grid: Vec<Vec<(NodeId, usize)>> = rows.iter().map(|row| self.cells(*row)).collect();
        let columns = grid
            .iter()
            .map(|cells| cells.iter().map(|(_, span)| span).sum::<usize>())
            .max()
            .unwrap_or(0)
            .clamp(1, 200);
        let spacing = 2;
        let mut min = alloc::vec![0i32; columns];
        let mut max = alloc::vec![0i32; columns];
        for cells in &grid {
            let mut column = 0;
            for (cell, span) in cells {
                if column >= columns {
                    break;
                }
                if *span == 1 {
                    let cell_min = self.minimum(*cell);
                    let cell_max = self.preferred(*cell).max(cell_min);
                    // A width given to a cell is its column's wish.
                    let wished = match self.style(*cell).width {
                        Length::Px(pixels) => Some(pixels),
                        Length::Percent(milli) => Some((width as i64 * milli as i64 / 100_000) as i32),
                        Length::Auto => None,
                    };
                    min[column] = min[column].max(cell_min);
                    max[column] = max[column].max(wished.unwrap_or(cell_max).max(cell_min));
                }
                column += span;
            }
        }
        let available = (width - spacing * (columns as i32 + 1)).max(columns as i32);
        let sum_min: i32 = min.iter().sum();
        let sum_max: i32 = max.iter().sum();
        let mut widths: Vec<i32> = if sum_max <= available {
            // Room for every preferred width; a table with a width spreads
            // the rest.
            let mut widths = max.clone();
            if self.style(node).width != Length::Auto && sum_max > 0 {
                let extra = available - sum_max;
                for (index, slot) in widths.iter_mut().enumerate() {
                    *slot += (extra as i64 * max[index] as i64 / sum_max as i64) as i32;
                }
            }
            widths
        } else if sum_min >= available {
            // Not even the minimum fits: share by minimum.
            min.iter()
                .map(|value| if sum_min > 0 { (available as i64 * *value as i64 / sum_min as i64) as i32 } else { available / columns as i32 })
                .collect()
        } else {
            let slack = (available - sum_min) as i64;
            let range = (sum_max - sum_min).max(1) as i64;
            min.iter()
                .zip(max.iter())
                .map(|(low, high)| low + (slack * (high - low) as i64 / range) as i32)
                .collect()
        };
        for slot in widths.iter_mut() {
            *slot = (*slot).max(1);
        }

        for (row_index, cells) in grid.iter().enumerate() {
            let row = rows[row_index];
            let mut column = 0;
            let mut placed: Vec<(Page, i32, i32, i32, NodeId)> = Vec::new();
            let mut row_height = 0;
            for (cell, span) in cells {
                if column >= columns {
                    break;
                }
                let span = (*span).min(columns - column);
                let cell_x: i32 = widths[..column].iter().sum::<i32>() + spacing * (column as i32 + 1);
                let cell_width: i32 = widths[column..column + span].iter().sum::<i32>() + spacing * (span as i32 - 1);
                let saved = self.begin_fragment();
                let (height, _) = self.block(*cell, 0, 0, cell_width, 0);
                let fragment = self.end_fragment(saved, cell_width, height);
                row_height = row_height.max(height);
                placed.push((fragment, cell_x, cell_width, height, *cell));
                column += span;
            }
            if let Some(background) = self.style(row).background {
                self.page.paints.push(Paint::Rect { x, y: cursor, w: width, h: row_height + spacing, color: background });
            }
            for (fragment, cell_x, _cell_width, height, _cell) in placed {
                // Cells are centred vertically, as HTML tables are by
                // default.
                let offset = (row_height - height) / 2;
                self.page.place(fragment, x + cell_x, cursor + spacing + offset);
            }
            cursor += row_height + spacing;
        }
        cursor - y + spacing
    }

    // --------------------------------------------------------
    //  Flex and grid
    // --------------------------------------------------------

    /// Lays out one flex item or anonymous run at `width`, at the origin.
    fn flex_item(&mut self, container: NodeId, item: &FlexItem, width: i32) -> (Page, i32) {
        let saved = self.begin_fragment();
        let height = match item {
            FlexItem::Node(child) => {
                let (height, bottom) = self.block(*child, 0, 0, width.max(1), 0);
                height + bottom
            }
            FlexItem::Run(run) => self.inline(container, run, 0, 0, width.max(1), None, None),
        };
        let fragment = self.end_fragment(saved, width.max(1), height);
        (fragment, height)
    }

    /// (preferred, minimum, fixed width, grow) of a flex item.
    fn flex_sizes(&mut self, item: &FlexItem, width: i32) -> (i32, i32, Option<i32>, i32) {
        match item {
            FlexItem::Node(child) => {
                let style = self.style(*child);
                let extra = Self::horizontal_extra(style, width);
                let fixed = style.width.resolve(width).map(|given| (given + extra).min(width));
                let grow = style.flex_grow;
                let preferred = self.preferred(*child).min(width);
                let minimum = self.minimum(*child).min(width);
                (preferred.max(minimum), minimum, fixed, grow)
            }
            FlexItem::Run(run) => {
                let mut preferred = 0;
                let mut minimum = 0;
                for node in run {
                    preferred += self.preferred(*node) + GLYPH_W;
                    minimum = minimum.max(self.minimum(*node));
                }
                (preferred.min(width), minimum.min(width), None, 0)
            }
        }
    }

    /// Flex rows, and grids.
    fn flex(&mut self, node: NodeId, x: i32, y: i32, width: i32) -> i32 {
        let style = self.style(node);
        let justify = style.justify;
        let gap = style.gap;
        let wrap = style.flex_wrap;
        let tracks = style.grid_columns.clone();
        let is_grid = style.grid;
        let areas = style.grid_areas.clone();
        let align_center = style.align_center;

        // Items: element children, and runs of inline content between
        // them as anonymous items.
        let mut items: Vec<FlexItem> = Vec::new();
        let mut run: Vec<NodeId> = Vec::new();
        for child in self.children(node) {
            let inline_text = match &self.document.nodes[child].data {
                NodeData::Text(content) => {
                    if is_blank(content) {
                        continue;
                    }
                    true
                }
                NodeData::Element { .. } => self.style(child).display == Display::Inline,
                NodeData::Document => false,
            };
            if inline_text {
                run.push(child);
                continue;
            }
            if self.hidden(child) {
                continue;
            }
            if self.style(child).out_of_flow {
                self.defer_absolute(child, x, y);
                continue;
            }
            if !run.is_empty() {
                items.push(FlexItem::Run(core::mem::take(&mut run)));
            }
            items.push(FlexItem::Node(child));
        }
        if !run.is_empty() {
            items.push(FlexItem::Run(run));
        }
        if items.is_empty() {
            return 0;
        }
        // `order`, stable among equals.
        items.sort_by_key(|item| match item {
            FlexItem::Node(child) => self.style(*child).order,
            FlexItem::Run(_) => 0,
        });

        if is_grid {
            let tracks: Vec<Track> = if tracks.is_empty() {
                // No columns given: as many as the areas name, else one.
                let count = areas.iter().map(|row| row.len()).max().unwrap_or(1).max(1);
                alloc::vec![Track::Fraction(1000); count]
            } else {
                tracks.to_vec()
            };
            return self.grid(node, &items, &tracks, &areas, x, y, width, gap);
        }

        let sizes: Vec<(i32, i32, Option<i32>, i32)> = items.iter().map(|item| self.flex_sizes(item, width)).collect();
        let gaps = gap * (items.len() as i32 - 1);
        let wanted: i32 = sizes.iter().map(|(preferred, _, fixed, _)| fixed.unwrap_or(*preferred)).sum::<i32>() + gaps;
        let least: i32 = sizes.iter().map(|(_, minimum, fixed, _)| fixed.unwrap_or(*minimum)).sum::<i32>() + gaps;

        // One line when it may be one: shrink towards the minimums, grow
        // into free space. Wrapped otherwise.
        // `nowrap` is one line even when the minimums do not fit: the
        // items keep them and overflow -- usually into a clipped
        // scroller, as navigation bars are built.
        let mut lines: Vec<Vec<(usize, i32)>> = Vec::new();
        if !wrap && least > width {
            let widths = sizes.iter().map(|(_, minimum, fixed, _)| fixed.unwrap_or(*minimum));
            lines.push(widths.enumerate().collect());
        } else if !wrap {
            let mut widths: Vec<i32> = sizes.iter().map(|(preferred, _, fixed, _)| fixed.unwrap_or(*preferred)).collect();
            if wanted > width {
                let slack = (width - least) as i64;
                let range: i64 = sizes
                    .iter()
                    .map(|(preferred, minimum, fixed, _)| if fixed.is_some() { 0 } else { (preferred - minimum) as i64 })
                    .sum::<i64>()
                    .max(1);
                for (index, (preferred, minimum, fixed, _)) in sizes.iter().enumerate() {
                    if fixed.is_none() {
                        widths[index] = minimum + (slack * (preferred - minimum) as i64 / range) as i32;
                    }
                }
            } else {
                let free = (width - wanted) as i64;
                let total_grow: i64 = sizes.iter().map(|(_, _, _, grow)| *grow as i64).sum();
                if total_grow > 0 {
                    for (index, (_, _, _, grow)) in sizes.iter().enumerate() {
                        widths[index] += (free * *grow as i64 / total_grow) as i32;
                    }
                }
            }
            lines.push(widths.into_iter().enumerate().collect());
        } else {
            let mut line: Vec<(usize, i32)> = Vec::new();
            let mut used = 0;
            for (index, (preferred, minimum, fixed, _)) in sizes.iter().enumerate() {
                let item_width = fixed.unwrap_or(*preferred).max(*minimum).min(width);
                let needed = if line.is_empty() { item_width } else { used + gap + item_width };
                if needed > width && !line.is_empty() {
                    lines.push(core::mem::take(&mut line));
                    used = 0;
                }
                used = if line.is_empty() { item_width } else { used + gap + item_width };
                line.push((index, item_width));
            }
            if !line.is_empty() {
                lines.push(line);
            }
        }

        let mut cursor = y;
        for (line_index, line) in lines.iter().enumerate() {
            let used: i32 = line.iter().map(|(_, w)| *w).sum::<i32>() + gap * (line.len() as i32 - 1);
            let free = (width - used).max(0);
            let mut offset = match justify {
                Align::Center => free / 2,
                Align::Right => free,
                Align::Left => 0,
            };
            let mut laid: Vec<(Page, i32, i32)> = Vec::new();
            let mut line_height = 0;
            for (index, item_width) in line {
                let (fragment, height) = self.flex_item(node, &items[*index], *item_width);
                line_height = line_height.max(height);
                laid.push((fragment, *item_width, height));
            }
            for (fragment, item_width, height) in laid {
                let dy = if align_center { (line_height - height) / 2 } else { 0 };
                self.page.place(fragment, x + offset, cursor + dy);
                offset += item_width + gap;
            }
            cursor += line_height;
            if line_index + 1 < lines.len() {
                cursor += gap.max(4);
            }
        }
        cursor - y
    }

    /// A grid: named areas first, explicit columns next, the rest
    /// placed row by row in the first free cells.
    #[allow(clippy::too_many_arguments)]
    fn grid(
        &mut self,
        node: NodeId,
        items: &[FlexItem],
        tracks: &[Track],
        areas: &[Vec<String>],
        x: i32,
        y: i32,
        width: i32,
        gap: i32,
    ) -> i32 {
        let tracks: Vec<Track> = match tracks.first() {
            Some(Track::AutoFill(minimum)) => {
                let count = ((width + gap) / (minimum + gap)).max(1);
                alloc::vec![Track::Fraction(1000); count as usize]
            }
            _ => tracks.to_vec(),
        };
        let columns = tracks.len().max(areas.iter().map(|row| row.len()).max().unwrap_or(0)).clamp(1, 24);
        let available = (width - gap * (columns as i32 - 1)).max(columns as i32);
        let mut widths = alloc::vec![0i32; columns];
        let mut fractions = 0i64;
        let mut fixed = 0;
        for index in 0..columns {
            match tracks.get(index).copied().unwrap_or(Track::Fraction(1000)) {
                Track::Px(pixels) => {
                    widths[index] = pixels.min(available);
                    fixed += widths[index];
                }
                Track::Percent(milli) => {
                    widths[index] = (available as i64 * milli as i64 / 100_000) as i32;
                    fixed += widths[index];
                }
                Track::Fraction(share) => fractions += share as i64,
                Track::AutoFill(_) => fractions += 1000,
            }
        }
        let rest = (available - fixed).max(0) as i64;
        for index in 0..columns {
            if let Track::Fraction(share) = tracks.get(index).copied().unwrap_or(Track::Fraction(1000))
                && fractions > 0
            {
                widths[index] = (rest * share as i64 / fractions) as i32;
            }
        }

        // Where each item goes: (row, column, row span, column span).
        let area_of = |name: &str| -> Option<(usize, usize, usize, usize)> {
            let mut found: Option<(usize, usize, usize, usize)> = None;
            for (row, names) in areas.iter().enumerate() {
                for (column, cell) in names.iter().enumerate() {
                    if cell == name {
                        found = Some(match found {
                            None => (row, column, row, column),
                            Some((r0, c0, r1, c1)) => (r0.min(row), c0.min(column), r1.max(row), c1.max(column)),
                        });
                    }
                }
            }
            found.map(|(r0, c0, r1, c1)| (r0, c0, r1 - r0 + 1, (c1 - c0 + 1).min(columns - c0.min(columns - 1))))
        };
        let mut occupied: Vec<Vec<bool>> = Vec::new();
        let take = |occupied: &mut Vec<Vec<bool>>, row: usize, column: usize, rows: usize, span: usize| {
            while occupied.len() < row + rows {
                occupied.push(alloc::vec![false; columns]);
            }
            for r in row..row + rows {
                for c in column..(column + span).min(columns) {
                    occupied[r][c] = true;
                }
            }
        };
        let mut placements: Vec<Option<(usize, usize, usize, usize)>> = alloc::vec![None; items.len()];
        for (index, item) in items.iter().enumerate() {
            if let FlexItem::Node(child) = item
                && let Some(name) = &self.style(*child).grid_area
                && let Some(area) = area_of(name)
            {
                take(&mut occupied, area.0, area.1, area.2, area.3);
                placements[index] = Some(area);
            }
        }
        let mut cursor_row = 0;
        let mut cursor_column = 0;
        for (index, item) in items.iter().enumerate() {
            if placements[index].is_some() {
                continue;
            }
            let (start, span) = match item {
                FlexItem::Node(child) => {
                    let style = self.style(*child);
                    let span = match style.grid_span {
                        u8::MAX => columns,
                        span => (span as usize).clamp(1, columns),
                    };
                    let start = match style.grid_column {
                        0 => None,
                        line if line > 0 => Some(((line - 1) as usize).min(columns - 1)),
                        line => Some(((columns as i16 + 1 + line).max(1) - 1) as usize),
                    };
                    let span = if style.grid_span == u8::MAX { columns - start.unwrap_or(0) } else { span };
                    (start, span.min(columns))
                }
                FlexItem::Run(_) => (None, 1),
            };
            // First free cell from the cursor, row by row.
            let (row, column) = loop {
                while occupied.len() <= cursor_row {
                    occupied.push(alloc::vec![false; columns]);
                }
                let column = start.unwrap_or(cursor_column);
                if column + span <= columns && (column..column + span).all(|c| !occupied[cursor_row][c]) {
                    break (cursor_row, column);
                }
                if start.is_some() || cursor_column + 1 + span > columns {
                    cursor_row += 1;
                    cursor_column = 0;
                } else {
                    cursor_column += 1;
                }
                if cursor_row > 10_000 {
                    break (cursor_row, 0);
                }
            };
            take(&mut occupied, row, column, 1, span);
            placements[index] = Some((row, column, 1, span));
            cursor_row = row;
            cursor_column = column + span;
        }

        // Lay every item out at its width, then size the rows.
        let rows = occupied.len().max(1);
        let mut laid: Vec<(Page, usize, usize, usize, i32)> = Vec::new();
        for (index, item) in items.iter().enumerate() {
            let Some((row, column, row_span, span)) = placements[index] else {
                continue;
            };
            let span = span.max(1).min(columns - column.min(columns - 1));
            let item_width = widths[column..column + span].iter().sum::<i32>() + gap * (span as i32 - 1);
            let (fragment, height) = self.flex_item(node, item, item_width);
            laid.push((fragment, row, column, row_span.max(1), height));
        }
        let mut heights = alloc::vec![0i32; rows];
        for (_, row, _, row_span, height) in &laid {
            if *row_span == 1 && *row < rows {
                heights[*row] = heights[*row].max(*height);
            }
        }
        for (_, row, _, row_span, height) in &laid {
            if *row_span > 1 {
                let last = (row + row_span - 1).min(rows - 1);
                let covered: i32 = heights[*row..=last].iter().sum::<i32>() + gap * (last - row) as i32;
                if *height > covered {
                    heights[last] += height - covered;
                }
            }
        }
        let mut tops = alloc::vec![0i32; rows];
        let mut cursor = y;
        for row in 0..rows {
            tops[row] = cursor;
            cursor += heights[row];
            if heights[row] > 0 && row + 1 < rows {
                cursor += gap;
            }
        }
        for (fragment, row, column, _, _) in laid {
            let item_x = x + widths[..column].iter().sum::<i32>() + gap * column as i32;
            self.page.place(fragment, item_x, tops[row.min(rows - 1)]);
        }
        cursor - y
    }
}

/// An item of a flex or grid container.
enum FlexItem {
    Node(NodeId),
    /// Inline content between element items: an anonymous item.
    Run(Vec<NodeId>),
}

enum Placed {
    Text {
        text: String,
        node: NodeId,
        link: Option<usize>,
        width: i32,
        height: i32,
    },
    Box {
        fragment: Page,
        link: Option<usize>,
        width: i32,
        height: i32,
    },
}

/// Lays out a whole document `width` pixels wide.
///
/// `headroom` gives the bytes of stack left, so that a page nested deeper
/// than the stack allows is cut rather than overflowing it.
pub fn layout(document: &Document, styles: &[Style], width: i32, viewport_height: i32, headroom: &dyn Fn() -> usize) -> Laid {
    let mut engine = Engine {
        document,
        styles,
        page: Page::default(),
        forms: Vec::new(),
        hrefs: Vec::new(),
        viewport_height,
        headroom,
        preferred: alloc::vec![None; document.nodes.len()],
        minimum: alloc::vec![None; document.nodes.len()],
        truncated: false,
        floats: Vec::new(),
        containers: alloc::vec![Vec::new()],
    };
    let _ = engine.viewport_height;
    let root = document.elements("html").first().copied().unwrap_or(0);
    let body = document.body();
    let background = styles[body]
        .background
        .or(styles[root].background)
        .unwrap_or(0xffffff);
    let (height, bottom) = if root == 0 {
        (engine.flow(0, 0, 0, width), 0)
    } else {
        engine.block(root, 0, 0, width, 0)
    };
    engine.page.height = height + bottom;
    // Floats hanging below the last block still take room.
    let float_bottom = engine.floats.iter().map(|f| f.y1).max().unwrap_or(0);
    engine.page.height = engine.page.height.max(float_bottom);
    // Positioned boxes whose containing block is the viewport.
    let pending = engine.containers.pop().unwrap_or_default();
    let page_height = engine.page.height.max(viewport_height);
    engine.layout_absolutes(pending, 0, 0, width, page_height);
    if engine.truncated {
        engine.page.paints.push(Paint::Text {
            x: 8,
            y: engine.page.height + 8,
            text: String::from("[page tronquee : imbrication trop profonde]"),
            color: 0x990000,
            bold: false,
            scale: 1,
            underline: false,
            strike: false,
        });
        engine.page.height += 32;
    }
    Laid {
        page: engine.page,
        forms: engine.forms,
        background,
        width,
    }
}
