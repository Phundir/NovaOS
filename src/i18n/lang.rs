//! 10.2.6 -- The `.lang` format (spec 14.3).
//!
//! ## The format
//!
//! Key-value, one pair per line, with `{name}` placeholders for the
//! dynamic parts:
//!
//! ```text
//!   # a comment
//!   login.title = Connexion
//!   login.welcome = Bonjour {user}, {count} message(s)
//! ```
//!
//! Everything after the first `=` belongs to the value, spaces included
//! once the surrounding ones are trimmed. A key with no `=` is not an
//! error worth refusing the file for -- it is skipped and counted, which
//! is what lets a partially translated catalogue still be useful.
//!
//! ## Why substitution is not `format!`
//!
//! The names of the placeholders come from a **file**, hence from
//! outside. A `{` with no closing brace, a name that appears twice, a
//! variable the caller did not supply: none of these may panic, and none
//! may loop. An unknown placeholder is left as written, braces included:
//! seeing `{user}` on screen says exactly which key and which variable
//! are missing, where an empty string says nothing at all.
//!
//! ## Fallback (14.2, "Langue" row)
//!
//! A missing key is looked up in the fallback catalogue, then handed back
//! as the key itself. Returning the key rather than an empty string
//! follows the same reasoning as above: an untranslated interface must
//! stay usable and must name what is missing.

use alloc::string::String;
use alloc::vec::Vec;

/// Ceiling on the number of entries of one catalogue.
///
/// A `.lang` comes from the disk, so its size is not ours to choose. The
/// bound is what stops a corrupt file from eating the heap; it is wide
/// enough that no real translation reaches it.
pub const MAX_ENTRIES: usize = 512;

/// Ceiling on the length of one value, in bytes.
pub const MAX_VALUE: usize = 512;

/// Maximum number of substitutions in a single string.
///
/// Bounds the loop, and with it the time an adversarial catalogue can
/// take: `{a}{a}{a}...` on a 512-byte value would otherwise be 256
/// lookups.
const MAX_SUBSTITUTIONS: usize = 32;

/// One translation catalogue.
#[derive(Debug, Clone, Default)]
pub struct Catalogue {
    /// Locale this catalogue describes, e.g. `fr-FR`.
    pub locale: String,
    entries: Vec<(String, String)>,
    /// Lines seen that were not a valid pair.
    pub skipped: usize,
}

impl Catalogue {
    pub fn new(locale: &str) -> Self {
        Catalogue {
            locale: String::from(locale),
            entries: Vec::new(),
            skipped: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Adds or replaces a key.
    ///
    /// Replacement rather than duplication: a catalogue loaded on top of
    /// another must override it, which is what makes an application
    /// catalogue able to specialise a system key (14.1).
    pub fn insert(&mut self, key: &str, value: &str) -> bool {
        if key.is_empty() || value.len() > MAX_VALUE {
            return false;
        }
        if let Some(slot) = self.entries.iter_mut().find(|(name, _)| name == key) {
            slot.1 = String::from(value);
            return true;
        }
        if self.entries.len() >= MAX_ENTRIES {
            return false;
        }
        self.entries.push((String::from(key), String::from(value)));
        true
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|(key, _)| key.as_str())
    }

    /// Merges the text of a `.lang` file. Returns how many pairs entered.
    pub fn merge(&mut self, text: &str) -> usize {
        let mut accepted = 0;
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                self.skipped += 1;
                continue;
            };
            let key = key.trim();
            let value = value.trim();
            if key.is_empty() {
                self.skipped += 1;
                continue;
            }
            if self.insert(key, value) {
                accepted += 1;
            } else {
                self.skipped += 1;
            }
        }
        accepted
    }

    /// Serialises the catalogue back to the `.lang` format.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for (key, value) in self.entries.iter() {
            out.push_str(key);
            out.push_str(" = ");
            out.push_str(value);
            out.push('\n');
        }
        out
    }
}

/// Parses a complete `.lang` file.
pub fn parse(locale: &str, text: &str) -> Catalogue {
    let mut catalogue = Catalogue::new(locale);
    catalogue.merge(text);
    catalogue
}

/// Replaces every `{name}` by the value supplied for it.
///
/// An unresolved placeholder is left as it stands. An unterminated `{` is
/// copied literally to the end -- it is a broken catalogue, not an
/// attack, and eating the rest of the line would hide the mistake.
pub fn substitute(template: &str, variables: &[(&str, &str)]) -> String {
    let bytes = template.as_bytes();
    let mut out = String::with_capacity(template.len());
    let mut index = 0;
    let mut done = 0;

    while index < bytes.len() {
        if bytes[index] != b'{' {
            // Pushing the byte works because a `{` cannot appear inside a
            // multi-byte UTF-8 sequence: every continuation byte has its
            // high bit set.
            out.push(bytes[index] as char);
            index += 1;
            continue;
        }

        let Some(offset) = bytes[index + 1..].iter().position(|byte| *byte == b'}') else {
            // No closing brace: the rest is literal.
            out.push_str(&template[index..]);
            break;
        };
        let name = &template[index + 1..index + 1 + offset];
        let end = index + offset + 2;

        // An empty name names nothing: `{}` stays literal whatever the
        // caller passes. Letting an empty key match would make a
        // catalogue typo silently swallow a value.
        match variables
            .iter()
            .find(|(key, _)| !key.is_empty() && *key == name)
        {
            Some((_, value)) if done < MAX_SUBSTITUTIONS => {
                out.push_str(value);
                done += 1;
            }
            // Unknown, or budget exhausted: kept as written.
            _ => out.push_str(&template[index..end]),
        }
        index = end;
    }

    out
}

/// Names of the placeholders a template uses, in order of appearance.
///
/// Used by the self-test and by `dev` to check that a translation
/// declares the same variables as the source string: a French value that
/// writes `{utilisateur}` where the English one writes `{user}` produces
/// no error at all -- just a brace on screen, months later.
pub fn placeholders(template: &str) -> Vec<String> {
    let bytes = template.as_bytes();
    let mut names = Vec::new();
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index] != b'{' {
            index += 1;
            continue;
        }
        let Some(offset) = bytes[index + 1..].iter().position(|byte| *byte == b'}') else {
            break;
        };
        let name = &template[index + 1..index + 1 + offset];
        if !name.is_empty() && !names.iter().any(|known: &String| known == name) {
            names.push(String::from(name));
        }
        index += offset + 2;
    }

    names
}
