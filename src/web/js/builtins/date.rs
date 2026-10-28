//! `Date`.
//!
//! A date is a time value: milliseconds since 1970-01-01T00:00:00Z, or
//! NaN for an invalid date, in the `Kind::Date` of its object. The
//! calendar arithmetic is integer (proleptic Gregorian, the civil-day
//! algorithms of H. Hinnant), exact over the whole range of the
//! language, +/-100 000 000 days.
//!
//! **Local time** is UTC plus the offset the host gives
//! (`Host::tz_offset_minutes`): Nova's time zones are fixed offsets with
//! the summer variants listed apart (`i18n::tz`), so one offset holds for
//! every date -- a page sees exactly the clock of the taskbar.

use alloc::string::String;
use alloc::vec::Vec;

use super::arg;
use crate::web::js::interp::{Ctx, R};
use crate::web::js::num::Num;
use crate::web::js::ops::{self, Hint};
use crate::web::js::value::*;

const MS_PER_DAY: i64 = 86_400_000;
/// The range of time values: 8.64e15 ms either side of the epoch.
const MAX_TIME: i64 = 8_640_000_000_000_000;

const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

// ------------------------------------------------------------
//  Calendar
// ------------------------------------------------------------

/// Days since the epoch of a civil date (month 1..=12).
pub fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month_index = (month + 9) % 12;
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The civil date of a day count: (year, month 1..=12, day 1..=31).
pub fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z - era * 146_097;
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 { month_index + 3 } else { month_index - 9 };
    let year = year_of_era + era * 400 + if month <= 2 { 1 } else { 0 };
    (year, month, day)
}

/// A time value broken down: year, month 0..=11, day, hours, minutes,
/// seconds, milliseconds, weekday 0..=6 (Sunday first).
#[derive(Clone, Copy)]
pub struct Fields {
    pub year: i64,
    pub month: i64,
    pub day: i64,
    pub hours: i64,
    pub minutes: i64,
    pub seconds: i64,
    pub ms: i64,
    pub weekday: i64,
}

pub fn fields(t: i64) -> Fields {
    let days = t.div_euclid(MS_PER_DAY);
    let in_day = t.rem_euclid(MS_PER_DAY);
    let (year, month, day) = civil_from_days(days);
    Fields {
        year,
        month: month - 1,
        day,
        hours: in_day / 3_600_000,
        minutes: in_day / 60_000 % 60,
        seconds: in_day / 1000 % 60,
        ms: in_day % 1000,
        weekday: (days + 4).rem_euclid(7),
    }
}

/// `MakeDay` + `MakeTime` + `MakeDate` on integers; months and days out
/// of range carry over, as the language requires (`new Date(2026, 13)`).
fn make_time(year: i64, month: i64, day: i64, hours: i64, minutes: i64, seconds: i64, ms: i64) -> Option<i64> {
    let year = year.checked_add(month.div_euclid(12))?;
    let month = month.rem_euclid(12);
    if year.abs() > 400_000 {
        return None;
    }
    let days = days_from_civil(year, month + 1, 1).checked_add(day.checked_sub(1)?)?;
    let time = hours
        .checked_mul(3_600_000)?
        .checked_add(minutes.checked_mul(60_000)?)?
        .checked_add(seconds.checked_mul(1000)?)?
        .checked_add(ms)?;
    days.checked_mul(MS_PER_DAY)?.checked_add(time)
}

/// `TimeClip`.
fn clip(t: Option<i64>) -> Num {
    match t {
        Some(t) if t.abs() <= MAX_TIME => Num::from_i64(t),
        _ => Num::NAN,
    }
}

fn valid(time: Num) -> Option<i64> {
    if time.is_nan() {
        None
    } else {
        Some(time.to_i64_saturating())
    }
}

fn offset_ms(ctx: &mut Ctx) -> i64 {
    ctx.host.tz_offset_minutes() as i64 * 60_000
}

// ------------------------------------------------------------
//  Formatting
// ------------------------------------------------------------

fn year_text(year: i64) -> String {
    if (0..=9999).contains(&year) {
        alloc::format!("{:04}", year)
    } else {
        alloc::format!("{}{:06}", if year < 0 { '-' } else { '+' }, year.abs())
    }
}

/// `toISOString` form, for the console and `toJSON`.
pub fn iso_string(time: Num) -> Option<String> {
    let t = valid(time)?;
    let f = fields(t);
    Some(alloc::format!(
        "{}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        year_text(f.year),
        f.month + 1,
        f.day,
        f.hours,
        f.minutes,
        f.seconds,
        f.ms
    ))
}

fn plain_year(year: i64) -> String {
    if year < 0 {
        alloc::format!("-{:06}", -year)
    } else {
        alloc::format!("{:04}", year)
    }
}

fn offset_text(offset_minutes: i64, colon: bool) -> String {
    let sign = if offset_minutes < 0 { '-' } else { '+' };
    let a = offset_minutes.abs();
    if colon {
        alloc::format!("{}{:02}:{:02}", sign, a / 60, a % 60)
    } else {
        alloc::format!("{}{:02}{:02}", sign, a / 60, a % 60)
    }
}

fn date_part(f: &Fields) -> String {
    alloc::format!("{} {} {:02} {}", DAYS[f.weekday as usize], MONTHS[f.month as usize], f.day, plain_year(f.year))
}

fn time_part(f: &Fields, offset_minutes: i64) -> String {
    alloc::format!(
        "{:02}:{:02}:{:02} GMT{}",
        f.hours,
        f.minutes,
        f.seconds,
        offset_text(offset_minutes, false)
    )
}

/// `toLocaleDateString` and friends, in the default `en-US` form.
fn locale_date(f: &Fields) -> String {
    alloc::format!("{}/{}/{}", f.month + 1, f.day, f.year)
}

fn locale_time(f: &Fields) -> String {
    let hour = match f.hours % 12 {
        0 => 12,
        h => h,
    };
    alloc::format!("{}:{:02}:{:02} {}", hour, f.minutes, f.seconds, if f.hours < 12 { "AM" } else { "PM" })
}

// ------------------------------------------------------------
//  Parsing
// ------------------------------------------------------------

/// Reads the date-time string format of the language (a subset of ISO
/// 8601), then the forms `toString` and `toUTCString` print, and the
/// usual "Oct 28, 2026 10:00" variants browsers accept.
pub fn parse_date(text: &str, local_offset_ms: i64) -> Option<i64> {
    let text = text.trim();
    parse_iso(text, local_offset_ms).or_else(|| parse_loose(text, local_offset_ms))
}

fn digits(bytes: &[u8], at: usize, count: usize) -> Option<i64> {
    let slice = bytes.get(at..at + count)?;
    if !slice.iter().all(u8::is_ascii_digit) {
        return None;
    }
    Some(slice.iter().fold(0i64, |v, d| v * 10 + (d - b'0') as i64))
}

fn parse_iso(text: &str, local_offset_ms: i64) -> Option<i64> {
    let b = text.as_bytes();
    let (year, mut i) = match b.first()? {
        b'+' | b'-' => {
            let value = digits(b, 1, 6)?;
            // -000000 is not a year (the specification says so).
            if b[0] == b'-' && value == 0 {
                return None;
            }
            (if b[0] == b'-' { -value } else { value }, 7)
        }
        _ => (digits(b, 0, 4)?, 4),
    };
    let (mut month, mut day) = (1, 1);
    if b.get(i) == Some(&b'-') {
        month = digits(b, i + 1, 2)?;
        i += 3;
        if b.get(i) == Some(&b'-') {
            day = digits(b, i + 1, 2)?;
            i += 3;
        }
    }
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let (mut hours, mut minutes, mut seconds, mut ms) = (0, 0, 0, 0);
    let mut has_time = false;
    let mut offset: Option<i64> = None;
    if matches!(b.get(i), Some(b'T') | Some(b't') | Some(b' ')) && i + 1 < b.len() {
        has_time = true;
        hours = digits(b, i + 1, 2)?;
        if b.get(i + 3) != Some(&b':') {
            return None;
        }
        minutes = digits(b, i + 4, 2)?;
        i += 6;
        if b.get(i) == Some(&b':') {
            seconds = digits(b, i + 1, 2)?;
            i += 3;
            if matches!(b.get(i), Some(b'.') | Some(b',')) {
                let start = i + 1;
                let mut end = start;
                while end < b.len() && b[end].is_ascii_digit() {
                    end += 1;
                }
                if end == start {
                    return None;
                }
                // Milliseconds: the first three digits, the rest ignored.
                let mut value = 0;
                for k in 0..3 {
                    value = value * 10 + b.get(start + k).filter(|d| d.is_ascii_digit()).map_or(0, |d| (d - b'0') as i64);
                }
                ms = value;
                i = end;
            }
        }
        if hours > 24 || minutes > 59 || seconds > 59 || (hours == 24 && (minutes | seconds | ms) != 0) {
            return None;
        }
        match b.get(i) {
            Some(b'Z') | Some(b'z') => {
                offset = Some(0);
                i += 1;
            }
            Some(sign @ (b'+' | b'-')) => {
                let h = digits(b, i + 1, 2)?;
                let (m, used) = if b.get(i + 3) == Some(&b':') { (digits(b, i + 4, 2)?, 6) } else { (digits(b, i + 3, 2)?, 5) };
                let value = (h * 60 + m) * 60_000;
                offset = Some(if *sign == b'-' { -value } else { value });
                i += used;
            }
            _ => {}
        }
    }
    if i != b.len() {
        return None;
    }
    let t = make_time(year, month - 1, day, hours, minutes, seconds, ms)?;
    // A date alone is UTC; a date-time without offset is local time.
    let offset = offset.unwrap_or(if has_time { local_offset_ms } else { 0 });
    Some(t - offset)
}

fn month_of(word: &str) -> Option<i64> {
    let lower = word.to_ascii_lowercase();
    MONTHS.iter().position(|m| lower.starts_with(&m.to_ascii_lowercase())).map(|p| p as i64)
}

fn parse_loose(text: &str, local_offset_ms: i64) -> Option<i64> {
    let mut numbers: Vec<i64> = Vec::new();
    let mut month: Option<i64> = None;
    let mut time: Option<(i64, i64, i64)> = None;
    let mut offset: Option<i64> = None;
    let mut pm: Option<bool> = None;
    let cleaned: String = text.chars().map(|c| if c == ',' { ' ' } else { c }).collect();
    let mut tokens = cleaned.split_whitespace().peekable();
    while let Some(token) = tokens.next() {
        if token.starts_with('(') {
            // "(Central European Time)": a comment.
            if !token.ends_with(')') {
                for rest in tokens.by_ref() {
                    if rest.ends_with(')') {
                        break;
                    }
                }
            }
            continue;
        }
        let upper = token.to_ascii_uppercase();
        if upper == "AM" || upper == "PM" {
            pm = Some(upper == "PM");
        } else if upper == "Z" || upper == "UTC" || upper == "GMT" || upper == "UT" {
            offset = Some(0);
        } else if let Some(rest) = upper.strip_prefix("GMT").or_else(|| upper.strip_prefix("UTC")) {
            offset = Some(parse_offset(rest)?);
        } else if (token.starts_with('+') || token.starts_with('-')) && token.len() >= 3 && time.is_some() {
            offset = Some(parse_offset(token)?);
        } else if token.contains(':') {
            let mut parts = token.split(':');
            let h = parts.next()?.parse::<i64>().ok()?;
            let m = parts.next()?.parse::<i64>().ok()?;
            let s = match parts.next() {
                Some(s) => s.split('.').next()?.parse::<i64>().ok()?,
                None => 0,
            };
            time = Some((h, m, s));
        } else if token.contains('/') {
            // 10/28/2026: month first, as browsers read it.
            let parts: Vec<&str> = token.split('/').collect();
            if parts.len() != 3 {
                return None;
            }
            month = Some(parts[0].parse::<i64>().ok()? - 1);
            numbers.push(parts[1].parse::<i64>().ok()?);
            numbers.push(parts[2].parse::<i64>().ok()?);
        } else if let Ok(n) = token.parse::<i64>() {
            numbers.push(n);
        } else if let Some(m) = month_of(token) {
            month = Some(m);
        } else if DAYS.iter().any(|d| upper.starts_with(&d.to_ascii_uppercase())) {
            // A weekday name says nothing the date does not.
        } else {
            return None;
        }
    }
    let month = month?;
    let (day, year) = match numbers.as_slice() {
        [day, year] if *day <= 31 => (*day, *year),
        [year, day] => (*day, *year),
        _ => return None,
    };
    let year = if (0..50).contains(&year) {
        year + 2000
    } else if (50..100).contains(&year) {
        year + 1900
    } else {
        year
    };
    let (mut h, m, s) = time.unwrap_or((0, 0, 0));
    match pm {
        Some(true) if h < 12 => h += 12,
        Some(false) if h == 12 => h = 0,
        _ => {}
    }
    let t = make_time(year, month, day, h, m, s, 0)?;
    Some(t - offset.unwrap_or(local_offset_ms))
}

/// "+0100", "+01:00", "-5".
fn parse_offset(text: &str) -> Option<i64> {
    let (sign, rest) = match text.as_bytes().first()? {
        b'+' => (1, &text[1..]),
        b'-' => (-1, &text[1..]),
        _ => return None,
    };
    let rest: String = rest.chars().filter(|c| *c != ':').collect();
    let value: i64 = rest.parse().ok()?;
    let minutes = if rest.len() <= 2 { value * 60 } else { (value / 100) * 60 + value % 100 };
    Some(sign * minutes * 60_000)
}

// ------------------------------------------------------------
//  Constructor and statics
// ------------------------------------------------------------

fn this_time(ctx: &mut Ctx, this: &Value) -> R<Num> {
    if let Value::Obj(object) = this {
        if let Kind::Date(t) = object.borrow().kind {
            return Ok(t);
        }
    }
    ctx.throw_type("this is not a Date object.")
}

fn set_this_time(this: &Value, time: Num) {
    if let Value::Obj(object) = this {
        object.borrow_mut().kind = Kind::Date(time);
    }
}

/// The integer of an argument; `None` for NaN or infinity.
fn integer_arg(ctx: &mut Ctx, value: &Value) -> R<Option<i64>> {
    let n = ops::to_number(ctx, value)?;
    Ok(if n.is_finite() { Some(n.trunc().to_i64_saturating()) } else { None })
}

/// The seven components of `new Date(y, m, ...)` and `Date.UTC`.
fn components(ctx: &mut Ctx, args: &[Value]) -> R<Option<i64>> {
    let mut values = [0i64, 0, 1, 0, 0, 0, 0];
    for (index, slot) in values.iter_mut().enumerate() {
        if index >= args.len() {
            break;
        }
        match integer_arg(ctx, &args[index])? {
            Some(v) => *slot = v,
            None => return Ok(None),
        }
    }
    if (0..=99).contains(&values[0]) {
        values[0] += 1900;
    }
    Ok(make_time(values[0], values[1], values[2], values[3], values[4], values[5], values[6]))
}

fn date_constructor(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    if ctx.new_target.is_none() {
        let now = ctx.now();
        let offset = offset_ms(ctx);
        return Ok(Value::Str(JsStr::from(to_string_text(Num::from_i64(now), offset))));
    }
    let time = match args.len() {
        0 => Num::from_i64(ctx.now()),
        1 => {
            let value = &args[0];
            let existing = match value {
                Value::Obj(object) => match object.borrow().kind {
                    Kind::Date(t) => Some(t),
                    _ => None,
                },
                _ => None,
            };
            match existing {
                Some(t) => t,
                None => {
                    let primitive = ops::to_primitive(ctx, value, Hint::Default)?;
                    match primitive {
                        Value::Str(s) => {
                            let offset = offset_ms(ctx);
                            clip(parse_date(&s, offset))
                        }
                        other => {
                            let n = ops::to_number(ctx, &other)?;
                            if n.is_finite() { clip(Some(n.trunc().to_i64_saturating())) } else { Num::NAN }
                        }
                    }
                }
            }
        }
        _ => {
            let local = components(ctx, args)?;
            let offset = offset_ms(ctx);
            clip(local.map(|t| t - offset))
        }
    };
    let proto = ctx.proto_for_new(ctx.realm.protos.date.clone())?;
    Ok(Value::Obj(ctx.alloc(Kind::Date(time), Some(proto), "Date")))
}

fn now(ctx: &mut Ctx, _: &Value, _: &[Value], _: &Value) -> R<Value> {
    Ok(Value::num(ctx.now()))
}

fn parse(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    let text = ops::to_string(ctx, &arg(args, 0))?;
    let offset = offset_ms(ctx);
    Ok(Value::Num(clip(parse_date(&text, offset))))
}

fn utc(ctx: &mut Ctx, _: &Value, args: &[Value], _: &Value) -> R<Value> {
    Ok(Value::Num(clip(components(ctx, args)?)))
}

// ------------------------------------------------------------
//  Getters
// ------------------------------------------------------------

/// Which field a getter reads; the data of each getter native.
fn field_of(f: &Fields, which: i64) -> i64 {
    match which {
        0 => f.year,
        1 => f.month,
        2 => f.day,
        3 => f.weekday,
        4 => f.hours,
        5 => f.minutes,
        6 => f.seconds,
        _ => f.ms,
    }
}

fn which(data: &Value) -> (i64, bool) {
    match data {
        Value::Num(n) => {
            let v = n.to_i64_saturating();
            (v % 100, v >= 100)
        }
        _ => (0, false),
    }
}

fn getter(ctx: &mut Ctx, this: &Value, _: &[Value], data: &Value) -> R<Value> {
    let time = this_time(ctx, this)?;
    let Some(t) = valid(time) else {
        return Ok(Value::Num(Num::NAN));
    };
    let (field, utc) = which(data);
    let t = if utc { t } else { t + offset_ms(ctx) };
    Ok(Value::num(field_of(&fields(t), field)))
}

fn get_time(ctx: &mut Ctx, this: &Value, _: &[Value], _: &Value) -> R<Value> {
    Ok(Value::Num(this_time(ctx, this)?))
}

fn get_timezone_offset(ctx: &mut Ctx, this: &Value, _: &[Value], _: &Value) -> R<Value> {
    let time = this_time(ctx, this)?;
    if time.is_nan() {
        return Ok(Value::Num(Num::NAN));
    }
    Ok(Value::num(-(ctx.host.tz_offset_minutes() as i64)))
}

fn get_year(ctx: &mut Ctx, this: &Value, _: &[Value], _: &Value) -> R<Value> {
    let time = this_time(ctx, this)?;
    let Some(t) = valid(time) else {
        return Ok(Value::Num(Num::NAN));
    };
    let t = t + offset_ms(ctx);
    Ok(Value::num(fields(t).year - 1900))
}

// ------------------------------------------------------------
//  Setters
// ------------------------------------------------------------

/// The setters: data = first field set (0 year .. 6 ms) + 100 for UTC.
/// Each takes the following fields too (`setHours(h, m, s, ms)`).
fn setter(ctx: &mut Ctx, this: &Value, args: &[Value], data: &Value) -> R<Value> {
    let time = this_time(ctx, this)?;
    let (first, utc) = which(data);
    let offset = if utc { 0 } else { offset_ms(ctx) };
    // setFullYear on an invalid date starts from +0; every other setter
    // leaves an invalid date invalid.
    let base = match valid(time) {
        Some(t) => t + offset,
        None if first == 0 => 0,
        None => {
            for value in args {
                ops::to_number(ctx, value)?;
            }
            return Ok(Value::Num(Num::NAN));
        }
    };
    let f = fields(base);
    // year, month, day, hours, minutes, seconds, ms -- field indices of
    // the setter, skipping the weekday.
    let mut values = [f.year, f.month, f.day, f.hours, f.minutes, f.seconds, f.ms];
    let start = match first {
        0 => 0,
        1 => 1,
        2 => 2,
        4 => 3,
        5 => 4,
        6 => 5,
        _ => 6,
    };
    let count = match start {
        0 => 3,
        1 => 2,
        2 => 1,
        3 => 4,
        4 => 3,
        5 => 2,
        _ => 1,
    };
    let mut invalid = args.is_empty();
    for k in 0..count.min(args.len().max(1)) {
        match integer_arg(ctx, &arg(args, k))? {
            Some(v) => values[start + k] = v,
            None => invalid = true,
        }
    }
    let new_time = if invalid {
        Num::NAN
    } else {
        clip(make_time(values[0], values[1], values[2], values[3], values[4], values[5], values[6]).map(|t| t - offset))
    };
    set_this_time(this, new_time);
    Ok(Value::Num(new_time))
}

fn set_time(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    this_time(ctx, this)?;
    let n = ops::to_number(ctx, &arg(args, 0))?;
    let time = if n.is_finite() { clip(Some(n.trunc().to_i64_saturating())) } else { Num::NAN };
    set_this_time(this, time);
    Ok(Value::Num(time))
}

// ------------------------------------------------------------
//  Strings
// ------------------------------------------------------------

fn to_string_text(time: Num, offset_ms: i64) -> String {
    match valid(time) {
        None => String::from("Invalid Date"),
        Some(t) => {
            let f = fields(t + offset_ms);
            alloc::format!("{} {}", date_part(&f), time_part(&f, offset_ms / 60_000))
        }
    }
}

/// The string methods: data selects the form.
fn string_form(ctx: &mut Ctx, this: &Value, _: &[Value], data: &Value) -> R<Value> {
    let time = this_time(ctx, this)?;
    let form = match data {
        Value::Num(n) => n.to_i64_saturating(),
        _ => 0,
    };
    let Some(t) = valid(time) else {
        if form == 3 {
            return ctx.throw_range("Invalid time value");
        }
        return Ok(Value::str("Invalid Date"));
    };
    let offset = offset_ms(ctx);
    let local = fields(t + offset);
    let text = match form {
        0 => to_string_text(time, offset),
        1 => date_part(&local),
        2 => time_part(&local, offset / 60_000),
        3 => iso_string(time).unwrap_or_default(),
        4 => {
            let f = fields(t);
            alloc::format!(
                "{}, {:02} {} {} {:02}:{:02}:{:02} GMT",
                DAYS[f.weekday as usize],
                f.day,
                MONTHS[f.month as usize],
                plain_year(f.year),
                f.hours,
                f.minutes,
                f.seconds
            )
        }
        5 => alloc::format!("{}, {}", locale_date(&local), locale_time(&local)),
        6 => locale_date(&local),
        _ => locale_time(&local),
    };
    Ok(Value::Str(JsStr::from(text)))
}

/// `toJSON`: `toISOString`, or null for an invalid date.
fn to_json(ctx: &mut Ctx, this: &Value, _: &[Value], _: &Value) -> R<Value> {
    let primitive = ops::to_primitive(ctx, this, Hint::Number)?;
    if let Value::Num(n) = primitive {
        if !n.is_finite() {
            return Ok(Value::Null);
        }
    }
    let method = ops::get(ctx, this, "toISOString")?;
    ctx.call(&method, this.clone(), &[])
}

fn value_of(ctx: &mut Ctx, this: &Value, _: &[Value], _: &Value) -> R<Value> {
    Ok(Value::Num(this_time(ctx, this)?))
}

/// `Date.prototype[Symbol.toPrimitive](hint)`: a date prefers its
/// string, unless a number is asked for.
fn to_primitive(ctx: &mut Ctx, this: &Value, args: &[Value], _: &Value) -> R<Value> {
    let hint = match arg(args, 0) {
        Value::Str(s) if s.as_str() == "number" => Hint::Number,
        Value::Str(s) if s.as_str() == "string" || s.as_str() == "default" => Hint::String,
        _ => return ctx.throw_type("Invalid hint"),
    };
    let order: [&str; 2] = if matches!(hint, Hint::Number) { ["valueOf", "toString"] } else { ["toString", "valueOf"] };
    for name in order {
        let method = ops::get(ctx, this, name)?;
        if method.as_obj().is_some_and(|m| m.is_callable()) {
            let result = ctx.call(&method, this.clone(), &[])?;
            if !matches!(result, Value::Obj(_)) {
                return Ok(result);
            }
        }
    }
    ctx.throw_type("Cannot convert object to primitive value")
}

pub fn install(ctx: &mut Ctx) {
    let proto = ctx.realm.protos.date.clone();
    let constructor = super::constructor(ctx, "Date", 7, date_constructor, &proto);
    ctx.method(&constructor, "now", 0, now);
    ctx.method(&constructor, "parse", 1, parse);
    ctx.method(&constructor, "UTC", 7, utc);

    let getters: [(&str, i64); 8] = [
        ("FullYear", 0),
        ("Month", 1),
        ("Date", 2),
        ("Day", 3),
        ("Hours", 4),
        ("Minutes", 5),
        ("Seconds", 6),
        ("Milliseconds", 7),
    ];
    for (name, field) in getters {
        for utc in [false, true] {
            let full = alloc::format!("get{}{}", if utc { "UTC" } else { "" }, name);
            let native = ctx.native_with(&full, 0, getter, Value::num(field + if utc { 100 } else { 0 }));
            proto.put_hidden(&full, Value::Obj(native));
            if field == 3 {
                continue;
            }
            let full = alloc::format!("set{}{}", if utc { "UTC" } else { "" }, name);
            let length = match field {
                4 => 4,
                0 | 5 => 3,
                1 | 6 => 2,
                _ => 1,
            };
            let native = ctx.native_with(&full, length, setter, Value::num(field + if utc { 100 } else { 0 }));
            proto.put_hidden(&full, Value::Obj(native));
        }
    }
    ctx.method(&proto, "getTime", 0, get_time);
    ctx.method(&proto, "valueOf", 0, value_of);
    ctx.method(&proto, "setTime", 1, set_time);
    ctx.method(&proto, "getTimezoneOffset", 0, get_timezone_offset);
    ctx.method(&proto, "getYear", 0, get_year);
    let forms: [(&str, i64); 9] = [
        ("toString", 0),
        ("toDateString", 1),
        ("toTimeString", 2),
        ("toISOString", 3),
        ("toUTCString", 4),
        ("toGMTString", 4),
        ("toLocaleString", 5),
        ("toLocaleDateString", 6),
        ("toLocaleTimeString", 7),
    ];
    for (name, form) in forms {
        let native = ctx.native_with(name, 0, string_form, Value::num(form));
        proto.put_hidden(name, Value::Obj(native));
    }
    ctx.method(&proto, "toJSON", 1, to_json);
    let key = ctx.realm.symbols.to_primitive.clone();
    let native = ctx.native("[Symbol.toPrimitive]", 1, to_primitive);
    proto.put_key(key, Value::Obj(native), CONFIGURABLE);
}
