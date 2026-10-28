//! Request bodies: the encodings of HTML forms and of scripts.
//!
//! A form says how it wants to be sent with `enctype`:
//!
//! * `application/x-www-form-urlencoded`, the default: `name=value&...`,
//!   percent-encoded;
//! * `multipart/form-data` (RFC 7578): one part per field, the only
//!   encoding that carries a **file**;
//! * `text/plain`: `name=value` lines, for humans.
//!
//! A script sends what it likes -- a string, JSON, bytes -- with the type
//! it names; `json` builds the common case.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use super::url;

/// A field of a form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Field {
    Text { name: String, value: String },
    File { name: String, filename: String, content_type: String, data: Vec<u8> },
}

impl Field {
    pub fn text(name: &str, value: &str) -> Field {
        Field::Text {
            name: String::from(name),
            value: String::from(value),
        }
    }

    fn name(&self) -> &str {
        match self {
            Field::Text { name, .. } | Field::File { name, .. } => name,
        }
    }
}

/// The encodings a form may ask for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    UrlEncoded,
    Multipart,
    TextPlain,
}

impl Encoding {
    /// From an `enctype` attribute; anything unknown is the default, as
    /// HTML says.
    pub fn parse(enctype: &str) -> Encoding {
        match enctype.trim().to_ascii_lowercase().as_str() {
            "multipart/form-data" => Encoding::Multipart,
            "text/plain" => Encoding::TextPlain,
            _ => Encoding::UrlEncoded,
        }
    }
}

/// `name=value&...`. A file sends its name only, as browsers do when the
/// form was not multipart.
pub fn urlencoded(fields: &[Field]) -> String {
    let pairs: Vec<String> = fields
        .iter()
        .map(|field| match field {
            Field::Text { name, value } => format!("{}={}", url::form_encode(name), url::form_encode(value)),
            Field::File { name, filename, .. } => format!("{}={}", url::form_encode(name), url::form_encode(filename)),
        })
        .collect();
    pairs.join("&")
}

/// `text/plain`: one `name=value` per line, CRLF-terminated.
pub fn text_plain(fields: &[Field]) -> Vec<u8> {
    let mut out = String::new();
    for field in fields {
        let value = match field {
            Field::Text { value, .. } => value.as_str(),
            Field::File { filename, .. } => filename.as_str(),
        };
        out.push_str(&format!("{}={}\r\n", field.name(), value));
    }
    out.into_bytes()
}

/// Quotes a name in a `Content-Disposition` header: quotes and line
/// breaks escaped, as HTML's multipart algorithm does.
fn quoted(text: &str) -> String {
    text.replace('"', "%22").replace('\r', "%0D").replace('\n', "%0A")
}

/// `multipart/form-data` with `boundary`. Returns (content type, body).
///
/// The boundary must not occur in any part; the caller draws it from the
/// random generator, 32 characters of it, which no real content contains.
pub fn multipart(fields: &[Field], boundary: &str) -> (String, Vec<u8>) {
    let mut body = Vec::new();
    for field in fields {
        body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
        match field {
            Field::Text { name, value } => {
                body.extend_from_slice(format!("Content-Disposition: form-data; name=\"{}\"\r\n\r\n", quoted(name)).as_bytes());
                body.extend_from_slice(value.as_bytes());
            }
            Field::File { name, filename, content_type, data } => {
                body.extend_from_slice(
                    format!(
                        "Content-Disposition: form-data; name=\"{}\"; filename=\"{}\"\r\nContent-Type: {}\r\n\r\n",
                        quoted(name),
                        quoted(filename),
                        if content_type.is_empty() { "application/octet-stream" } else { content_type }
                    )
                    .as_bytes(),
                );
                body.extend_from_slice(data);
            }
        }
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{}--\r\n", boundary).as_bytes());
    (format!("multipart/form-data; boundary={}", boundary), body)
}

/// A boundary from 24 random bytes: letters and digits only.
pub fn boundary(random: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut out = String::from("----NovaFormBoundary");
    for byte in random {
        out.push(ALPHABET[*byte as usize % ALPHABET.len()] as char);
    }
    out
}

/// Encodes `fields` as `encoding` asks. Returns (content type, body).
pub fn encode(fields: &[Field], encoding: Encoding, random: &[u8]) -> (String, Vec<u8>) {
    match encoding {
        Encoding::UrlEncoded => (String::from("application/x-www-form-urlencoded"), urlencoded(fields).into_bytes()),
        Encoding::TextPlain => (String::from("text/plain;charset=UTF-8"), text_plain(fields)),
        Encoding::Multipart => multipart(fields, &boundary(random)),
    }
}

/// A content type guessed from a file name, for a file part.
pub fn guess_type(filename: &str) -> &'static str {
    let lower = filename.to_ascii_lowercase();
    let extension = lower.rsplit('.').next().unwrap_or("");
    match extension {
        "txt" | "md" | "log" | "cfg" => "text/plain",
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "js" => "text/javascript",
        "json" => "application/json",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "pdf" => "application/pdf",
        "wav" => "audio/wav",
        "zip" => "application/zip",
        _ => "application/octet-stream",
    }
}
