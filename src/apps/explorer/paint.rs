//! Drawing of the explorer window.
//!
//! Every block below states how many commands it emits, because the sum is
//! bounded by `surface::MAX_COMMANDS` and `layout::FIXED_COMMANDS` is only
//! true as long as these counts are. A block that grows must grow the
//! constant with it -- the self-test checks the constant, not this file.
//!
//! Hover and state are drawn differently, as entry 43 of DECISIONS.md asks:
//! the hover says where the mouse is (towards white), the selection says
//! what the keyboard acts on (the accent bar on the left of the row), the
//! mark says what the actions act on (the box).

use alloc::format;
use alloc::string::String;

use crate::graphics::framebuffer::mix;
use crate::gui::client::{Client, ellipsize};

use super::layout::{ACTION_LABELS, Action, Hit, Layout};
use super::ops::{SortKey, format_size, kind_label};
use super::{Explorer, Prompt};

const WHITE: u32 = 0xFF_FF_FF;

/// 16x16 folder, one `u16` per row, leftmost pixel in the high bit.
const FOLDER: [u16; 16] = [
    0x0000, 0x0000, 0x7C00, 0x8200, 0xFFFE, 0x8002, 0x8002, 0x8002, 0x8002, 0x8002, 0x8002,
    0x8002, 0x8002, 0xFFFE, 0x0000, 0x0000,
];
/// 16x16 page with a folded corner.
const FILE: [u16; 16] = [
    0x0000, 0x1FC0, 0x1060, 0x1050, 0x1078, 0x1008, 0x13C8, 0x1008, 0x13C8, 0x1008, 0x13C8,
    0x1008, 0x1008, 0x1FF8, 0x0000, 0x0000,
];

/// The bytes `OP_DRAW_ICON` carries: rows in big-endian order.
const fn icon_bits(rows: [u16; 16]) -> [u8; 32] {
    let mut bits = [0u8; 32];
    let mut row = 0;
    while row < 16 {
        bits[row * 2] = (rows[row] >> 8) as u8;
        bits[row * 2 + 1] = rows[row] as u8;
        row += 1;
    }
    bits
}

const FOLDER_BITS: [u8; 32] = icon_bits(FOLDER);
const FILE_BITS: [u8; 32] = icon_bits(FILE);

pub fn paint(client: &mut Client, ex: &Explorer) {
    let layout = ex.layout();
    let theme = &ex.theme;
    let surface = ex.surface;

    client.reset(surface);
    // 1
    client.fill(surface, theme.background);

    paint_nav(client, ex, &layout);
    paint_actions(client, ex, &layout);
    paint_sidebar(client, ex, &layout);
    paint_header(client, ex, &layout);

    if let Some(lines) = &ex.info {
        paint_info(client, ex, &layout, lines);
    } else {
        paint_rows(client, ex, &layout);
    }

    paint_status(client, ex, &layout);
    client.commit(surface);
}

/// A button: background, frame, centred label. 3 commands.
#[allow(clippy::too_many_arguments)]
fn button(
    client: &mut Client,
    ex: &Explorer,
    layout: &Layout,
    rect: crate::gui::surface::Rect,
    label: &str,
    enabled: bool,
    hovered: bool,
    base: u32,
) {
    let theme = &ex.theme;
    // A disabled button gets no hover shade: a highlight on something that
    // will not answer is the lie of entry 49.
    let fill = if enabled && hovered { mix(base, WHITE, 45) } else { base };
    client.rect(ex.surface, rect.x, rect.y, rect.width, rect.height, fill);
    client.frame(
        ex.surface,
        rect.x,
        rect.y,
        rect.width,
        rect.height,
        if enabled && hovered { theme.text_dim } else { theme.border },
        1,
    );
    let text_w = layout.glyph_w * label.len() as i32;
    client.text(
        ex.surface,
        rect.x + (rect.width - text_w) / 2,
        layout.text_y(rect.y, rect.height),
        label,
        if enabled { theme.text } else { theme.text_dim },
        None,
    );
}

/// 14 commands.
fn paint_nav(client: &mut Client, ex: &Explorer, layout: &Layout) {
    let theme = &ex.theme;
    let surface = ex.surface;
    let nav = layout.nav;
    client.rect(surface, nav.x, nav.y, nav.width, nav.height, theme.surface);

    let buttons = [
        (layout.back(), "<", !ex.back.is_empty(), Hit::Back),
        (layout.forward(), ">", !ex.forward.is_empty(), Hit::Forward),
        (layout.up(), "^", super::ops::parent(&ex.cwd).is_some(), Hit::Parent),
    ];
    for (rect, label, enabled, hit) in buttons {
        button(client, ex, layout, rect, label, enabled, ex.hover == hit, theme.surface_alt);
    }

    let path = layout.path_box();
    let editing = ex.prompt == Prompt::Path;
    client.rect(
        surface,
        path.x,
        path.y,
        path.width,
        path.height,
        if ex.hover == Hit::PathBox && !editing {
            mix(theme.background, theme.surface, 110)
        } else {
            theme.background
        },
    );
    client.frame(
        surface,
        path.x,
        path.y,
        path.width,
        path.height,
        if editing { theme.accent } else { theme.border },
        if editing { 2 } else { 1 },
    );
    // A long path keeps its **end**: the directory one is in matters more
    // than the disk it is on.
    let shown = if editing {
        format!("{}_", ex.entry)
    } else {
        ex.cwd.clone()
    };
    // Counted in characters, not bytes: slicing a `str` inside a
    // multi-byte character panics, and a name read from a foreign disk
    // is not bound by the ASCII rule of the names created here.
    let chars = layout.path_chars();
    let length = shown.chars().count();
    let shown = if length > chars {
        let keep = chars.saturating_sub(3);
        let tail: String = shown.chars().skip(length - keep).collect();
        format!("...{}", tail)
    } else {
        shown
    };
    client.text(
        surface,
        path.x + layout.padding,
        layout.text_y(path.y, path.height),
        &shown,
        theme.text,
        None,
    );

    client.rect(surface, 0, nav.bottom() - 1, nav.width, 1, theme.border);
}

/// 26 commands.
fn paint_actions(client: &mut Client, ex: &Explorer, layout: &Layout) {
    let theme = &ex.theme;
    let bar = layout.actions;
    client.rect(ex.surface, bar.x, bar.y, bar.width, bar.height, theme.surface);

    for (index, action) in Action::ALL.iter().enumerate() {
        let enabled = ex.can(*action);
        let base = if *action == Action::Delete && enabled {
            mix(theme.surface_alt, theme.danger, 60)
        } else {
            theme.surface_alt
        };
        button(
            client,
            ex,
            layout,
            layout.action(index),
            ACTION_LABELS[index],
            enabled,
            ex.hover == Hit::Action(*action),
            base,
        );
    }

    client.rect(ex.surface, 0, bar.bottom() - 1, bar.width, 1, theme.border);
}

/// 3 + 2 per place.
fn paint_sidebar(client: &mut Client, ex: &Explorer, layout: &Layout) {
    let theme = &ex.theme;
    let surface = ex.surface;
    let side = layout.sidebar;
    client.rect(surface, side.x, side.y, side.width, side.height, theme.surface);
    client.rect(surface, side.right() - 1, side.y, 1, side.height, theme.border);
    client.text(
        surface,
        side.x + layout.padding,
        side.y + layout.padding / 2 + 2,
        "Emplacements",
        theme.accent,
        None,
    );

    let cells = ((side.width - 3 * layout.padding) / layout.glyph_w).max(1) as usize;
    for (index, (label, path)) in ex.places.iter().enumerate() {
        let rect = layout.place(index);
        if rect.bottom() > side.bottom() {
            break;
        }
        let current = *path == ex.cwd;
        let hovered = ex.hover == Hit::Place(index);
        if current || hovered {
            let fill = match (current, hovered) {
                (true, true) => mix(theme.surface_alt, theme.accent, 56),
                (true, false) => theme.surface_alt,
                _ => mix(theme.surface, theme.surface_alt, 85),
            };
            client.rect(surface, rect.x, rect.y, rect.width, rect.height, fill);
        }
        client.text(
            surface,
            rect.x + 2 * layout.padding,
            layout.text_y(rect.y, rect.height),
            &ellipsize(label, cells),
            if current || hovered { theme.text } else { theme.text_dim },
            None,
        );
    }
}

/// 5 commands.
fn paint_header(client: &mut Client, ex: &Explorer, layout: &Layout) {
    let theme = &ex.theme;
    let surface = ex.surface;
    let header = layout.header;
    client.rect(surface, header.x, header.y, header.width, header.height, theme.surface);

    let arrow = if ex.ascending { " +" } else { " -" };
    for (key, label, x) in [
        (SortKey::Name, "Nom", layout.name_x()),
        (SortKey::Kind, "Type", layout.type_x()),
        (
            SortKey::Size,
            "Taille",
            layout.size_right() - 8 * layout.glyph_w,
        ),
    ] {
        let text = if ex.sort == key {
            format!("{}{}", label, arrow)
        } else {
            String::from(label)
        };
        client.text(
            surface,
            x,
            layout.text_y(header.y, header.height),
            &text,
            if ex.sort == key || ex.hover == Hit::Header(key) {
                theme.text
            } else {
                theme.text_dim
            },
            None,
        );
    }
    client.rect(surface, header.x, header.bottom() - 1, header.width, 1, theme.border);
}

/// `ROW_COMMANDS` per row, plus the scroll bar (2) and a notice (1).
fn paint_rows(client: &mut Client, ex: &Explorer, layout: &Layout) {
    let theme = &ex.theme;
    let surface = ex.surface;
    let rows = layout.visible_rows();

    if let Some(error) = &ex.listing_error {
        client.text(
            surface,
            layout.name_x(),
            layout.list.y + layout.padding,
            &ellipsize(&format!("lecture impossible : {}", error), layout.name_chars() + 20),
            theme.danger,
            None,
        );
        return;
    }
    if ex.items.is_empty() {
        client.text(
            surface,
            layout.name_x(),
            layout.list.y + layout.padding,
            "(dossier vide)",
            theme.text_dim,
            None,
        );
    }

    let name_chars = layout.name_chars();
    for rank in 0..rows {
        let index = ex.top + rank;
        let Some(item) = ex.items.get(index) else {
            break;
        };
        let row = layout.row(rank);
        let selected = ex.selected == Some(index);
        let hovered = matches!(ex.hover, Hit::Row(i) | Hit::Mark(i) if i == index);
        let marked = ex.marked.get(index).copied().unwrap_or(false);

        // 1: background, or the selection bar alone.
        if selected || hovered {
            let fill = match (selected, hovered) {
                (true, true) => mix(theme.surface_alt, theme.accent, 56),
                (true, false) => theme.surface_alt,
                _ => mix(theme.background, theme.surface_alt, 110),
            };
            client.rect(surface, row.x, row.y, row.width, row.height, fill);
        }

        // 2-3: the mark box, and its fill when marked.
        let mark = layout.mark_box(rank);
        client.frame(
            surface,
            mark.x,
            mark.y,
            mark.width,
            mark.height,
            if marked { theme.accent } else { theme.border },
            1,
        );
        if marked {
            client.rect(surface, mark.x + 3, mark.y + 3, mark.width - 6, mark.height - 6, theme.accent);
        } else if selected {
            // The selection bar takes the mark's second command when the
            // row is not marked: three pixels of accent on the left edge.
            client.rect(surface, row.x, row.y, 3, row.height, theme.accent);
        }

        // 4: icon.
        client.icon(
            surface,
            layout.icon_x(),
            row.y + (layout.row_h - 16) / 2,
            if item.is_dir { &FOLDER_BITS } else { &FILE_BITS },
            if item.is_dir { theme.accent } else { theme.text_dim },
            None,
        );

        // 5-7: name, type, size.
        let text_y = layout.text_y(row.y, row.height);
        let cut = ex.clipboard.as_ref().is_some_and(|clip| {
            clip.cut && clip.paths.iter().any(|path| *path == super::ops::join(&ex.cwd, &item.name))
        });
        client.text(
            surface,
            layout.name_x(),
            text_y,
            &ellipsize(&item.name, name_chars),
            // An entry waiting to be moved is dimmed, as every explorer
            // does: it is still here, and it will not be for long.
            if cut || !item.known { theme.text_dim } else { theme.text },
            None,
        );
        client.text(
            surface,
            layout.type_x(),
            text_y,
            if item.known { kind_label(&item.name, item.is_dir) } else { "inaccessible" },
            theme.text_dim,
            None,
        );
        if !item.is_dir && item.known {
            let size = format_size(item.size);
            client.text(
                surface,
                layout.size_right() - layout.glyph_w * size.len() as i32,
                text_y,
                &size,
                theme.text_dim,
                None,
            );
        }
    }

    // Scroll bar: the track, then the thumb.
    if let Some(thumb) = layout.thumb(ex.items.len(), ex.top) {
        let track = layout.scrollbar;
        client.rect(surface, track.x, track.y, track.width, track.height, theme.surface);
        client.rect(
            surface,
            thumb.x,
            thumb.y,
            thumb.width,
            thumb.height,
            if matches!(ex.hover, Hit::PageUp | Hit::PageDown) {
                theme.text_dim
            } else {
                theme.border
            },
        );
    }
}

/// The properties panel, over the list. 2 + one per line.
fn paint_info(client: &mut Client, ex: &Explorer, layout: &Layout, lines: &[String]) {
    let theme = &ex.theme;
    let surface = ex.surface;
    let area = layout.list;
    let inner = crate::gui::surface::Rect::new(
        area.x + layout.padding,
        area.y + layout.padding,
        area.width - 2 * layout.padding,
        area.height - 2 * layout.padding,
    );
    client.rect(surface, inner.x, inner.y, inner.width, inner.height, theme.surface);
    client.frame(surface, inner.x, inner.y, inner.width, inner.height, theme.accent, 1);

    let cells = ((inner.width - 2 * layout.padding) / layout.glyph_w).max(1) as usize;
    let fit = ((inner.height - 2 * layout.padding) / layout.row_h).max(0) as usize;
    for (rank, line) in lines.iter().take(fit.min(super::INFO_LINES)).enumerate() {
        client.text(
            surface,
            inner.x + layout.padding,
            inner.y + layout.padding + rank as i32 * layout.row_h,
            &ellipsize(line, cells),
            if rank == 0 { theme.accent } else { theme.text },
            None,
        );
    }
}

/// 10 commands at most.
fn paint_status(client: &mut Client, ex: &Explorer, layout: &Layout) {
    let theme = &ex.theme;
    let surface = ex.surface;
    let status = layout.status;
    client.rect(surface, status.x, status.y, status.width, status.height, theme.surface);
    client.rect(surface, 0, status.y, status.width, 1, theme.border);
    let text_y = layout.text_y(status.y, status.height);

    match ex.prompt {
        Prompt::None | Prompt::Path => {
            let left = ex.summary();
            client.text(surface, layout.padding, text_y, &left, theme.text, None);
            if !ex.message.is_empty() {
                let room = ((status.width - layout.glyph_w * left.len() as i32 - 4 * layout.padding)
                    / layout.glyph_w)
                    .max(1) as usize;
                let message = ellipsize(&ex.message, room);
                client.text(
                    surface,
                    status.width - layout.glyph_w * message.len() as i32 - layout.padding,
                    text_y,
                    &message,
                    if ex.error { theme.danger } else { theme.accent },
                    None,
                );
            }
        }
        Prompt::ConfirmDelete => {
            let question = format!(
                "Supprimer {} element(s) et leur contenu ? Entree = oui, Echap = non",
                ex.pending.len()
            );
            let room = ((layout.answer(true).x - 2 * layout.padding) / layout.glyph_w).max(1) as usize;
            client.text(
                surface,
                layout.padding,
                text_y,
                &ellipsize(&question, room),
                theme.danger,
                None,
            );
            button(client, ex, layout, layout.answer(true), "Oui", true, ex.hover == Hit::Yes, mix(theme.surface_alt, theme.danger, 60));
            button(client, ex, layout, layout.answer(false), "Non", true, ex.hover == Hit::No, theme.surface_alt);
        }
        Prompt::Rename | Prompt::NewFolder | Prompt::NewFile => {
            let line = format!("{} {}_", ex.prompt.label(), ex.entry);
            client.text(surface, layout.padding, text_y, &line, theme.text, None);
            let hint = "Entree = valider  Echap = annuler";
            client.text(
                surface,
                status.width - layout.glyph_w * hint.len() as i32 - layout.padding,
                text_y,
                hint,
                theme.text_dim,
                None,
            );
        }
    }
}
