//! Check of `alloc` support: Box, Vec, String, BTreeMap.

extern crate alloc;

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use crate::vga::Color;

/// Exercises the main structures of the `alloc` crate and reports any
/// discrepancy. Returns false if a test fails.
pub fn run() -> bool {
    let mut ok = true;

    // --- Box: allocation of a single value ---
    let boxed = Box::new(0xDEAD_BEEF_u64);
    let boxed_addr = &*boxed as *const u64 as u64;
    if *boxed != 0xDEAD_BEEF {
        ok = false;
    }
    crate::serial_println!(
        "[alloc] Box<u64>   = 0x{:x} a l'adresse 0x{:016x}",
        *boxed,
        boxed_addr
    );
    if boxed_addr < crate::heap::HEAP_START
        || boxed_addr >= crate::heap::HEAP_START + crate::heap::HEAP_SIZE
    {
        crate::serial_println!("[alloc] ERREUR : adresse hors de la region du heap");
        ok = false;
    }
    drop(boxed);

    // --- Vec: growth and reallocation ---
    let mut vec: Vec<u32> = Vec::new();
    for i in 0..256 {
        vec.push(i * i);
    }
    let sum: u64 = vec.iter().map(|&x| x as u64).sum();
    crate::serial_println!(
        "[alloc] Vec<u32>   = {} elements, capacite {}, somme {}",
        vec.len(),
        vec.capacity(),
        sum
    );
    // Sum of the squares from 0 to 255: n(n+1)(2n+1)/6 with n = 255.
    if sum != 5559680 {
        crate::serial_println!("[alloc] ERREUR : somme attendue 5559680");
        ok = false;
    }
    drop(vec);

    // --- String: concatenation and formatting ---
    let mut s = String::from("Nova");
    s.push_str(" OS");
    s.push(' ');
    s.push_str("heap");
    crate::serial_println!("[alloc] String     = \"{}\" ({} octets)", s, s.len());
    if s != "Nova OS heap" {
        ok = false;
    }
    drop(s);

    // --- BTreeMap: multiple and nested allocations ---
    let mut map: BTreeMap<u32, String> = BTreeMap::new();
    map.insert(1, String::from("dsa"));
    map.insert(2, String::from("ss"));
    map.insert(3, String::from("gs"));
    map.insert(4, String::from("cus"));
    map.insert(5, String::from("ras"));
    crate::serial_println!("[alloc] BTreeMap   = {} entrees", map.len());
    if map.get(&3).map(|v| v.as_str()) != Some("gs") {
        ok = false;
    }
    drop(map);

    // --- allocate/free cycle: checks that blocks are merged ---
    let before = crate::heap::stats().1;
    for _ in 0..64 {
        let v: Vec<u8> = Vec::with_capacity(1024);
        drop(v);
    }
    let after = crate::heap::stats().1;
    crate::serial_println!(
        "[alloc] fusion     = {} octets libres avant / {} apres",
        before,
        after
    );
    if after < before {
        crate::serial_println!("[alloc] ERREUR : fuite detectee ({} octets)", before - after);
        ok = false;
    }

    let color = if ok { Color::LightGreen } else { Color::LightRed };
    crate::vga::set_color(color, Color::Black);
    crate::kprintln!(
        "[alloc] Box, Vec, String, BTreeMap : {}",
        if ok { "OK" } else { "ECHEC" }
    );
    crate::vga::set_color(Color::LightGray, Color::Black);

    ok
}
