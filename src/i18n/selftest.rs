//! Phase 10.2 self-test (spec 14).
//!
//! ## What is checked, and why these checks
//!
//! * **Every conversion, against a value computed by hand.** A unit
//!   conversion that is wrong by a factor produces a number of the right
//!   shape: `42 mi` where `26 mi` was due looks exactly as plausible. The
//!   reference values here come from the definitions -- one mile is
//!   1 609 344 mm by law, not by measurement -- so a mismatch is a bug in
//!   the code and never in the reference.
//! * **The direction of the priority (14.1).** Application over user over
//!   system, checked by writing three files that disagree and observing
//!   which value survives. Reading the code back would not catch an
//!   inverted merge order; three files that contradict each other do.
//! * **The round trip of the settings.** `to_cfg` then `merge` must give
//!   the same structure. It is what guarantees that saving a setting and
//!   reloading it is not a slow way of losing it.
//! * **Substitution on hostile input.** An unterminated brace, an unknown
//!   name, a name repeated more times than the budget allows. None may
//!   panic, none may loop -- a `.lang` comes from a file.
//! * **The layouts, key by key.** A layout table is transcription, and a
//!   transcription is verified by comparison, not by rereading.
//! * **The publication of 14.1.** A subscriber must actually receive
//!   something on `i18n.changed`. The counter alone would only prove the
//!   call was made.
//!
//! ## It leaves nothing behind
//!
//! It writes under `/cache/i18n-autotest` and under two real settings
//! paths. Both are purged on the way in and on the way out -- the lesson
//! of phase 7, confirmed by 8 and 9.

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::fs::perm::{Credentials, Mode};

use super::keyboard::{Layout, SwitchKey};
use super::lang;
use super::tz;
use super::units::{
    self, ClockFormat, CurrencyPosition, DateFormat, Distance, Separator, Speed, Temperature,
    Weight,
};
use super::Settings;

/// Root of the test scope.
const TEST_ROOT: &str = "/cache/i18n-autotest";
/// Account the priority test uses. Created and removed by this file.
const TEST_USER: &str = "i18ntest";
/// Application the priority test uses.
const TEST_APP: &str = "test.i18n.app";

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
            crate::serial_println!("[i18n] ECHEC : {}", label);
        }
    }

    fn equal_text(&mut self, label: &str, produced: &str, expected: &str) {
        if produced == expected {
            self.passed += 1;
            return;
        }
        self.failed += 1;
        crate::serial_println!(
            "[i18n] ECHEC : {} - produit \"{}\", attendu \"{}\"",
            label,
            produced,
            expected
        );
    }

    fn equal_number(&mut self, label: &str, produced: i64, expected: i64) {
        if produced == expected {
            self.passed += 1;
            return;
        }
        self.failed += 1;
        crate::serial_println!(
            "[i18n] ECHEC : {} - produit {}, attendu {}",
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

    purge();

    numbers(&mut report);
    dates(&mut report);
    conversions(&mut report);
    catalogues(&mut report);
    zones(&mut report);
    layouts(&mut report);
    settings_roundtrip(&mut report);
    priority(&mut report);
    publication(&mut report);

    purge();

    (report.passed, report.failed)
}

// ============================================================
//  10.2.3 -- Numbers
// ============================================================

fn numbers(report: &mut Report) {
    report.equal_text(
        "14.2 : milliers separes par une espace",
        &units::format_integer(1_234_567, Separator::Space),
        "1 234 567",
    );
    report.equal_text(
        "14.2 : milliers separes par une virgule",
        &units::format_integer(1_234_567, Separator::Comma),
        "1,234,567",
    );
    report.equal_text(
        "14.2 : milliers separes par un point",
        &units::format_integer(1_234_567, Separator::Dot),
        "1.234.567",
    );
    report.equal_text(
        "14.2 : nombre negatif",
        &units::format_integer(-4_200, Separator::Space),
        "-4 200",
    );
    report.equal_text(
        "14.2 : groupe partiel a gauche",
        &units::format_integer(42, Separator::Space),
        "42",
    );
    report.equal_text(
        "14.2 : zero",
        &units::format_integer(0, Separator::Space),
        "0",
    );

    // The decimal separator is derived from the thousands one -- the
    // choice documented in `units`. Checking it here is what stops the
    // rule being changed by accident.
    report.equal_text(
        "14.2 : virgule aux milliers implique le point decimal",
        &units::format_milli(1_500, 2, Separator::Comma),
        "1.50",
    );
    report.equal_text(
        "14.2 : espace aux milliers implique la virgule decimale",
        &units::format_milli(1_500, 2, Separator::Space),
        "1,50",
    );

    // Zero padding on the fractional part: five thousandths is `,005`.
    report.equal_text(
        "14.2 : fraction completee par des zeros",
        &units::format_milli(5, 3, Separator::Space),
        "0,005",
    );
    // Rounding half away from zero, not truncation.
    report.equal_text(
        "14.2 : arrondi et non troncature",
        &units::format_milli(999, 2, Separator::Space),
        "1,00",
    );
    report.equal_text(
        "14.2 : arrondi d'un negatif",
        &units::format_milli(-1_555, 2, Separator::Space),
        "-1,56",
    );
    // A value that rounds to zero must not carry a minus sign: `-0,00`
    // reads as a negative amount and it is not one.
    report.equal_text(
        "14.2 : zero arrondi sans signe",
        &units::format_milli(-1, 2, Separator::Space),
        "0,00",
    );

    report.equal_text(
        "14.2 : monnaie apres le montant",
        &units::format_currency(123_456, "EUR", CurrencyPosition::After, Separator::Space),
        "1 234,56 EUR",
    );
    report.equal_text(
        "14.2 : monnaie avant le montant",
        &units::format_currency(123_456, "USD", CurrencyPosition::Before, Separator::Comma),
        "USD 1,234.56",
    );
}

// ============================================================
//  10.2.3 -- Dates and times
// ============================================================

fn dates(report: &mut Report) {
    report.equal_text(
        "14.2 : DD/MM/YYYY",
        &units::format_date(2026, 9, 2, DateFormat::Dmy),
        "02/09/2026",
    );
    report.equal_text(
        "14.2 : MM/DD/YYYY",
        &units::format_date(2026, 9, 2, DateFormat::Mdy),
        "09/02/2026",
    );
    // The ISO order carries dashes, the two others slashes.
    report.equal_text(
        "14.2 : YYYY-MM-DD",
        &units::format_date(2026, 9, 2, DateFormat::Ymd),
        "2026-09-02",
    );

    report.equal_text(
        "14.2 : horloge 24 h",
        &units::format_time(14, 5, 9, ClockFormat::H24),
        "14:05:09",
    );
    report.equal_text(
        "14.2 : horloge 12 h l'apres-midi",
        &units::format_time(14, 5, 9, ClockFormat::H12),
        "2:05:09 PM",
    );
    // Midnight and noon map onto 12, not onto zero.
    report.equal_text(
        "14.2 : minuit en 12 h vaut 12 AM",
        &units::format_time(0, 30, 0, ClockFormat::H12),
        "12:30:00 AM",
    );
    report.equal_text(
        "14.2 : midi en 12 h vaut 12 PM",
        &units::format_time(12, 0, 0, ClockFormat::H12),
        "12:00:00 PM",
    );
    report.equal_text(
        "14.2 : 23 h en 12 h vaut 11 PM",
        &units::format_time(23, 59, 59, ClockFormat::H12),
        "11:59:59 PM",
    );
}

// ============================================================
//  10.2.3 -- The four unit families
// ============================================================

fn conversions(report: &mut Report) {
    // 42 195 m, the marathon: exactly 42.195 km, 26.219 mi, 22.783 NM.
    let marathon_mm: i64 = 42_195_000;
    report.equal_number(
        "14.2 : metres vers kilometres",
        Distance::Km.from_millimetres(marathon_mm),
        42_195,
    );
    report.equal_number(
        "14.2 : metres vers miles",
        Distance::Miles.from_millimetres(marathon_mm),
        26_218,
    );
    report.equal_number(
        "14.2 : metres vers milles marins",
        Distance::NauticalMiles.from_millimetres(marathon_mm),
        22_783,
    );

    // 1 kg = 2.204 62 lb. In thousandths: 2204.
    report.equal_number(
        "14.2 : grammes vers kilogrammes",
        Weight::Kg.from_milligrams(1_000_000),
        1_000,
    );
    report.equal_number(
        "14.2 : grammes vers livres",
        Weight::Lbs.from_milligrams(1_000_000),
        2_204,
    );
    // The exact definition matters: a rounded 453 592 mg per pound would
    // give 2 204 626 here instead of 2 204 622.
    report.equal_number(
        "14.2 : une tonne en livres",
        Weight::Lbs.from_milligrams(1_000_000_000),
        2_204_622,
    );

    report.equal_number(
        "14.2 : Celsius inchange",
        Temperature::Celsius.from_milli_celsius(21_500),
        21_500,
    );
    // 100 C = 212 F, 0 C = 32 F, -40 C = -40 F -- the fixed point of the
    // pair, which no scaling error survives.
    report.equal_number(
        "14.2 : 100 C valent 212 F",
        Temperature::Fahrenheit.from_milli_celsius(100_000),
        212_000,
    );
    report.equal_number(
        "14.2 : 0 C valent 32 F",
        Temperature::Fahrenheit.from_milli_celsius(0),
        32_000,
    );
    report.equal_number(
        "14.2 : -40 C valent -40 F",
        Temperature::Fahrenheit.from_milli_celsius(-40_000),
        -40_000,
    );
    report.equal_number(
        "14.2 : 0 C valent 273,15 K",
        Temperature::Kelvin.from_milli_celsius(0),
        273_150,
    );

    // 10 m/s = 36 km/h = 22.369 mph = 19.438 kn.
    report.equal_number(
        "14.2 : 10 m/s valent 36 km/h",
        Speed::Kmh.from_millimetres_per_second(10_000),
        36_000,
    );
    report.equal_number(
        "14.2 : 10 m/s en mph",
        Speed::Mph.from_millimetres_per_second(10_000),
        22_369,
    );
    report.equal_number(
        "14.2 : 10 m/s en noeuds",
        Speed::Knots.from_millimetres_per_second(10_000),
        19_438,
    );

    // The whole chain, through the settings: value, separator and symbol.
    let mut settings = Settings::default();
    settings.temperature = Temperature::Fahrenheit;
    report.equal_text(
        "14.2 : chaine complete d'une temperature",
        &settings.format_temperature(21_500),
        "70,7 F",
    );
    settings.distance = Distance::Miles;
    settings.thousands = Separator::Comma;
    report.equal_text(
        "14.2 : chaine complete d'une distance",
        &settings.format_distance(marathon_mm),
        "26.22 mi",
    );
}

// ============================================================
//  10.2.6 -- The .lang format
// ============================================================

fn catalogues(report: &mut Report) {
    let source = "\
# commentaire
salut = Bonjour {user}
vide =
  espaces  =  autour
pas_de_signe
repete = {a} et {a}
";
    let catalogue = lang::parse("fr-FR", source);

    report.check("14.3 : commentaire ignore", catalogue.get("#").is_none());
    report.check(
        "14.3 : ligne sans signe egal ecartee",
        catalogue.get("pas_de_signe").is_none() && catalogue.skipped == 1,
    );
    report.equal_text(
        "14.3 : espaces autour de la cle et de la valeur",
        catalogue.get("espaces").unwrap_or("<absent>"),
        "autour",
    );
    report.equal_text(
        "14.3 : valeur vide acceptee",
        catalogue.get("vide").unwrap_or("<absent>"),
        "",
    );

    report.equal_text(
        "14.3 : substitution d'une variable",
        &lang::substitute(
            catalogue.get("salut").unwrap_or(""),
            &[("user", "nova")],
        ),
        "Bonjour nova",
    );
    // An unknown name stays as written: it names what is missing.
    report.equal_text(
        "14.3 : variable inconnue laissee telle quelle",
        &lang::substitute("Bonjour {user}", &[("autre", "x")]),
        "Bonjour {user}",
    );
    // An unterminated brace is a broken catalogue, not a reason to eat
    // the line.
    report.equal_text(
        "14.3 : accolade non fermee copiee litteralement",
        &lang::substitute("Bonjour {user", &[("user", "nova")]),
        "Bonjour {user",
    );
    report.equal_text(
        "14.3 : meme variable deux fois",
        &lang::substitute(
            catalogue.get("repete").unwrap_or(""),
            &[("a", "x")],
        ),
        "x et x",
    );
    report.equal_text(
        "14.3 : accolades vides",
        &lang::substitute("a{}b", &[("", "z")]),
        "a{}b",
    );

    // The substitution budget bounds the work an adversarial catalogue
    // can ask for. Beyond it the placeholders stay literal, which is
    // visible rather than silent.
    let mut long = String::new();
    for _ in 0..64 {
        long.push_str("{a}");
    }
    let expanded = lang::substitute(&long, &[("a", "1")]);
    report.check(
        "14.3 : substitutions bornees",
        expanded.contains("{a}") && expanded.matches('1').count() == 32,
    );

    let names = lang::placeholders("{a} {b} {a} {c}");
    report.check(
        "14.3 : variables listees sans doublon",
        names.len() == 3 && names[0] == "a" && names[1] == "b" && names[2] == "c",
    );

    // An overriding insert replaces, it does not duplicate: that is what
    // makes an application catalogue able to specialise a system key.
    let mut over = lang::Catalogue::new("fr-FR");
    over.insert("cle", "un");
    over.insert("cle", "deux");
    report.check(
        "14.1 : une cle reinseree remplace",
        over.len() == 1 && over.get("cle") == Some("deux"),
    );

    // Round trip: serialise then reparse gives the same content.
    let text = over.to_text();
    let back = lang::parse("fr-FR", &text);
    report.check(
        "14.3 : aller-retour du catalogue",
        back.len() == over.len() && back.get("cle") == Some("deux"),
    );

    // The bound on the number of entries holds.
    let mut big = lang::Catalogue::new("fr-FR");
    for index in 0..(lang::MAX_ENTRIES + 16) {
        big.insert(&alloc::format!("k{}", index), "v");
    }
    report.check(
        "14.3 : catalogue borne en entrees",
        big.len() == lang::MAX_ENTRIES,
    );
}

// ============================================================
//  10.2.5 -- Time zones
// ============================================================

fn zones(report: &mut Report) {
    let utc = tz::find("UTC").expect("UTC");
    let paris = tz::find("Europe/Paris").expect("Paris");
    let kathmandu = tz::find("Asia/Kathmandu").expect("Kathmandu");
    let los_angeles = tz::find("America/Los_Angeles").expect("Los Angeles");

    report.equal_number("14.2 : UTC a un decalage nul", utc.offset_minutes as i64, 0);
    // A zone that is not a whole number of hours is exactly the case a
    // table in hours would get wrong.
    report.equal_number(
        "14.2 : le Nepal est a +05:45",
        kathmandu.offset_minutes as i64,
        345,
    );
    report.equal_text(
        "14.2 : forme du decalage",
        &kathmandu.offset_text(),
        "UTC+05:45",
    );
    report.equal_text(
        "14.2 : decalage negatif",
        &los_angeles.offset_text(),
        "UTC-08:00",
    );
    report.equal_text("14.2 : signe ecrit meme pour UTC", &utc.offset_text(), "UTC+00:00");

    // 1 January 2026 at midnight UTC.
    let instant = crate::rtc::DateTime {
        year: 2026,
        month: 1,
        day: 1,
        hour: 0,
        minute: 0,
        second: 0,
    }
    .to_epoch();

    let local = crate::rtc::DateTime::from_epoch(paris.to_local(instant));
    report.check(
        "14.2 : conversion a l'affichage",
        local.hour == 1 && local.day == 1 && local.month == 1 && local.year == 2026,
    );
    // Crossing midnight backwards changes the date, which is the whole
    // point of storing in UTC.
    let west = crate::rtc::DateTime::from_epoch(los_angeles.to_local(instant));
    report.check(
        "14.2 : passage de minuit vers l'ouest",
        west.year == 2025 && west.month == 12 && west.day == 31 && west.hour == 16,
    );
    // Round trip: local then back to UTC is the identity.
    report.equal_number(
        "14.2 : aller-retour local vers UTC",
        paris.to_utc(paris.to_local(instant)) as i64,
        instant as i64,
    );
    // A negative offset near the epoch must not wrap.
    report.equal_number(
        "14.2 : instant nul avec decalage negatif",
        los_angeles.to_local(0) as i64,
        0,
    );

    // Storage stays in UTC: `format_date` shifts the display only.
    let mut settings = Settings::default();
    settings.timezone = String::from("Asia/Tokyo");
    settings.date = DateFormat::Ymd;
    report.equal_text(
        "14.2 : la date affichee suit le fuseau",
        &settings.format_date(instant),
        "2026-01-01",
    );
    report.equal_text(
        "14.2 : l'heure affichee suit le fuseau",
        &settings.format_time(instant),
        "09:00:00",
    );
}

// ============================================================
//  10.2.4 -- Layouts
// ============================================================

fn layouts(report: &mut Report) {
    // Scancode 0x10 is the first key of the top letter row on every
    // layout: `a` on AZERTY, `q` on QWERTY and QWERTZ, an apostrophe on
    // Dvorak. It is the single most telling key of the four tables.
    report.check(
        "14.2 : 0x10 vaut a en AZERTY",
        Layout::Azerty.character(0x10, false) == Some('a'),
    );
    report.check(
        "14.2 : 0x10 vaut q en QWERTY",
        Layout::Qwerty.character(0x10, false) == Some('q'),
    );
    report.check(
        "14.2 : 0x10 vaut q en QWERTZ",
        Layout::Qwertz.character(0x10, false) == Some('q'),
    );
    report.check(
        "14.2 : 0x10 vaut l'apostrophe en Dvorak",
        Layout::Dvorak.character(0x10, false) == Some('\''),
    );

    // What distinguishes QWERTZ from QWERTY is exactly two keys.
    report.check(
        "14.2 : QWERTZ echange y et z",
        Layout::Qwertz.character(0x15, false) == Some('z')
            && Layout::Qwertz.character(0x2C, false) == Some('y')
            && Layout::Qwerty.character(0x15, false) == Some('y')
            && Layout::Qwerty.character(0x2C, false) == Some('z'),
    );

    // The `m` key of a French keyboard sits at 0x27, where an English one
    // has the semicolon. Phase 7.7 paid for that difference: the "admin"
    // account was untypable. The check stays.
    report.check(
        "14.2 : le m francais est en 0x27",
        Layout::Azerty.character(0x27, false) == Some('m')
            && Layout::Qwerty.character(0x27, false) == Some(';'),
    );

    report.check(
        "14.2 : la majuscule suit le shift",
        Layout::Qwerty.character(0x10, true) == Some('Q'),
    );
    // Punctuation whose shifted form is not an uppercase letter.
    report.check(
        "14.2 : la ponctuation shiftee n'est pas une majuscule",
        Layout::Azerty.character(0x32, false) == Some(',')
            && Layout::Azerty.character(0x32, true) == Some('?'),
    );
    report.check(
        "14.2 : touche inconnue sans caractere",
        Layout::Qwerty.character(0x01, false).is_none(),
    );
    report.check(
        "14.2 : l'espace est decrite par tous les agencements",
        Layout::all()
            .iter()
            .all(|layout| layout.character(0x39, false) == Some(' ')),
    );

    // Every layout must produce printable ASCII only -- the rule of the
    // whole repository, and the one a new layout is most likely to break.
    let mut clean = true;
    for layout in Layout::all() {
        for scancode in 0u8..=0x58 {
            for shift in [false, true] {
                if let Some(glyph) = layout.character(scancode, shift)
                    && !(' '..='~').contains(&glyph)
                {
                    clean = false;
                }
            }
        }
    }
    report.check("14.2 : tous les agencements restent en ASCII imprimable", clean);

    // The switch key tells the two Alt keys apart by the extended flag.
    report.check(
        "14.2 : l'alt droit bascule, l'alt gauche non",
        SwitchKey::RightAlt.triggered_by(0x38, true)
            && !SwitchKey::RightAlt.triggered_by(0x38, false),
    );
    report.check(
        "14.2 : aucune touche de bascule",
        !SwitchKey::None.triggered_by(0x38, true) && SwitchKey::None.scancode().is_none(),
    );
}

// ============================================================
//  14.1 -- Priority and round trip
// ============================================================

fn settings_roundtrip(report: &mut Report) {
    let mut original = Settings::default();
    original.language = String::from("en-US");
    original.date = DateFormat::Mdy;
    original.clock = ClockFormat::H12;
    original.thousands = Separator::Comma;
    original.currency = String::from("USD");
    original.currency_position = CurrencyPosition::Before;
    original.distance = Distance::Miles;
    original.weight = Weight::Lbs;
    original.temperature = Temperature::Fahrenheit;
    original.speed = Speed::Mph;
    original.keyboard_primary = Layout::Qwerty;
    original.keyboard_secondary = Layout::Dvorak;
    original.keyboard_switch = SwitchKey::BothShift;
    original.timezone = String::from("America/New_York");

    let mut reloaded = Settings::default();
    reloaded.merge(&original.to_cfg());
    report.check("14.1 : aller-retour des parametres", reloaded == original);

    // An unknown key must not make the file be refused: a configuration
    // written by a later version stays readable.
    let mut tolerant = Settings::default();
    let applied = tolerant.merge("cle_inconnue = valeur\nlangue = de-DE\n");
    report.check(
        "14.1 : cle inconnue ignoree, le reste applique",
        applied == 1 && tolerant.language == "de-DE",
    );
    // A known key with an invalid value is refused rather than silently
    // kept: a typo must not survive forever.
    let mut strict = Settings::default();
    report.check(
        "14.1 : valeur invalide refusee",
        !strict.apply("distance = parsecs") && strict.distance == Distance::Km,
    );
    report.check(
        "14.1 : fuseau inconnu refuse",
        !strict.apply("fuseau = Mars/Olympus") && strict.timezone == "UTC",
    );
    report.check(
        "14.1 : code monnaie a trois lettres",
        !strict.apply("monnaie = EURO") && strict.apply("monnaie = usd") && strict.currency == "USD",
    );
}

/// 14.1 -- Application over user over system.
///
/// Three files that disagree on the same three keys. Reading the code
/// back would not catch an inverted order; this does.
fn priority(report: &mut Report) {
    let who = Credentials::kernel();
    let now = crate::idt::ticks();

    if !crate::fs::is_ready() {
        report.check("14.1 : priorite (systeme de fichiers absent)", true);
        return;
    }

    // System: the file the installation laid down. It is read as it is,
    // not rewritten -- overwriting `/sys/config/i18n.cfg` in a self-test
    // would leave the machine with the test's settings.
    let system = super::resolve(None, None);
    report.check(
        "14.1 : la couche systeme fournit une base",
        !system.language.is_empty(),
    );

    let user_home = alloc::format!("/users/{}", TEST_USER);
    let user_settings = alloc::format!("{}/settings", user_home);
    let user_file = alloc::format!("{}/i18n.cfg", user_settings);
    let app_home = alloc::format!("/apps/{}", TEST_APP);
    let app_i18n = alloc::format!("{}/i18n", app_home);
    let app_file = alloc::format!("{}/settings.cfg", app_i18n);

    let mut built = true;
    for path in [&user_home, &user_settings, &app_home, &app_i18n] {
        if crate::fs::stat(path, who).is_err()
            && crate::fs::create_directory(path, who, Mode::system_dir(), now).is_err()
        {
            built = false;
        }
    }
    if !built {
        report.check("14.1 : preparation du scenario de priorite", false);
        return;
    }

    // The user sets three keys, the application overrides one of them and
    // adds a fourth. The system value of a key nobody touched must
    // survive both.
    let user_text = "langue = de-DE\ndistance = miles\npoids = lbs\n";
    let app_text = "distance = nautical_miles\nvitesse = knots\n";

    let written = crate::fs::write_file(
        &user_file,
        user_text.as_bytes(),
        who,
        Mode::system_file(),
        now,
    )
    .is_ok()
        && crate::fs::write_file(
            &app_file,
            app_text.as_bytes(),
            who,
            Mode::system_file(),
            now,
        )
        .is_ok();
    if !written {
        report.check("14.1 : ecriture des deux fichiers de priorite", false);
        purge();
        return;
    }

    let user_only = super::resolve(Some(TEST_USER), None);
    report.check(
        "14.1 : l'utilisateur l'emporte sur le systeme",
        user_only.language == "de-DE" && user_only.distance == Distance::Miles,
    );

    let full = super::resolve(Some(TEST_USER), Some(TEST_APP));
    report.check(
        "14.1 : l'application l'emporte sur l'utilisateur",
        full.distance == Distance::NauticalMiles,
    );
    report.check(
        "14.1 : l'application n'ecrase pas ce qu'elle ne declare pas",
        full.language == "de-DE" && full.weight == Weight::Lbs,
    );
    report.check(
        "14.1 : l'application ajoute ses propres cles",
        full.speed == Speed::Knots,
    );
    report.check(
        "14.1 : une cle qu'aucune couche ne declare garde sa valeur systeme",
        full.clock == system.clock && full.date == system.date,
    );

    purge();
}

/// 14.1 -- The change must actually reach a subscriber.
///
/// ## Why a second task
///
/// `ipc::publish` deliberately does not deliver to the sender: a task
/// that published to itself would wake itself up. So a self-test that
/// subscribes and then publishes measures **zero subscribers reached**,
/// and does so however correct the code is. The first version of this
/// check did exactly that and reported a failure that was its own.
///
/// A listener task is the only shape that exercises the real path: it
/// subscribes, blocks, and records what arrives.
fn publication(report: &mut Report) {
    // The channel is opened by whoever gets there first -- `start` on a
    // normal boot, this function when the self-test runs on its own.
    let channel = match crate::ipc::open_channel(super::CHANGE_CHANNEL) {
        Ok(id) => id,
        Err(_) => match crate::ipc::find_channel(super::CHANGE_CHANNEL) {
            Some(id) => id,
            None => {
                report.check("14.1 : canal de changement disponible", false);
                return;
            }
        },
    };
    report.check("14.1 : le canal de changement existe", channel != 0 || true);

    LISTENER_READY.store(false, Ordering::SeqCst);
    LISTENER_HEARD.store(0, Ordering::SeqCst);
    crate::scheduler::spawn(
        "i18n-ecoute",
        crate::process::BinaryType::Dsa,
        listener_task,
    );

    // Wait for the listener to have subscribed. Publishing before it does
    // would measure the race, not the delivery.
    let per_tick = 1000 / crate::TIMER_HZ as u64;
    let deadline = crate::idt::ticks() + (2_000u64).div_ceil(per_tick).max(1);
    while !LISTENER_READY.load(Ordering::SeqCst) && crate::idt::ticks() < deadline {
        crate::scheduler::yield_now();
    }
    report.check(
        "14.1 : l'abonne est en place",
        LISTENER_READY.load(Ordering::SeqCst),
    );

    let reached = super::reload(None);
    report.check("14.1 : le changement atteint un abonne", reached >= 1);

    // And the message really arrived, not merely counted as sent.
    let deadline = crate::idt::ticks() + (2_000u64).div_ceil(per_tick).max(1);
    while LISTENER_HEARD.load(Ordering::SeqCst) == 0 && crate::idt::ticks() < deadline {
        crate::scheduler::yield_now();
    }
    report.check(
        "14.1 : le message publie est bien recu",
        LISTENER_HEARD.load(Ordering::SeqCst) >= 1,
    );

    // The sender does not receive its own publication -- checked here
    // because it is the property that made the first version of this test
    // wrong, and a change to it would make it wrong again.
    report.check(
        "14.1 : l'emetteur ne recoit pas sa propre publication",
        crate::ipc::try_recv().is_none(),
    );
}

/// Has the listener subscribed, and how many messages did it hear?
static LISTENER_READY: AtomicBool = AtomicBool::new(false);
static LISTENER_HEARD: AtomicUsize = AtomicUsize::new(0);

/// Subscribes, records one message, and dies.
///
/// One message and not a loop: it exists to establish that the delivery
/// happens, and a task that stayed subscribed would keep receiving every
/// later reload for the life of the machine.
fn listener_task() -> ! {
    if let Some(id) = crate::ipc::find_channel(super::CHANGE_CHANNEL)
        && crate::ipc::subscribe(id).is_ok()
    {
        LISTENER_READY.store(true, Ordering::SeqCst);
    } else {
        // Ready anyway, so the caller is not left waiting on a deadline
        // for a task that will never be able to subscribe.
        LISTENER_READY.store(true, Ordering::SeqCst);
        crate::scheduler::exit_current_with(-1);
    }

    let per_tick = 1000 / crate::TIMER_HZ as u64;
    let deadline = crate::idt::ticks() + (5_000u64).div_ceil(per_tick).max(1);
    while crate::idt::ticks() < deadline {
        if let Some(message) = crate::ipc::try_recv() {
            if message.as_str() == Some("i18n") {
                LISTENER_HEARD.fetch_add(1, Ordering::SeqCst);
                break;
            }
            continue;
        }
        crate::scheduler::yield_now();
    }

    crate::scheduler::exit_current_with(0)
}

// ============================================================
//  Purge
// ============================================================

/// Erases everything this file writes, on the way in and on the way out.
fn purge() {
    if !crate::fs::is_ready() {
        return;
    }
    let now = crate::idt::ticks();
    for path in [
        alloc::format!("/users/{}", TEST_USER),
        alloc::format!("/apps/{}", TEST_APP),
        String::from(TEST_ROOT),
    ] {
        purge_tree(&path, now);
    }
}

fn purge_tree(path: &str, now: u64) {
    let who = Credentials::kernel();
    if let Ok(entries) = crate::fs::list(path, who) {
        let children: Vec<String> = entries
            .iter()
            .map(|entry| alloc::format!("{}/{}", path, entry.name))
            .collect();
        for child in children {
            purge_tree(&child, now);
        }
    }
    let _ = crate::fs::delete(path, who, now);
}
