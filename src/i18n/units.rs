//! 10.2.3 -- Formatting of dates, times, numbers and units (spec 14.2).
//!
//! ## Why everything is an integer
//!
//! The kernel runs soft-float and SSE is never enabled: an `f64` here
//! would be an `f64` in the kernel. So every physical quantity is carried
//! as an integer in a **milli-unit of the SI base**:
//!
//! | Domain | Carried in | Base unit |
//! |---|---|---|
//! | Distance | millimetres | metre |
//! | Weight | milligrams | gram |
//! | Temperature | milli-degrees Celsius | degree Celsius |
//! | Speed | millimetres per second | metre per second |
//!
//! A conversion produces **thousandths of the target unit**, and the
//! decimal point is placed by hand when the string is built. That is
//! exactly what a fixed-point representation is, and it is enough here:
//! nothing in 14.2 asks for more than three decimals.
//!
//! ## Where the spec leaves the choice
//!
//! 14.2 lists the thousands separator (space, comma, dot) and says
//! nothing about the **decimal** separator. Leaving it fixed would print
//! `1,234,56` in English. The rule taken here, and it is a choice:
//! a comma used for the thousands forces a dot for the decimals, any
//! other thousands separator forces a comma. It is the convention of the
//! two families of locales, and it is the only one that never produces an
//! ambiguous number.

use alloc::string::String;

// ============================================================
//  Options of the 14.2 table
// ============================================================

/// Date order (14.2, "Formats date" row).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateFormat {
    /// `DD/MM/YYYY`
    Dmy,
    /// `MM/DD/YYYY`
    Mdy,
    /// `YYYY-MM-DD`
    Ymd,
}

impl DateFormat {
    pub fn as_str(&self) -> &'static str {
        match self {
            DateFormat::Dmy => "DD/MM/YYYY",
            DateFormat::Mdy => "MM/DD/YYYY",
            DateFormat::Ymd => "YYYY-MM-DD",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "DD/MM/YYYY" | "dmy" => Some(DateFormat::Dmy),
            "MM/DD/YYYY" | "mdy" => Some(DateFormat::Mdy),
            "YYYY-MM-DD" | "ymd" => Some(DateFormat::Ymd),
            _ => None,
        }
    }

    /// The separator goes with the order: ISO writes dashes, the two
    /// others slashes. Mixing them would give `2026-09/02`.
    fn separator(&self) -> char {
        match self {
            DateFormat::Ymd => '-',
            _ => '/',
        }
    }
}

/// Clock (14.2, "Formats heure" row).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockFormat {
    H24,
    H12,
}

impl ClockFormat {
    pub fn as_str(&self) -> &'static str {
        match self {
            ClockFormat::H24 => "24h",
            ClockFormat::H12 => "12h",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "24h" | "24" => Some(ClockFormat::H24),
            "12h" | "12" => Some(ClockFormat::H12),
            _ => None,
        }
    }
}

/// Thousands separator (14.2, "Nombres" row).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Separator {
    Space,
    Comma,
    Dot,
}

impl Separator {
    pub fn as_str(&self) -> &'static str {
        match self {
            Separator::Space => "espace",
            Separator::Comma => "virgule",
            Separator::Dot => "point",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "espace" | "space" => Some(Separator::Space),
            "virgule" | "comma" => Some(Separator::Comma),
            "point" | "dot" => Some(Separator::Dot),
            _ => None,
        }
    }

    pub fn glyph(&self) -> char {
        match self {
            Separator::Space => ' ',
            Separator::Comma => ',',
            Separator::Dot => '.',
        }
    }

    /// Decimal separator implied by the thousands one -- see the module
    /// header for why this is derived rather than declared.
    pub fn decimal(&self) -> char {
        match self {
            Separator::Comma => '.',
            _ => ',',
        }
    }
}

/// Position of the currency code relative to the amount (14.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CurrencyPosition {
    Before,
    After,
}

impl CurrencyPosition {
    pub fn as_str(&self) -> &'static str {
        match self {
            CurrencyPosition::Before => "avant",
            CurrencyPosition::After => "apres",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "avant" | "before" => Some(CurrencyPosition::Before),
            "apres" | "after" => Some(CurrencyPosition::After),
            _ => None,
        }
    }
}

/// Distance unit (14.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Distance {
    Km,
    Miles,
    NauticalMiles,
}

/// Weight unit (14.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Weight {
    Kg,
    Lbs,
}

/// Temperature unit (14.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Temperature {
    Celsius,
    Fahrenheit,
    Kelvin,
}

/// Speed unit (14.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Speed {
    Kmh,
    Mph,
    Knots,
}

impl Distance {
    pub fn as_str(&self) -> &'static str {
        match self {
            Distance::Km => "km",
            Distance::Miles => "miles",
            Distance::NauticalMiles => "nautical_miles",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "km" => Some(Distance::Km),
            "miles" => Some(Distance::Miles),
            "nautical_miles" | "nm" => Some(Distance::NauticalMiles),
            _ => None,
        }
    }

    /// Symbol shown after the value.
    pub fn symbol(&self) -> &'static str {
        match self {
            Distance::Km => "km",
            Distance::Miles => "mi",
            Distance::NauticalMiles => "NM",
        }
    }

    /// Millimetres -> thousandths of the unit.
    ///
    /// 1 mile = 1 609 344 mm and 1 nautical mile = 1 852 000 mm, both
    /// exact by definition: the international mile and the nautical mile
    /// are defined from the metre, not measured against it.
    pub fn from_millimetres(&self, millimetres: i64) -> i64 {
        match self {
            Distance::Km => millimetres / 1_000,
            Distance::Miles => millimetres.saturating_mul(1_000) / 1_609_344,
            Distance::NauticalMiles => millimetres.saturating_mul(1_000) / 1_852_000,
        }
    }
}

impl Weight {
    pub fn as_str(&self) -> &'static str {
        match self {
            Weight::Kg => "kg",
            Weight::Lbs => "lbs",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "kg" => Some(Weight::Kg),
            "lbs" | "lb" => Some(Weight::Lbs),
            _ => None,
        }
    }

    pub fn symbol(&self) -> &'static str {
        match self {
            Weight::Kg => "kg",
            Weight::Lbs => "lb",
        }
    }

    /// Milligrams -> thousandths of the unit.
    ///
    /// 1 lb = 453 592.37 mg exactly, hence the multiplication by 100 000
    /// against 45 359 237: writing 453 592 would drift by 0.8 g per tonne.
    pub fn from_milligrams(&self, milligrams: i64) -> i64 {
        match self {
            Weight::Kg => milligrams / 1_000,
            Weight::Lbs => milligrams.saturating_mul(100_000) / 45_359_237,
        }
    }
}

impl Temperature {
    pub fn as_str(&self) -> &'static str {
        match self {
            Temperature::Celsius => "celsius",
            Temperature::Fahrenheit => "fahrenheit",
            Temperature::Kelvin => "kelvin",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "celsius" | "C" => Some(Temperature::Celsius),
            "fahrenheit" | "F" => Some(Temperature::Fahrenheit),
            "kelvin" | "K" => Some(Temperature::Kelvin),
            _ => None,
        }
    }

    /// No degree sign: it is outside the 0x20-0x7E range the display
    /// accepts, and the VGA writer would turn it into a solid square.
    pub fn symbol(&self) -> &'static str {
        match self {
            Temperature::Celsius => "C",
            Temperature::Fahrenheit => "F",
            Temperature::Kelvin => "K",
        }
    }

    /// Milli-Celsius -> thousandths of the unit.
    pub fn from_milli_celsius(&self, milli: i64) -> i64 {
        match self {
            Temperature::Celsius => milli,
            Temperature::Fahrenheit => milli.saturating_mul(9) / 5 + 32_000,
            Temperature::Kelvin => milli + 273_150,
        }
    }
}

impl Speed {
    pub fn as_str(&self) -> &'static str {
        match self {
            Speed::Kmh => "km/h",
            Speed::Mph => "mph",
            Speed::Knots => "knots",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "km/h" | "kmh" => Some(Speed::Kmh),
            "mph" => Some(Speed::Mph),
            "knots" | "kn" => Some(Speed::Knots),
            _ => None,
        }
    }

    pub fn symbol(&self) -> &'static str {
        match self {
            Speed::Kmh => "km/h",
            Speed::Mph => "mph",
            Speed::Knots => "kn",
        }
    }

    /// Millimetres per second -> thousandths of the unit.
    pub fn from_millimetres_per_second(&self, millimetres: i64) -> i64 {
        match self {
            // mm/s * 3600 / 1e6 km, in thousandths: * 3600 / 1000.
            Speed::Kmh => millimetres.saturating_mul(18) / 5,
            Speed::Mph => millimetres.saturating_mul(3_600_000) / 1_609_344,
            Speed::Knots => millimetres.saturating_mul(3_600) / 1_852,
        }
    }
}

// ============================================================
//  Numbers
// ============================================================

/// Groups the integer part in threes and inserts the separator.
///
/// The grouping runs from the **right**, which is why the digits are
/// pushed in reverse then flipped: grouping from the left would put the
/// odd group at the wrong end and print `123,456,7`.
pub fn group_digits(value: u64, separator: Separator) -> String {
    let mut digits = String::new();
    let mut rest = value;
    let mut count = 0;

    loop {
        if count == 3 {
            digits.push(separator.glyph());
            count = 0;
        }
        digits.push((b'0' + (rest % 10) as u8) as char);
        rest /= 10;
        count += 1;
        if rest == 0 {
            break;
        }
    }

    digits.chars().rev().collect()
}

/// Formats a fixed-point value carried in thousandths.
///
/// `decimals` says how many are kept, from zero to three. Rounding is
/// half away from zero on the discarded digits: truncating would show
/// 0.999 as 0.99, which reads as a measurement error rather than as a
/// display choice.
pub fn format_milli(value: i64, decimals: u8, separator: Separator) -> String {
    let decimals = decimals.min(3);
    let negative = value < 0;
    let magnitude = value.unsigned_abs();

    // Rounding factor: 1 for three decimals, 10 for two, 100 for one,
    // 1000 for none.
    let factor = match decimals {
        3 => 1u64,
        2 => 10,
        1 => 100,
        _ => 1000,
    };
    let scaled = (magnitude + factor / 2) / factor;

    let unit_scale = 1000 / factor;
    let integer = scaled / unit_scale;
    let fraction = scaled % unit_scale;

    let mut out = String::new();
    if negative && (integer != 0 || fraction != 0) {
        out.push('-');
    }
    out.push_str(&group_digits(integer, separator));

    if decimals > 0 {
        out.push(separator.decimal());
        // Left-padded with zeros: 5 thousandths of a unit is `.005`, not
        // `.5`.
        let mut buffer = String::new();
        let mut rest = fraction;
        for _ in 0..decimals {
            buffer.push((b'0' + (rest % 10) as u8) as char);
            rest /= 10;
        }
        out.extend(buffer.chars().rev());
    }

    out
}

/// Formats a whole number with the thousands separator.
pub fn format_integer(value: i64, separator: Separator) -> String {
    let mut out = String::new();
    if value < 0 {
        out.push('-');
    }
    out.push_str(&group_digits(value.unsigned_abs(), separator));
    out
}

/// Formats an amount given in cents, with its currency code.
///
/// Two decimals always: an amount shown as `12,5` is read as twelve euros
/// fifty by some and as twelve euros five by others.
pub fn format_currency(
    cents: i64,
    code: &str,
    position: CurrencyPosition,
    separator: Separator,
) -> String {
    let amount = format_milli(cents.saturating_mul(10), 2, separator);
    match position {
        CurrencyPosition::Before => alloc::format!("{} {}", code, amount),
        CurrencyPosition::After => alloc::format!("{} {}", amount, code),
    }
}

// ============================================================
//  Dates and times
// ============================================================

fn two(value: u8) -> [u8; 2] {
    [b'0' + value / 10, b'0' + value % 10]
}

/// Formats a date in the requested order.
pub fn format_date(year: u16, month: u8, day: u8, format: DateFormat) -> String {
    let separator = format.separator();
    let month = two(month.min(99));
    let day = two(day.min(99));
    let month = core::str::from_utf8(&month).unwrap_or("00");
    let day = core::str::from_utf8(&day).unwrap_or("00");

    match format {
        DateFormat::Dmy => alloc::format!("{}{}{}{}{:04}", day, separator, month, separator, year),
        DateFormat::Mdy => alloc::format!("{}{}{}{}{:04}", month, separator, day, separator, year),
        DateFormat::Ymd => alloc::format!("{:04}{}{}{}{}", year, separator, month, separator, day),
    }
}

/// Formats a time of day.
///
/// The 12-hour clock maps midnight and noon onto **12**, not onto zero:
/// `00:30 AM` does not exist, it is `12:30 AM`. The modulo alone gets
/// this wrong, which is why the zero case is written out.
pub fn format_time(hour: u8, minute: u8, second: u8, format: ClockFormat) -> String {
    let minute = two(minute.min(99));
    let second = two(second.min(99));
    let minute = core::str::from_utf8(&minute).unwrap_or("00");
    let second = core::str::from_utf8(&second).unwrap_or("00");

    match format {
        ClockFormat::H24 => {
            let hour = two(hour.min(99));
            let hour = core::str::from_utf8(&hour).unwrap_or("00");
            alloc::format!("{}:{}:{}", hour, minute, second)
        }
        ClockFormat::H12 => {
            let meridiem = if hour < 12 { "AM" } else { "PM" };
            let displayed = match hour % 12 {
                0 => 12,
                other => other,
            };
            alloc::format!("{}:{}:{} {}", displayed, minute, second, meridiem)
        }
    }
}
