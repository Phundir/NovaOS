//! `unicodedata` and the `\N{...}` escape: the Unicode Character
//! Database of CPython 3.13 (Unicode 15.1), read from `unidata.bin`.
//!
//! The tables are generated from the host CPython by
//! `tools/unicode/generate.py` (`make unicode`) and embedded as bytes:
//! property runs searched by bisection, names stored as indices into a
//! word list (algorithmic names -- CJK ideographs, Hangul syllables --
//! computed), decompositions and primary composites for the four
//! normalization forms. Nothing is decoded ahead of use: a program that
//! never asks pays nothing.

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use super::super::value::*;
use super::super::vm::{PyResult, Vm};

static DATA: &[u8] = include_bytes!("unidata.bin");

fn section(tag: &[u8; 4]) -> &'static [u8] {
    let mut i = 16;
    while i + 8 <= DATA.len() {
        let len = u32::from_le_bytes([DATA[i + 4], DATA[i + 5], DATA[i + 6], DATA[i + 7]]) as usize;
        let body = &DATA[i + 8..(i + 8 + len).min(DATA.len())];
        if &DATA[i..i + 4] == tag {
            return body;
        }
        i += 8 + len;
    }
    &[]
}

fn u24(b: &[u8], at: usize) -> u32 {
    b[at] as u32 | (b[at + 1] as u32) << 8 | (b[at + 2] as u32) << 16
}

fn varint(b: &[u8], at: &mut usize) -> u32 {
    let mut n = 0u32;
    let mut shift = 0;
    while *at < b.len() {
        let byte = b[*at];
        *at += 1;
        n |= ((byte & 0x7f) as u32) << shift;
        if byte & 0x80 == 0 {
            break;
        }
        shift += 7;
    }
    n
}

fn string(index: u8) -> &'static str {
    let strs = section(b"STRS");
    let part = strs.split(|b| *b == 0).nth(index as usize).unwrap_or(&[]);
    core::str::from_utf8(part).unwrap_or("")
}

/// Category, bidirectional class, east asian width, combining class,
/// mirrored, of a code point.
pub fn properties(cp: u32) -> (&'static str, &'static str, &'static str, u8, bool) {
    let prop = section(b"PROP");
    let n = prop.len() / 8;
    let (mut lo, mut hi) = (0, n);
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        if u24(prop, mid * 8) <= cp {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let r = &prop[lo * 8..lo * 8 + 8];
    (string(r[3]), string(r[4]), string(r[5]), r[6], r[7] != 0)
}

pub fn combining(cp: u32) -> u8 {
    properties(cp).3
}

// The character classes of str methods, as CPython defines them.

/// `str.isprintable` and repr: not Cc, Cf, Cs, Co, Cn, Zl, Zp, Zs, but
/// the space.
pub fn is_printable(c: char) -> bool {
    if (c as u32) < 0x7f {
        return c >= ' ';
    }
    let cat = properties(c as u32).0.as_bytes();
    !matches!(cat, b"Cc" | b"Cf" | b"Cs" | b"Co" | b"Cn" | b"Zl" | b"Zp" | b"Zs")
}

pub fn is_alpha(c: char) -> bool {
    if c.is_ascii() {
        return c.is_ascii_alphabetic();
    }
    matches!(properties(c as u32).0.as_bytes(), b"Lu" | b"Ll" | b"Lt" | b"Lm" | b"Lo")
}

pub fn is_decimal(c: char) -> bool {
    if c.is_ascii() {
        return c.is_ascii_digit();
    }
    numeric_of(c as u32).is_some_and(|n| n.0 >= 0)
}

pub fn is_digit(c: char) -> bool {
    if c.is_ascii() {
        return c.is_ascii_digit();
    }
    numeric_of(c as u32).is_some_and(|n| n.1 >= 0)
}

pub fn is_numeric(c: char) -> bool {
    if c.is_ascii() {
        return c.is_ascii_digit();
    }
    numeric_of(c as u32).is_some()
}

/// `str.isspace`: bidirectional WS, B or S, or category Zs.
pub fn is_space(c: char) -> bool {
    if c.is_ascii() {
        return matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0b' | '\x0c' | '\x1c'..='\x1f');
    }
    let (cat, bidi, _, _, _) = properties(c as u32);
    cat == "Zs" || matches!(bidi, "WS" | "B" | "S")
}

// ------------------------------------------------------------
//  Names
// ------------------------------------------------------------

const ALGO_PREFIX: [&str; 6] = [
    "CJK UNIFIED IDEOGRAPH-", "HANGUL SYLLABLE ", "TANGUT IDEOGRAPH-", "KHITAN SMALL SCRIPT CHARACTER-", "NUSHU CHARACTER-",
    "CJK COMPATIBILITY IDEOGRAPH-",
];

const JAMO_L: [&str; 19] = ["G", "GG", "N", "D", "DD", "R", "M", "B", "BB", "S", "SS", "", "J", "JJ", "C", "K", "T", "P", "H"];
const JAMO_V: [&str; 21] = [
    "A", "AE", "YA", "YAE", "EO", "E", "YEO", "YE", "O", "WA", "WAE", "OE", "YO", "U", "WEO", "WE", "WI", "YU", "EU", "YI", "I",
];
const JAMO_T: [&str; 28] = [
    "", "G", "GG", "GS", "N", "NJ", "NH", "D", "L", "LG", "LM", "LB", "LS", "LT", "LP", "LH", "M", "B", "BS", "S", "SS", "NG", "J",
    "C", "K", "T", "P", "H",
];

const S_BASE: u32 = 0xac00;
const L_BASE: u32 = 0x1100;
const V_BASE: u32 = 0x1161;
const T_BASE: u32 = 0x11a7;
const V_COUNT: u32 = 21;
const T_COUNT: u32 = 28;
const N_COUNT: u32 = V_COUNT * T_COUNT;
const S_COUNT: u32 = 19 * N_COUNT;

fn words() -> Vec<&'static [u8]> {
    section(b"WORD").split(|b| *b == 0).collect()
}

pub fn name(cp: u32) -> Option<String> {
    let algo = section(b"ALGO");
    for r in algo.chunks(7) {
        let (start, end, kind) = (u24(r, 0), u24(r, 3), r[6]);
        if start <= cp && cp <= end {
            if kind == 1 {
                let s = cp - S_BASE;
                let (l, v, t) = (s / N_COUNT, (s % N_COUNT) / T_COUNT, s % T_COUNT);
                return Some(alloc::format!("HANGUL SYLLABLE {}{}{}", JAMO_L[l as usize], JAMO_V[v as usize], JAMO_T[t as usize]));
            }
            return Some(alloc::format!("{}{:04X}", ALGO_PREFIX[kind as usize], cp));
        }
    }
    let names = section(b"NAME");
    let mut at = 0;
    let mut current = 0u32;
    while at < names.len() {
        current += varint(names, &mut at);
        let count = varint(names, &mut at);
        if current == cp {
            let list = words();
            let mut out = String::new();
            for i in 0..count {
                if i > 0 {
                    out.push(' ');
                }
                let w = varint(names, &mut at) as usize;
                out.push_str(core::str::from_utf8(list.get(w).copied().unwrap_or(&[])).unwrap_or(""));
            }
            return Some(out);
        }
        for _ in 0..count {
            varint(names, &mut at);
        }
        if current > cp {
            return None;
        }
    }
    None
}

/// The code point named `name` (any case), as `\N{...}` and lookup().
pub fn lookup(name: &str) -> Option<u32> {
    let upper = name.to_ascii_uppercase();
    let algo = section(b"ALGO");
    for (kind, prefix) in ALGO_PREFIX.iter().enumerate() {
        let Some(rest) = upper.strip_prefix(prefix) else {
            continue;
        };
        let cp = if kind == 1 {
            hangul_lookup(rest)?
        } else {
            if rest.len() < 4 || rest.len() > 5 {
                return None;
            }
            u32::from_str_radix(rest, 16).ok()?
        };
        let known = algo.chunks(7).any(|r| r[6] as usize == kind && u24(r, 0) <= cp && cp <= u24(r, 3));
        return if known { Some(cp) } else { None };
    }
    let list = words();
    let mut wanted = Vec::new();
    for part in upper.split(' ') {
        wanted.push(list.iter().position(|w| *w == part.as_bytes())? as u32);
    }
    let names = section(b"NAME");
    let mut at = 0;
    let mut current = 0u32;
    while at < names.len() {
        current += varint(names, &mut at);
        let count = varint(names, &mut at) as usize;
        let mut same = count == wanted.len();
        for i in 0..count {
            let w = varint(names, &mut at);
            if same && w != wanted[i] {
                same = false;
            }
        }
        if same {
            return Some(current);
        }
    }
    None
}

fn hangul_lookup(rest: &str) -> Option<u32> {
    // The longest matching jamo at each step, as CPython's find_syllable.
    let take = |table: &[&str], s: &str| -> Option<(usize, usize)> {
        let mut best: Option<(usize, usize)> = None;
        for (i, j) in table.iter().enumerate() {
            if s.starts_with(j) && best.is_none_or(|(_, l)| j.len() > l) {
                best = Some((i, j.len()));
            }
        }
        best
    };
    let (l, ll) = take(&JAMO_L, rest)?;
    let (v, vl) = take(&JAMO_V, &rest[ll..])?;
    let (t, tl) = take(&JAMO_T, &rest[ll + vl..])?;
    if ll + vl + tl != rest.len() {
        return None;
    }
    Some(S_BASE + (l as u32 * V_COUNT + v as u32) * T_COUNT + t as u32)
}

// ------------------------------------------------------------
//  Decomposition and normalization
// ------------------------------------------------------------

/// The decomposition of `cp`: its tag (0 canonical, else a STRS index)
/// and code points.
fn decomposition_of(cp: u32) -> Option<(u8, Vec<u32>)> {
    let index = section(b"DIDX");
    let n = index.len() / 6;
    let (mut lo, mut hi) = (0, n);
    while lo < hi {
        let mid = (lo + hi) / 2;
        let c = u24(index, mid * 6);
        if c == cp {
            let deco = section(b"DECO");
            let off = u24(index, mid * 6 + 3) as usize;
            let (tag, count) = (deco[off], deco[off + 1] as usize);
            let cps = (0..count).map(|i| u24(deco, off + 2 + 3 * i)).collect();
            return Some((tag, cps));
        }
        if c < cp {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    None
}

fn decompose_into(cp: u32, compat: bool, out: &mut Vec<u32>) {
    if (S_BASE..S_BASE + S_COUNT).contains(&cp) {
        let s = cp - S_BASE;
        out.push(L_BASE + s / N_COUNT);
        out.push(V_BASE + (s % N_COUNT) / T_COUNT);
        if s % T_COUNT != 0 {
            out.push(T_BASE + s % T_COUNT);
        }
        return;
    }
    match decomposition_of(cp) {
        Some((tag, cps)) if tag == 0 || compat => {
            for c in cps {
                decompose_into(c, compat, out);
            }
        }
        _ => out.push(cp),
    }
}

fn compose_pair(a: u32, b: u32) -> Option<u32> {
    if (L_BASE..L_BASE + 19).contains(&a) && (V_BASE..V_BASE + V_COUNT).contains(&b) {
        return Some(S_BASE + ((a - L_BASE) * V_COUNT + (b - V_BASE)) * T_COUNT);
    }
    if (S_BASE..S_BASE + S_COUNT).contains(&a) && (a - S_BASE) % T_COUNT == 0 && (T_BASE + 1..T_BASE + T_COUNT).contains(&b) {
        return Some(a + (b - T_BASE));
    }
    let comp = section(b"COMP");
    let n = comp.len() / 9;
    let (mut lo, mut hi) = (0, n);
    while lo < hi {
        let mid = (lo + hi) / 2;
        let key = (u24(comp, mid * 9), u24(comp, mid * 9 + 3));
        if key == (a, b) {
            return Some(u24(comp, mid * 9 + 6));
        }
        if key < (a, b) {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    None
}

pub fn normalize(form: &str, text: &str) -> Option<String> {
    let (compat, compose) = match form {
        "NFC" => (false, true),
        "NFD" => (false, false),
        "NFKC" => (true, true),
        "NFKD" => (true, false),
        _ => return None,
    };
    let mut cps = Vec::with_capacity(text.len());
    for c in text.chars() {
        decompose_into(c as u32, compat, &mut cps);
    }
    // Canonical ordering: non-starters sorted by combining class, stably.
    let classes: Vec<u8> = cps.iter().map(|c| combining(*c)).collect();
    let mut pairs: Vec<(u32, u8)> = cps.into_iter().zip(classes).collect();
    let mut i = 0;
    while i < pairs.len() {
        if pairs[i].1 == 0 {
            i += 1;
            continue;
        }
        let start = i;
        while i < pairs.len() && pairs[i].1 != 0 {
            i += 1;
        }
        pairs[start..i].sort_by_key(|p| p.1);
    }
    if compose {
        let mut out: Vec<(u32, u8)> = Vec::with_capacity(pairs.len());
        let mut starter: Option<usize> = None;
        let mut last_cc = 0u8;
        for (c, cc) in pairs {
            if let Some(si) = starter {
                let blocked = out.len() > si + 1 && (last_cc == 0 || last_cc >= cc);
                if !blocked {
                    if let Some(p) = compose_pair(out[si].0, c) {
                        out[si].0 = p;
                        continue;
                    }
                }
            }
            if cc == 0 {
                starter = Some(out.len());
                last_cc = 0;
            } else {
                last_cc = cc;
            }
            out.push((c, cc));
        }
        pairs = out;
    }
    Some(pairs.iter().filter_map(|(c, _)| char::from_u32(*c)).collect())
}

/// (decimal, digit, numerator, denominator) of a numeric character.
fn numeric_of(cp: u32) -> Option<(i8, i8, i64, u32)> {
    let numb = section(b"NUMB");
    let n = numb.len() / 17;
    let (mut lo, mut hi) = (0, n);
    while lo < hi {
        let mid = (lo + hi) / 2;
        let r = &numb[mid * 17..mid * 17 + 17];
        let c = u24(r, 0);
        if c == cp {
            let mut num = [0u8; 8];
            num.copy_from_slice(&r[5..13]);
            let den = u32::from_le_bytes([r[13], r[14], r[15], r[16]]);
            return Some((r[3] as i8, r[4] as i8, i64::from_le_bytes(num), den));
        }
        if c < cp {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    None
}

// ------------------------------------------------------------
//  The module
// ------------------------------------------------------------

fn char_arg(vm: &mut Vm, a: &[Value], function: &str) -> PyResult<u32> {
    match a.first() {
        Some(Value::Str(s)) if s.chars().count() == 1 => Ok(s.chars().next().map(|c| c as u32).unwrap_or(0)),
        Some(Value::Str(_)) => {
            let message = alloc::format!("{}() argument must be a unicode character, not str", function);
            vm.type_error(&message)
        }
        Some(other) => {
            let name = vm.type_name(other);
            let message = alloc::format!("{}() argument must be a unicode character, not {}", function, name);
            vm.type_error(&message)
        }
        None => {
            let message = alloc::format!("{}() takes at least 1 argument (0 given)", function);
            vm.type_error(&message)
        }
    }
}

fn py_name(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let cp = char_arg(vm, a, "name")?;
    match name(cp) {
        Some(n) => Ok(Value::str(&n)),
        None => match a.get(1) {
            Some(default) => Ok(default.clone()),
            None => vm.value_error("no such name"),
        },
    }
}

fn py_lookup(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let Some(Value::Str(n)) = a.first() else {
        return vm.type_error("lookup() argument must be str");
    };
    match lookup(n).and_then(char::from_u32) {
        Some(c) => Ok(Value::str(&alloc::format!("{}", c))),
        None => {
            let class = vm.types.key_error.clone();
            let message = alloc::format!("undefined character name '{}'", n);
            Err(vm.error(&class, &message))
        }
    }
}

fn py_category(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let cp = char_arg(vm, a, "category")?;
    Ok(Value::str(properties(cp).0))
}

fn py_bidirectional(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let cp = char_arg(vm, a, "bidirectional")?;
    Ok(Value::str(properties(cp).1))
}

fn py_east_asian_width(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let cp = char_arg(vm, a, "east_asian_width")?;
    Ok(Value::str(properties(cp).2))
}

fn py_combining(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let cp = char_arg(vm, a, "combining")?;
    Ok(Value::int(properties(cp).3 as i64))
}

fn py_mirrored(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let cp = char_arg(vm, a, "mirrored")?;
    Ok(Value::int(properties(cp).4 as i64))
}

fn py_decomposition(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let cp = char_arg(vm, a, "decomposition")?;
    let Some((tag, cps)) = decomposition_of(cp) else {
        return Ok(Value::str(""));
    };
    let mut parts: Vec<String> = Vec::new();
    if tag != 0 {
        parts.push(String::from(string(tag)));
    }
    parts.extend(cps.iter().map(|c| alloc::format!("{:04X}", c)));
    Ok(Value::str(&parts.join(" ")))
}

fn numeric_field(vm: &mut Vm, a: &[Value], function: &str, which: u8) -> PyResult<Value> {
    let cp = char_arg(vm, a, function)?;
    let found = numeric_of(cp);
    let value = match (found, which) {
        (Some((d, _, _, _)), 0) if d >= 0 => Some(Value::int(d as i64)),
        (Some((_, d, _, _)), 1) if d >= 0 => Some(Value::int(d as i64)),
        (Some((_, _, n, den)), 2) => {
            let f = crate::web::js::num::Num::from_i64(n).div(crate::web::js::num::Num::from_i64(den as i64));
            Some(Value::Float(f))
        }
        _ => None,
    };
    match value {
        Some(v) => Ok(v),
        None => match a.get(1) {
            Some(default) => Ok(default.clone()),
            None => {
                let message = match which {
                    0 => "not a decimal",
                    1 => "not a digit",
                    _ => "not a numeric character",
                };
                vm.value_error(message)
            }
        },
    }
}

fn py_decimal(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    numeric_field(vm, a, "decimal", 0)
}

fn py_digit(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    numeric_field(vm, a, "digit", 1)
}

fn py_numeric(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    numeric_field(vm, a, "numeric", 2)
}

fn form_and_text(vm: &mut Vm, a: &[Value]) -> PyResult<(Rc<str>, Rc<str>)> {
    match (a.first(), a.get(1)) {
        (Some(Value::Str(f)), Some(Value::Str(t))) => Ok((f.clone(), t.clone())),
        (Some(Value::Str(_)), Some(other)) => {
            let name = vm.type_name(other);
            let message = alloc::format!("normalize() argument 2 must be str, not {}", name);
            vm.type_error(&message)
        }
        _ => vm.type_error("normalize() argument 1 must be str"),
    }
}

fn py_normalize(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let (form, text) = form_and_text(vm, a)?;
    match normalize(&form, &text) {
        Some(s) => Ok(Value::str(&s)),
        None => vm.value_error("invalid normalization form"),
    }
}

fn py_is_normalized(vm: &mut Vm, a: &[Value], _: &[(Rc<str>, Value)]) -> PyResult<Value> {
    let (form, text) = form_and_text(vm, a)?;
    match normalize(&form, &text) {
        Some(s) => Ok(Value::Bool(*s == *text)),
        None => vm.value_error("invalid normalization form"),
    }
}

pub fn version() -> &'static str {
    let v = &DATA[3..16];
    let end = v.iter().position(|b| *b == 0).unwrap_or(v.len());
    core::str::from_utf8(&v[..end]).unwrap_or("")
}

pub fn module(vm: &mut Vm) -> Rc<Module> {
    let functions: &[(&'static str, NativeFn)] = &[
        ("name", py_name), ("lookup", py_lookup), ("category", py_category), ("bidirectional", py_bidirectional),
        ("east_asian_width", py_east_asian_width), ("combining", py_combining), ("mirrored", py_mirrored),
        ("decomposition", py_decomposition), ("decimal", py_decimal), ("digit", py_digit), ("numeric", py_numeric),
        ("normalize", py_normalize), ("is_normalized", py_is_normalized),
    ];
    let module = super::native_module(vm, "unicodedata", functions);
    super::set(&module, "unidata_version", Value::str(version()));
    module
}
