//! Text of web pages: character sets, HTML entities, and the ASCII the
//! screen can draw.
//!
//! ## Why transliterate
//!
//! The system font has glyphs for 0x20..0x7E only, and CLAUDE.md makes
//! that the rule for everything displayed. A French page is full of
//! accents, and a `?` for each would make it unreadable. So text is
//! folded to ASCII on its way to the screen: `e` for `é`, `oe` for `œ`,
//! `"` for curly quotes, `EUR` for the euro sign. The page keeps its real
//! text -- forms send what the user typed and links keep their bytes --
//! only what is drawn is folded.

use alloc::string::String;
use alloc::vec::Vec;

/// Windows-1252 bytes 0x80..0x9F, which Latin-1 leaves as controls and
/// every "ISO-8859-1" page actually means.
const CP1252: [u16; 32] = [
    0x20ac, 0x81, 0x201a, 0x0192, 0x201e, 0x2026, 0x2020, 0x2021, 0x02c6, 0x2030, 0x0160, 0x2039, 0x0152,
    0x8d, 0x017d, 0x8f, 0x90, 0x2018, 0x2019, 0x201c, 0x201d, 0x2022, 0x2013, 0x2014, 0x02dc, 0x2122,
    0x0161, 0x203a, 0x0153, 0x9d, 0x017e, 0x0178,
];

/// The charset a page declares in its first bytes: `<meta charset=...>`
/// or the `http-equiv` form.
pub fn sniff_charset(bytes: &[u8]) -> Option<String> {
    let head = &bytes[..bytes.len().min(2048)];
    let lower: Vec<u8> = head.iter().map(|byte| byte.to_ascii_lowercase()).collect();
    let needle = b"charset=";
    let position = lower.windows(needle.len()).position(|window| window == needle)?;
    let rest = &lower[position + needle.len()..];
    let rest = rest.strip_prefix(b"\"").or_else(|| rest.strip_prefix(b"'")).unwrap_or(rest);
    let end = rest
        .iter()
        .position(|byte| !(byte.is_ascii_alphanumeric() || *byte == b'-' || *byte == b'_'))
        .unwrap_or(rest.len());
    String::from_utf8(rest[..end].to_vec()).ok()
}

/// Bytes to text, by charset. UTF-8 unless Latin-1 / Windows-1252 is
/// declared; a UTF-8 page with an error keeps its readable parts.
pub fn decode(bytes: &[u8], charset: Option<&str>) -> String {
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    match charset {
        Some("iso-8859-1" | "latin1" | "windows-1252" | "cp1252" | "iso-8859-15" | "us-ascii") => bytes
            .iter()
            .map(|byte| match byte {
                0x80..=0x9f => char::from_u32(CP1252[(byte - 0x80) as usize] as u32).unwrap_or('?'),
                _ => *byte as char,
            })
            .collect(),
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}

/// Named entities: the ones pages use. The whole HTML list has two
/// thousand; these cover the text of real pages, and an unknown one is
/// shown as written, as browsers do.
const ENTITIES: &[(&str, char)] = &[
    ("amp", '&'), ("lt", '<'), ("gt", '>'), ("quot", '"'), ("apos", '\''), ("nbsp", '\u{a0}'),
    ("copy", '\u{a9}'), ("reg", '\u{ae}'), ("trade", '\u{2122}'), ("euro", '\u{20ac}'), ("pound", '\u{a3}'),
    ("yen", '\u{a5}'), ("cent", '\u{a2}'), ("sect", '\u{a7}'), ("para", '\u{b6}'), ("deg", '\u{b0}'),
    ("plusmn", '\u{b1}'), ("times", '\u{d7}'), ("divide", '\u{f7}'), ("middot", '\u{b7}'), ("bull", '\u{2022}'),
    ("hellip", '\u{2026}'), ("ndash", '\u{2013}'), ("mdash", '\u{2014}'), ("lsquo", '\u{2018}'),
    ("rsquo", '\u{2019}'), ("sbquo", '\u{201a}'), ("ldquo", '\u{201c}'), ("rdquo", '\u{201d}'),
    ("bdquo", '\u{201e}'), ("laquo", '\u{ab}'), ("raquo", '\u{bb}'), ("lsaquo", '\u{2039}'), ("rsaquo", '\u{203a}'),
    ("larr", '\u{2190}'), ("rarr", '\u{2192}'), ("uarr", '\u{2191}'), ("darr", '\u{2193}'), ("harr", '\u{2194}'),
    ("iexcl", '\u{a1}'), ("iquest", '\u{bf}'), ("shy", '\u{ad}'), ("ordf", '\u{aa}'), ("ordm", '\u{ba}'),
    ("sup1", '\u{b9}'), ("sup2", '\u{b2}'), ("sup3", '\u{b3}'), ("frac12", '\u{bd}'), ("frac14", '\u{bc}'),
    ("frac34", '\u{be}'), ("micro", '\u{b5}'), ("thinsp", '\u{2009}'), ("ensp", '\u{2002}'), ("emsp", '\u{2003}'),
    ("zwnj", '\u{200c}'), ("zwj", '\u{200d}'), ("lrm", '\u{200e}'), ("rlm", '\u{200f}'), ("prime", '\u{2032}'),
    ("Agrave", '\u{c0}'), ("Aacute", '\u{c1}'), ("Acirc", '\u{c2}'), ("Atilde", '\u{c3}'), ("Auml", '\u{c4}'),
    ("Aring", '\u{c5}'),
    ("AElig", '\u{c6}'), ("Ccedil", '\u{c7}'), ("Egrave", '\u{c8}'), ("Eacute", '\u{c9}'), ("Ecirc", '\u{ca}'),
    ("Euml", '\u{cb}'), ("Igrave", '\u{cc}'), ("Iacute", '\u{cd}'), ("Icirc", '\u{ce}'), ("Iuml", '\u{cf}'),
    ("Ntilde", '\u{d1}'), ("Ograve", '\u{d2}'), ("Oacute", '\u{d3}'), ("Ocirc", '\u{d4}'), ("Otilde", '\u{d5}'),
    ("Ouml", '\u{d6}'), ("Oslash", '\u{d8}'), ("Ugrave", '\u{d9}'), ("Uacute", '\u{da}'), ("Ucirc", '\u{db}'),
    ("Uuml", '\u{dc}'), ("Yacute", '\u{dd}'), ("szlig", '\u{df}'), ("agrave", '\u{e0}'), ("aacute", '\u{e1}'),
    ("acirc", '\u{e2}'), ("atilde", '\u{e3}'), ("auml", '\u{e4}'), ("aring", '\u{e5}'), ("aelig", '\u{e6}'),
    ("ccedil", '\u{e7}'), ("egrave", '\u{e8}'), ("eacute", '\u{e9}'), ("ecirc", '\u{ea}'), ("euml", '\u{eb}'),
    ("igrave", '\u{ec}'), ("iacute", '\u{ed}'), ("icirc", '\u{ee}'), ("iuml", '\u{ef}'), ("ntilde", '\u{f1}'),
    ("ograve", '\u{f2}'), ("oacute", '\u{f3}'), ("ocirc", '\u{f4}'), ("otilde", '\u{f5}'), ("ouml", '\u{f6}'),
    ("oslash", '\u{f8}'), ("ugrave", '\u{f9}'), ("uacute", '\u{fa}'), ("ucirc", '\u{fb}'), ("uuml", '\u{fc}'),
    ("yacute", '\u{fd}'), ("yuml", '\u{ff}'), ("OElig", '\u{152}'), ("oelig", '\u{153}'), ("Scaron", '\u{160}'),
    ("scaron", '\u{161}'), ("Yuml", '\u{178}'), ("fnof", '\u{192}'), ("circ", '\u{2c6}'), ("tilde", '\u{2dc}'),
    ("alpha", '\u{3b1}'), ("beta", '\u{3b2}'), ("gamma", '\u{3b3}'), ("delta", '\u{3b4}'), ("pi", '\u{3c0}'),
    ("mu", '\u{3bc}'), ("sigma", '\u{3c3}'), ("omega", '\u{3c9}'), ("infin", '\u{221e}'), ("ne", '\u{2260}'),
    ("le", '\u{2264}'), ("ge", '\u{2265}'), ("minus", '\u{2212}'), ("asymp", '\u{2248}'), ("check", '\u{2713}'),
    ("star", '\u{2606}'), ("hearts", '\u{2665}'), ("dagger", '\u{2020}'), ("Dagger", '\u{2021}'), ("permil", '\u{2030}'),
];

/// Decodes the character references of a text (`&eacute;`, `&#233;`,
/// `&#xE9;`).
pub fn decode_entities(text: &str) -> String {
    if !text.contains('&') {
        return String::from(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(position) = rest.find('&') {
        out.push_str(&rest[..position]);
        rest = &rest[position..];
        let end = rest[1..]
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '#'))
            .map(|index| index + 1)
            .unwrap_or(rest.len());
        let name = &rest[1..end];
        let with_semicolon = rest[end..].starts_with(';');
        let decoded = if let Some(number) = name.strip_prefix('#') {
            let value = if let Some(hex) = number.strip_prefix(['x', 'X']) {
                u32::from_str_radix(hex, 16).ok()
            } else {
                number.parse::<u32>().ok()
            };
            value.map(|value| match value {
                0x80..=0x9f => char::from_u32(CP1252[(value - 0x80) as usize] as u32).unwrap_or('?'),
                _ => char::from_u32(value).unwrap_or('\u{fffd}'),
            })
        } else {
            ENTITIES.iter().find(|(entity, _)| *entity == name).map(|(_, value)| *value)
        };
        match decoded {
            Some(character) if !name.is_empty() => {
                out.push(character);
                rest = &rest[end + usize::from(with_semicolon)..];
            }
            _ => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// One character folded to what the font can draw.
pub fn fold_char(character: char, out: &mut String) {
    let code = character as u32;
    if (0x20..0x7f).contains(&code) {
        out.push(character);
        return;
    }
    let folded: &str = match code {
        0x09 | 0x0a | 0x0d | 0xa0 | 0x2000..=0x200b | 0x202f | 0x205f | 0x3000 => " ",
        0xad | 0x200c..=0x200f | 0xfeff | 0x2060 => "",
        0xa1 => "!",
        0xa2 => "c",
        0xa3 => "GBP",
        0xa5 => "JPY",
        0xa7 => "S",
        0xa9 => "(c)",
        0xab | 0xbb => "\"",
        0xae => "(R)",
        0xb0 => "o",
        0xb1 => "+/-",
        0xb2 => "2",
        0xb3 => "3",
        0xb5 => "u",
        0xb7 | 0x2022 | 0x2027 | 0x2219 | 0x22c5 => "*",
        0xb9 => "1",
        0xbc => "1/4",
        0xbd => "1/2",
        0xbe => "3/4",
        0xbf => "?",
        0xc0..=0xc5 => "A",
        0xc6 => "AE",
        0xc7 => "C",
        0xc8..=0xcb => "E",
        0xcc..=0xcf => "I",
        0xd0 => "D",
        0xd1 => "N",
        0xd2..=0xd6 | 0xd8 => "O",
        0xd7 => "x",
        0xd9..=0xdc => "U",
        0xdd => "Y",
        0xde => "Th",
        0xdf => "ss",
        0xe0..=0xe5 => "a",
        0xe6 => "ae",
        0xe7 => "c",
        0xe8..=0xeb => "e",
        0xec..=0xef => "i",
        0xf0 => "d",
        0xf1 => "n",
        0xf2..=0xf6 | 0xf8 => "o",
        0xf7 => "/",
        0xf9..=0xfc => "u",
        0xfd | 0xff => "y",
        0xfe => "th",
        0x100..=0x105 => if code % 2 == 0 { "A" } else { "a" },
        0x106..=0x10d => if code % 2 == 0 { "C" } else { "c" },
        0x10e..=0x111 => if code % 2 == 0 { "D" } else { "d" },
        0x112..=0x11b => if code % 2 == 0 { "E" } else { "e" },
        0x11c..=0x123 => if code % 2 == 0 { "G" } else { "g" },
        0x124..=0x127 => if code % 2 == 0 { "H" } else { "h" },
        0x128..=0x131 => if code % 2 == 0 { "I" } else { "i" },
        0x134 | 0x135 => if code % 2 == 0 { "J" } else { "j" },
        0x136 | 0x137 => if code % 2 == 0 { "K" } else { "k" },
        0x139..=0x142 => if code % 2 == 1 { "L" } else { "l" },
        0x143..=0x148 => if code % 2 == 1 { "N" } else { "n" },
        0x14c..=0x151 => if code % 2 == 0 { "O" } else { "o" },
        0x152 => "OE",
        0x153 => "oe",
        0x154..=0x159 => if code % 2 == 0 { "R" } else { "r" },
        0x15a..=0x161 => if code % 2 == 0 { "S" } else { "s" },
        0x162..=0x167 => if code % 2 == 0 { "T" } else { "t" },
        0x168..=0x173 => if code % 2 == 0 { "U" } else { "u" },
        0x174 | 0x175 => if code % 2 == 0 { "W" } else { "w" },
        0x176 | 0x178 => "Y",
        0x177 => "y",
        0x179..=0x17e => if code % 2 == 1 { "Z" } else { "z" },
        0x192 => "f",
        0x2c6 => "^",
        0x2dc => "~",
        0x2010..=0x2015 | 0x2212 => "-",
        0x2018 | 0x2019 | 0x201a | 0x201b | 0x2032 | 0x2039 | 0x203a => "'",
        0x201c..=0x201f | 0x2033 => "\"",
        0x2020 => "+",
        0x2021 => "++",
        0x2026 => "...",
        0x2030 => "o/oo",
        0x20ac => "EUR",
        0x2122 => "TM",
        0x2190 => "<-",
        0x2192 => "->",
        0x2191 => "^",
        0x2193 => "v",
        0x2194 => "<->",
        0x21d2 => "=>",
        0x2260 => "!=",
        0x2264 => "<=",
        0x2265 => ">=",
        0x2248 => "~=",
        0x221e => "oo",
        0x2713 | 0x2714 => "v",
        0x2715 | 0x2716 | 0x2717 | 0x2718 => "x",
        0x25b6 | 0x25ba => ">",
        0x25c0 | 0x25c4 => "<",
        0x25b2 => "^",
        0x25bc => "v",
        0x2605 | 0x2606 => "*",
        0x3b1 => "a",
        0x3b2 => "b",
        0x3b3 => "g",
        0x3b4 => "d",
        0x3bc => "u",
        0x3c0 => "pi",
        0x3c3 => "s",
        0x3c9 => "w",
        0xfffd => "?",
        // Emoji and pictographs: nothing the font can hint at.
        0x1f000..=0x1faff | 0x2600..=0x27bf | 0xfe00..=0xfe0f => "",
        _ => "?",
    };
    out.push_str(folded);
}

/// A whole text folded to printable ASCII.
pub fn fold(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        fold_char(character, &mut out);
    }
    out
}

/// Collapses runs of white space into one space, as HTML does outside
/// `pre`.
pub fn collapse_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut space = false;
    for character in text.chars() {
        if character.is_whitespace() && character != '\u{a0}' {
            if !space {
                out.push(' ');
                space = true;
            }
        } else {
            out.push(character);
            space = false;
        }
    }
    out
}
