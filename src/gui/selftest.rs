//! Phase 7 self-test (7.2 to 7.7).
//!
//! ## What is checkable with no human eye
//!
//! Nothing allows establishing from the kernel that a window is *pretty*.
//! What can be checked, and what covers the mistakes that hurt:
//!
//! * the geometry -- intersections, unions, clipping, containment;
//! * the painting order -- a wrong z-order shows on screen and nowhere
//!   else, except here;
//! * the watertightness of the surfaces -- a command overflowing its
//!   window is a security fault as much as a display one;
//! * the protocol -- encoding then decoding must compose to the identity,
//!   and a truncated payload must end in a refusal;
//! * the spec tables -- 9.1 and 9.3 are data, and a transcription is
//!   checked line by line;
//! * the `.mod` format -- an altered file must be refused;
//! * anti-bruteforce -- the three thresholds of 2.4.

use alloc::string::String;

use crate::binfmt::header::{BinaryFormat, SignatureType};

use super::module::{self, Slot};
use super::notify::{self, Condition, Dismissal, Level};
use super::protocol::{self as proto, CursorShape, KeyEvent, PointerEvent, Reader, Writer};
use super::surface::{Command, Layer, Rect, Stack};

fn wait_for_compositor(timeout_ms: u64) -> bool {
    let per_tick = 1000 / crate::TIMER_HZ as u64;
    let deadline = crate::idt::ticks() + timeout_ms.div_ceil(per_tick).max(1);
    loop {
        // An active compositor is not enough: the modules must have had
        // time to create their surfaces, otherwise the self-test would
        // only observe the order in which the tasks were created.
        // Waiting for the surfaces is not enough either: a module creates
        // its own before publishing anything into it, and an empty
        // display list paints nothing. A self-test starting at that
        // moment would only observe the task creation order.
        let ready = super::is_active()
            && super::with(|comp| {
                comp.stack.len() >= 4
                    && comp
                        .stack
                        .iter()
                        .filter(|surface| !surface.display_list().is_empty())
                        .count()
                        >= 2
            })
            .unwrap_or(false);
        if ready {
            return true;
        }
        if crate::idt::ticks() >= deadline {
            return false;
        }
        let _ = crate::syscall::invoke(
            crate::syscall::domain_time::SYS_TIME_SLEEP_MS,
            [50, 0, 0, 0, 0, 0],
        );
    }
}

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
            crate::serial_println!("[gui] ECHEC : {}", label);
        }
    }

    fn note(&self, text: &str) {
        crate::serial_println!("[gui]   {}", text);
    }
}

pub fn run() -> (usize, usize) {
    // The compositor starts in phase 5 of 2.1, this self-test is a task
    // like any other: nothing guarantees it is elected after. Exceeding
    // the deadline is not a reason to give up -- the purely computational
    // checks hold all the same, and `live_compositor` will say what is
    // missing.
    wait_for_compositor(15_000);

    crate::serial_println!("[gui] --- autotest phase 7 : systeme graphique ---");

    let mut report = Report {
        passed: 0,
        failed: 0,
    };

    geometry(&mut report);
    protocol_round_trip(&mut report);
    stacking(&mut report);
    clipping(&mut report);
    module_format(&mut report);
    theme_parsing(&mut report);
    notification_table(&mut report);
    dismissal(&mut report);
    bruteforce(&mut report);
    login_layout(&mut report);
    blending(&mut report);
    hover_state(&mut report);
    close_cross(&mut report);
    text_buffer(&mut report);
    file_explorer(&mut report);
    screen_size(&mut report);
    settings_tabs(&mut report);
    draw_batches(&mut report);
    canvases(&mut report);
    window_titles(&mut report);
    shortcuts(&mut report);
    session_controls(&mut report);
    live_compositor(&mut report);
    routing(&mut report);
    damage_containment(&mut report);
    lifecycle(&mut report);

    crate::serial_println!(
        "[gui] --- phase 7 : {} verifications, {} echecs ---",
        report.passed,
        report.failed
    );
    (report.passed, report.failed)
}

// ============================================================
//  Geometry
// ============================================================

fn geometry(report: &mut Report) {
    let a = Rect::new(10, 10, 100, 50);
    let b = Rect::new(60, 30, 100, 50);

    report.check(
        "l'intersection de deux rectangles qui se chevauchent",
        a.intersect(&b) == Rect::new(60, 30, 50, 30),
    );
    report.check(
        "deux rectangles disjoints n'ont pas d'intersection",
        a.intersect(&Rect::new(500, 500, 10, 10)).is_empty(),
    );
    report.check(
        "l'union englobe les deux",
        a.union(&b) == Rect::new(10, 10, 150, 70),
    );
    // The union with the empty one is the identity: that is what allows
    // accumulating a damaged area starting from nothing.
    report.check(
        "l'union avec un rectangle vide ne change rien",
        a.union(&Rect::EMPTY) == a && Rect::EMPTY.union(&a) == a,
    );
    report.check(
        "l'appartenance exclut le bord droit et bas",
        a.contains(10, 10) && !a.contains(110, 10) && !a.contains(10, 60),
    );
    report.check(
        "le retrecissement rend vide quand il depasse",
        Rect::new(0, 0, 10, 10).shrink(6).is_empty(),
    );
    report.check(
        "un rectangle de largeur nulle est vide",
        Rect::new(5, 5, 0, 20).is_empty(),
    );
}

// ============================================================
//  Protocol (7.4)
// ============================================================

fn protocol_round_trip(report: &mut Report) {
    // --- field-by-field reading ---
    let payload = Writer::new()
        .u8(0xAB)
        .u16(0x1234)
        .u32(0xDEAD_BEEF)
        .i16(-300)
        .text("titre")
        .build();
    let mut reader = Reader::new(&payload);
    report.check(
        "les entiers traversent l'encodage",
        reader.u8() == Some(0xAB)
            && reader.u16() == Some(0x1234)
            && reader.u32() == Some(0xDEAD_BEEF)
            && reader.i16() == Some(-300),
    );
    report.check("le texte suit les entiers", reader.text() == "titre");

    // A truncated payload must return None, never an invented value: it
    // is the only discipline that holds against a malicious client.
    let mut short = Reader::new(&[0x01, 0x02]);
    report.check(
        "une charge utile tronquee refuse au lieu d'inventer",
        short.u32().is_none(),
    );

    let mut empty = Reader::new(&[]);
    report.check(
        "une charge utile vide ne panique pas",
        empty.u8().is_none() && empty.text().is_empty(),
    );

    // --- input events ---
    let key = KeyEvent {
        scancode: 0x1E,
        pressed: true,
        modifiers: KeyEvent::MOD_CTRL | KeyEvent::MOD_SHIFT,
        codepoint: 'q' as u32,
    };
    report.check(
        "un evenement clavier fait l'aller-retour",
        KeyEvent::decode(&key.encode()) == Some(key),
    );
    report.check(
        "les modificateurs se relisent",
        key.has(KeyEvent::MOD_CTRL) && !key.has(KeyEvent::MOD_ALT),
    );

    let pointer = PointerEvent {
        buttons: PointerEvent::BUTTON_LEFT,
        x: 640,
        y: 400,
        dx: -3,
        dy: 7,
        shape: 0,
        wheel: -2,
    };
    report.check(
        "un evenement de pointeur fait l'aller-retour",
        PointerEvent::decode(&pointer.encode()) == Some(pointer),
    );

    // The shape took the place of a padding nobody read. Two things must
    // hold: it survives the round trip, and a payload that stops before it
    // still decodes -- otherwise every producer not recompiled would see
    // its events refused.
    let shaped = PointerEvent {
        shape: CursorShape::Text.as_code(),
        ..pointer
    };
    report.check(
        "la forme du curseur survit a l'aller-retour",
        PointerEvent::decode(&shaped.encode()) == Some(shaped)
            && shaped.cursor_shape() == CursorShape::Text,
    );
    let encoded = shaped.encode();
    report.check(
        "une charge utile sans octet de forme reste lisible",
        PointerEvent::decode(&encoded[..encoded.len() - 2])
            .map(|event| event.cursor_shape())
            == Some(CursorShape::Arrow),
    );
    report.check(
        "un code de forme inconnu retombe sur la fleche",
        CursorShape::from_code(200) == CursorShape::Arrow
            && CursorShape::from_code(1) == CursorShape::Hand,
    );

    // Every code this build routes must have a name. `op_name` feeds the
    // log and `OP_MODULE_LIST`, and a "?" in a trace is a code one then has
    // to go and look up in a header.
    let named = [
        proto::OP_CURSOR_SHAPE,
        proto::OP_MENU_TOGGLE,
        proto::OP_SESSION_ACTION,
        proto::EV_MENU_TOGGLE,
        proto::EV_SCREEN_RESIZED,
        proto::EV_POINTER,
    ];
    report.check(
        "les codes du pointeur et du menu ont tous un nom",
        named.iter().all(|op| proto::op_name(*op) != "?"),
    );
    // Two codes that collide would send a menu toggle where a redraw was
    // meant. The whole table is checked, not just the new entries: a
    // collision is only ever introduced by adding one.
    let all = [
        proto::OP_SURFACE_CREATE,
        proto::OP_SURFACE_DESTROY,
        proto::OP_SURFACE_MOVE,
        proto::OP_SURFACE_RESIZE,
        proto::OP_SURFACE_RAISE,
        proto::OP_SURFACE_LOWER,
        proto::OP_SURFACE_SHOW,
        proto::OP_SURFACE_FOCUS,
        proto::OP_SURFACE_LIST,
        proto::OP_DRAW_RESET,
        proto::OP_DRAW_FILL,
        proto::OP_DRAW_RECT,
        proto::OP_DRAW_FRAME,
        proto::OP_DRAW_TEXT,
        proto::OP_DRAW_LINE,
        proto::OP_DRAW_ICON,
        proto::OP_COMMIT,
        proto::OP_SCREEN_INFO,
        proto::OP_THEME_GET,
        proto::OP_THEME_RELOAD,
        proto::OP_CURSOR_SHAPE,
        proto::OP_NOTIFY_POST,
        proto::OP_NOTIFY_DISMISS,
        proto::OP_NOTIFY_COUNT,
        proto::OP_NOTIFY_ACTION,
        proto::OP_MODULE_LIST,
        proto::OP_MODULE_RESCAN,
        proto::OP_MENU_TOGGLE,
        proto::OP_SESSION_ACTION,
        proto::OP_DRAW_BATCH,
        proto::OP_WINDOW_ENTRIES,
        proto::OP_SURFACE_TITLE,
        proto::OP_DRAW_CANVAS,
        proto::EV_WINDOWS_CHANGED,
        proto::EV_KEY,
        proto::EV_POINTER,
        proto::EV_FOCUS,
        proto::EV_CLOSE,
        proto::EV_RESIZED,
        proto::EV_NOTIFY_ACTION,
        proto::EV_REDRAW,
        proto::EV_MENU_TOGGLE,
        proto::EV_SCREEN_RESIZED,
    ];
    let mut unique = true;
    for (index, op) in all.iter().enumerate() {
        if all[index + 1..].contains(op) {
            unique = false;
        }
    }
    report.check("aucun code d'operation n'est utilise deux fois", unique);

    // An event of another kind must not be read as a keyboard one.
    report.check(
        "le genre discrimine clavier et pointeur",
        KeyEvent::decode(&pointer.encode()).is_none()
            && PointerEvent::decode(&key.encode()).is_none(),
    );

    report.check(
        "chaque operation a un nom",
        proto::op_name(proto::OP_SURFACE_CREATE) == "SURFACE_CREATE"
            && proto::op_name(0xDEAD) == "?",
    );
}

// ============================================================
//  Z-order (7.2.3)
// ============================================================

fn stacking(report: &mut Report) {
    let mut stack = Stack::new();

    let wallpaper = stack
        .create(1, Layer::Wallpaper, Rect::new(0, 0, 800, 600), String::from("fond"))
        .expect("fond");
    let window_a = stack
        .create(2, Layer::Window, Rect::new(50, 50, 200, 150), String::from("A"))
        .expect("A");
    let window_b = stack
        .create(3, Layer::Window, Rect::new(100, 80, 200, 150), String::from("B"))
        .expect("B");
    let cursor = stack
        .create(4, Layer::Overlay, Rect::new(0, 0, 16, 16), String::from("curseur"))
        .expect("curseur");

    let order = stack.painting_order();
    report.check(
        "le fond se peint en premier, le curseur en dernier",
        order.first() == Some(&wallpaper) && order.last() == Some(&cursor),
    );
    report.check(
        "une surface nait au-dessus de sa couche",
        order.iter().position(|id| *id == window_b)
            > order.iter().position(|id| *id == window_a),
    );

    // The layer takes precedence over the z: that is what stops a window
    // coming in front of the cursor, whatever the value asked for.
    stack.raise(window_a);
    if let Some(surface) = stack.get_mut(window_a) {
        surface.z = 10_000;
    }
    let order = stack.painting_order();
    report.check(
        "aucun z ne fait passer une fenetre devant le curseur",
        order.last() == Some(&cursor),
    );

    stack.raise(window_a);
    report.check(
        "raise remonte au sommet de sa couche",
        stack.painting_order().iter().rposition(|id| *id == window_a)
            > stack.painting_order().iter().rposition(|id| *id == window_b),
    );

    // --- focus ---
    report.check(
        "le focus va a la derniere fenetre creee",
        stack.set_focus(window_b) && stack.focus() == Some(window_b),
    );
    report.check(
        "le fond d'ecran ne prend jamais le focus",
        !stack.set_focus(wallpaper) && stack.focus() == Some(window_b),
    );
    report.check(
        "le curseur ne prend jamais le focus",
        !stack.set_focus(cursor),
    );

    // --- designation by the pointer ---
    report.check(
        "le pointeur designe la fenetre du dessus",
        stack.hit(150, 100, 0, 0) == Some(window_a),
    );
    report.check(
        "le curseur n'attrape jamais un clic",
        stack.hit(5, 5, 0, 0) == Some(wallpaper),
    );
    report.check(
        "un point hors de toute surface ne designe rien",
        stack.hit(5000, 5000, 0, 0).is_none(),
    );

    // --- disappearance of an owner ---
    report.check(
        "la mort d'un processus emporte ses surfaces",
        stack.destroy_owned_by(2) == 1 && stack.get(window_a).is_none(),
    );
    report.check(
        "le focus se reporte quand sa surface disparait",
        stack.focus() == Some(window_b),
    );

    // --- cap ---
    let mut full = Stack::new();
    for index in 0..super::surface::MAX_SURFACES {
        full.create(
            1,
            Layer::Window,
            Rect::new(0, 0, 10, 10),
            alloc::format!("s{}", index),
        );
    }
    report.check(
        "le plafond de surfaces est applique",
        full.create(1, Layer::Window, Rect::new(0, 0, 10, 10), String::new())
            .is_none(),
    );

    // --- command cap ---
    let mut surface = super::surface::Surface::new(
        1,
        1,
        Layer::Window,
        Rect::new(0, 0, 10, 10),
        String::new(),
        0,
    );
    for _ in 0..super::surface::MAX_COMMANDS {
        surface.push(Command::Fill { color: 0 });
    }
    report.check(
        "une surface ne peut pas emettre sans fin",
        !surface.push(Command::Fill { color: 0 }) && surface.refused == 1,
    );

    // The published list changes only at the commit: it is the double
    // buffer transposed to the level of the instructions.
    let mut staged = super::surface::Surface::new(
        2,
        1,
        Layer::Window,
        Rect::new(0, 0, 10, 10),
        String::new(),
        0,
    );
    staged.push(Command::Fill { color: 1 });
    report.check(
        "une commande non publiee n'est pas rendue",
        staged.display_list().is_empty() && staged.pending_len() == 1,
    );
    staged.commit();
    report.check(
        "le commit publie la liste d'un bloc",
        staged.display_list().len() == 1 && staged.commits == 1,
    );
}

// ============================================================
//  Watertightness of the surfaces
// ============================================================

/// A command must never paint outside its surface.
///
/// The test draws in a surface deliberately placed at the edge, with
/// coordinates that overflow widely, then reads back the pixels around.
/// It is the most important check of this whole file: a leak here is an
/// application that can write on the window of another.
fn clipping(report: &mut Report) {
    use crate::graphics::framebuffer::rgb;
    use super::painter::Painter;

    if !crate::graphics::is_active() {
        report.note("pas de framebuffer - etancheite non exercee");
        return;
    }

    let intruder = rgb(0xFF, 0x00, 0xFF);
    let ground = rgb(0x00, 0x00, 0x00);

    let leaked = crate::graphics::with_surface(|fb| {
        let width = fb.width() as i32;
        let height = fb.height() as i32;

        // Working area: a 64-pixel square, far from the top edge so that
        // one can observe above it.
        let area = Rect::new(width - 96, height - 96, 64, 64);
        fb.fill_rect(
            (area.x - 16) as usize,
            (area.y - 16) as usize,
            96,
            96,
            ground,
        );

        {
            let mut screen = Painter::screen(fb);
            let mut zone = screen.region(area);
            // Insane coordinates, in all four directions.
            zone.fill(Rect::new(-500, -500, 2000, 2000), intruder);
            zone.line(-800, -800, 800, 800, intruder);
            zone.text(-200, -200, "debordement", intruder, Some(intruder));
            zone.frame(Rect::new(-50, -50, 400, 400), intruder, 6);
        }

        // Nothing must have changed outside the square.
        let mut outside = 0;
        for y in (area.y - 16)..(area.bottom() + 16) {
            for x in (area.x - 16)..(area.right() + 16) {
                if area.contains(x, y) {
                    continue;
                }
                if x < 0 || y < 0 || x >= width || y >= height {
                    continue;
                }
                if fb.read_pixel(x as usize, y as usize) == Some(intruder) {
                    outside += 1;
                }
            }
        }

        // And the inside must indeed have been painted: a clip that
        // refused everything would pass the previous test proving nothing.
        let inside = fb.read_pixel(area.x as usize, area.y as usize) == Some(intruder)
            && fb.read_pixel((area.right() - 1) as usize, (area.bottom() - 1) as usize)
                == Some(intruder);

        (outside, inside)
    });

    let (outside, inside) = leaked.unwrap_or((0, false));
    report.check("aucune commande ne deborde de sa surface", outside == 0);
    report.check("le decoupe laisse passer ce qui est dedans", inside);

    // The clip composes: a sub-area cannot grow.
    let composed = crate::graphics::with_surface(|fb| {
        let mut screen = Painter::screen(fb);
        let mut outer = screen.region(Rect::new(100, 100, 200, 200));
        let inner = outer.region(Rect::new(-50, -50, 400, 400));
        inner.clip()
    });
    report.check(
        "le decoupe se compose au lieu de s'elargir",
        composed == Some(Rect::new(100, 100, 200, 200)),
    );

    // The test painted directly on the framebuffer, short-circuiting the
    // compositor. Asking it to repaint everything erases the traces: 8.1
    // is momentarily broken by the self-test, it must not stay so on
    // screen.
    super::with(|comp| comp.damage_all());
}

// ============================================================
//  .mod format (7.3)
// ============================================================

fn module_format(report: &mut Report) {
    let payload = b"module=os.nova.test\nslot=Taskbar\n";
    let file = module::build(&module::BuildSpec {
        slot: Slot::Taskbar,
        signature: SignatureType::SelfSigned,
        app_id: "os.nova.test",
        display_name: "Test",
        author: "autotest",
        version: [2, 1, 0, 7],
        extra_flags: 0,
        payload,
    });

    let header = module::parse(&file);
    report.check("un .mod bien forme s'analyse", header.is_ok());

    if let Ok(header) = header.as_ref() {
        report.check("l'emplacement se relit", header.slot == Slot::Taskbar);
        report.check("l'identite se relit", header.app_id == "os.nova.test");
        report.check("la version se relit", header.version_string() == "2.1.0+7");
        report.check(
            "un emplacement remplacable porte le drapeau",
            header.is_replaceable(),
        );
        report.check(
            "un auto-signe est admis dans un emplacement remplacable",
            module::authorize(header).is_ok(),
        );
    }

    // 7.3.2: an altered payload must be refused. That is the whole point
    // of the digest, and the only way to prove it is to corrupt a byte.
    let mut tampered = file.clone();
    let last = tampered.len() - 1;
    tampered[last] ^= 0xFF;
    report.check(
        "une charge utile alteree est refusee",
        module::parse(&tampered) == Err(module::ModuleError::DigestMismatch),
    );

    let mut bad_magic = file.clone();
    bad_magic[0] = b'X';
    report.check(
        "un magic faux est refuse",
        module::parse(&bad_magic) == Err(module::ModuleError::BadMagic),
    );

    report.check(
        "un fichier trop court est refuse",
        module::parse(&[0u8; 8]) == Err(module::ModuleError::TooShort),
    );

    // The flag and the 8.2 table must say the same thing.
    let mut contradiction = file.clone();
    contradiction[0x06] = Slot::Core as u8;
    report.check(
        "un remplacable dans l'emplacement Core est refuse",
        module::parse(&contradiction) == Err(module::ModuleError::SlotContradiction),
    );

    // A self-signed one in Core: the file is coherent, it is the policy
    // that refuses.
    let core_file = module::build(&module::BuildSpec {
        slot: Slot::Core,
        signature: SignatureType::SelfSigned,
        app_id: "os.pirate.core",
        display_name: "Faux noyau",
        author: "inconnu",
        version: [1, 0, 0, 0],
        extra_flags: 0,
        payload: b"x",
    });
    let core_header = module::parse(&core_file);
    report.check("le fichier Core s'analyse", core_header.is_ok());
    if let Ok(header) = core_header.as_ref() {
        report.check(
            "un auto-signe est refuse dans un emplacement non remplacable",
            module::authorize(header) == Err(module::ModuleError::SelfSignedCore),
        );
    }

    // SYSTEM with no digest in the store.
    let unofficial = module::build(&module::BuildSpec {
        slot: Slot::Taskbar,
        signature: SignatureType::System,
        app_id: "os.pirate.taskbar",
        display_name: "Fausse barre",
        author: "inconnu",
        version: [1, 0, 0, 0],
        payload: b"charge utile inconnue du magasin",
        extra_flags: 0,
    });
    if let Ok(header) = module::parse(&unofficial) {
        report.check(
            "SYSTEM sans empreinte enregistree est refuse",
            module::authorize(&header) == Err(module::ModuleError::NotOfficial),
        );
    }

    report.check(
        "seul l'emplacement Core n'est pas remplacable",
        !Slot::Core.replaceable()
            && Slot::Taskbar.replaceable()
            && Slot::WindowManager.replaceable()
            && Slot::Notifications.replaceable()
            && Slot::Wallpaper.replaceable()
            && Slot::Launcher.replaceable()
            && Slot::Overlay.replaceable(),
    );
}

// ============================================================
//  Theme (7.5)
// ============================================================

fn theme_parsing(report: &mut Report) {
    use super::theme::Theme;

    let mut theme = Theme::dark();
    report.check(
        "une couleur hexadecimale se lit",
        theme.apply("accent = #FF8800") && theme.accent == 0x00FF_8800,
    );
    report.check(
        "le croisillon est facultatif",
        theme.apply("danger = 112233") && theme.danger == 0x0011_2233,
    );
    report.check(
        "une dimension se lit",
        theme.apply("taskbar_height = 40") && theme.taskbar_height == 40,
    );
    report.check(
        "une dimension delirante est bornee",
        theme.apply("taskbar_height = 99999") && theme.taskbar_height == 512,
    );
    report.check("un commentaire est ignore", !theme.apply("# rien"));
    report.check("une ligne vide est ignoree", !theme.apply("   "));
    report.check(
        "une cle inconnue est ignoree sans erreur",
        !theme.apply("chose_inconnue = 42"),
    );
    report.check(
        "une couleur malformee est ignoree",
        !theme.apply("accent = #ZZZZZZ") && theme.accent == 0x00FF_8800,
    );
    report.check(
        "le nom de police se lit",
        theme.apply("font = terminus") && theme.font_name() == "terminus",
    );

    // The round trip through the file must preserve the theme: that is
    // what allows generating the reference theme rather than copying it.
    let written = theme.to_cfg();
    let mut reloaded = Theme::dark();
    let keys = reloaded.merge(&written);
    report.check("le theme fait l'aller-retour par son fichier", reloaded == theme);
    report.check("toutes les cles du fichier sont reconnues", keys >= 18);

    // 7.5.1: the stacking, hence the priority.
    let mut layered = Theme::dark();
    layered.merge("accent = #111111\ntaskbar_height = 30\n");
    layered.merge("accent = #222222\n");
    report.check(
        "la derniere source appliquee l'emporte",
        layered.accent == 0x0022_2222 && layered.taskbar_height == 30,
    );
}

// ============================================================
//  Notifications (7.6)
// ============================================================

/// Transcription of the 9.1 table, line by line.
fn notification_table(report: &mut Report) {
    report.check(
        "9.1 : le plein ecran est reserve a PRIORITAIRE et URGENT",
        Level::Prioritaire.fullscreen()
            && Level::Urgent.fullscreen()
            && !Level::Haute.fullscreen()
            && !Level::Normale.fullscreen()
            && !Level::Basse.fullscreen(),
    );
    report.check(
        "9.1 : deux niveaux ignorent le mode ne-pas-deranger",
        Level::Prioritaire.ignores_do_not_disturb()
            && Level::Urgent.ignores_do_not_disturb()
            && !Level::Haute.ignores_do_not_disturb(),
    );
    report.check(
        "9.1 : les echeances sont 15, 8 et 5 secondes",
        Level::Urgent.auto_timeout_ms() == Some(15_000)
            && Level::Haute.auto_timeout_ms() == Some(8_000)
            && Level::Normale.auto_timeout_ms() == Some(5_000),
    );
    report.check(
        "9.1 : PRIORITAIRE exige un bouton, BASSE ne fait pas de toast",
        Level::Prioritaire.auto_timeout_ms().is_none()
            && Level::Prioritaire.requires_action()
            && !Level::Basse.toast(true),
    );
    report.check(
        "9.1 : NORMALE ne fait un toast que si la session est active",
        Level::Normale.toast(true) && !Level::Normale.toast(false),
    );

    // 9.3, the five lines.
    use BinaryFormat::*;
    report.check(
        "9.3 : PRIORITAIRE n'est ouvert qu'aux DSA et SS",
        notify::allowed(Level::Prioritaire, Dsa)
            && notify::allowed(Level::Prioritaire, Ss)
            && !notify::allowed(Level::Prioritaire, Gs)
            && !notify::allowed(Level::Prioritaire, Cus)
            && !notify::allowed(Level::Prioritaire, Ras),
    );
    report.check(
        "9.3 : URGENT s'ouvre aux GS",
        notify::allowed(Level::Urgent, Gs)
            && !notify::allowed(Level::Urgent, Cus)
            && !notify::allowed(Level::Urgent, Ras),
    );
    report.check(
        "9.3 : HAUTE s'ouvre aux CUS mais jamais aux RAS",
        notify::allowed(Level::Haute, Cus) && !notify::allowed(Level::Haute, Ras),
    );
    report.check(
        "9.3 : NORMALE et BASSE sont ouvertes a tous",
        notify::allowed(Level::Normale, Ras) && notify::allowed(Level::Basse, Ras),
    );

    // --- behaviour of the centre ---
    let mut center = notify::Center::new();
    let now = 1000;

    report.check(
        "un RAS se voit refuser une PRIORITAIRE",
        center
            .post(
                1,
                "os.test",
                Ras,
                Level::Prioritaire,
                "titre",
                "corps",
                Dismissal::automatic(),
                1,
                false,
                now,
            )
            .is_err(),
    );
    report.check(
        "une PRIORITAIRE sans bouton est refusee",
        center.post(
            1,
            "os.test",
            Dsa,
            Level::Prioritaire,
            "titre",
            "corps",
            Dismissal::none(),
            0,
            false,
            now,
        ) == Err(notify::PostError::ActionRequired),
    );

    let urgent = center.post(
        1,
        "os.test",
        Dsa,
        Level::Urgent,
        "urgent",
        "corps",
        Dismissal::automatic(),
        0,
        false,
        now,
    );
    report.check("une URGENT depuis un DSA passe", urgent.is_ok());

    // 15 seconds at 100 Hz = 1500 ticks.
    report.check(
        "l'echeance automatique suit le niveau",
        center.expire(now + 1499) == 0 && center.expire(now + 1500) == 1,
    );

    // Do-not-disturb mode: the notification exists but with no toast.
    center.do_not_disturb = true;
    let muted = center
        .post(
            1,
            "os.test",
            Dsa,
            Level::Haute,
            "haute",
            "corps",
            Dismissal::automatic(),
            0,
            false,
            now,
        )
        .expect("HAUTE depuis un DSA");
    report.check(
        "le mode ne-pas-deranger retire le toast sans supprimer",
        center.len() == 1 && center.toasts().is_empty(),
    );
    let _ = muted;

    let forced = center.post(
        1,
        "os.test",
        Dsa,
        Level::Urgent,
        "urgent",
        "corps",
        Dismissal::automatic(),
        0,
        false,
        now,
    );
    report.check(
        "une URGENT traverse le mode ne-pas-deranger",
        forced.is_ok() && !center.toasts().is_empty(),
    );
    center.do_not_disturb = false;
}

/// 9.2 -- the five conditions and their combination.
fn dismissal(report: &mut Report) {
    let now = 500;

    let any = Dismissal::any(&[Condition::Timeout(3000), Condition::Automatic]);
    // 3 s = 300 ticks, the automatic duration of an URGENT is 15 s = 1500.
    // The first met wins: it is the shortest.
    report.check(
        "Any prend la condition qui tombe la premiere",
        any.deadline_ticks(Level::Urgent, now) == Some(now + 300),
    );
    report.check(
        "une combinaison sans condition temporelle n'a pas d'echeance",
        Dismissal::any(&[Condition::Manual]).deadline_ticks(Level::Urgent, now) == None,
    );
    report.check(
        "un vecteur vide ne fait jamais disparaitre",
        Dismissal::none().deadline_ticks(Level::Normale, now) == None,
    );
    report.check(
        "les conditions non temporelles se reconnaissent",
        Dismissal::any(&[Condition::Action(2), Condition::IpcSignal(77)]).accepts_action(2)
            && Dismissal::any(&[Condition::IpcSignal(77)]).accepts_signal(77)
            && !Dismissal::any(&[Condition::IpcSignal(77)]).accepts_signal(78),
    );

    // --- manual close ---
    let mut center = notify::Center::new();
    let manual = center
        .post(
            5,
            "os.test",
            BinaryFormat::Cus,
            Level::Normale,
            "titre",
            "corps",
            Dismissal::any(&[Condition::Manual]),
            0,
            false,
            now,
        )
        .expect("NORMALE depuis un CUS");
    report.check("une notification manuelle se ferme", center.dismiss(manual));

    let stubborn = center
        .post(
            5,
            "os.test",
            BinaryFormat::Cus,
            Level::Normale,
            "titre",
            "corps",
            Dismissal::any(&[Condition::Action(1)]),
            1,
            false,
            now,
        )
        .expect("NORMALE avec bouton");
    report.check(
        "une notification sans condition manuelle resiste au clic",
        !center.dismiss(stubborn),
    );
    report.check(
        "le bouton declare la fait disparaitre",
        center.act(stubborn, 1) == Some(5) && center.is_empty(),
    );

    // --- IPC signal ---
    let signalled = center
        .post(
            9,
            "os.test",
            BinaryFormat::Cus,
            Level::Basse,
            "transfert",
            "en cours",
            Dismissal::any(&[Condition::IpcSignal(0x42)]),
            0,
            false,
            now,
        )
        .expect("BASSE");
    let _ = signalled;
    report.check(
        "un signal IPC ne retire que les notifications de son emetteur",
        center.signal(8, 0x42) == 0 && center.signal(9, 0x42) == 1,
    );

    // --- round trip of the encoding ---
    let dismissal = Dismissal::any(&[
        Condition::Timeout(2500),
        Condition::Manual,
        Condition::Action(3),
        Condition::IpcSignal(0xABCD),
    ]);
    let encoded = notify::encode_post(Level::Haute, 2, true, &dismissal, "Titre", "Le corps");
    let decoded = notify::decode_post(&encoded);
    report.check(
        "une emission fait l'aller-retour par le protocole",
        decoded
            == Some((
                Level::Haute,
                2,
                true,
                dismissal,
                String::from("Titre"),
                String::from("Le corps"),
            )),
    );
    report.check(
        "une emission tronquee est refusee",
        notify::decode_post(&encoded[..2]).is_none(),
    );
}

// ============================================================
//  Anti-bruteforce (7.7.2)
// ============================================================

fn bruteforce(report: &mut Report) {
    use super::login::{self, Verdict};

    let user = "autotest-login";
    login::reset(user);

    // The first two failures do not delay.
    let first = login::record_failure(user);
    let second = login::record_failure(user);
    report.check(
        "2.4 : les deux premiers echecs ne temporisent pas",
        first == Verdict::WrongPassword && second == Verdict::WrongPassword,
    );

    let third = login::record_failure(user);
    report.check(
        "2.4 : le troisieme echec donne 30 secondes",
        third == Verdict::Throttled(login::DELAY_MS),
    );
    report.check(
        "2.4 : le compte temporise refuse avant l'echeance",
        matches!(login::status(user), Verdict::Throttled(_)),
    );

    login::record_failure(user);
    let fifth = login::record_failure(user);
    report.check(
        "2.4 : le cinquieme echec donne 5 minutes",
        fifth == Verdict::Throttled(login::LONG_DELAY_MS),
    );

    for _ in 0..4 {
        login::record_failure(user);
    }
    let tenth = login::record_failure(user);
    report.check("2.4 : le dixieme echec verrouille", tenth == Verdict::Locked);
    report.check(
        "2.4 : un compte verrouille le reste",
        login::status(user) == Verdict::Locked,
    );
    report.check(
        "un administrateur peut deverrouiller",
        login::unlock(user) && login::status(user) == Verdict::Accepted,
    );

    // --- digests ---
    let salt = [0x5Au8; 16];
    let digest = login::hash_password(&salt, "motdepasse");
    report.check(
        "le meme mot de passe donne la meme empreinte",
        login::hash_password(&salt, "motdepasse") == digest,
    );
    report.check(
        "un sel different donne une empreinte differente",
        login::hash_password(&[0xA5u8; 16], "motdepasse") != digest,
    );
    report.check(
        "un mot de passe different donne une empreinte differente",
        login::hash_password(&salt, "motdepassf") != digest,
    );

    // --- real authentication, if the disk answers ---
    if crate::fs::is_ready() {
        let now = crate::idt::ticks();
        login::reset(user);
        if login::set_password(user, "secret-autotest", now) {
            report.check(
                "un mot de passe correct est accepte",
                login::authenticate(user, "secret-autotest") == Verdict::Accepted,
            );
            report.check(
                "un mot de passe faux est refuse",
                login::authenticate(user, "autre") == Verdict::WrongPassword,
            );
            report.check(
                "un compte inconnu est refuse",
                login::authenticate("fantome-autotest", "x") == Verdict::UnknownAccount,
            );
            login::reset(user);
            // The digest file survives the reboot: the self-test has
            // nothing to leave in it.
            report.check(
                "l'autotest retire son compte du fichier",
                login::remove_record(user, now)
                    && !login::load_records().iter().any(|record| record.user == user),
            );
        }
    }
}

// ============================================================
//  7.7.1 -- Geometry of the login panel
// ============================================================

/// The click must land where the row is painted.
///
/// This check exists because the geometry used to be computed inline as
/// `draw` walked down the panel, and a hit test added beside it would
/// have recomputed the same numbers a second time. That is the defect of
/// the cursor damage rectangle -- written "14 by 20" for a 16 by 16
/// surface -- transposed: it would read as "the click does not select the
/// account under the pointer", and nothing would point at the cause.
///
/// So the rectangles come from `Layout`, and what is checked here is that
/// every one of them is reachable, that they do not overlap, and that
/// they stay inside the panel.
fn login_layout(report: &mut Report) {
    use super::login::{Layout, Target};
    use super::protocol::CursorShape;

    let theme = super::theme::Theme::dark();
    let layout = Layout::new(1024, 768, &theme, 3);

    // --- each row is hit at its own centre ---
    let mut rows_reachable = true;
    for index in 0..3usize {
        let (x, y, w, h) = layout.row(index);
        if layout.hit(x + w / 2, y + h / 2) != Target::Account(index) {
            rows_reachable = false;
        }
    }
    report.check(
        "2.4 : le centre de chaque ligne de compte designe ce compte",
        rows_reachable,
    );

    // A row is painted from its rectangle and hit from the same one: the
    // first pixel and the last must both answer.
    let (row_x, row_y, row_w, row_h) = layout.row(1);
    report.check(
        "le premier et le dernier pixel d'une ligne repondent",
        layout.hit(row_x, row_y) == Target::Account(1)
            && layout.hit(row_x + row_w - 1, row_y + row_h - 1) == Target::Account(1),
    );
    report.check(
        "un pixel juste au-dela d'une ligne ne la designe plus",
        layout.hit(row_x + row_w, row_y) != Target::Account(1)
            && layout.hit(row_x, row_y - 1) != Target::Account(1),
    );

    // --- the password field ---
    let (field_x, field_y, field_w, field_h) = layout.field;
    report.check(
        "le champ de mot de passe est atteignable au clic",
        layout.hit(field_x + field_w / 2, field_y + field_h / 2) == Target::Password,
    );
    report.check(
        "le champ n'empiete sur aucune ligne de compte",
        (0..3usize).all(|index| {
            let (_, y, _, h) = layout.row(index);
            y + h <= field_y
        }),
    );

    // --- the validation arrow, beside the field ---
    let (submit_x, submit_y, submit_w, submit_h) = layout.submit;
    report.check(
        "la fleche de validation est atteignable au clic",
        layout.hit(submit_x + submit_w / 2, submit_y + submit_h / 2) == Target::Submit,
    );
    report.check(
        "la fleche et le champ ne se recouvrent pas",
        field_x + field_w <= submit_x,
    );
    // They are adjacent: the last pixel of one and the first of the other
    // must not answer for the same target. A gap that closed would make
    // the check of `hit` order the only thing keeping them apart.
    report.check(
        "le dernier pixel du champ et le premier de la fleche se distinguent",
        layout.hit(field_x + field_w - 1, field_y + field_h / 2) == Target::Password
            && layout.hit(submit_x, submit_y + submit_h / 2) == Target::Submit,
    );
    report.check(
        "la fleche reste dans le panneau et le champ garde une largeur utile",
        submit_x + submit_w <= layout.panel_x + layout.panel_width
            && field_w > layout.glyph_width * 12,
    );
    report.check(
        "la fleche est a la hauteur du champ",
        submit_y == field_y && submit_h == field_h,
    );

    // --- the Accessibility button, always visible (2.4) ---
    let (button_x, button_y, button_w, button_h) = layout.button;
    report.check(
        "2.4 : le bouton Accessibilite est atteignable au clic",
        layout.hit(button_x + button_w / 2, button_y + button_h / 2) == Target::Accessibility,
    );
    report.check(
        "le bouton reste dans le panneau",
        button_x >= layout.panel_x
            && button_x + button_w <= layout.panel_x + layout.panel_width
            && button_y + button_h <= layout.panel_y + layout.panel_height,
    );
    // The rectangle is sized on the longer of the two labels: a button
    // that shrinks when pressed moves out from under the pointer, and the
    // second click misses.
    report.check(
        "la largeur du bouton couvre le libelle le plus long",
        button_w >= layout.glyph_width * "[F1] Accessibilite : active".len() as i32,
    );

    // --- outside ---
    report.check(
        "un clic hors du panneau ne designe rien",
        layout.hit(0, 0) == Target::Elsewhere
            && layout.hit(1023, 767) == Target::Elsewhere,
    );
    report.check(
        "un clic entre le champ et le bouton ne designe rien",
        layout.hit(field_x, (field_y + field_h + button_y) / 2) == Target::Elsewhere,
    );

    // --- the geometry must hold whatever the number of accounts ---
    //
    // The panel height is a function of that count. A single account, or
    // eight, must not put the field or the button outside the panel: it is
    // the kind of defect a machine with three accounts never shows.
    let mut every_count_holds = true;
    for accounts in 1..=8usize {
        let other = Layout::new(1024, 768, &theme, accounts);
        let (fx, fy, fw, fh) = other.field;
        let (sx, sy, sw, sh) = other.submit;
        let (bx, by, bw, bh) = other.button;
        let inside_panel = |x: i32, y: i32, w: i32, h: i32| {
            x >= other.panel_x
                && x + w <= other.panel_x + other.panel_width
                && y >= other.panel_y
                && y + h <= other.panel_y + other.panel_height
        };
        if !inside_panel(fx, fy, fw, fh)
            || !inside_panel(sx, sy, sw, sh)
            || !inside_panel(bx, by, bw, bh)
        {
            every_count_holds = false;
        }
        if fy + fh > by {
            // The field would run into the button.
            every_count_holds = false;
        }
        if other.hit(fx + fw / 2, fy + fh / 2) != Target::Password {
            every_count_holds = false;
        }
        if other.hit(sx + sw / 2, sy + sh / 2) != Target::Submit {
            every_count_holds = false;
        }
        if other.hit(bx + bw / 2, by + bh / 2) != Target::Accessibility {
            every_count_holds = false;
        }
        for index in 0..accounts {
            let (x, y, w, h) = other.row(index);
            if !inside_panel(x, y, w, h) || other.hit(x + w / 2, y + h / 2) != Target::Account(index)
            {
                every_count_holds = false;
            }
        }
    }
    report.check(
        "la geometrie tient de 1 a 8 comptes",
        every_count_holds,
    );

    // A panel with no account at all: the list is empty, the field and
    // the button must still answer. `account_names` cannot return nothing
    // today, but a filter that removes ROOT and the services can, and a
    // login screen that swallows its own clicks would be unusable with no
    // way to say why.
    // --- the cursor shape each target asks for ---
    //
    // This is the rule of the request, transcribed: a different cursor over
    // a button and over an entry. A table nothing checks is a table that
    // drifts -- the 16.4 shortcuts already cost that lesson once.
    report.check(
        "un bouton et une ligne cliquable demandent la main",
        Target::Account(0).cursor() == CursorShape::Hand
            && Target::Submit.cursor() == CursorShape::Hand
            && Target::Accessibility.cursor() == CursorShape::Hand,
    );
    report.check(
        "le champ de saisie demande la barre de texte",
        Target::Password.cursor() == CursorShape::Text,
    );
    report.check(
        "hors de tout element, la fleche",
        Target::Elsewhere.cursor() == CursorShape::Arrow,
    );
    // The point of two shapes is that they differ. Checking it looks silly
    // until the day a refactor maps both to the same variant.
    report.check(
        "les trois formes sont distinctes",
        Target::Submit.cursor() != Target::Password.cursor()
            && Target::Password.cursor() != Target::Elsewhere.cursor()
            && Target::Submit.cursor() != Target::Elsewhere.cursor(),
    );

    let empty = Layout::new(1024, 768, &theme, 0);
    let (ex, ey, ew, eh) = empty.field;
    let (qx, qy, qw, qh) = empty.submit;
    report.check(
        "sans aucun compte, le champ et la fleche repondent encore",
        empty.hit(ex + ew / 2, ey + eh / 2) == Target::Password
            && empty.hit(qx + qw / 2, qy + qh / 2) == Target::Submit,
    );
}

// ============================================================
//  The close cross of a window
// ============================================================

/// The cross must be where it is painted, and reachable.
///
/// It was painted from phase 7 and tested nowhere: `route_pointer` looked
/// only for a drag of the title bar, so the button did nothing and
/// `EV_CLOSE` was never emitted by any line of the system. The event
/// existed, the IDE of phase 9 handled it, and nothing sent it.
///
/// What is checked here is the pair the defect needed: the drawing and the
/// hit test read the **same** rectangle, and the hit area is big enough for
/// a hand -- a missed close lands on the title bar and drags the window,
/// so the window jumps instead of closing.
fn close_cross(report: &mut Report) {
    use super::compositor::{close_button, close_target};

    let theme = super::theme::Theme::dark();
    let title = theme.title_height as i32;
    let border = theme.border_width as i32;
    // A window of 400 by 300 at (100, 80), decorations included.
    let inner = Rect::new(100, 80 + title + border, 400, 300);
    let mut surface = super::surface::Surface::new(
        1,
        7,
        Layer::Window,
        inner,
        String::from("fenetre"),
        0,
    );
    surface.decorated = true;
    let outer = surface.outer_rect(title, border);

    let cross = close_button(outer, &theme);
    let target = close_target(outer, &theme);

    report.check(
        "la zone cliquable contient la croix dessinee",
        target.x <= cross.x
            && target.y <= cross.y
            && target.right() >= cross.right()
            && target.bottom() >= cross.bottom(),
    );
    report.check(
        "la croix tient dans la barre de titre",
        cross.x >= 0 && cross.y >= 0 && cross.bottom() <= title && cross.right() <= outer.width,
    );
    report.check(
        "le centre de la croix est dans la zone cliquable",
        target.contains(cross.x + cross.width / 2, cross.y + cross.height / 2),
    );
    // Big enough to aim at: eight pixels is about the smallest a hand hits
    // reliably, and the drawn cross is nine wide by itself.
    report.check(
        "la zone cliquable fait au moins 14 pixels de cote",
        target.width >= 14 && target.height >= 14,
    );
    // And it must be on the right of the title, not over it: a cross over
    // the title would swallow a drag started on the name of the window.
    report.check(
        "la croix est a droite, loin du titre",
        cross.x > outer.width / 2,
    );

    // The rectangle is local to `outer`: a click is translated by
    // subtracting the origin of `outer`, and getting that wrong by the
    // decoration height is the mistake that would make the cross answer
    // clicks inside the text of the window.
    let click = (
        outer.x + cross.x + cross.width / 2,
        outer.y + cross.y + cross.height / 2,
    );
    report.check(
        "un clic au centre de la croix, en coordonnees ecran, retombe dedans",
        target.contains(click.0 - outer.x, click.1 - outer.y),
    );
    report.check(
        "un clic dans le corps de la fenetre ne touche pas la croix",
        !target.contains(inner.x + 10 - outer.x, inner.y + 10 - outer.y),
    );
}

// ============================================================
//  The text buffer
// ============================================================

/// Runs the self-test of `textbuf` and folds its verdict in here.
///
/// Reported as two numbers rather than transcribed check by check: the rules
/// belong to `textbuf`, and a second copy of them here would be the
/// duplication the module exists to avoid.
fn text_buffer(report: &mut Report) {
    let (passed, failed) = crate::textbuf::selftest();
    report.note(&alloc::format!(
        "textbuf : {} verifications, {} echecs",
        passed,
        failed
    ));
    report.check("le tampon de texte tient ses regles", failed == 0);
    // That the test **ran** is a separate claim from its passing: a
    // `selftest` gutted to `return (0, 0)` reports no failure at all. The
    // bound is on the total, not on the passes, so adding a check never
    // turns this into a tripwire that has to be edited in step.
    report.check(
        "le tampon de texte est reellement eprouve",
        passed + failed >= 15,
    );
}

/// A screen that follows the host: the driver's checks, and where windows
/// go when the screen shrinks.
fn screen_size(report: &mut Report) {
    let (passed, failed) = crate::graphics::virtio_gpu::selftest::run();
    report.note(&alloc::format!(
        "virtio-gpu : {} verifications, {} echecs",
        passed,
        failed
    ));
    report.check("le pilote virtio-gpu tient ses regles", failed == 0);
    report.check("le pilote virtio-gpu est reellement eprouve", passed + failed >= 12);

    use super::compositor::clamp_window;
    let title = 24;
    // A window at the right edge of a 1920 screen, which becomes 1024.
    let moved = clamp_window(Rect::new(1500, 300, 620, 420), 1024, 768, title);
    report.check(
        "une fenetre hors d'un ecran reduit revient entierement",
        moved.x == 1024 - 620 && moved.y == 300 && moved.width == 620 && moved.height == 420,
    );
    // Taller than the new screen: the title bar stays reachable, the
    // bottom is what is cut.
    let tall = clamp_window(Rect::new(10, 400, 300, 900), 1024, 768, title);
    report.check("une fenetre trop haute garde sa barre de titre", tall.y == title);
    let kept = clamp_window(Rect::new(100, 100, 300, 200), 1024, 768, title);
    report.check(
        "une fenetre deja visible ne bouge pas",
        kept == Rect::new(100, 100, 300, 200),
    );
}

/// Drawing commands packed in one message land as if sent one by one.
/// Pixel canvases: only their creator may show them, and they are gone
/// once dropped.
fn canvases(report: &mut Report) {
    use super::canvas::{self, Canvas};

    let pid = crate::scheduler::current_pid();
    let mut canvas = Canvas::new(8, 4);
    report.check("un canevas a la taille demandee", canvas.pixels.len() == 32);
    report.check("son createur le retrouve", canvas::lookup(canvas.id, pid).is_some());
    report.check("une autre tache ne le retrouve pas", canvas::lookup(canvas.id, pid + 1).is_none());
    canvas.pixels.fill(0x00ff00);
    canvas.publish();
    let shared = canvas::lookup(canvas.id, pid);
    report.check(
        "la publication donne l'image au compositeur",
        shared.is_some_and(|shared| shared.0.lock().data.iter().all(|pixel| *pixel == 0x00ff00)),
    );
    let id = canvas.id;
    drop(canvas);
    report.check("un canevas lache disparait du registre", canvas::lookup(id, pid).is_none());
}

/// The task bar reads the windows with their titles and focus.
fn window_titles(report: &mut Report) {
    let names = [
        proto::op_name(proto::OP_WINDOW_ENTRIES),
        proto::op_name(proto::OP_SURFACE_TITLE),
        proto::op_name(proto::EV_WINDOWS_CHANGED),
        proto::op_name(proto::OP_DRAW_CANVAS),
    ];
    report.check("les nouvelles operations ont un nom", names.iter().all(|name| *name != "?"));
}

fn draw_batches(report: &mut Report) {
    if !super::is_active() {
        report.note("compositeur inactif - lots de dessin non eprouves");
        return;
    }
    let pid = crate::scheduler::current_pid();
    let create = Writer::new()
        .u16(10)
        .u16(10)
        .u16(50)
        .u16(40)
        .u8(Layer::Window as u8)
        .text("autotest-lots")
        .build();
    let id = super::with(|comp| super::compositor::handle(comp, pid, proto::OP_SURFACE_CREATE, &create))
        .flatten()
        .and_then(|reply| {
            let mut reader = Reader::new(&reply.payload);
            if reader.u8()? != proto::ERR_NONE {
                return None;
            }
            reader.u32()
        });
    let Some(id) = id else {
        report.check("la surface d'essai des lots se cree", false);
        return;
    };

    // Two rectangles, in the entry form: operation, length, body without
    // the surface identifier.
    let rect = Writer::new().i16(1).i16(2).u16(3).u16(4).u32(0x00FF_0000).build();
    let mut batch = Writer::new().u32(id).build();
    for _ in 0..2 {
        batch.push(proto::OP_DRAW_RECT as u8);
        batch.push(rect.len() as u8);
        batch.extend_from_slice(&rect);
    }
    let mut smuggled = batch.clone();
    smuggled.push(proto::OP_COMMIT as u8);
    smuggled.push(0);

    let outcome = super::with(|comp| {
        let handle = |comp: &mut super::compositor::Compositor, sender, op, payload: &[u8]| {
            super::compositor::handle(comp, sender, op, payload)
                .and_then(|reply| reply.payload.first().copied())
        };
        let id_only = Writer::new().u32(id).build();
        let _ = handle(comp, pid, proto::OP_DRAW_RESET, &id_only);
        let accepted = handle(comp, pid, proto::OP_DRAW_BATCH, &batch);
        let _ = handle(comp, pid, proto::OP_COMMIT, &id_only);
        let published = comp.stack.get(id).map(|surface| surface.display_list().len());
        let refused = handle(comp, pid, proto::OP_DRAW_BATCH, &smuggled);
        let foreign = handle(comp, pid + 7777, proto::OP_DRAW_BATCH, &batch);
        comp.stack.destroy(id);
        comp.damage_all();
        (accepted, published, refused, foreign)
    });
    let Some((accepted, published, refused, foreign)) = outcome else {
        return;
    };
    report.check(
        "un lot de deux rectangles donne deux commandes publiees",
        accepted == Some(proto::ERR_NONE) && published == Some(2),
    );
    report.check(
        "un COMMIT glisse dans un lot est refuse",
        refused == Some(proto::ERR_BAD_REQUEST),
    );
    report.check(
        "un lot vers la surface d'un autre est refuse",
        foreign == Some(proto::ERR_NOT_OWNER),
    );
}

/// The tabs of `parametres.ss`: the contract every `.ss` provider keeps.
fn settings_tabs(report: &mut Report) {
    let (passed, failed) = crate::config::selftest::run();
    report.note(&alloc::format!(
        "onglets de parametres : {} verifications, {} echecs",
        passed,
        failed
    ));
    report.check("les onglets de parametres tiennent leur contrat", failed == 0);
    report.check("les onglets de parametres sont reellement eprouves", passed + failed >= 12);
}

/// The explorer's own checks, counted as one line like the text buffer.
fn file_explorer(report: &mut Report) {
    let (passed, failed) = crate::apps::explorer::selftest::run();
    report.note(&alloc::format!(
        "explorateur : {} verifications, {} echecs",
        passed,
        failed
    ));
    report.check("l'explorateur de fichiers tient ses regles", failed == 0);
    // Same bound on the total as `text_buffer`: a `run` emptied to
    // `(0, 0)` must not pass.
    report.check(
        "l'explorateur de fichiers est reellement eprouve",
        passed + failed >= 40,
    );
}

// ============================================================
//  The shared hover state of the modules
// ============================================================

/// `Hover::set` must answer "changed" and not "is set".
///
/// The whole cost argument rests on that return value: repainting on the
/// pointer's **position** costs a score of IPC messages per millimetre of
/// mouse, repainting on the **boundary** costs one per element entered. A
/// `set` that answered `true` every time would silently restore the cost
/// the task bar refused a hover effect over for two phases, and nothing on
/// screen would look different.
fn hover_state(report: &mut Report) {
    use super::modules::Hover;

    let hover = Hover::new();
    report.check(
        "au depart, rien n'est survole",
        hover.get() == Hover::NONE && !hover.is(0),
    );
    report.check("entrer dans un element est un changement", hover.set(3));
    report.check(
        "y rester n'en est pas un",
        !hover.set(3) && !hover.set(3) && hover.is(3),
    );
    report.check("passer a un autre en est un", hover.set(4) && hover.is(4));
    report.check(
        "en sortir en est un, et une seule fois",
        hover.clear() && !hover.clear() && hover.get() == Hover::NONE,
    );
    // `NONE` must not pass for an element: a bar with no zone under the
    // pointer would otherwise highlight the zone of rank -1.
    report.check(
        "l'absence de survol ne designe aucun element",
        !hover.is(Hover::NONE),
    );
}

// ============================================================
//  Colour blending (hover shades)
// ============================================================

/// `mix` is integer arithmetic, and that is the whole point.
///
/// No `f32` or `f64` anywhere in the kernel while SSE is off in
/// `CR0`/`CR4`. What must hold: the ends are exact, the middle stays
/// between them, and no component wraps.
fn blending(report: &mut Report) {
    use crate::graphics::framebuffer::{blue_of, green_of, mix, red_of, rgb};

    let black = rgb(0, 0, 0);
    let white = rgb(0xFF, 0xFF, 0xFF);

    report.check(
        "un poids nul rend la premiere couleur, un poids plein la seconde",
        mix(black, white, 0) == black && mix(black, white, 255) == white,
    );
    let half = mix(black, white, 128);
    report.check(
        "le milieu est entre les deux, sur les trois composantes",
        red_of(half) > 0x70
            && red_of(half) < 0x90
            && red_of(half) == green_of(half)
            && green_of(half) == blue_of(half),
    );

    // Each component must stay in its own eight bits: an overflow would
    // spill into the next one, and a hover shade would come out a
    // completely different hue rather than a lighter one.
    let mut no_spill = true;
    let a = rgb(0xFF, 0x00, 0xFF);
    let b = rgb(0x00, 0xFF, 0x00);
    for weight in 0..=255u8 {
        let blended = mix(a, b, weight);
        if blended > 0x00FF_FFFF {
            no_spill = false;
        }
        // Green climbs, red and blue fall, and none of them turns back.
        let expected_green = (0xFFu32 * weight as u32) / 255;
        if green_of(blended) as u32 != expected_green {
            no_spill = false;
        }
    }
    report.check("aucune composante ne deborde sur la voisine", no_spill);

    // A hover shade must be visibly different from what it lightens, and
    // visibly different from the colour it must not be mistaken for. This
    // is the check that would have caught a weight left at zero.
    let theme = super::theme::Theme::dark();
    let hovered = mix(theme.surface, theme.surface_alt, 85);
    report.check(
        "la teinte de survol se distingue du fond et de la selection",
        hovered != theme.surface && hovered != theme.surface_alt,
    );
}

// ============================================================
//  7.4.2 and 16.4 -- Global shortcuts
// ============================================================

/// Transcription of the 16.4 table.
///
/// This check exists because the first version had none: `route_key`
/// promised in a comment to intercept the global shortcuts and
/// intercepted nothing. A written promise nothing checks is worse than a
/// missing feature -- it reads as done.
fn shortcuts(report: &mut Report) {
    use super::compositor::{is_global_shortcut, SCAN_SUPER_LEFT, SCAN_SUPER_RIGHT};

    fn key(scancode: u8, modifiers: u8) -> KeyEvent {
        KeyEvent {
            scancode,
            pressed: true,
            modifiers,
            codepoint: 0,
        }
    }

    const ALT: u8 = KeyEvent::MOD_ALT;
    const SUPER: u8 = KeyEvent::MOD_SUPER;
    const TAB: u8 = 0x0F;
    const F4: u8 = 0x3E;
    const F6: u8 = 0x40;

    report.check(
        "16.4 : Super seul est global",
        is_global_shortcut(&key(SCAN_SUPER_LEFT, 0))
            && is_global_shortcut(&key(SCAN_SUPER_RIGHT, 0)),
    );
    report.check(
        "16.4 : Super+Tab est global, Tab seul ne l'est pas",
        is_global_shortcut(&key(TAB, SUPER)) && !is_global_shortcut(&key(TAB, 0)),
    );
    report.check(
        "16.4 : Alt+F4 est global, F4 seul ne l'est pas",
        is_global_shortcut(&key(F4, ALT)) && !is_global_shortcut(&key(F4, 0)),
    );
    report.check("16.4 : F6 est global", is_global_shortcut(&key(F6, 0)));

    // The four accessibility toggles: s z c k on AZERTY.
    let accessibility = [0x1Fu8, 0x11, 0x2E, 0x25];
    report.check(
        "16.4 : les quatre bascules d'accessibilite sont globales",
        accessibility
            .iter()
            .all(|code| is_global_shortcut(&key(*code, ALT | SUPER))),
    );
    report.check(
        "16.4 : ces memes touches sans les deux modificateurs restent locales",
        accessibility
            .iter()
            .all(|code| !is_global_shortcut(&key(*code, ALT)) && !is_global_shortcut(&key(*code, 0))),
    );
    report.check(
        "une frappe ordinaire n'est pas un raccourci global",
        !is_global_shortcut(&key(0x10, 0)) && !is_global_shortcut(&key(0x1E, ALT)),
    );
}

// ============================================================
//  7.4.2 -- Real routing of the events
// ============================================================

/// Checks that the events arrive where they must arrive.
///
/// ## What this test would have caught
///
/// Two defects lived in the routing, and neither showed in a log:
/// journal :
///
/// * a pointer event went only to the surface named by `Stack::hit`,
///   which **excludes the overlay layer** -- the cursor module therefore
///   never received anything, and the mouse moved in the state of the
///   compositor while staying frozen on screen;
/// * a keystroke went only to the focused surface, so that the global
///   shortcuts never reached the shell modules.
fn routing(report: &mut Report) {
    use super::compositor::{route_key, route_pointer, SCAN_SUPER_LEFT};
    use super::surface::Layer;

    if !super::is_active() {
        report.note("compositeur inactif - routage non exerce");
        return;
    }

    let overlay = super::with(|comp| comp.stack.overlay_owner()).flatten();
    let shell = super::with(|comp| comp.stack.owners_in(Layer::Shell)).unwrap_or_default();

    if overlay.is_none() || shell.is_empty() {
        report.note("modules du shell absents - routage non exerce");
        return;
    }

    // --- pointer ---
    let event = PointerEvent {
        buttons: 0,
        x: 300,
        y: 300,
        dx: 1,
        dy: 1,
        shape: 0,
        wheel: 0,
    };
    let routed = super::with(|comp| route_pointer(comp, event)).unwrap_or_default();

    report.check(
        "un evenement de pointeur atteint la surimpression",
        routed.iter().any(|target| Some(target.to) == overlay),
    );
    report.check(
        "il atteint aussi la surface designee",
        routed.len() >= 2 || routed.iter().any(|target| Some(target.to) != overlay),
    );
    report.check(
        "aucune destination n'est servie deux fois",
        routed
            .iter()
            .enumerate()
            .all(|(index, target)| {
                !routed[..index].iter().any(|other| other.to == target.to)
            }),
    );
    report.check(
        "la position du pointeur suit l'evenement",
        super::with(|comp| (comp.pointer_x, comp.pointer_y)) == Some((300, 300)),
    );

    // --- global shortcut ---
    let press = KeyEvent {
        scancode: SCAN_SUPER_LEFT,
        pressed: true,
        modifiers: 0,
        codepoint: 0,
    };
    let routed = super::with(|comp| route_key(comp, press)).unwrap_or_default();
    report.check(
        "un raccourci global atteint tous les modules du shell",
        !routed.is_empty() && shell.iter().all(|pid| routed.iter().any(|t| t.to == *pid)),
    );

    // The Super modifier must appear in what is passed on: it is the
    // compositor that lays it down, the driver does not know it.
    let carried = routed
        .first()
        .and_then(|target| KeyEvent::decode(&target.payload))
        .map(|event| event.has(KeyEvent::MOD_SUPER))
        .unwrap_or(false);
    report.check("le compositeur injecte le modificateur Super", carried);

    // While Super is held, Tab becomes a global shortcut.
    let tab = KeyEvent {
        scancode: 0x0F,
        pressed: true,
        modifiers: 0,
        codepoint: 0,
    };
    let with_super = super::with(|comp| route_key(comp, tab)).unwrap_or_default();
    report.check(
        "Super maintenue transforme Tab en raccourci global",
        with_super.len() == shell.len(),
    );

    // Release: the state must fall back, otherwise Super would stay armed
    // forever and every keystroke would become a shortcut.
    let release = KeyEvent {
        scancode: SCAN_SUPER_LEFT,
        pressed: false,
        modifiers: 0,
        codepoint: 0,
    };
    super::with(|comp| route_key(comp, release));
    let after = super::with(|comp| route_key(comp, tab)).unwrap_or_default();
    report.check(
        "Super relachee rend Tab a la surface focalisee",
        after.len() <= 1,
    );
}

// ============================================================
//  7.2.2 -- The damaged rectangle really bounds the painting
// ============================================================

/// A frame must write nothing outside the area it publishes.
///
/// ## The defect this test locks down
///
/// The render replayed each surface with the clip of **the whole
/// screen**, and not that of the damaged area. A surface touching the
/// area -- the wallpaper, which always touches it -- therefore repainted
/// the whole back buffer, including over the higher surfaces which, for
/// their part, were ignored for want of intersecting the area.
///
/// The back buffer went out of sync with the front buffer. The symptom
/// observed: the mouse cursor, erased on every clock tick of the task bar
/// then redrawn in pieces, appeared four rows short. Nothing in the log
/// said so, and a full repaint did not fix it -- it is the kind of defect
/// one only finds by comparing pixels.
fn damage_containment(report: &mut Report) {
    use crate::graphics::framebuffer::rgb;
    use super::protocol::Writer;
    use super::surface::Layer;

    if !super::is_active() || !crate::graphics::is_active() {
        report.note("compositeur inactif - confinement non exerce");
        return;
    }

    let pid = crate::scheduler::current_pid();
    let below = rgb(0x80, 0x00, 0x00);
    let above = rgb(0x00, 0xFF, 0x00);

    // The test builds **both** its surfaces rather than relying on the
    // modules: their display list depends on their scheduling, and a
    // self-test depending on what another had time to do proves nothing
    // reproducibly.
    //
    // The layers are chosen **above everything that exists** at the
    // moment the self-test runs -- the splash occupies `Modal` and covers
    // the whole screen. A first version placed the witness in
    // `Notification` and only measured the colour of the splash.
    let probe = Rect::new(64, 64, 48, 48);
    // Wide enough to cover the witness **and** the damaged area, without
    // covering the whole screen: the task bar stays visible.
    let backdrop = Rect::new(0, 0, 700, 700);

    let create = |layer: Layer, rect: Rect, title: &str| -> Option<u32> {
        let payload = Writer::new()
            .u16(rect.x as u16)
            .u16(rect.y as u16)
            .u16(rect.width as u16)
            .u16(rect.height as u16)
            .u8(layer as u8)
            .text(title)
            .build();
        super::with(|comp| {
            super::compositor::handle(comp, pid, proto::OP_SURFACE_CREATE, &payload)
        })
        .flatten()
        .and_then(|reply| {
            let mut reader = Reader::new(&reply.payload);
            if reader.u8()? != proto::ERR_NONE {
                return None;
            }
            reader.u32()
        })
    };

    let paint = |id: u32, color: u32| {
        super::with(|comp| {
            let _ = super::compositor::handle(
                comp,
                pid,
                proto::OP_DRAW_FILL,
                &Writer::new().u32(id).u32(color).build(),
            );
            let _ = super::compositor::handle(
                comp,
                pid,
                proto::OP_COMMIT,
                &Writer::new().u32(id).build(),
            );
        });
    };

    let (Some(low), Some(high)) = (
        create(Layer::Modal, backdrop, "fond-temoin"),
        create(Layer::Overlay, probe, "temoin"),
    ) else {
        report.note("surfaces temoins refusees - confinement non exerce");
        return;
    };
    paint(low, below);
    paint(high, above);

    super::with(|comp| comp.damage_all());
    super::with(super::compositor::render);

    let painted = crate::graphics::with_surface(|fb| fb.read_pixel(80, 80)).flatten();
    report.check(
        "la surface haute recouvre la surface basse",
        painted == Some(above),
    );

    // The heart of the test. The damaged area does not touch the high
    // surface, but it touches the low one -- which covers the whole
    // screen. If the render replays the low surface without bounding it
    // to the area, it repaints over the high one, which is not redrawn
    // since it does not intersect the area.
    super::with(|comp| comp.damage_rect(Rect::new(600, 600, 40, 40)));
    super::with(super::compositor::render);

    let after = crate::graphics::with_surface(|fb| fb.read_pixel(80, 80)).flatten();
    report.check(
        "une zone endommagee lointaine ne repeint pas le reste",
        after == Some(above),
    );

    // And the area aimed at must indeed have been painted: a render doing
    // nothing at all would pass the previous check proving nothing.
    let inside = crate::graphics::with_surface(|fb| fb.read_pixel(620, 620)).flatten();
    report.check("la zone endommagee est bien peinte", inside == Some(below));

    for id in [high, low] {
        super::with(|comp| {
            let _ = super::compositor::handle(
                comp,
                pid,
                proto::OP_SURFACE_DESTROY,
                &Writer::new().u32(id).build(),
            );
        });
    }
    super::with(|comp| comp.damage_all());
    super::with(super::compositor::render);
    let cleared = crate::graphics::with_surface(|fb| fb.read_pixel(80, 80)).flatten();
    report.check(
        "les surfaces temoins ont disparu de l'ecran",
        cleared != Some(above) && cleared != Some(below),
    );
}

// ============================================================
//  7.3.4 and 7.5.1 -- Pause and priority, on the real loader
// ============================================================

/// Name of the test module, in a slot the desktop does not depend on.
const PROBE_ID: &str = "os.nova.probe";
const PROBE_SYSTEM: &str = "/sys/gui/modules/accessibility/probe.mod";
const PROBE_PAUSED: &str = "probe.pause";
const PROBE_USER_DIR: &str = "/users/nova/settings/gui/modules";

/// Exercises the life cycle on files really laid on the disk.
///
/// ## Why this test module rather than a native one
///
/// Pausing `wallpaper.mod` to check that the pause works would switch off
/// the wallpaper of the user during the test. The probe module occupies
/// the `Diagnostic` slot, which no native claims: the loader treats it
/// exactly like the others -- same sweep, same signature, same priority
/// rule -- with nothing visible depending on it.
///
/// It used to occupy `Accessibility`, for the same reason. Step 10.1 gave
/// that slot a real occupant -- `accessibility.mod` of 16.2 -- so the
/// probe moved to a slot reserved for it. Had it stayed, the two would
/// have contended for one slot and the failure would have looked like a
/// loader bug.
fn lifecycle(report: &mut Report) {
    use crate::fs::perm::{Credentials, Mode};

    if !crate::fs::is_ready() || !super::is_active() {
        report.note("pas de volume ou pas de compositeur - 7.3.4 non exerce");
        return;
    }
    let who = Credentials::kernel();
    let now = crate::idt::ticks();

    let file = module::build(&module::BuildSpec {
        slot: Slot::Diagnostic,
        signature: SignatureType::SelfSigned,
        app_id: PROBE_ID,
        display_name: "Sonde",
        author: "autotest",
        version: [1, 0, 0, 0],
        extra_flags: module::flags::ACCESSIBILITY,
        payload: b"sonde de cycle de vie",
    });

    // An interrupted earlier pass may have left one name or the other.
    let paused_path = alloc::format!("/sys/gui/modules/accessibility/{}", PROBE_PAUSED);
    let _ = crate::fs::delete(PROBE_SYSTEM, who, now);
    let _ = crate::fs::delete(&paused_path, who, now);

    if crate::fs::write_file(PROBE_SYSTEM, &file, who, Mode::system_file(), now).is_err() {
        report.note("ecriture du module de sonde impossible - 7.3.4 non exerce");
        return;
    }

    super::rescan_modules(None);
    report.check(
        "un .mod depose est detecte au balayage",
        state_of(PROBE_ID) == Some(module::State::Active),
    );
    report.check(
        "il occupe son emplacement",
        super::module_in(Slot::Diagnostic)
            .map(|module| module.app_id == PROBE_ID)
            .unwrap_or(false),
    );

    // --- 7.3.4: .mod -> .pause ---
    let renamed = crate::fs::rename(PROBE_SYSTEM, PROBE_PAUSED, who, now).is_ok();
    report.check("le module se renomme en .pause", renamed);
    super::rescan_modules(None);
    report.check(
        "un .pause passe le module en pause",
        state_of(PROBE_ID) == Some(module::State::Paused),
    );
    report.check(
        "l'emplacement est libere",
        super::module_in(Slot::Diagnostic).is_none(),
    );

    // --- back again ---
    let restored = crate::fs::rename(&paused_path, "probe.mod", who, now).is_ok();
    report.check("le module se renomme en .mod", restored);
    super::rescan_modules(None);
    report.check(
        "le module revient en service",
        state_of(PROBE_ID) == Some(module::State::Active),
    );

    // --- 7.5.1: priority of the user module ---
    let user_file = module::build(&module::BuildSpec {
        slot: Slot::Diagnostic,
        signature: SignatureType::SelfSigned,
        app_id: "os.nova.probe.user",
        display_name: "Sonde utilisateur",
        author: "autotest",
        version: [2, 0, 0, 0],
        extra_flags: module::flags::ACCESSIBILITY,
        payload: b"sonde utilisateur",
    });
    let user_path = alloc::format!("{}/probe.mod", PROBE_USER_DIR);
    let _ = crate::fs::delete(&user_path, who, now);

    if crate::fs::write_file(&user_path, &user_file, who, Mode::system_file(), now).is_ok() {
        super::rescan_modules(Some("nova"));

        report.check(
            "8.4 : le module utilisateur prend l'emplacement",
            super::module_in(Slot::Diagnostic)
                .map(|module| {
                    module.app_id == "os.nova.probe.user"
                        && module.origin == module::Origin::User
                })
                .unwrap_or(false),
        );
        // The system module is not erased from the diagnostics: it is
        // shadowed. Hiding it would make the priority invisible to
        // whoever asks why their system module no longer applies.
        report.check(
            "le module systeme est masque, pas oublie",
            state_of(PROBE_ID) == Some(module::State::Shadowed),
        );

        // With no account, the priority no longer applies: system wins.
        super::rescan_modules(None);
        report.check(
            "hors session, le module systeme reprend l'emplacement",
            state_of(PROBE_ID) == Some(module::State::Active),
        );

        let _ = crate::fs::delete(&user_path, who, now);
    } else {
        report.note("dossier de modules utilisateur absent - 7.5.1 non exerce");
    }

    // --- refusal of an altered file, on the real path ---
    let mut corrupt = file.clone();
    let last = corrupt.len() - 1;
    corrupt[last] ^= 0xFF;
    let corrupt_path = "/sys/gui/modules/accessibility/corrompu.mod";
    let _ = crate::fs::delete(corrupt_path, who, now);
    if crate::fs::write_file(corrupt_path, &corrupt, who, Mode::system_file(), now).is_ok() {
        super::rescan_modules(None);
        let rejected = super::loaded_modules().into_iter().any(|module| {
            module.path == corrupt_path
                && module.state == module::State::Rejected
                && module.error == Some(module::ModuleError::DigestMismatch)
        });
        report.check("un .mod altere est refuse au balayage", rejected);
        let _ = crate::fs::delete(corrupt_path, who, now);
    }

    // Cleanup: the self-test leaves nothing on a persistent volume.
    let _ = crate::fs::delete(PROBE_SYSTEM, who, now);
    let _ = crate::fs::delete(&paused_path, who, now);
    super::rescan_modules(None);
    report.check(
        "l'autotest ne laisse aucun module derriere lui",
        state_of(PROBE_ID).is_none() && super::module_in(Slot::Diagnostic).is_none(),
    );
}

fn state_of(app_id: &str) -> Option<module::State> {
    super::loaded_modules()
        .into_iter()
        .find(|module| module.app_id == app_id)
        .map(|module| module.state)
}

// ============================================================
//  Live compositor
// ============================================================

/// Exercises the really started compositor, if it is.
///
/// The preceding tests bear on structures built for them. This one bears
/// on the instance that paints the screen: it is the only one that can
/// say the protocol really goes through.
// ============================================================
//  Session and power buttons
// ============================================================

fn session_controls(report: &mut Report) {
    use super::protocol::SessionAction;
    use crate::acpi::find_s5;

    // --- the wire codes ---
    report.check(
        "les quatre actions de session se relisent par leur code",
        SessionAction::ALL
            .iter()
            .all(|action| SessionAction::from_code(action.as_code()) == Some(*action)),
    );
    // An unknown code must be refused, not mapped to some action: the
    // nearest neighbour of "lock" must not be "power off".
    report.check(
        "un code d'action inconnu est refuse",
        SessionAction::from_code(4).is_none() && SessionAction::from_code(0xFF).is_none(),
    );

    // --- \_S5 in AML, the three encodings of a constant ---
    // Name(_S5, Package(4) {5, 5, 0, 0}) with BytePrefix, as many
    // firmwares write it.
    let byte_prefix = [
        0x10, 0x08, b'_', b'S', b'5', b'_', 0x12, 0x08, 0x04, 0x0A, 0x05, 0x0A, 0x05, 0x00,
        0x00,
    ];
    report.check(
        "\\_S5 en BytePrefix se lit 5/5",
        find_s5(&byte_prefix) == Some((5, 5)),
    );
    // QEMU's form: ZeroOp constants, root-prefixed name.
    let zero_ops = [
        0x08, b'\\', b'_', b'S', b'5', b'_', 0x12, 0x06, 0x04, 0x00, 0x00, 0x00, 0x00,
    ];
    report.check(
        "\\_S5 en ZeroOp avec prefixe racine se lit 0/0",
        find_s5(&zero_ops) == Some((0, 0)),
    );
    // A reference first, then the real Name: the search must move past
    // the first occurrence rather than give up on it.
    let reference_first = [
        0x70, b'_', b'S', b'5', b'_', 0x60, 0x08, b'_', b'S', b'5', b'_', 0x12, 0x06, 0x02,
        0x01, 0x0A, 0x07,
    ];
    report.check(
        "une reference a _S5_ avant sa declaration ne trompe pas la recherche",
        find_s5(&reference_first) == Some((1, 7)),
    );
    // A package built by a method is not a constant: refused, not guessed.
    let computed = [
        0x08, b'_', b'S', b'5', b'_', 0x12, 0x06, 0x02, 0x72, 0x60, 0x61, 0x62,
    ];
    report.check(
        "un \\_S5 calcule est refuse plutot que devine",
        find_s5(&computed).is_none(),
    );
    report.check(
        "un \\_S5 tronque est refuse sans lecture hors borne",
        find_s5(&byte_prefix[..11]).is_none(),
    );

    // --- the task bar geometry, at the fallback and at the requested
    // resolution ---
    use super::modules::taskbar::{launcher_rect, session_rects};
    let theme = super::theme::Theme::default();
    let padding = theme.padding as i32;
    let height = theme.taskbar_height as i32;
    for width in [640, 1024] {
        let buttons = session_rects(width, height, padding);
        let launcher = launcher_rect(height, padding);
        let apart = buttons
            .windows(2)
            .all(|pair| pair[0].intersect(&pair[1]).is_empty() && pair[0].right() < pair[1].x);
        let inside = buttons
            .iter()
            .all(|rect| rect.x >= 0 && rect.right() <= width && !rect.is_empty());
        report.check(
            &alloc::format!("les boutons de session tiennent dans la barre en {}", width),
            apart && inside && buttons[0].x > launcher.right(),
        );
    }

    // --- the compositor refuses a session action from outside the shell ---
    if !super::is_active() {
        report.note("compositeur inactif - refus d'OP_SESSION_ACTION non exerce");
        return;
    }
    // This self-test owns no surface of `Layer::Shell`, which is exactly
    // the position of an ordinary application. The request is a power
    // off: were the check missing, the machine would stop here, which is
    // a loud enough failure.
    let pid = crate::scheduler::current_pid();
    let outcome = super::with(|comp| {
        let before = comp.session_request;
        let reply = super::compositor::handle(
            comp,
            pid,
            proto::OP_SESSION_ACTION,
            &[SessionAction::PowerOff.as_code()],
        );
        let malformed =
            super::compositor::handle(comp, pid, proto::OP_SESSION_ACTION, &[9]);
        let after = comp.session_request;
        (
            reply.map(|r| r.payload.first().copied()),
            malformed.map(|r| r.payload.first().copied()),
            before.is_none() && after.is_none(),
        )
    });
    report.check(
        "une action de session hors du shell est refusee",
        matches!(outcome, Some((Some(Some(proto::ERR_DENIED)), _, true))),
    );
    report.check(
        "une action de session inconnue est une requete malformee",
        matches!(outcome, Some((_, Some(Some(proto::ERR_BAD_REQUEST)), _))),
    );

    let power = crate::power::info();
    report.note(&alloc::format!(
        "extinction ACPI {} - registre de reinitialisation {}",
        if power.can_power_off() {
            "disponible"
        } else {
            "indisponible"
        },
        if power.reset.is_some() {
            "declare"
        } else {
            "absent (repli 0xCF9)"
        }
    ));
}

fn live_compositor(report: &mut Report) {
    if !super::is_active() {
        report.note("compositeur inactif - 7.2 non exerce en vivant");
        return;
    }

    let pid = crate::scheduler::current_pid();

    // Creation through the protocol, as an application would do.
    let payload = Writer::new()
        .u16(40)
        .u16(40)
        .u16(200)
        .u16(120)
        .u8(Layer::Window as u8)
        .text("autotest")
        .build();

    let created = super::with(|comp| {
        super::compositor::handle(comp, pid, proto::OP_SURFACE_CREATE, &payload)
    })
    .flatten();

    let id = created.and_then(|reply| {
        let mut reader = Reader::new(&reply.payload);
        if reader.u8()? != proto::ERR_NONE {
            return None;
        }
        reader.u32()
    });

    report.check("une surface se cree par le protocole", id.is_some());
    let Some(id) = id else {
        return;
    };

    // The owner check: another task must not be able to touch this
    // surface.
    let stolen = super::with(|comp| {
        super::compositor::handle(
            comp,
            pid + 9999,
            proto::OP_SURFACE_MOVE,
            &Writer::new().u32(id).u16(0).u16(0).build(),
        )
    })
    .flatten();
    report.check(
        "une surface n'est pas deplacable par un autre processus",
        stolen.map(|reply| reply.payload.first().copied()) == Some(Some(proto::ERR_NOT_OWNER)),
    );

    let unknown = super::with(|comp| {
        super::compositor::handle(
            comp,
            pid,
            proto::OP_SURFACE_MOVE,
            &Writer::new().u32(0xFFFF).u16(0).u16(0).build(),
        )
    })
    .flatten();
    report.check(
        "une surface inconnue est signalee comme telle",
        unknown.map(|reply| reply.payload.first().copied()) == Some(Some(proto::ERR_NO_SURFACE)),
    );

    let malformed = super::with(|comp| {
        super::compositor::handle(comp, pid, proto::OP_SURFACE_CREATE, &[0x01])
    })
    .flatten();
    report.check(
        "une requete tronquee est refusee",
        malformed.map(|reply| reply.payload.first().copied()) == Some(Some(proto::ERR_BAD_REQUEST)),
    );

    // A drawing command, a commit, and the damaged area must cover the
    // surface. The damage is read under the same lock as the commit: the
    // compositor's task, on another core, publishes and clears it as
    // soon as the lock is free -- read in a second step, it was gone one
    // boot in ten.
    let damage = super::with(|comp| {
        comp.damage_rect(Rect::EMPTY);
        let _ = super::compositor::handle(
            comp,
            pid,
            proto::OP_DRAW_RECT,
            &Writer::new()
                .u32(id)
                .i16(0)
                .i16(0)
                .u16(50)
                .u16(50)
                .u32(0x00FF_0000)
                .build(),
        );
        let _ = super::compositor::handle(comp, pid, proto::OP_COMMIT, &Writer::new().u32(id).build());
        comp.pending_damage()
    })
    .unwrap_or(Rect::EMPTY);
    report.check(
        "un commit endommage la zone de sa surface",
        !damage.is_empty() && damage.contains(45, 45),
    );

    let commands = super::with(|comp| {
        comp.stack
            .get(id)
            .map(|surface| surface.display_list().len())
            .unwrap_or(0)
    })
    .unwrap_or(0);
    report.check("la commande est arrivee dans la liste publiee", commands == 1);

    // The screen answers SCREEN_INFO with the real resolution.
    let info = super::with(|comp| {
        super::compositor::handle(comp, pid, proto::OP_SCREEN_INFO, &[])
    })
    .flatten();
    let resolution = crate::graphics::resolution();
    if let (Some(reply), Some((width, height))) = (info, resolution) {
        let mut reader = Reader::new(&reply.payload);
        let ok = reader.u8() == Some(proto::ERR_NONE)
            && reader.u16() == Some(width as u16)
            && reader.u16() == Some(height as u16);
        report.check("SCREEN_INFO rend la resolution reelle", ok);
    }

    // Cleanup: the self-test does not leave its window on screen.
    super::with(|comp| {
        let _ = super::compositor::handle(
            comp,
            pid,
            proto::OP_SURFACE_DESTROY,
            &Writer::new().u32(id).build(),
        );
    });
    report.check(
        "la surface de test a ete rendue",
        super::with(|comp| comp.stack.get(id).is_none()) == Some(true),
    );

    // --- modules loaded ---
    let modules = super::loaded_modules();
    report.note(&alloc::format!(
        "{} module(s) au registre, {} actif(s)",
        modules.len(),
        modules
            .iter()
            .filter(|module| module.state == module::State::Active)
            .count()
    ));
    report.check(
        "le noyau du compositeur n'est jamais remplacable",
        !Slot::Core.replaceable(),
    );
}
