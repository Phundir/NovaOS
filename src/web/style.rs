//! The cascade: from style sheets to one computed style per element.
//!
//! Order of precedence, lowest first, as CSS 2.1 section 6.4.1 has it:
//! the browser's own sheet (`USER_AGENT`), presentational attributes
//! (`bgcolor`, `align`, `<font color>`...), the page's sheets in order,
//! the `style` attribute, then the `!important` declarations. Inherited
//! properties come from the parent; custom properties (`--x`) inherit and
//! are substituted into `var()` before a value is read.

use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use super::css::{self, Declaration, Index, Stylesheet};
use super::html::{Document, NodeData, NodeId};

/// The browser's style sheet: the defaults every page starts from.
pub const USER_AGENT: &str = "
html, address, blockquote, body, dd, div, dl, dt, fieldset, form, frame, frameset, h1, h2, h3, h4, h5, h6,
noframes, ol, p, ul, center, dir, hr, menu, pre, header, footer, nav, section, article, aside, main,
figure, figcaption, details, summary, hgroup, legend, search { display: block }
li { display: list-item }
head, script, style, title, meta, link, base, template, noembed, datalist, param, source, track,
area, map, dialog, [hidden], input[type=hidden], iframe, object, embed, svg, canvas, video, audio,
picture > source, frame, frameset, noframes, select option { display: none }
table { display: table; border-spacing: 2px }
tr { display: table-row }
thead, tbody, tfoot { display: table-row-group }
td, th { display: table-cell; padding: 1px 3px }
caption { display: block; text-align: center }
body { margin: 8px; color: black }
p, dl, blockquote, figure, pre, fieldset, form, menu { margin-top: 1em; margin-bottom: 1em }
form { margin: 0 }
ul, ol, menu, dir { margin-top: 1em; margin-bottom: 1em; padding-left: 40px }
ul ul, ol ul, ul ol, ol ol { margin-top: 0; margin-bottom: 0 }
ol { list-style-type: decimal }
blockquote, figure { margin-left: 40px; margin-right: 40px }
dd { margin-left: 40px }
h1 { font-size: 2em; margin: .67em 0; font-weight: bold }
h2 { font-size: 1.5em; margin: .83em 0; font-weight: bold }
h3 { font-size: 1.17em; margin: 1em 0; font-weight: bold }
h4 { margin: 1.33em 0; font-weight: bold }
h5 { font-size: .83em; margin: 1.67em 0; font-weight: bold }
h6 { font-size: .67em; margin: 2.33em 0; font-weight: bold }
b, strong, th, dt, legend, summary { font-weight: bold }
i, em, cite, var, dfn, address { font-style: italic }
u, ins { text-decoration: underline }
s, strike, del { text-decoration: line-through }
pre, xmp, listing, plaintext, textarea { white-space: pre }
code, kbd, samp, tt { font-family: monospace }
pre { background-color: #f4f4f4 }
center { text-align: center }
table { text-align: left }
center > table, table[align=center] { margin-left: auto; margin-right: auto }
th { text-align: center }
a:link { color: #0645ad; text-decoration: underline }
hr { border: 1px solid #aaaaaa; margin: .5em 0 }
mark { background-color: yellow }
img { display: inline-block }
input, button, select, textarea { display: inline-block }
button { padding: 1px 6px }
small, sub, sup { font-size: .83em }
big { font-size: 1.17em }
noscript { display: block }
";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Display {
    None,
    Block,
    Inline,
    InlineBlock,
    ListItem,
    Table,
    TableRow,
    TableRowGroup,
    TableCell,
    /// Children in rows: `flex` and `grid`, laid out alike here.
    Flex,
    /// The element disappears, its children stay.
    Contents,
}

impl Display {
    pub fn is_block_level(self) -> bool {
        matches!(
            self,
            Display::Block
                | Display::ListItem
                | Display::Table
                | Display::TableRow
                | Display::TableRowGroup
                | Display::TableCell
                | Display::Flex
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

/// A column of `grid-template-columns`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Track {
    Px(i32),
    /// Thousandths of a percent of the container.
    Percent(i32),
    /// Share of what is left, in thousandths of `fr`.
    Fraction(i32),
    /// `repeat(auto-fill, minmax(N px, ...))`: as many columns of at
    /// least N pixels as fit.
    AutoFill(i32),
}

/// Parses `grid-template-columns`. Named lines are skipped; `repeat()`
/// is expanded; `minmax(a, b)` is taken as a fraction with its minimum.
pub fn parse_tracks(value: &str) -> Vec<Track> {
    parse_tracks_at(value, 0)
}

/// `repeat()` inside `repeat()` is not CSS, but a sheet may write it:
/// the recursion stops at one level.
fn parse_tracks_at(value: &str, depth: usize) -> Vec<Track> {
    let mut tracks = Vec::new();
    if depth > 1 {
        return tracks;
    }
    let value = value.to_ascii_lowercase();
    for token in css::split_top_level(value.trim(), ' ') {
        let token = token.trim();
        if token.is_empty() || token.starts_with('[') {
            continue;
        }
        if let Some(inner) = token.strip_prefix("repeat(").and_then(|rest| rest.strip_suffix(')')) {
            let Some((count, pattern)) = inner.split_once(',') else {
                continue;
            };
            let count = count.trim();
            if count == "auto-fill" || count == "auto-fit" {
                let minimum = pattern
                    .trim()
                    .strip_prefix("minmax(")
                    .and_then(|rest| rest.split(',').next())
                    .and_then(|min| css::parse_length_px(min, 16, 16, 0, 0))
                    .unwrap_or(200);
                tracks.push(Track::AutoFill(minimum.max(20)));
                return tracks;
            }
            let count: usize = count.parse().unwrap_or(1).clamp(1, 24);
            let inner_tracks = parse_tracks_at(pattern, depth + 1);
            for _ in 0..count {
                tracks.extend(inner_tracks.iter().copied());
            }
            tracks.truncate(64);
            continue;
        }
        let track = if let Some(fraction) = token.strip_suffix("fr") {
            Track::Fraction(css::parse_milli(fraction).unwrap_or(1000) as i32)
        } else if let Some(percent) = token.strip_suffix('%') {
            Track::Percent(css::parse_milli(percent).unwrap_or(0) as i32)
        } else if token.starts_with("minmax(") || token == "auto" || token.starts_with("fit-content") || token == "min-content" || token == "max-content" {
            Track::Fraction(1000)
        } else {
            match css::parse_length_px(token, 16, 16, 0, 0) {
                Some(pixels) => Track::Px(pixels),
                None => Track::Fraction(1000),
            }
        };
        tracks.push(track);
    }
    tracks
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListStyle {
    Disc,
    Decimal,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Float {
    None,
    Left,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Position {
    Static,
    Relative,
    Absolute,
    Fixed,
}

/// A length before layout: a percentage waits for its containing width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Length {
    Px(i32),
    /// Thousandths of a percent.
    Percent(i32),
    Auto,
}

impl Length {
    pub fn resolve(self, base: i32) -> Option<i32> {
        match self {
            Length::Px(pixels) => Some(pixels),
            Length::Percent(milli) => Some((base as i64 * milli as i64 / 100_000) as i32),
            Length::Auto => None,
        }
    }

    pub fn or_zero(self, base: i32) -> i32 {
        self.resolve(base).unwrap_or(0)
    }
}

#[derive(Debug, Clone)]
pub struct Style {
    pub display: Display,
    pub color: u32,
    pub background: Option<u32>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    pub align: Align,
    pub pre: bool,
    pub nowrap: bool,
    pub font_size: i32,
    pub margin: [Length; 4],
    pub padding: [Length; 4],
    pub border: [i32; 4],
    pub border_color: u32,
    pub width: Length,
    pub min_width: Length,
    pub max_width: Length,
    pub height: Length,
    pub list_style: ListStyle,
    pub visible: bool,
    /// Taken out of the flow (`position: absolute/fixed` with an offset
    /// far off screen, clipped to nothing): the "visually hidden" text
    /// written for screen readers.
    pub hidden_offscreen: bool,
    /// `position: absolute` or `fixed`: out of the flow.
    pub out_of_flow: bool,
    pub position: Position,
    /// `top`, `right`, `bottom`, `left`.
    pub offsets: [Length; 4],
    pub float: Float,
    /// `clear`: bit 0 left, bit 1 right.
    pub clear: u8,
    /// `line-height` in pixels; zero for `normal`.
    pub line_height: i32,
    /// `box-sizing: border-box`: `width` includes padding and border.
    pub border_box: bool,
    /// `overflow` other than visible: the content is clipped to the box.
    pub clip: bool,
    /// `border-radius`, in pixels.
    pub radius: i32,
    /// `inline-flex` / `inline-grid`: an inline box laid out as a flex or
    /// grid container.
    pub inline_flex: bool,
    /// `align-items: center` in a flex row.
    pub align_center: bool,
    /// `order` of a flex item.
    pub order: i32,
    pub min_height: Length,
    /// Generated content of `::before` and `::after`.
    pub before: Option<Rc<String>>,
    pub after: Option<Rc<String>>,
    /// `opacity: 0`.
    pub transparent: bool,
    /// Translated by a whole width or height out of view.
    pub slid_out: bool,
    pub flex_column: bool,
    /// `flex-grow`, in thousandths.
    pub flex_grow: i32,
    /// `column-gap` of a flex or grid container.
    pub gap: i32,
    /// `grid-template-columns`, for a grid container.
    pub grid_columns: Rc<Vec<Track>>,
    /// A grid container (a flex one otherwise, when `Display::Flex`).
    pub grid: bool,
    /// `grid-template-areas`, row by row.
    pub grid_areas: Rc<Vec<Vec<String>>>,
    /// The named area an item goes into.
    pub grid_area: Option<Rc<String>>,
    /// First column line of an item (1-based; negative from the end;
    /// zero for automatic placement).
    pub grid_column: i16,
    /// Columns an item spans; `u8::MAX` for the whole row.
    pub grid_span: u8,
    pub flex_wrap: bool,
    pub justify: Align,
    pub uppercase: bool,
    /// Custom properties, shared with the parent until one is set.
    pub variables: Rc<BTreeMap<String, String>>,
}

impl Style {
    pub fn root() -> Style {
        Style {
            display: Display::Block,
            color: 0x000000,
            background: None,
            bold: false,
            italic: false,
            underline: false,
            strike: false,
            align: Align::Left,
            pre: false,
            nowrap: false,
            font_size: 16,
            margin: [Length::Px(0); 4],
            padding: [Length::Px(0); 4],
            border: [0; 4],
            border_color: 0x000000,
            width: Length::Auto,
            min_width: Length::Auto,
            max_width: Length::Auto,
            height: Length::Auto,
            list_style: ListStyle::Disc,
            visible: true,
            hidden_offscreen: false,
            out_of_flow: false,
            position: Position::Static,
            offsets: [Length::Auto; 4],
            float: Float::None,
            clear: 0,
            border_box: false,
            clip: false,
            radius: 0,
            inline_flex: false,
            align_center: false,
            order: 0,
            min_height: Length::Auto,
            before: None,
            after: None,
            line_height: 0,
            transparent: false,
            slid_out: false,
            flex_column: false,
            flex_grow: 0,
            gap: 0,
            grid_columns: Rc::new(Vec::new()),
            grid: false,
            grid_areas: Rc::new(Vec::new()),
            grid_area: None,
            grid_column: 0,
            grid_span: 1,
            flex_wrap: false,
            justify: Align::Left,
            uppercase: false,
            variables: Rc::new(BTreeMap::new()),
        }
    }

    /// What a child starts from: the inherited properties of `self`, the
    /// initial value of the others.
    fn inherit(&self) -> Style {
        Style {
            display: Display::Inline,
            background: None,
            strike: false,
            margin: [Length::Px(0); 4],
            padding: [Length::Px(0); 4],
            border: [0; 4],
            border_color: self.color,
            width: Length::Auto,
            min_width: Length::Auto,
            max_width: Length::Auto,
            height: Length::Auto,
            hidden_offscreen: false,
            out_of_flow: false,
            position: Position::Static,
            offsets: [Length::Auto; 4],
            float: Float::None,
            clear: 0,
            border_box: false,
            clip: false,
            radius: 0,
            inline_flex: false,
            align_center: false,
            order: 0,
            min_height: Length::Auto,
            before: None,
            after: None,
            transparent: false,
            slid_out: false,
            flex_column: false,
            flex_grow: 0,
            gap: 0,
            grid_columns: Rc::new(Vec::new()),
            grid: false,
            grid_areas: Rc::new(Vec::new()),
            grid_area: None,
            grid_column: 0,
            grid_span: 1,
            flex_wrap: false,
            justify: Align::Left,
            ..self.clone()
        }
    }

    /// How many times the 8x16 font is scaled for this text: the only way
    /// to make a heading bigger with one bitmap font.
    pub fn scale(&self) -> i32 {
        if self.font_size >= 22 { 2 } else { 1 }
    }
}

/// What lengths are relative to: the page size and the root font.
#[derive(Debug, Clone, Copy)]
struct Viewport {
    width: i32,
    height: i32,
    rem: i32,
}

/// Substitutes `var(--name, fallback)`, a few levels deep.
fn substitute(value: &str, variables: &BTreeMap<String, String>, depth: usize) -> String {
    if depth > 8 || !value.contains("var(") {
        return value.to_string();
    }
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = rest.find("var(") {
        out.push_str(&rest[..start]);
        let inner_start = start + 4;
        let bytes = rest.as_bytes();
        let mut depth_paren = 1;
        let mut end = inner_start;
        while end < bytes.len() && depth_paren > 0 {
            match bytes[end] {
                b'(' => depth_paren += 1,
                b')' => depth_paren -= 1,
                _ => {}
            }
            end += 1;
        }
        let inner = &rest[inner_start..end.saturating_sub(1).max(inner_start)];
        let (name, fallback) = match inner.split_once(',') {
            Some((name, fallback)) => (name.trim(), Some(fallback.trim())),
            None => (inner.trim(), None),
        };
        let replacement = variables
            .get(name)
            .map(|value| value.as_str())
            .or(fallback)
            .unwrap_or("");
        out.push_str(&substitute(replacement, variables, depth + 1));
        rest = &rest[end.min(rest.len())..];
    }
    out.push_str(rest);
    out
}

fn parse_length(value: &str, style: &Style, viewport: Viewport) -> Option<Length> {
    let value = value.trim().to_ascii_lowercase();
    if value == "auto" || value.is_empty() {
        return Some(Length::Auto);
    }
    if let Some(percent) = value.strip_suffix('%')
        && let Some(milli) = css::parse_milli(percent)
    {
        return Some(Length::Percent(milli as i32));
    }
    css::parse_length_px(&value, style.font_size, viewport.rem, viewport.width, viewport.height).map(Length::Px)
}

/// Expands the 1-to-4 value shorthand of margin and padding.
fn four<T: Copy>(values: &[T]) -> Option<[T; 4]> {
    match values.len() {
        1 => Some([values[0]; 4]),
        2 => Some([values[0], values[1], values[0], values[1]]),
        3 => Some([values[0], values[1], values[2], values[1]]),
        4 => Some([values[0], values[1], values[2], values[3]]),
        _ => None,
    }
}

const SIDES: [&str; 4] = ["top", "right", "bottom", "left"];

fn border_width(token: &str, style: &Style, rem: i32) -> Option<i32> {
    match token {
        "thin" => Some(1),
        "medium" => Some(3),
        "thick" => Some(5),
        "none" | "hidden" => Some(0),
        other => css::parse_length_px(other, style.font_size, rem, 0, 0),
    }
}

/// Applies one declaration.
fn apply(style: &mut Style, parent: &Style, property: &str, raw: &str, viewport: Viewport) {
    let value_owned = substitute(raw, &style.variables, 0);
    let value = value_owned.trim();
    let lower = value.to_ascii_lowercase();
    if lower == "inherit" {
        inherit_one(style, parent, property);
        return;
    }
    let background_under = parent.background.unwrap_or(0xffffff);
    match property {
        "display" => {
            style.display = match lower.split_whitespace().next().unwrap_or("") {
                "none" => Display::None,
                "block" | "flow-root" | "table-caption" | "run-in" => Display::Block,
                "inline" => Display::Inline,
                "inline-block" | "inline-table" => Display::InlineBlock,
                "inline-flex" | "inline-grid" => {
                    style.inline_flex = true;
                    style.grid = lower.starts_with("inline-grid");
                    Display::InlineBlock
                }
                "list-item" => Display::ListItem,
                "table" => Display::Table,
                "table-row" => Display::TableRow,
                "table-row-group" | "table-header-group" | "table-footer-group" => Display::TableRowGroup,
                "table-cell" => Display::TableCell,
                "flex" | "-webkit-box" | "-webkit-flex" | "-ms-flexbox" => {
                    style.grid = false;
                    Display::Flex
                }
                "grid" | "-ms-grid" => {
                    style.grid = true;
                    Display::Flex
                }
                "contents" => Display::Contents,
                _ => style.display,
            };
        }
        "color" => {
            if lower == "currentcolor" {
                style.color = parent.color;
            } else if let Some(color) = css::parse_color(value, background_under) {
                style.color = color;
            }
        }
        "background-color" => {
            style.background = css::parse_color(value, background_under);
        }
        "background-image" => {
            // Images are not drawn; a gradient gives its first colour, which
            // keeps light text on a dark banner readable.
            if let Some(color) = gradient_color(&lower, background_under) {
                style.background = Some(color);
            }
        }
        "background" => {
            // The colour of the shorthand, when it has one; images are not
            // drawn, and `none` clears.
            if lower == "none" || lower == "transparent" {
                style.background = None;
            } else {
                for token in css::split_top_level(value, ' ') {
                    if let Some(color) = css::parse_color(token, background_under) {
                        style.background = Some(color);
                    } else if let Some(color) = gradient_color(token, background_under) {
                        style.background = Some(color);
                    }
                }
            }
        }
        "font-weight" => {
            style.bold = match lower.as_str() {
                "bold" | "bolder" => true,
                "normal" | "lighter" => false,
                number => number.parse::<u32>().map(|weight| weight >= 600).unwrap_or(style.bold),
            };
        }
        "font-style" => style.italic = lower.starts_with("italic") || lower.starts_with("oblique"),
        "font-size" => {
            style.font_size = match lower.as_str() {
                "xx-small" => 9,
                "x-small" => 10,
                "small" => 13,
                "medium" => 16,
                "large" => 18,
                "x-large" => 24,
                "xx-large" => 32,
                "xxx-large" => 48,
                "smaller" => parent.font_size * 5 / 6,
                "larger" => parent.font_size * 6 / 5,
                other => {
                    // `em` and `%` refer to the parent's size here.
                    if let Some(percent) = other.strip_suffix('%') {
                        css::parse_milli(percent)
                            .map(|milli| (parent.font_size as i64 * milli / 100_000) as i32)
                            .unwrap_or(style.font_size)
                    } else {
                        css::parse_length_px(other, parent.font_size, viewport.rem, viewport.width, viewport.height)
                            .unwrap_or(style.font_size)
                    }
                }
            }
            .clamp(6, 96);
        }
        "font" => {
            // The size and the weight of the shorthand.
            for token in lower.split_whitespace() {
                if token == "bold" || token.parse::<u32>().is_ok_and(|weight| weight >= 600) {
                    style.bold = true;
                }
                if token == "italic" {
                    style.italic = true;
                }
                let size = token.split('/').next().unwrap_or("");
                if size.ends_with("px") || size.ends_with("em") || size.ends_with("pt") || size.ends_with('%') {
                    apply(style, parent, "font-size", size, viewport);
                }
            }
        }
        "text-decoration" | "text-decoration-line" => {
            if lower.contains("none") {
                style.underline = false;
                style.strike = false;
            }
            if lower.contains("underline") {
                style.underline = true;
            }
            if lower.contains("line-through") {
                style.strike = true;
            }
        }
        "text-align" => {
            style.align = match lower.as_str() {
                "center" | "-webkit-center" | "-moz-center" => Align::Center,
                "right" | "end" | "-webkit-right" => Align::Right,
                _ => Align::Left,
            };
        }
        "text-transform" => style.uppercase = lower == "uppercase",
        "white-space" => {
            style.pre = lower.starts_with("pre") && lower != "pre-line";
            style.nowrap = lower == "nowrap" || lower == "pre";
        }
        "text-wrap" | "text-wrap-mode" => style.nowrap = lower == "nowrap",
        "visibility" => style.visible = lower != "hidden" && lower != "collapse",
        "opacity" => style.transparent = css::parse_milli(&lower).is_some_and(|milli| milli <= 0),
        "list-style-type" | "list-style" => {
            if lower.contains("none") {
                style.list_style = ListStyle::None;
            } else if lower.contains("decimal") || lower.contains("roman") || lower.contains("alpha") {
                style.list_style = ListStyle::Decimal;
            } else if lower.contains("disc") || lower.contains("circle") || lower.contains("square") {
                style.list_style = ListStyle::Disc;
            }
        }
        "margin" | "padding" => {
            let values: Vec<Length> = lower
                .split_whitespace()
                .filter_map(|token| parse_length(token, style, viewport))
                .collect();
            if let Some(expanded) = four(&values) {
                if property == "margin" {
                    style.margin = expanded;
                } else {
                    style.padding = expanded.map(|length| if length == Length::Auto { Length::Px(0) } else { length });
                }
            }
        }
        "margin-inline" | "padding-inline" | "margin-block" | "padding-block" => {
            let values: Vec<Length> = lower
                .split_whitespace()
                .filter_map(|token| parse_length(token, style, viewport))
                .collect();
            if let Some(first) = values.first().copied() {
                let second = values.get(1).copied().unwrap_or(first);
                let (a, b) = if property.ends_with("inline") { (3, 1) } else { (0, 2) };
                let target = if property.starts_with("margin") { &mut style.margin } else { &mut style.padding };
                target[a] = first;
                target[b] = second;
            }
        }
        "border" | "border-top" | "border-right" | "border-bottom" | "border-left" => {
            let mut width = None;
            let mut color = None;
            let mut none = false;
            for token in css::split_top_level(&lower, ' ') {
                let token = token.trim();
                if token.is_empty() {
                    continue;
                }
                if token == "none" || token == "hidden" {
                    none = true;
                } else if let Some(pixels) = border_width(token, style, viewport.rem) {
                    width = Some(pixels);
                } else if let Some(parsed) = css::parse_color(token, background_under) {
                    color = Some(parsed);
                }
            }
            let pixels = if none { 0 } else { width.unwrap_or(3).clamp(0, 20) };
            let sides: &[usize] = match property {
                "border-top" => &[0],
                "border-right" => &[1],
                "border-bottom" => &[2],
                "border-left" => &[3],
                _ => &[0, 1, 2, 3],
            };
            for side in sides {
                style.border[*side] = pixels;
            }
            if let Some(color) = color {
                style.border_color = color;
            }
        }
        "border-width" => {
            let values: Vec<i32> = lower.split_whitespace().filter_map(|t| border_width(t, style, viewport.rem)).collect();
            if let Some(expanded) = four(&values) {
                style.border = expanded.map(|pixels| pixels.clamp(0, 20));
            }
        }
        "border-style" => {
            if lower == "none" || lower == "hidden" {
                style.border = [0; 4];
            }
        }
        "border-color" => {
            if let Some(color) = lower.split_whitespace().find_map(|t| css::parse_color(t, background_under)) {
                style.border_color = color;
            }
        }
        "width" => style.width = parse_length(&lower, style, viewport).unwrap_or(style.width),
        "min-width" => style.min_width = parse_length(&lower, style, viewport).unwrap_or(style.min_width),
        "max-width" => {
            style.max_width = if lower == "none" {
                Length::Auto
            } else {
                parse_length(&lower, style, viewport).unwrap_or(style.max_width)
            }
        }
        "height" => style.height = parse_length(&lower, style, viewport).unwrap_or(style.height),
        "position" => {
            style.position = match lower.as_str() {
                "relative" | "sticky" | "-webkit-sticky" => Position::Relative,
                "absolute" => Position::Absolute,
                "fixed" => Position::Fixed,
                _ => Position::Static,
            };
            style.out_of_flow = matches!(style.position, Position::Absolute | Position::Fixed);
        }
        "float" => {
            style.float = match lower.as_str() {
                "left" | "inline-start" => Float::Left,
                "right" | "inline-end" => Float::Right,
                _ => Float::None,
            };
        }
        "clear" => {
            style.clear = match lower.as_str() {
                "left" | "inline-start" => 1,
                "right" | "inline-end" => 2,
                "both" => 3,
                _ => 0,
            };
        }
        "right" | "bottom" => {
            let side = if property == "right" { 1 } else { 2 };
            style.offsets[side] = parse_length(&lower, style, viewport).unwrap_or(Length::Auto);
        }
        "inset" => {
            let values: Vec<Length> = lower.split_whitespace().filter_map(|token| parse_length(token, style, viewport)).collect();
            if let Some(expanded) = four(&values) {
                style.offsets = expanded;
            }
        }
        "line-height" => {
            style.line_height = if lower == "normal" {
                0
            } else if let Some((milli, unit)) = css::split_unit(&lower)
                && unit.is_empty()
            {
                (style.font_size as i64 * milli / 1000) as i32
            } else if let Some(percent) = lower.strip_suffix('%') {
                css::parse_milli(percent).map(|milli| (style.font_size as i64 * milli / 100_000) as i32).unwrap_or(0)
            } else {
                css::parse_length_px(&lower, style.font_size, viewport.rem, viewport.width, viewport.height).unwrap_or(0)
            }
            .clamp(0, 200);
        }
        "box-sizing" => style.border_box = lower == "border-box",
        "overflow" | "overflow-y" | "overflow-x" => {
            style.clip = matches!(lower.split_whitespace().next().unwrap_or(""), "hidden" | "clip" | "auto" | "scroll");
        }
        "border-radius" => {
            style.radius = lower
                .split_whitespace()
                .next()
                .and_then(|first| css::parse_length_px(first, style.font_size, viewport.rem, 100, 100))
                .unwrap_or(0)
                .clamp(0, 400);
        }
        "align-items" => style.align_center = lower == "center",
        "order" => style.order = lower.parse::<i32>().unwrap_or(0).clamp(-1000, 1000),
        "min-height" => style.min_height = parse_length(&lower, style, viewport).unwrap_or(style.min_height),
        "flex-direction" => style.flex_column = lower.starts_with("column"),
        "flex-flow" => {
            style.flex_column = lower.contains("column");
            style.flex_wrap = lower.contains("wrap") && !lower.contains("nowrap");
        }
        "flex-wrap" => style.flex_wrap = lower.starts_with("wrap"),
        "flex-grow" => style.flex_grow = css::parse_milli(&lower).unwrap_or(0).clamp(0, 100_000) as i32,
        "flex" => {
            style.flex_grow = match lower.as_str() {
                "none" | "initial" | "0" => 0,
                "auto" => 1000,
                other => other
                    .split_whitespace()
                    .next()
                    .and_then(css::parse_milli)
                    .unwrap_or(0)
                    .clamp(0, 100_000) as i32,
            };
        }
        "gap" | "column-gap" | "grid-gap" | "grid-column-gap" => {
            let first = if property == "gap" || property == "grid-gap" {
                lower.split_whitespace().last().unwrap_or("0")
            } else {
                lower.as_str()
            };
            style.gap = css::parse_length_px(first, style.font_size, viewport.rem, viewport.width, viewport.height)
                .unwrap_or(0)
                .clamp(0, 200);
        }
        "grid-template-columns" => style.grid_columns = Rc::new(parse_tracks(&lower)),
        "grid-template" => {
            if let Some((_, columns)) = lower.split_once('/') {
                style.grid_columns = Rc::new(parse_tracks(columns));
            }
        }
        "grid-template-areas" => {
            let rows: Vec<Vec<String>> = value
                .split(['"', '\''])
                .enumerate()
                .filter(|(index, _)| index % 2 == 1)
                .map(|(_, row)| row.split_whitespace().map(String::from).collect::<Vec<String>>())
                .filter(|row| !row.is_empty())
                .collect();
            style.grid_areas = Rc::new(rows);
        }
        "grid-area" => {
            let first = value.split('/').next().unwrap_or("").trim();
            if !first.is_empty() && first.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && first != "auto" {
                style.grid_area = Some(Rc::new(String::from(first)));
            }
        }
        "grid-column" | "grid-column-start" => {
            let (start, end) = match lower.split_once('/') {
                Some((start, end)) => (start.trim(), end.trim()),
                None => (lower.trim(), ""),
            };
            if let Some(span) = start.strip_prefix("span") {
                style.grid_span = span.trim().parse::<u8>().unwrap_or(1).max(1);
            } else if let Ok(line) = start.parse::<i16>() {
                style.grid_column = line;
                if end == "-1" {
                    style.grid_span = u8::MAX;
                } else if let Some(span) = end.strip_prefix("span") {
                    style.grid_span = span.trim().parse::<u8>().unwrap_or(1).max(1);
                } else if let Ok(end_line) = end.parse::<i16>() {
                    style.grid_span = (end_line - line).clamp(1, 100) as u8;
                }
            }
        }
        "justify-content" => {
            style.justify = match lower.as_str() {
                "center" => Align::Center,
                "flex-end" | "end" | "right" => Align::Right,
                _ => Align::Left,
            };
        }
        "clip" | "clip-path" => {
            // `rect(0 0 0 0)`, `rect(1px, 1px, 1px, 1px)`, `inset(50%)`:
            // clipped to nothing.
            if lower.starts_with("rect(") || lower.starts_with("inset(50%") || lower.starts_with("inset(100%") {
                style.hidden_offscreen = true;
            }
        }
        "transform" => {
            // A drawer slid out of view until a script slides it in.
            let offscreen = ["translatex(-100%)", "translate(-100%", "translatey(-100%)", "translatex(100%)", "translate3d(-100%", "translate3d(100%"];
            if offscreen.iter().any(|pattern| lower.replace(' ', "").contains(pattern)) {
                style.slid_out = true;
            }
        }
        "left" | "top" | "text-indent" => {
            if css::parse_length_px(&lower, style.font_size, viewport.rem, viewport.width, viewport.height).is_some_and(|pixels| pixels <= -999) {
                style.hidden_offscreen = true;
            }
            if property != "text-indent" {
                let side = if property == "top" { 0 } else { 3 };
                style.offsets[side] = parse_length(&lower, style, viewport).unwrap_or(Length::Auto);
            }
        }
        _ => {
            if property.starts_with("--") {
                Rc::make_mut(&mut style.variables).insert(property.to_string(), value.to_string());
            } else if let Some(side) = property
                .strip_prefix("margin-")
                .or_else(|| property.strip_prefix("padding-"))
                .and_then(|side| SIDES.iter().position(|name| *name == side))
            {
                if let Some(length) = parse_length(&lower, style, viewport) {
                    if property.starts_with("margin") {
                        style.margin[side] = length;
                    } else {
                        style.padding[side] = if length == Length::Auto { Length::Px(0) } else { length };
                    }
                }
            } else if let Some(rest) = property.strip_prefix("border-") {
                let (side, what) = rest.split_once('-').unwrap_or((rest, ""));
                if let Some(side) = SIDES.iter().position(|name| *name == side) {
                    match what {
                        "width" => {
                            if let Some(pixels) = border_width(&lower, style, viewport.rem) {
                                style.border[side] = pixels.clamp(0, 20);
                            }
                        }
                        "style" if lower == "none" => style.border[side] = 0,
                        "color" => {
                            if let Some(color) = css::parse_color(&lower, background_under) {
                                style.border_color = color;
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }
}

/// The first colour of a `linear-gradient(...)` or `radial-gradient(...)`.
fn gradient_color(value: &str, background: u32) -> Option<u32> {
    let lower = value.to_ascii_lowercase();
    let start = lower.find("gradient(")? + "gradient(".len();
    let inner = &value[start..];
    css::split_top_level(inner.trim_end_matches(')'), ',')
        .iter()
        .find_map(|part| {
            let color = part.trim().split_whitespace().next().unwrap_or("");
            css::parse_color(color, background).or_else(|| {
                // `rgb(...)` with spaces inside is one colour.
                css::parse_color(part.trim().rsplit_once(')').map(|(head, _)| head).map(|head| alloc::format!("{})", head)).as_deref().unwrap_or(""), background)
            })
        })
}

/// The text of a `content` value: strings, `attr()`, quotes. Characters
/// of the private use areas -- the glyphs of icon fonts -- are dropped:
/// the system font cannot draw them, and a row of `?` is worse than
/// nothing. `None` when nothing is left.
fn content_text(value: &str, document: &Document, node: NodeId) -> Option<String> {
    let value = value.trim();
    if value == "none" || value == "normal" || value.is_empty() {
        return None;
    }
    let mut out = String::new();
    let mut rest = value;
    while !rest.is_empty() {
        rest = rest.trim_start();
        if let Some(quote) = rest.chars().next().filter(|c| *c == '"' || *c == '\'') {
            let body = &rest[1..];
            let mut end = body.len();
            let mut escaped = false;
            let mut text = String::new();
            let mut chars = body.char_indices().peekable();
            while let Some((index, character)) = chars.next() {
                if escaped {
                    // `\201C`: a code point in hex, then an optional space.
                    let mut hex = String::from(character);
                    while hex.len() < 6 && chars.peek().is_some_and(|(_, c)| c.is_ascii_hexdigit()) {
                        hex.push(chars.next().map(|(_, c)| c).unwrap_or('0'));
                    }
                    if character.is_ascii_hexdigit() {
                        if let Some(decoded) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                            text.push(decoded);
                        }
                        if chars.peek().is_some_and(|(_, c)| *c == ' ') {
                            chars.next();
                        }
                    } else {
                        text.push(character);
                    }
                    escaped = false;
                    continue;
                }
                if character == '\\' {
                    escaped = true;
                } else if character == quote {
                    end = index;
                    break;
                } else {
                    text.push(character);
                }
            }
            out.push_str(&text);
            rest = body.get(end + 1..).unwrap_or("");
        } else if let Some(after) = rest.strip_prefix("attr(") {
            let (name, tail) = after.split_once(')').unwrap_or((after, ""));
            out.push_str(document.attribute(node, name.trim()).unwrap_or(""));
            rest = tail;
        } else if let Some(tail) = rest.strip_prefix("open-quote").or_else(|| rest.strip_prefix("close-quote")) {
            out.push('"');
            rest = tail;
        } else {
            // counter(), url(), keywords: skipped up to the next space.
            let end = rest.find(' ').unwrap_or(rest.len());
            let skipped = &rest[..end];
            let end = if skipped.contains('(') { rest.find(')').map(|i| i + 1).unwrap_or(rest.len()) } else { end };
            rest = &rest[end..];
        }
    }
    let visible: String = out
        .chars()
        .filter(|c| !matches!(*c as u32, 0xe000..=0xf8ff | 0xf0000..=0x10ffff))
        .collect();
    (!visible.trim().is_empty()).then_some(visible)
}

fn inherit_one(style: &mut Style, parent: &Style, property: &str) {
    match property {
        "color" => style.color = parent.color,
        "background-color" | "background" => style.background = parent.background,
        "font-weight" => style.bold = parent.bold,
        "font-size" => style.font_size = parent.font_size,
        "text-align" => style.align = parent.align,
        "display" => style.display = parent.display,
        _ => {}
    }
}

/// Presentational attributes, as author rules of the lowest precedence.
fn attribute_declarations(document: &Document, node: NodeId, tag: &str) -> Vec<Declaration> {
    let mut out = Vec::new();
    let mut push = |property: &str, value: String| {
        out.push(Declaration {
            property: property.to_string(),
            value,
            important: false,
        })
    };
    let attribute = |name: &str| document.attribute(node, name);
    if let Some(color) = attribute("bgcolor") {
        push("background-color", color.to_string());
    }
    if tag == "font"
        && let Some(color) = attribute("color")
    {
        push("color", color.to_string());
    }
    if tag == "body"
        && let Some(color) = attribute("text")
    {
        push("color", color.to_string());
    }
    if let Some(align) = attribute("align") {
        match tag {
            "table" if align.eq_ignore_ascii_case("center") => {
                push("margin-left", String::from("auto"));
                push("margin-right", String::from("auto"));
            }
            "img" => {}
            _ => push("text-align", align.to_string()),
        }
    }
    if matches!(tag, "table" | "td" | "th" | "img" | "col" | "hr" | "iframe" | "input")
        && let Some(width) = attribute("width")
    {
        let width = width.trim();
        let value = if width.ends_with('%') { width.to_string() } else { alloc::format!("{}px", width) };
        push("width", value);
    }
    if matches!(tag, "img" | "td" | "tr" | "table")
        && let Some(height) = attribute("height")
    {
        push("height", alloc::format!("{}px", height.trim().trim_end_matches("px")));
    }
    if tag == "table"
        && let Some(border) = attribute("border")
        && border.trim() != "0"
    {
        push("border", alloc::format!("{}px solid gray", border.trim()));
    }
    if matches!(tag, "td" | "th")
        && let Some(table) = document.nodes[node]
            .parent
            .and_then(|row| document.nodes[row].parent)
            .and_then(|section| {
                if document.tag(section) == Some("table") {
                    Some(section)
                } else {
                    document.nodes[section].parent
                }
            })
    {
        if let Some(padding) = document.attribute(table, "cellpadding") {
            push("padding", alloc::format!("{}px", padding.trim()));
        }
        if document.attribute(table, "border").is_some_and(|border| border.trim() != "0") {
            push("border", String::from("1px solid gray"));
        }
    }
    if attribute("nowrap").is_some() {
        push("white-space", String::from("nowrap"));
    }
    if attribute("hidden").is_some() {
        push("display", String::from("none"));
    }
    out
}

/// Computes the style of every node. Text nodes get their parent's.
pub fn compute(document: &Document, author: &[Stylesheet], size: (i32, i32)) -> Vec<Style> {
    let user_agent = css::parse(USER_AGENT, size.0);
    let mut viewport = Viewport {
        width: size.0,
        height: size.1,
        rem: 16,
    };
    let mut sheets: Vec<&Stylesheet> = alloc::vec![&user_agent];
    sheets.extend(author.iter());
    let index = Index::build(&sheets);

    let root = Style::root();
    let mut styles: Vec<Style> = alloc::vec![root.clone(); document.nodes.len()];
    // Document order: a parent always precedes its children, since the
    // parser appends nodes as it opens them.
    for node in 0..document.nodes.len() {
        let parent_style = match document.nodes[node].parent {
            Some(parent) => styles[parent].clone(),
            None => root.clone(),
        };
        let NodeData::Element { tag, .. } = &document.nodes[node].data else {
            let mut style = parent_style.inherit();
            style.display = Display::Inline;
            styles[node] = style;
            continue;
        };
        let mut style = parent_style.inherit();

        // User agent, then attributes, then the page, then `style=`; and
        // the important declarations again on top, in the same order.
        let mut matched = index.matching(document, &sheets, node);
        matched.sort_by(|a, b| {
            let a_ua = a.2 == 0;
            let b_ua = b.2 == 0;
            b_ua.cmp(&a_ua).then(a.0.cmp(&b.0)).then(a.1.cmp(&b.1))
        });
        let presentational = attribute_declarations(document, node, tag);
        let inline = document.attribute(node, "style").map(css::parse_declarations).unwrap_or_default();

        // Custom properties first, so `var()` in any declaration sees them.
        let mut passes: Vec<&Declaration> = Vec::new();
        for (_, _, sheet, rule) in matched.iter().filter(|m| m.2 == 0) {
            passes.extend(sheets[*sheet].rules[*rule].declarations.iter().filter(|d| !d.important));
        }
        passes.extend(presentational.iter());
        for (_, _, sheet, rule) in matched.iter().filter(|m| m.2 != 0) {
            passes.extend(sheets[*sheet].rules[*rule].declarations.iter().filter(|d| !d.important));
        }
        passes.extend(inline.iter().filter(|d| !d.important));
        for (_, _, sheet, rule) in matched.iter() {
            passes.extend(sheets[*sheet].rules[*rule].declarations.iter().filter(|d| d.important));
        }
        passes.extend(inline.iter().filter(|d| d.important));

        for declaration in passes.iter().filter(|d| d.property.starts_with("--")) {
            apply(&mut style, &parent_style, &declaration.property, &declaration.value, viewport);
        }
        for declaration in passes.iter().filter(|d| !d.property.starts_with("--")) {
            apply(&mut style, &parent_style, &declaration.property, &declaration.value, viewport);
        }
        // The "visually hidden" pattern: a box of one pixel or less, for
        // screen readers only. Drawn in the flow it would stack its text
        // one letter per line.
        let tiny = |length: Length| matches!(length, Length::Px(pixels) if pixels <= 1);
        if tiny(style.width) && tiny(style.height) {
            style.hidden_offscreen = true;
        }
        // Generated content: only the `content` of `::before` and
        // `::after` is kept -- drawn in the element's own style.
        for which in [1u8, 2] {
            let mut pseudo = index.matching_pseudo(document, &sheets, node, which);
            if pseudo.is_empty() {
                continue;
            }
            pseudo.sort_by(|a, b| (a.2 != 0).cmp(&(b.2 != 0)).then(a.0.cmp(&b.0)).then(a.1.cmp(&b.1)));
            let mut content: Option<String> = None;
            let mut display_none = false;
            for (_, _, sheet, rule) in &pseudo {
                for declaration in &sheets[*sheet].rules[*rule].declarations {
                    match declaration.property.as_str() {
                        "content" => content = Some(substitute(&declaration.value, &style.variables, 0)),
                        "display" => display_none = declaration.value.trim() == "none",
                        _ => {}
                    }
                }
            }
            let text = if display_none {
                None
            } else {
                content.and_then(|value| content_text(&value, document, node)).map(Rc::new)
            };
            if which == 1 {
                style.before = text;
            } else {
                style.after = text;
            }
        }
        // `rem` is the root element's size: `html { font-size: 62.5% }`
        // makes it 10 px, and a page sized in `rem` depends on it.
        if tag == "html" {
            viewport.rem = style.font_size;
        }
        styles[node] = style;
    }
    styles
}
