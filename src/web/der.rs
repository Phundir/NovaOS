//! A DER reader (ITU-T X.690), for certificates.
//!
//! Only what X.509 uses: definite lengths, low tag numbers, and the few
//! universal types a certificate holds. Every read is bounds-checked and
//! returns `None` rather than panicking -- a certificate is data from the
//! network, and the first malformed one must fail the handshake, not the
//! machine.

pub const BOOLEAN: u8 = 0x01;
pub const INTEGER: u8 = 0x02;
pub const BIT_STRING: u8 = 0x03;
pub const OCTET_STRING: u8 = 0x04;
pub const NULL: u8 = 0x05;
pub const OID: u8 = 0x06;
pub const UTC_TIME: u8 = 0x17;
pub const GENERALIZED_TIME: u8 = 0x18;
pub const SEQUENCE: u8 = 0x30;
pub const SET: u8 = 0x31;

/// One element: its tag, its content, and its whole encoding.
#[derive(Debug, Clone, Copy)]
pub struct Element<'a> {
    pub tag: u8,
    pub content: &'a [u8],
    pub raw: &'a [u8],
}

impl<'a> Element<'a> {
    /// A reader over the content of a constructed element.
    pub fn reader(&self) -> Reader<'a> {
        Reader::new(self.content)
    }

    /// The bits of a BIT STRING with no unused bits.
    pub fn bits(&self) -> Option<&'a [u8]> {
        if self.tag != BIT_STRING {
            return None;
        }
        let (unused, rest) = self.content.split_first()?;
        if *unused != 0 {
            return None;
        }
        Some(rest)
    }
}

/// Reads elements one after the other.
#[derive(Debug, Clone, Copy)]
pub struct Reader<'a> {
    data: &'a [u8],
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Reader { data }
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// The tag of the next element, without reading it.
    pub fn peek(&self) -> Option<u8> {
        self.data.first().copied()
    }

    pub fn next(&mut self) -> Option<Element<'a>> {
        let data = self.data;
        let tag = *data.first()?;
        // High tag numbers (low five bits all set) do not occur in X.509.
        if tag & 0x1f == 0x1f {
            return None;
        }
        let first = *data.get(1)? as usize;
        let (length, header) = if first < 0x80 {
            (first, 2)
        } else {
            let count = first & 0x7f;
            if count == 0 || count > 4 {
                return None;
            }
            let mut length = 0usize;
            for index in 0..count {
                length = (length << 8) | *data.get(2 + index)? as usize;
            }
            (length, 2 + count)
        };
        let end = header.checked_add(length)?;
        if end > data.len() {
            return None;
        }
        self.data = &data[end..];
        Some(Element {
            tag,
            content: &data[header..end],
            raw: &data[..end],
        })
    }

    /// The next element, which must carry `tag`.
    pub fn expect(&mut self, tag: u8) -> Option<Element<'a>> {
        let element = self.next()?;
        (element.tag == tag).then_some(element)
    }

    /// The next element if it carries `tag`; nothing consumed otherwise.
    pub fn optional(&mut self, tag: u8) -> Option<Element<'a>> {
        if self.peek() == Some(tag) {
            self.next()
        } else {
            None
        }
    }
}

/// An OID in dotted text form, for comparisons against constants.
pub fn oid_text(content: &[u8]) -> alloc::string::String {
    use core::fmt::Write;
    let mut text = alloc::string::String::new();
    let Some((first, rest)) = content.split_first() else {
        return text;
    };
    let _ = write!(text, "{}.{}", first / 40, first % 40);
    let mut value: u64 = 0;
    for byte in rest {
        value = (value << 7) | (byte & 0x7f) as u64;
        if byte & 0x80 == 0 {
            let _ = write!(text, ".{}", value);
            value = 0;
        }
    }
    text
}

/// A UTCTime or GeneralizedTime (`YYMMDDHHMMSSZ` / `YYYYMMDDHHMMSSZ`) as
/// seconds since 1970.
pub fn time(element: &Element) -> Option<u64> {
    let text = core::str::from_utf8(element.content).ok()?;
    let digits = text.strip_suffix('Z')?;
    let (year, rest) = match element.tag {
        UTC_TIME if digits.len() == 12 => {
            let short: u64 = digits[..2].parse().ok()?;
            // RFC 5280: 50..99 is 19xx, 00..49 is 20xx.
            (if short >= 50 { 1900 + short } else { 2000 + short }, &digits[2..])
        }
        GENERALIZED_TIME if digits.len() == 14 => (digits[..4].parse().ok()?, &digits[4..]),
        _ => return None,
    };
    let field = |index: usize| -> Option<u64> { rest.get(index..index + 2)?.parse().ok() };
    let (month, day, hour, minute, second) = (field(0)?, field(2)?, field(4)?, field(6)?, field(8)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second)
}

/// Days from 1970-01-01 to a date of the proleptic Gregorian calendar.
pub fn days_from_civil(year: u64, month: u64, day: u64) -> u64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year / 400;
    let year_of_era = year - era * 400;
    let month_index = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * month_index + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    (era * 146_097 + day_of_era).saturating_sub(719_468)
}
