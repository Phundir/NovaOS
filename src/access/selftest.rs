//! Phase 10.1 self-test (spec 16).
//!
//! ## What is checked, and why these checks
//!
//! * **Tab is a cycle, and Shift+Tab is its exact inverse.** Checked from
//!   every position, wrap included. A dead end in the focus order traps a
//!   keyboard-only user with no key that gets them out, and an off-by-one
//!   at the wrap is invisible everywhere else.
//! * **Every focusable element is reachable.** Not "most": one element
//!   nobody can reach is one dialog nobody can answer.
//! * **The four toggles fire whatever the state.** Including with
//!   keyboard navigation off, which is exactly the state a person is in
//!   when they need to switch it on. A toggle that required navigation to
//!   already be on would be a lock with the key inside.
//! * **The colour transform measurably increases contrast**, and the
//!   daltonisation measurably increases the *separation* of two colours a
//!   given deficiency confuses. "It looks better" is not checkable; a
//!   luminance difference is.
//! * **The magnifier's inverse map covers its destination.** Every
//!   destination pixel must have a source: a forward map leaves holes as
//!   soon as the factor is not an integer, and the holes are what a
//!   magnifier with a grid through it looks like.
//! * **The synthesiser produces sound, and different sounds for different
//!   text.** A formant synthesiser whose filters are wrong produces
//!   silence or a constant, and both are perfectly plausible buffers. The
//!   check bears on the energy and on the difference.
//! * **The letter-to-sound rules, word by word.** A rule table is
//!   transcription; transcription is verified by comparison.
//!
//! ## It leaves nothing behind
//!
//! The tree is emptied on the way in and on the way out, and the four
//! features are restored to the state they had on entry. A self-test that
//! left the screen reader on would be a self-test that talks.

use alloc::string::String;
use alloc::vec::Vec;

use crate::graphics::framebuffer::rgb;
use crate::gui::protocol::KeyEvent;

use super::contrast::{self, Mode};
use super::keynav::{self, Action};
use super::speech::{self, Phoneme, VoiceSettings};
use super::tree::{Role, Tree};
use super::zoom;
use super::Feature;

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
            crate::serial_println!("[access] ECHEC : {}", label);
        }
    }

    fn equal_text(&mut self, label: &str, produced: &str, expected: &str) {
        if produced == expected {
            self.passed += 1;
            return;
        }
        self.failed += 1;
        crate::serial_println!(
            "[access] ECHEC : {} - produit \"{}\", attendu \"{}\"",
            label,
            produced,
            expected
        );
    }
}

pub fn run() -> (usize, usize) {
    let mut report = Report {
        passed: 0,
        failed: 0,
    };

    // The state on entry, restored on the way out. The colour mode is
    // read from the running system, not rebuilt from code zero: reading
    // `from_code(0)` gave `Mode::None` whatever was configured, so a
    // machine set to a colour-blindness mode came out of its own
    // self-test with the transform silently turned off.
    let before: Vec<(Feature, bool)> = Feature::all()
        .iter()
        .map(|feature| (*feature, super::is_on(*feature)))
        .collect();
    let mode_before = super::contrast_mode();
    // Heard at every boot otherwise: the shortcuts below switch the
    // reader on, and it announces itself.
    super::reader::hush(true);

    accessibility_tree(&mut report);
    navigation(&mut report);
    shortcuts(&mut report);
    colours(&mut report);
    magnifier(&mut report);
    phonetics(&mut report);
    synthesis(&mut report);
    configuration(&mut report);

    for (feature, state) in before {
        super::set(feature, state);
    }
    super::set_contrast_mode(mode_before);
    super::reader::hush(false);
    // The global tree is **not** cleared here. Every test above works on
    // a local `Tree` precisely so that the real one survives -- see
    // `sample_tree` -- and emptying it on the way out undid that: the
    // shell's own nodes went with it, and Tab found nothing to walk for
    // the rest of the session.

    (report.passed, report.failed)
}

// ============================================================
//  16.1 -- The tree
// ============================================================

/// Builds a tree with a known shape, on a local instance.
///
/// Local and not the global one: the global tree carries the real nodes
/// of the shell, and a test that emptied it would leave the machine with
/// no accessibility tree until something declared its elements again.
fn sample_tree() -> Tree {
    let mut tree = Tree::new();
    let window = tree.add(0, Role::Window, "Fenetre principale", 1).unwrap();
    let left = tree.add(window, Role::Panel, "Panneau gauche", 1).unwrap();
    tree.add(left, Role::Text, "Titre", 1);
    tree.add(left, Role::Button, "Ouvrir", 1);
    tree.add(left, Role::Field, "Nom", 1);
    let right = tree.add(window, Role::Panel, "Panneau droit", 1).unwrap();
    tree.add(right, Role::CheckBox, "Se souvenir", 1);
    tree.add(right, Role::Button, "Valider", 1);
    tree
}

fn accessibility_tree(report: &mut Report) {
    let mut tree = sample_tree();

    report.check("16.1 : l'arbre contient tous les elements", tree.len() == 8);

    // A window is not focusable, a button is. That is the default of the
    // role, and it is what makes Tab skip the decoration.
    report.check(
        "16.1 : un role decide de la focalisabilite par defaut",
        !Role::Window.focusable_by_default()
            && !Role::Text.focusable_by_default()
            && Role::Button.focusable_by_default()
            && Role::Panel.focusable_by_default(),
    );

    // Reading order is declaration order, depth first. An order by screen
    // position would read a two-column form row by row.
    let order = tree.reading_order();
    let labels: Vec<&str> = order
        .iter()
        .filter_map(|id| tree.get(*id))
        .map(|node| node.label.as_str())
        .collect();
    report.check(
        "16.1 : l'ordre de lecture est l'ordre de declaration, en profondeur",
        labels
            == alloc::vec![
                "Fenetre principale",
                "Panneau gauche",
                "Titre",
                "Ouvrir",
                "Nom",
                "Panneau droit",
                "Se souvenir",
                "Valider",
            ],
    );

    // The announcement puts the role first: it tells the listener what
    // kind of thing is coming, which is what lets them stop listening.
    let button = order
        .iter()
        .find(|id| tree.get(**id).map(|node| node.label == "Ouvrir").unwrap_or(false))
        .copied()
        .unwrap();
    report.equal_text(
        "16.1 : l'annonce commence par le role",
        &tree.get(button).unwrap().announcement(),
        "bouton Ouvrir",
    );
    if let Some(node) = tree.get_mut(button) {
        node.value = String::from("actif");
        node.shortcuts.push(String::from("Ctrl+O"));
    }
    report.equal_text(
        "16.1 : l'annonce porte la valeur et le raccourci",
        &tree.get(button).unwrap().announcement(),
        "bouton Ouvrir, actif, raccourci Ctrl+O",
    );

    // Removing a node takes its subtree: half a subtree leaves survivors
    // that are unreachable and still counted.
    let left = order[1];
    let removed = tree.remove(left);
    report.check(
        "16.1 : supprimer un noeud emporte son sous-arbre",
        removed == 4 && tree.len() == 4,
    );

    // A dead owner takes its nodes with it, or Tab lands on windows that
    // are no longer on screen.
    let mut owned = sample_tree();
    owned.add(0, Role::Window, "Autre application", 2);
    let gone = owned.forget_owner(1);
    report.check(
        "16.1 : les noeuds d'une tache morte disparaissent",
        gone == 8 && owned.len() == 1,
    );

    // The tree is fed from outside, so it is bounded.
    let mut big = Tree::new();
    for index in 0..(super::tree::MAX_NODES + 8) {
        big.add(0, Role::Button, &alloc::format!("b{}", index), 1);
    }
    report.check(
        "16.1 : l'arbre est borne en noeuds",
        big.len() == super::tree::MAX_NODES && big.refused == 8,
    );

    // A parent that does not exist creates nothing: an orphan would be
    // counted and never reached.
    report.check(
        "16.1 : un parent inexistant ne cree pas d'orphelin",
        Tree::new().add(999, Role::Button, "x", 1).is_none(),
    );
}

// ============================================================
//  16.4 -- Navigation
// ============================================================

fn navigation(report: &mut Report) {
    let mut tree = sample_tree();
    let focusables = tree.focus_order();

    // Two panels, one button, one field, one box, one more button: six.
    // Text and the window are not focusable.
    report.check(
        "16.4 : seuls les elements focalisables entrent dans l'ordre",
        focusables.len() == 6,
    );

    // Every focusable element is reachable from the start, in at most as
    // many presses as there are elements.
    let mut seen = Vec::new();
    for _ in 0..focusables.len() {
        if let Some(id) = tree.move_focus(true) {
            seen.push(id);
        }
    }
    report.check(
        "16.4 : tout element focalisable est atteignable",
        focusables.iter().all(|id| seen.contains(id)),
    );

    // Tab wraps: the element after the last is the first. A dead end is a
    // trap with no key out of it.
    let after_last = tree.move_focus(true);
    report.check(
        "16.4 : Tab boucle sur le premier element",
        after_last == Some(focusables[0]),
    );

    // Shift+Tab is the exact inverse, from every position -- wrap
    // included, which is where an off-by-one lives.
    let mut inverse_holds = true;
    for start in focusables.iter() {
        tree.set_focus(*start);
        tree.move_focus(true);
        let back = tree.move_focus(false);
        if back != Some(*start) {
            inverse_holds = false;
        }
    }
    report.check("16.4 : Maj+Tab est l'inverse exact de Tab", inverse_holds);

    // With no focus, Tab starts at the beginning and Shift+Tab at the
    // end. Both are what a person expects to reach.
    let mut fresh = sample_tree();
    report.check(
        "16.4 : sans focus, Tab part du debut",
        fresh.move_focus(true) == Some(focusables[0]),
    );
    let mut fresh_back = sample_tree();
    report.check(
        "16.4 : sans focus, Maj+Tab part de la fin",
        fresh_back.move_focus(false) == Some(focusables[focusables.len() - 1]),
    );

    // F6 leaves the panel one is in, not the first one in the tree.
    let panels: Vec<u32> = tree
        .reading_order()
        .into_iter()
        .filter(|id| tree.get(*id).map(|node| node.role == Role::Panel).unwrap_or(false))
        .collect();
    report.check("16.4 : deux panneaux dans l'arbre d'essai", panels.len() == 2);
    // Focus inside the first panel: F6 must go to the second.
    let inside_first = tree
        .reading_order()
        .into_iter()
        .find(|id| tree.get(*id).map(|node| node.label == "Ouvrir").unwrap_or(false))
        .unwrap();
    tree.set_focus(inside_first);
    report.check(
        "16.4 : F6 quitte le panneau ou l'on est",
        tree.next_panel() == Some(panels[1]),
    );
    report.check(
        "16.4 : F6 boucle entre les panneaux",
        tree.next_panel() == Some(panels[0]),
    );

    // Escape leaves the element, it does not move to the next one.
    tree.set_focus(inside_first);
    let parent = tree.get(inside_first).map(|node| node.parent).unwrap();
    report.check(
        "16.4 : Echap remonte au panneau, il n'avance pas",
        keynav::apply(&mut tree, Action::Cancel) == Some(parent),
    );

    // An empty tree must not move anything, and must not panic.
    let mut empty = Tree::new();
    report.check(
        "16.4 : un arbre vide ne deplace rien",
        empty.move_focus(true).is_none() && empty.next_panel().is_none(),
    );

    // Setting the focus on a non-focusable element is refused: it would
    // be a position Tab could never return to.
    let mut strict = sample_tree();
    let text = strict
        .reading_order()
        .into_iter()
        .find(|id| strict.get(*id).map(|node| node.role == Role::Text).unwrap_or(false))
        .unwrap();
    report.check(
        "16.4 : le focus refuse un element non focalisable",
        !strict.set_focus(text) && strict.focus() != text,
    );
}

// ============================================================
//  16.4 -- The key table
// ============================================================

fn key(scancode: u8, modifiers: u8) -> KeyEvent {
    KeyEvent {
        scancode,
        pressed: true,
        modifiers,
        codepoint: 0,
    }
}

fn shortcuts(report: &mut Report) {
    const ALT_SUPER: u8 = KeyEvent::MOD_ALT | KeyEvent::MOD_SUPER;

    // The four toggles of the 16.4 table, one by one.
    for (scancode, expected) in [
        (keynav::SCAN_S, Action::ToggleReader),
        (keynav::SCAN_Z, Action::ToggleZoom),
        (keynav::SCAN_C, Action::ToggleContrast),
        (keynav::SCAN_K, Action::ToggleKeyboard),
    ] {
        report.check(
            "16.4 : Alt+Super+lettre bascule la fonction attendue",
            keynav::action_for(&key(scancode, ALT_SUPER)) == Some(expected),
        );
        // The same letter without the modifiers is an ordinary key: a
        // navigation module that consumed every `s` would make typing
        // impossible.
        report.check(
            "16.4 : la meme lettre seule reste une lettre",
            keynav::action_for(&key(scancode, 0)).is_none(),
        );
    }

    report.check(
        "16.4 : Tab avance, Maj+Tab recule",
        keynav::action_for(&key(keynav::SCAN_TAB, 0)) == Some(Action::FocusNext)
            && keynav::action_for(&key(keynav::SCAN_TAB, KeyEvent::MOD_SHIFT))
                == Some(Action::FocusPrevious),
    );
    // Super+Tab belongs to the window manager. If it matched the plain
    // Tab row, one key would have two owners.
    report.check(
        "16.4 : Super+Tab appartient au gestionnaire de fenetres",
        keynav::action_for(&key(keynav::SCAN_TAB, KeyEvent::MOD_SUPER)).is_none(),
    );
    report.check(
        "16.4 : Entree et Espace activent",
        keynav::action_for(&key(keynav::SCAN_ENTER, 0)) == Some(Action::Activate)
            && keynav::action_for(&key(keynav::SCAN_SPACE, 0)) == Some(Action::Activate),
    );
    report.check(
        "16.4 : F6 change de panneau",
        keynav::action_for(&key(keynav::SCAN_F6, 0)) == Some(Action::NextPanel),
    );
    // A release is not a shortcut: acting on both edges fires everything
    // twice.
    report.check(
        "16.4 : un relachement ne declenche rien",
        keynav::action_for(&KeyEvent {
            scancode: keynav::SCAN_TAB,
            pressed: false,
            modifiers: 0,
            codepoint: 0,
        })
        .is_none(),
    );

    // In a text field, Space belongs to the field. Everywhere else it
    // activates.
    report.check(
        "16.4 : Espace revient au champ de saisie",
        !keynav::consumes(Action::Activate, Some(Role::Field))
            && keynav::consumes(Action::Activate, Some(Role::Button)),
    );
    // The toggles are never given up, whatever has the focus. That is the
    // whole content of "global" in 16.4.
    report.check(
        "16.4 : les bascules ne sont jamais cedees",
        Action::ToggleReader.is_toggle()
            && keynav::consumes(Action::ToggleReader, Some(Role::Field)),
    );

    // --- the toggles really fire, on the live state ---
    let keyboard_before = super::is_on(Feature::Keyboard);
    super::set(Feature::Keyboard, false);

    // With navigation off, Tab is not consumed -- it belongs to the
    // application.
    report.check(
        "16.4 : sans navigation clavier, Tab revient a l'application",
        !super::handle_key(&key(keynav::SCAN_TAB, 0)),
    );
    // But the toggle fires anyway. This is the case that matters: it is
    // the state a person is in when they need to switch navigation on.
    let reader_before = super::is_on(Feature::Reader);
    report.check(
        "16.4 : la bascule du lecteur fonctionne meme tout eteint",
        super::handle_key(&key(keynav::SCAN_S, ALT_SUPER))
            && super::is_on(Feature::Reader) != reader_before,
    );
    super::set(Feature::Reader, reader_before);

    report.check(
        "16.4 : la bascule du clavier s'active par raccourci",
        super::handle_key(&key(keynav::SCAN_K, ALT_SUPER)) && super::is_on(Feature::Keyboard),
    );
    // And now Tab is consumed.
    report.check(
        "16.4 : navigation active, Tab est consomme",
        super::handle_key(&key(keynav::SCAN_TAB, 0)),
    );

    // Turning contrast on with no mode chosen must change something on
    // screen: a flag that flips and paints nothing reads as a broken
    // shortcut.
    super::set_contrast_mode(Mode::None);
    super::handle_key(&key(keynav::SCAN_C, ALT_SUPER));
    report.check(
        "16.4 : activer le contraste choisit un mode",
        super::is_on(Feature::Contrast) && super::contrast_mode().is_active(),
    );
    super::set(Feature::Contrast, false);
    report.check(
        "16.4 : contraste eteint, aucune transformation",
        !super::contrast_mode().is_active(),
    );

    super::set(Feature::Keyboard, keyboard_before);
}

// ============================================================
//  16.3 -- Colours
// ============================================================

fn colours(report: &mut Report) {
    // The five modes of 16.3 are all there, in the order it lists them.
    let modes = Mode::colour_blind();
    report.check(
        "16.3 : les cinq modes daltonisme sont declares",
        modes
            == [
                Mode::Protanopia,
                Mode::Deuteranopia,
                Mode::Tritanopia,
                Mode::Achromatopsia,
                Mode::Custom,
            ],
    );
    // Every mode round-trips through its code: the code is what the
    // settings file and the atomics carry.
    report.check(
        "16.3 : chaque mode fait l'aller-retour par son code",
        (0..contrast::MODE_COUNT as u8)
            .filter_map(Mode::from_code)
            .all(|mode| mode.code() as usize == Mode::from_code(mode.code()).unwrap().code() as usize),
    );

    let matrix = contrast::Matrix::default();

    // No mode changes nothing. It is the cheap path, and one a mistake
    // here would silently make expensive.
    let colour = rgb(90, 140, 200);
    report.check(
        "16.3 : le mode aucun ne touche rien",
        contrast::transform(Mode::None, &matrix, colour) == colour,
    );

    // High contrast pushes a dark colour darker and a light one lighter.
    // "It looks better" is not checkable; a luminance is.
    let dark = rgb(70, 70, 70);
    let light = rgb(190, 190, 190);
    let dark_after = contrast::transform(Mode::High, &matrix, dark);
    let light_after = contrast::transform(Mode::High, &matrix, light);
    report.check(
        "16.2 : le contraste eleve ecarte les luminances",
        contrast::luminance(dark_after) < contrast::luminance(dark)
            && contrast::luminance(light_after) > contrast::luminance(light),
    );
    // And it keeps the hue: a pure blue must not come back grey.
    let blue_after = contrast::transform(Mode::High, &matrix, rgb(0, 0, 200));
    report.check(
        "16.2 : le contraste eleve conserve la teinte",
        crate::graphics::framebuffer::blue_of(blue_after)
            > crate::graphics::framebuffer::red_of(blue_after),
    );

    // Achromatopsia gives a grey -- the three channels equal -- and
    // stretches it.
    let grey = contrast::transform(Mode::Achromatopsia, &matrix, rgb(200, 40, 40));
    report.check(
        "16.3 : l'achromatopsie rend un gris",
        crate::graphics::framebuffer::red_of(grey)
            == crate::graphics::framebuffer::green_of(grey)
            && crate::graphics::framebuffer::green_of(grey)
                == crate::graphics::framebuffer::blue_of(grey),
    );

    // Daltonisation: two colours a protanope confuses must come out
    // further apart than they went in. This is the check that would fail
    // if the redistribution matrix were symmetric, or shared between the
    // three deficiencies.
    let red = rgb(200, 60, 60);
    let green = rgb(60, 200, 60);
    let separation = |mode: Mode, a: u32, b: u32| -> i32 {
        let a = contrast::transform(mode, &matrix, a);
        let b = contrast::transform(mode, &matrix, b);
        // Distance in the channel a protanope keeps -- blue -- plus the
        // luminance difference. Both are what survives the deficiency.
        (crate::graphics::framebuffer::blue_of(a) as i32
            - crate::graphics::framebuffer::blue_of(b) as i32)
            .abs()
            + (contrast::luminance(a) - contrast::luminance(b)).abs()
    };
    report.check(
        "16.3 : la protanopie ecarte deux couleurs confondues",
        separation(Mode::Protanopia, red, green) > separation(Mode::None, red, green),
    );
    // The tritanopia matrix must not be the same one: it moves the error
    // into red and green, not into blue.
    let blue = rgb(60, 60, 200);
    let tri_before = contrast::transform(Mode::None, &matrix, blue);
    let tri_after = contrast::transform(Mode::Tritanopia, &matrix, blue);
    report.check(
        "16.3 : la tritanopie deplace l'erreur ailleurs que dans le bleu",
        tri_after != tri_before,
    );

    // The custom matrix: identity by default, so a mode nobody configured
    // changes nothing.
    report.check(
        "16.3 : la matrice personnalisee vaut l'identite par defaut",
        contrast::transform(Mode::Custom, &matrix, colour) == colour,
    );
    // A matrix half read is refused: applying three good rows and one of
    // zeros would blank a channel.
    report.check(
        "16.3 : une matrice incomplete est refusee",
        contrast::Matrix::parse("256,0,0,0,256,0").is_none()
            && contrast::Matrix::parse("256,0,0,0,256,0,0,0,256,1").is_none()
            && contrast::Matrix::parse("256,x,0,0,256,0,0,0,256").is_none(),
    );
    let swap = contrast::Matrix::parse("0,0,256,0,256,0,256,0,0").expect("matrice");
    let swapped = contrast::transform(Mode::Custom, &swap, rgb(255, 0, 0));
    report.check(
        "16.3 : une matrice personnalisee s'applique",
        crate::graphics::framebuffer::blue_of(swapped) == 255,
    );

    // Every transform must stay inside the byte range, on every corner of
    // the colour cube. A daltonisation that overflowed would wrap a
    // channel from white to black, which is the most visible bug possible.
    let mut in_range = true;
    for mode in [
        Mode::High,
        Mode::Protanopia,
        Mode::Deuteranopia,
        Mode::Tritanopia,
        Mode::Achromatopsia,
    ] {
        for red in [0u8, 128, 255] {
            for green in [0u8, 128, 255] {
                for blue in [0u8, 128, 255] {
                    let result = contrast::transform(mode, &matrix, rgb(red, green, blue));
                    if result > 0x00FF_FFFF {
                        in_range = false;
                    }
                }
            }
        }
    }
    report.check("16.3 : aucune transformation ne deborde", in_range);

    // Cycling steps through every mode and comes back.
    let start = super::contrast_mode();
    let mut seen = Vec::new();
    for _ in 0..contrast::MODE_COUNT {
        seen.push(super::cycle_contrast_mode());
    }
    report.check(
        "16.3 : le cycle parcourt tous les modes",
        seen.len() == contrast::MODE_COUNT
            && Mode::colour_blind().iter().all(|mode| seen.contains(mode)),
    );
    super::set_contrast_mode(start);
}

// ============================================================
//  16.2 -- The magnifier
// ============================================================

fn magnifier(report: &mut Report) {
    let global = zoom::Settings {
        shape: zoom::Shape::Global,
        percent: 200,
        area: (0, 0, 640, 360),
    };

    // The centre is a fixed point: magnifying around it must not move it.
    report.check(
        "16.2 : le centre du zoom est un point fixe",
        zoom::source_of(&global, 800, 600, 400, 300, 400, 300) == Some((400, 300)),
    );
    // At 200 %, a pixel a hundred to the right of the centre shows what
    // was fifty to the right.
    report.check(
        "16.2 : le facteur est bien applique",
        zoom::source_of(&global, 800, 600, 400, 300, 500, 300) == Some((450, 300)),
    );

    // Every destination pixel has a source. A forward map leaves holes as
    // soon as the factor is not an integer, and the holes are what a
    // magnifier with a grid through it looks like.
    let odd = zoom::Settings {
        percent: 150,
        ..global
    };
    let mut covered = true;
    for x in 0..800 {
        for y in (0..600).step_by(37) {
            if zoom::source_of(&odd, 800, 600, 400, 300, x, y).is_none() {
                covered = false;
            }
        }
    }
    report.check("16.2 : tout pixel de destination a une source", covered);

    // A source falling off the edge is clamped, not dropped: a hole would
    // show whatever the previous frame left there.
    let strong = zoom::Settings {
        percent: 800,
        ..global
    };
    let corner = zoom::source_of(&strong, 800, 600, 400, 300, 0, 0);
    report.check(
        "16.2 : une source hors ecran est ramenee au bord",
        matches!(corner, Some((x, y)) if (0..800).contains(&x) && (0..600).contains(&y)),
    );

    // The lens magnifies inside its disc and leaves the rest alone --
    // which is what makes it a lens and not a global zoom.
    let lens = zoom::Settings {
        shape: zoom::Shape::Lens,
        ..global
    };
    report.check(
        "16.2 : la loupe agrandit dans son disque",
        zoom::source_of(&lens, 800, 600, 400, 300, 420, 300).is_some(),
    );
    report.check(
        "16.2 : la loupe laisse le reste intact",
        zoom::source_of(&lens, 800, 600, 400, 300, 700, 300).is_none(),
    );
    // The disc is a disc, not a square: a corner at the same distance in
    // both axes as the radius is outside it.
    report.check(
        "16.2 : la loupe est un disque, pas un carre",
        zoom::source_of(
            &lens,
            800,
            600,
            400,
            300,
            400 + zoom::LENS_RADIUS - 10,
            300 + zoom::LENS_RADIUS - 10,
        )
        .is_none(),
    );

    // The area magnifies a fixed rectangle wherever the pointer is.
    let area = zoom::Settings {
        shape: zoom::Shape::Area,
        area: (100, 100, 200, 200),
        ..global
    };
    report.check(
        "16.2 : la zone agrandit son rectangle",
        zoom::source_of(&area, 800, 600, 700, 500, 150, 150).is_some()
            && zoom::source_of(&area, 800, 600, 700, 500, 700, 500).is_none(),
    );

    // A factor below one would shrink the screen; a factor of zero would
    // divide by zero. Both come from a settings file.
    report.check(
        "16.2 : le facteur est borne",
        zoom::Settings {
            percent: 0,
            ..global
        }
        .sanitised()
        .percent
            == zoom::MIN_PERCENT
            && zoom::Settings {
                percent: 100_000,
                ..global
            }
            .sanitised()
            .percent
                == zoom::MAX_PERCENT,
    );
    // At 100 % the map is the identity: switching the magnifier on at
    // minimum must not shift the image by a pixel.
    let none = zoom::Settings {
        percent: 100,
        ..global
    };
    report.check(
        "16.2 : a 100 % la correspondance est l'identite",
        zoom::source_of(&none, 800, 600, 400, 300, 123, 456) == Some((123, 456)),
    );
    report.check(
        "16.2 : zoomer et dezoomer se compensent",
        global.zoomed_in().zoomed_out().percent == global.percent,
    );
    report.check(
        "16.2 : le dezoom s'arrete a 100 %",
        zoom::Settings {
            percent: zoom::MIN_PERCENT,
            ..global
        }
        .zoomed_out()
        .percent
            == zoom::MIN_PERCENT,
    );

    // While the magnifier is on, the whole screen must be republished.
    let before = super::is_on(Feature::Zoom);
    super::set(Feature::Zoom, true);
    report.check(
        "16.2 : le zoom actif exige une trame complete",
        super::wants_full_frame() && super::transforms_pixels(),
    );
    super::set(Feature::Zoom, false);
    report.check(
        "16.2 : sans zoom ni contraste, le chemin rapide est repris",
        !super::wants_full_frame(),
    );
    super::set(Feature::Zoom, before);
}

// ============================================================
//  16.2 -- Letter to sound
// ============================================================

fn phonetics(report: &mut Report) {
    let spell = |text: &str| speech::transcribe(&speech::graphemes(text));

    // The digraphs must beat the single letters. `ch` read as `c` then
    // `h` gives a reader that says "s-h" for "chercher".
    report.equal_text("16.2 : ch est un seul phoneme", &spell("ch"), "sh-_");
    report.equal_text("16.2 : ou est un seul phoneme", &spell("ou"), "ou-_");
    report.equal_text("16.2 : qu vaut k", &spell("qui"), "k-i-_");
    report.equal_text("16.2 : ph vaut f", &spell("photo"), "f-o-t-o-_");

    // A nasal is a vowel plus n or m *not* followed by a vowel. Skipping
    // that test nasalises half the vocabulary.
    report.equal_text("16.2 : bon est nasal", &spell("bon"), "b-on-_");
    report.equal_text("16.2 : bonne ne l'est pas", &spell("bonne"), "b-o-n-_");

    // A final `e` is silent, which is the single most recognisable
    // mistake a foreign reader makes.
    report.equal_text("16.2 : le e final est muet", &spell("liste"), "l-i-s-t-_");
    // `h` is always silent.
    report.equal_text("16.2 : le h est muet", &spell("heure"), "eu-r-_");

    // `c` and `g` are soft before e, i, y.
    report.equal_text("16.2 : c doux devant i", &spell("ci"), "s-i-_");
    report.equal_text("16.2 : c dur devant a", &spell("ca"), "k-a-_");
    report.equal_text("16.2 : g doux devant e", &spell("gel"), "zh-e-l-_");

    // `s` between two vowels is /z/.
    report.equal_text("16.2 : s entre deux voyelles vaut z", &spell("usine"), "u-z-i-n-_");
    report.equal_text("16.2 : s initial reste s", &spell("sur"), "s-u-r-_");

    // The word the whole reader exists to say.
    report.equal_text("16.2 : bouton", &spell("bouton"), "b-ou-t-on-_");
    report.equal_text("16.2 : fenetre", &spell("fenetre"), "f-e-n-e-t-r-_");

    // Punctuation is a boundary, and several in a row collapse into one.
    report.equal_text("16.2 : la ponctuation borne les mots", &spell("a, b"), "a-_-b-_");
    report.equal_text(
        "16.2 : plusieurs separateurs ne font qu'une pause",
        &spell("a...b"),
        "a-_-b-_",
    );

    // An empty text produces one silence, not an empty buffer: a caller
    // that queued nothing must still get a valid utterance back.
    report.equal_text("16.2 : un texte vide donne un silence", &spell(""), "_");

    // The phoneme count is bounded: the text comes from a window title,
    // hence from outside.
    let long: String = core::iter::repeat_n('a', speech::MAX_PHONEMES * 2).collect();
    report.check(
        "16.2 : la suite de phonemes est bornee",
        speech::graphemes(&long).len() <= speech::MAX_PHONEMES + 1,
    );

    // Uppercase reads like lowercase: a label is not a different word
    // because it is capitalised.
    report.check(
        "16.2 : la casse ne change rien",
        spell("Bouton") == spell("bouton"),
    );

    // Vowels are vowels, consonants are not: the classification feeds the
    // rules above.
    report.check(
        "16.2 : la classification des voyelles",
        Phoneme::A.is_vowel() && Phoneme::On.is_vowel() && !Phoneme::S.is_vowel(),
    );
}

// ============================================================
//  16.2 -- The synthesiser
// ============================================================

fn synthesis(report: &mut Report) {
    let settings = VoiceSettings::default();

    // A vowel must produce sound. A formant synthesiser whose filters are
    // wrong produces silence, and silence is a perfectly plausible buffer.
    let vowel = speech::render(&[Phoneme::A], settings);
    let energy: i64 = vowel.iter().map(|sample| (*sample as i64).abs()).sum();
    report.check(
        "16.2 : une voyelle produit du signal",
        !vowel.is_empty() && energy > vowel.len() as i64 * 100,
    );

    // Silence must produce silence. If it did not, the boundary between
    // two words would carry sound.
    let quiet = speech::render(&[Phoneme::Silence], settings);
    report.check(
        "16.2 : le silence ne produit rien",
        quiet.iter().all(|sample| *sample == 0),
    );

    // Two different vowels must give two different signals. A synthesiser
    // that ignored its formants would give the same buffer for both, and
    // both would pass an energy check.
    let other = speech::render(&[Phoneme::I], settings);
    let differences = vowel
        .iter()
        .zip(other.iter())
        .filter(|(a, b)| a != b)
        .count();
    report.check(
        "16.2 : deux voyelles donnent deux signaux",
        differences > vowel.len().min(other.len()) / 2,
    );

    // Nothing clips: a filter that went unstable would sit at full scale,
    // which in a headset is not a cosmetic defect.
    let sentence = speech::synthesise("bouton ouvrir le menu", settings);
    let clipped = sentence
        .iter()
        .filter(|sample| **sample == i16::MAX || **sample == i16::MIN)
        .count();
    report.check(
        "16.2 : la synthese ne sature pas",
        !sentence.is_empty() && clipped * 100 < sentence.len(),
    );

    // A faster rate makes a shorter buffer, in proportion.
    let fast = VoiceSettings {
        rate_percent: 200,
        ..settings
    };
    let quick = speech::synthesise("bouton ouvrir le menu", fast);
    report.check(
        "16.2 : le debit raccourcit l'enonce",
        quick.len() * 3 < sentence.len() * 2,
    );

    // A rate of zero would divide by zero and a pitch of zero would give
    // a pulse train with no period. Both come from a settings file.
    let absurd = VoiceSettings {
        pitch_hz: 0,
        rate_percent: 0,
        volume: 9_999,
    }
    .sanitised();
    report.check(
        "16.2 : les reglages de voix sont bornes",
        absurd.pitch_hz >= 60 && absurd.rate_percent >= 25 && absurd.volume <= 256,
    );
    // And the bounded settings really render.
    report.check(
        "16.2 : un reglage borne produit quand meme du son",
        !speech::synthesise("a", absurd).is_empty(),
    );

    // The announced duration must match the buffer produced, within the
    // rounding of one sample per phoneme. A deadline computed from the
    // first and a buffer sized by the second would drift apart.
    let phonemes = speech::graphemes("bouton");
    let announced = speech::duration_ms(&phonemes, settings) as usize;
    let produced = speech::render(&phonemes, settings).len() * 1000 / speech::SAMPLE_RATE as usize;
    report.check(
        "16.2 : la duree annoncee correspond au tampon produit",
        announced.abs_diff(produced) <= phonemes.len() + 1,
    );

    // Zero volume gives silence, which is the one way to mute the reader
    // without switching it off.
    let mute = VoiceSettings {
        volume: 0,
        ..settings
    };
    report.check(
        "16.2 : un volume nul rend le silence",
        speech::synthesise("a", mute).iter().all(|s| *s == 0),
    );
}

// ============================================================
//  Settings
// ============================================================

fn configuration(report: &mut Report) {
    let before: Vec<(Feature, bool)> = Feature::all()
        .iter()
        .map(|feature| (*feature, super::is_on(*feature)))
        .collect();

    let applied = super::apply_config(
        "# essai\n\
         lecteur = oui\n\
         zoom = non\n\
         contraste = oui\n\
         clavier = oui\n\
         mode_couleur = deuteranopie\n\
         zoom_forme = loupe\n\
         zoom_facteur = 300\n\
         voix_debit = 150\n\
         cle_inconnue = valeur\n",
    );
    report.check("16 : les reglages connus sont appliques", applied == 8);
    report.check(
        "16 : les quatre fonctions suivent le fichier",
        super::is_on(Feature::Reader)
            && !super::is_on(Feature::Zoom)
            && super::is_on(Feature::Contrast)
            && super::is_on(Feature::Keyboard),
    );
    report.check(
        "16.3 : le mode couleur suit le fichier",
        super::contrast_mode() == Mode::Deuteranopia,
    );
    report.check(
        "16.2 : la forme et le facteur du zoom suivent le fichier",
        super::zoom_settings().shape == zoom::Shape::Lens
            && super::zoom_settings().percent == 300,
    );
    report.check(
        "16.2 : le debit de la voix suit le fichier",
        super::reader::voice().rate_percent == 150,
    );

    // An out-of-range value in the file is clamped rather than refused:
    // the alternative is a machine whose magnifier is off because a
    // number was too big.
    super::apply_config("zoom_facteur = 100000\n");
    report.check(
        "16.2 : un facteur aberrant est ramene dans les bornes",
        super::zoom_settings().percent == zoom::MAX_PERCENT,
    );

    // Setting a feature to what it already is changes nothing and
    // publishes nothing: a module woken for a change that did not happen
    // would repaint for nothing.
    let toggles = super::toggle_count();
    report.check(
        "16 : reappliquer un etat identique ne publie rien",
        !super::set(Feature::Reader, true) && super::toggle_count() == toggles,
    );

    super::apply_config("voix_debit = 100\n");
    super::set_zoom_settings(zoom::Settings::default());
    for (feature, state) in before {
        super::set(feature, state);
    }
}
