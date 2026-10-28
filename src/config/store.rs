//! A `key = value` file per account and per `.ss`, for providers with no
//! settings file of their own.
//!
//! `/users/{user}/settings/apps/{file}.cfg`. Written with the kernel's
//! credentials, like `audio.cfg` and the locale: the directory is the
//! account's own settings, and the settings panel is its only writer.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::fs::perm::{Credentials, Mode};

fn path_of(user: &str, file: &str) -> String {
    format!("/users/{}/settings/apps/{}.cfg", user, file)
}

/// Every pair of the file, in order. Empty when there is none.
pub fn load(user: &str, file: &str) -> Vec<(String, String)> {
    let Ok(bytes) = crate::fs::read_file(&path_of(user, file), Credentials::kernel()) else {
        return Vec::new();
    };
    parse(core::str::from_utf8(&bytes).unwrap_or(""))
}

pub fn parse(text: &str) -> Vec<(String, String)> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (String::from(key.trim()), String::from(value.trim())))
        .collect()
}

pub fn serialize(pairs: &[(String, String)]) -> String {
    let mut text = String::from("# Ecrit par parametres.ss\n");
    for (key, value) in pairs {
        text.push_str(&format!("{} = {}\n", key, value));
    }
    text
}

/// One value, or `default` when the file or the key is missing.
pub fn get(user: &str, file: &str, key: &str, default: &str) -> String {
    load(user, file)
        .into_iter()
        .find(|(candidate, _)| candidate == key)
        .map(|(_, value)| value)
        .unwrap_or_else(|| String::from(default))
}

/// Sets one value, keeping the others.
pub fn set(user: &str, file: &str, key: &str, value: &str) -> Result<(), crate::fs::FsError> {
    let mut pairs = load(user, file);
    match pairs.iter_mut().find(|(candidate, _)| candidate == key) {
        Some(pair) => pair.1 = String::from(value),
        None => pairs.push((String::from(key), String::from(value))),
    }
    let who = Credentials::kernel();
    let now = crate::idt::ticks();
    let _ = crate::fs::create_directory(
        &format!("/users/{}/settings/apps", user),
        who,
        Mode::private_dir(),
        now,
    );
    crate::fs::write_file(
        &path_of(user, file),
        serialize(&pairs).as_bytes(),
        who,
        Mode::private_file(),
        now,
    )
    .map(|_| ())
}
