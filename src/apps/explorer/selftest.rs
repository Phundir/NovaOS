//! Self-test of the explorer: paths, names, sorting, the drawing budget,
//! and the tree operations run for real on the disk.
//!
//! Called from the phase 7 self-test, which reports the count as one line
//! -- the same arrangement as `textbuf::selftest`.
//!
//! The tree operations are exercised in `/tmp` with the kernel's rights,
//! because the self-test runs before anybody has logged in. What they
//! check is the walk, not the permissions: those are the filesystem's, and
//! its own self-test covers them.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::fs::perm::{Credentials, Mode};

use super::layout::{self, ACTION_LABELS, Layout};
use super::ops::{self, Item, SortKey};

struct Count {
    passed: usize,
    failed: usize,
}

impl Count {
    fn check(&mut self, label: &str, condition: bool) {
        if condition {
            self.passed += 1;
        } else {
            self.failed += 1;
            crate::serial_println!("[explorer] ECHEC : {}", label);
        }
    }
}

pub fn run() -> (usize, usize) {
    let mut count = Count {
        passed: 0,
        failed: 0,
    };
    paths(&mut count);
    names(&mut count);
    sorting(&mut count);
    budget(&mut count);
    trees(&mut count);
    (count.passed, count.failed)
}

fn paths(count: &mut Count) {
    count.check(
        "un chemin se canonise",
        ops::canonical("/a/./b/../c").as_deref() == Some("/a/c"),
    );
    count.check(
        "la lettre de disque survit a la canonisation",
        ops::canonical("A:/x/../y").as_deref() == Some("A:/y"),
    );
    count.check("un chemin relatif n'est pas canonique", ops::canonical("docs").is_none());

    count.check("la racine n'a pas de parent", ops::parent("/").is_none());
    count.check("la racine d'un disque n'a pas de parent", ops::parent("A:/").is_none());
    count.check("le parent de /a est /", ops::parent("/a").as_deref() == Some("/"));
    count.check("le parent de /a/b est /a", ops::parent("/a/b").as_deref() == Some("/a"));
    count.check("le parent de A:/x est A:/", ops::parent("A:/x").as_deref() == Some("A:/"));

    count.check(
        "la jointure pose une seule barre",
        ops::join("/", "a") == "/a" && ops::join("/a", "b") == "/a/b" && ops::join("A:/", "x") == "A:/x",
    );

    // The guard against copying a directory into itself: whole
    // components, not a prefix of characters.
    count.check("/a/b est dans /a", ops::is_inside("/a/b", "/a"));
    count.check("/a est dans /a", ops::is_inside("/a", "/a"));
    count.check("/ab n'est pas dans /a", !ops::is_inside("/ab", "/a"));
    count.check("tout le disque est dans /", ops::is_inside("/x/y", "/"));
    count.check("un autre disque n'est pas dans /", !ops::is_inside("A:/x", "/"));
}

fn names(count: &mut Count) {
    let taken: Vec<String> = ["notes.txt", "notes (copie).txt", ".profile", "photos"]
        .iter()
        .map(|name| String::from(*name))
        .collect();
    count.check("un nom libre est garde", ops::unique_name(&taken, "neuf.txt") == "neuf.txt");
    count.check(
        "un nom pris recoit un rang, extension gardee",
        ops::unique_name(&taken, "notes.txt") == "notes (copie 2).txt",
    );
    count.check(
        "un point initial n'est pas une extension",
        ops::unique_name(&taken, ".profile") == ".profile (copie)",
    );
    count.check(
        "un dossier pris recoit (copie)",
        ops::unique_name(&taken, "photos") == "photos (copie)",
    );
    let long: String = core::iter::repeat_n('n', crate::fs::format::NAME_MAX).collect();
    let taken_long = alloc::vec![long.clone()];
    count.check(
        "un nom long reste dans NAME_MAX une fois suffixe",
        ops::unique_name(&taken_long, &long).len() <= crate::fs::format::NAME_MAX,
    );

    count.check("un nom vide est refuse", ops::name_problem("").is_some());
    count.check("un nom reserve est refuse", ops::name_problem("..").is_some());
    count.check("une barre est refusee", ops::name_problem("a/b").is_some());
    count.check("un nom non ASCII est refuse", ops::name_problem("caf\u{e9}").is_some());
    count.check("un nom ordinaire passe", ops::name_problem("rapport 2026.txt").is_none());

    count.check(
        "les tailles se lisent sans flottant",
        ops::format_size(0) == "0 o"
            && ops::format_size(1023) == "1023 o"
            && ops::format_size(1536) == "1.5 Kio"
            && ops::format_size(1024 * 1024) == "1.0 Mio",
    );
    count.check(
        "le texte se reconnait, le binaire aussi",
        ops::looks_like_text(b"ligne\n\tsuite\r\n")
            && !ops::looks_like_text(&[0x7F, b'E', b'L', b'F', 0x02])
            && !ops::looks_like_text(&[0xC3]),
    );
    count.check(
        "les formats de 3.1 ont un type lisible",
        ops::kind_label("notepad.cus", false) == "application"
            && ops::kind_label("explorer.ss", false) == "logiciel systeme"
            && ops::kind_label("docs", true) == "dossier",
    );
}

fn item(name: &str, is_dir: bool, size: u64) -> Item {
    Item {
        name: String::from(name),
        is_dir,
        size,
        known: true,
    }
}

fn sorting(count: &mut Count) {
    let mut items = alloc::vec![
        item("zeta.txt", false, 10),
        item("Beta", true, 0),
        item("alpha.txt", false, 300),
        item("alpha", true, 0),
    ];
    ops::sort(&mut items, SortKey::Name, true);
    let order: Vec<&str> = items.iter().map(|item| item.name.as_str()).collect();
    count.check(
        "dossiers d'abord, puis l'ordre sans casse",
        order == ["alpha", "Beta", "alpha.txt", "zeta.txt"],
    );

    ops::sort(&mut items, SortKey::Name, false);
    count.check(
        "l'ordre inverse garde les dossiers en tete",
        items[0].is_dir && items[1].is_dir && items[2].name == "zeta.txt",
    );

    ops::sort(&mut items, SortKey::Size, false);
    count.check(
        "le tri par taille met le plus gros fichier en premier",
        items[2].name == "alpha.txt",
    );
}

fn budget(count: &mut Count) {
    count.check(
        "le budget de dessin tient dans MAX_COMMANDS",
        layout::FIXED_COMMANDS + layout::MAX_ROWS * layout::ROW_COMMANDS
            <= crate::gui::surface::MAX_COMMANDS,
    );
    // The properties panel replaces the rows, so it must cost no more.
    count.check(
        "le panneau de proprietes coute moins que les lignes",
        2 + super::INFO_LINES <= layout::MAX_ROWS * layout::ROW_COMMANDS,
    );

    let theme = crate::gui::theme::Theme::default();
    // The narrowest window the task creates: a 640-pixel screen minus
    // its margin.
    for (width, height) in [(600, 400), (820, 540)] {
        let geometry = Layout::new(width, height, &theme);
        let last = geometry.action(ACTION_LABELS.len() - 1);
        count.check(
            &format!("la barre d'actions tient en {}", width),
            last.right() <= width,
        );
        let rows = geometry.visible_rows();
        count.check(
            &format!("les lignes restent dans la liste en {}x{}", width, height),
            rows >= 1 && geometry.row(rows - 1).bottom() <= geometry.list.bottom(),
        );
        count.check(
            &format!("le nom a de la place en {}", width),
            geometry.name_chars() >= 12 && geometry.path_box().width > 0,
        );
        let name = geometry.header_cell(SortKey::Name);
        let kind = geometry.header_cell(SortKey::Kind);
        let size = geometry.header_cell(SortKey::Size);
        count.check(
            &format!("les colonnes ne se chevauchent pas en {}", width),
            name.right() <= kind.x && kind.right() <= size.x,
        );
    }
}

fn trees(count: &mut Count) {
    if !crate::fs::is_ready() {
        crate::serial_println!("[explorer]   systeme de fichiers absent - arbres non eprouves");
        return;
    }
    let who = Credentials::kernel();
    let now = crate::idt::ticks();
    let root = "/tmp/explorer-autotest";
    let copy = "/tmp/explorer-autotest-copie";
    // Leftovers of an interrupted run would make every check below lie.
    let _ = ops::delete_tree(root, who, now);
    let _ = ops::delete_tree(copy, who, now);

    // `/tmp` first, and only then the measure: creating it on a fresh
    // disk takes blocks that are rightly never given back.
    let _ = crate::fs::create_directory("/tmp", who, Mode::system_dir(), now);
    let free_before = crate::fs::disk_mount(crate::fs::BOOT_LETTER)
        .map(|disk| disk.free_blocks)
        .unwrap_or(0);

    let built = crate::fs::create_directory(&format!("{}/a/b", root), who, Mode::private_dir(), now).is_ok()
        && crate::fs::write_file(&format!("{}/a/b/f.txt", root), b"contenu", who, Mode::private_file(), now).is_ok()
        && crate::fs::write_file(&format!("{}/g.txt", root), b"autre", who, Mode::private_file(), now).is_ok();
    count.check("l'arbre d'essai se construit", built);
    if !built {
        return;
    }

    let copied = ops::copy_tree(root, copy, who, now);
    count.check(
        "la copie d'un arbre compte ses cinq elements",
        copied.as_ref().ok() == Some(&5),
    );
    count.check(
        "le contenu profond est copie a l'identique",
        crate::fs::read_file(&format!("{}/a/b/f.txt", copy), who).as_deref() == Ok(b"contenu".as_slice()),
    );

    count.check(
        "un dossier ne se deplace pas dans lui-meme",
        crate::fs::move_entry(root, &format!("{}/a/dedans", root), who, now)
            == Err(crate::fs::FsError::InvalidPath),
    );
    count.check(
        "le refus laisse l'arbre intact",
        crate::fs::stat(&format!("{}/a/b/f.txt", root), who).is_ok(),
    );

    let moved = ops::move_tree(&format!("{}/g.txt", copy), &format!("{}/a/g.txt", copy), who, now);
    count.check(
        "un deplacement sur le meme disque aboutit",
        moved.is_ok() && crate::fs::stat(&format!("{}/a/g.txt", copy), who).is_ok(),
    );

    let deleted = ops::delete_tree(root, who, now).is_ok() && ops::delete_tree(copy, who, now).is_ok();
    count.check("la suppression d'un arbre aboutit", deleted);
    count.check(
        "rien ne reste des arbres supprimes",
        crate::fs::stat(root, who).is_err() && crate::fs::stat(copy, who).is_err(),
    );

    // The invariant of DECISIONS.md: every block goes back. A walk that
    // forgot a child would leave its blocks allocated and unreachable.
    let free_after = crate::fs::disk_mount(crate::fs::BOOT_LETTER)
        .map(|disk| disk.free_blocks)
        .unwrap_or(1);
    if free_before != free_after {
        // The figures, otherwise the failure says nothing: a leak of a
        // few blocks and an allocation made elsewhere during the test
        // read the same.
        crate::serial_println!(
            "[explorer]   blocs libres : {} avant, {} apres ({} de difference)",
            free_before,
            free_after,
            free_before as i64 - free_after as i64
        );
    }
    count.check("les blocs libres reviennent a leur valeur", free_before == free_after);
}
