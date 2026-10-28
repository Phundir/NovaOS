//! Phase 7.1 self-test.
//!
//! ## What a self-test can prove of a display, and what it cannot
//!
//! Nobody can check from inside the kernel that an image is *beautiful*,
//! nor even that it is *displayed*: the only witness of the video output
//! is the user's eye. What is checkable, and what covers the errors that
//! really hurt, comes to four points:
//!
//! * a pixel written at (x, y) reads back at (x, y) -- that is the
//!   arithmetic of the row stride, the most classic and silent mistake;
//! * a colour crossing the encoding and the decoding comes back -- that
//!   is the layout of the components, the one that gives blue where red
//!   was wanted on one machine and not on the other;
//! * clipping to the surface holds -- a rectangle overflowing a row
//!   crushes the first column of the next, a rectangle overflowing the
//!   bottom writes outside the framebuffer;
//! * scrolling really moves the content, not a shifted copy.
//!
//! ## Non-destructive
//!
//! The test draws on the screen the user is looking at. So it works in a
//! square thirty-two pixels on a side, whose content it saves before and
//! restores after -- except for the scrolling, which touches the whole
//! image and is therefore checked last, flattening the screen.

use super::framebuffer::{Color, Framebuffer, rgb};

struct Report {
    passed: usize,
    failed: usize,
}

impl Report {
    fn check(&mut self, label: &str, condition: bool) {
        if condition {
            self.passed += 1;
        } else {
            self.failed += 1;
            crate::serial_println!("[gfx] ECHEC : {}", label);
        }
    }
}

/// Side of the working square, in pixels.
const PATCH: usize = 32;

pub fn run() -> (usize, usize) {
    crate::serial_println!("[gfx] --- autotest phase 7.1 : framebuffer ---");

    let mut report = Report {
        passed: 0,
        failed: 0,
    };

    if !super::is_active() {
        crate::serial_println!("[gfx]   affichage graphique inactif - 7.1 non exerce");
        return (0, 0);
    }

    format_round_trip(&mut report);

    super::with_surface(|fb| {
        pixels(&mut report, fb);
        clipping(&mut report, fb);
        scrolling(&mut report, fb);
    });

    crate::serial_println!(
        "[gfx] --- phase 7.1 : {} verifications, {} echecs ---",
        report.passed,
        report.failed
    );
    (report.passed, report.failed)
}

/// Encoding to the hardware format then decoding must compose to the
/// identity, up to the bits lost.
///
/// The colours chosen are the extremes of each component: they are the
/// ones revealing an inverted field position or width, whereas a mid grey
/// crosses almost any format without complaining.
fn format_round_trip(report: &mut Report) {
    let cases = [
        rgb(0xFF, 0x00, 0x00),
        rgb(0x00, 0xFF, 0x00),
        rgb(0x00, 0x00, 0xFF),
        rgb(0xFF, 0xFF, 0xFF),
        rgb(0x00, 0x00, 0x00),
    ];

    super::with_surface(|fb| {
        // The read-back goes through the framebuffer itself: it is the
        // only way to exercise the real format of the mode rather than a
        // copy of its fields.
        for color in cases {
            fb.put_pixel(0, 0, color);
            let read = fb.read_pixel(0, 0);
            report.check(
                "une couleur primaire traverse l'encodage",
                read == Some(quantize(fb, color)),
            );
        }
        fb.put_pixel(0, 0, super::palette::DEFAULT_BACKGROUND);
    });
}

/// What a colour becomes after a round trip, given the field widths of
/// the mode.
fn quantize(fb: &Framebuffer, color: Color) -> Color {
    let format = fb.format();
    fn round(value: u8, size: u8) -> u8 {
        if size == 0 {
            return 0;
        }
        let size = size.min(8);
        (value >> (8 - size)) << (8 - size)
    }
    rgb(
        round(super::framebuffer::red_of(color), format.red_size),
        round(super::framebuffer::green_of(color), format.green_size),
        round(super::framebuffer::blue_of(color), format.blue_size),
    )
}

/// Working corner: bottom right, where the console takes longest to come
/// back.
fn patch_origin(fb: &Framebuffer) -> (usize, usize) {
    (
        fb.width().saturating_sub(PATCH),
        fb.height().saturating_sub(PATCH),
    )
}

fn save_patch(fb: &Framebuffer, buffer: &mut [Color; PATCH * PATCH]) {
    let (x0, y0) = patch_origin(fb);
    for row in 0..PATCH {
        for column in 0..PATCH {
            buffer[row * PATCH + column] =
                fb.read_pixel(x0 + column, y0 + row).unwrap_or(0);
        }
    }
}

fn restore_patch(fb: &mut Framebuffer, buffer: &[Color; PATCH * PATCH]) {
    let (x0, y0) = patch_origin(fb);
    for row in 0..PATCH {
        for column in 0..PATCH {
            fb.put_pixel(x0 + column, y0 + row, buffer[row * PATCH + column]);
        }
    }
    fb.present_rows(y0, PATCH);
}

/// A pixel reads back where it was written, and nowhere else.
fn pixels(report: &mut Report, fb: &mut Framebuffer) {
    let mut saved = [0u32; PATCH * PATCH];
    save_patch(fb, &mut saved);

    let (x0, y0) = patch_origin(fb);
    let mark = quantize(fb, rgb(0xFF, 0x00, 0xFF));
    let ground = quantize(fb, rgb(0x00, 0x00, 0x00));

    fb.fill_rect(x0, y0, PATCH, PATCH, ground);

    // A diagonal: it changes row **and** column at every step, which
    // fails on a wrong row stride as much as on a wrong pixel size. A
    // horizontal pattern would detect neither.
    for step in 0..PATCH {
        fb.put_pixel(x0 + step, y0 + step, mark);
    }

    let mut diagonal = true;
    let mut elsewhere = true;
    for row in 0..PATCH {
        for column in 0..PATCH {
            let read = fb.read_pixel(x0 + column, y0 + row);
            if row == column {
                diagonal &= read == Some(mark);
            } else {
                elsewhere &= read == Some(ground);
            }
        }
    }

    report.check("la diagonale se relit la ou elle a ete ecrite", diagonal);
    report.check("aucun pixel voisin n'a ete touche", elsewhere);

    restore_patch(fb, &saved);
}

/// A rectangle that overflows is clipped, never wrapped onto the next
/// row nor written outside the surface.
fn clipping(report: &mut Report, fb: &mut Framebuffer) {
    let mut saved = [0u32; PATCH * PATCH];
    save_patch(fb, &mut saved);

    let (x0, y0) = patch_origin(fb);
    let ground = quantize(fb, rgb(0x00, 0x00, 0x00));
    let mark = quantize(fb, rgb(0x00, 0xFF, 0x00));

    fb.fill_rect(x0, y0, PATCH, PATCH, ground);

    // Deliberately asks for four times too wide and too tall: the
    // rectangle must stop at the edge, and the call must return.
    fb.fill_rect(x0, y0, PATCH * 4, PATCH * 4, mark);

    let corner = fb.read_pixel(fb.width() - 1, fb.height() - 1);
    report.check("le dernier pixel de l'ecran est peint", corner == Some(mark));

    // An origin outside the surface must paint nothing at all.
    fb.fill_rect(fb.width(), 0, 16, 16, rgb(0xFF, 0xFF, 0xFF));
    fb.fill_rect(0, fb.height(), 16, 16, rgb(0xFF, 0xFF, 0xFF));
    report.check(
        "une origine hors surface ne peint rien",
        fb.read_pixel(fb.width() - 1, fb.height() - 1) == Some(mark),
    );

    // The pixel just above the square belongs to the console: the
    // rectangle must not have touched it.
    let above = fb.read_pixel(x0, y0 - 1);
    report.check("le rectangle n'a pas deborde vers le haut", above != Some(mark));

    restore_patch(fb, &saved);
}

/// Scrolling moves the content up and paints the freed band.
///
/// This test touches the whole screen -- there is no partial scroll --
/// and is therefore paid for with a clear. It comes last for that reason,
/// and the screen is left clean on the way out.
fn scrolling(report: &mut Report, fb: &mut Framebuffer) {
    let ground = quantize(fb, rgb(0x00, 0x00, 0x00));
    let mark = quantize(fb, rgb(0x00, 0x00, 0xFF));
    let shift = 16;

    fb.clear(ground);
    fb.fill_rect(0, shift, fb.width(), 1, mark);

    fb.scroll_up(shift, ground);

    report.check(
        "la ligne marquee est remontee du bon nombre de pixels",
        fb.read_pixel(0, 0) == Some(mark) && fb.read_pixel(fb.width() - 1, 0) == Some(mark),
    );
    report.check(
        "la bande liberee en bas est peinte au fond",
        fb.read_pixel(0, fb.height() - 1) == Some(ground),
    );
    report.check(
        "la ligne d'origine ne subsiste pas",
        fb.read_pixel(0, shift) == Some(ground),
    );

    fb.clear(super::palette::DEFAULT_BACKGROUND);
    fb.present();
}
