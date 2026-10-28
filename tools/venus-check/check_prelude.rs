// Rust half of the Venus encoder check: the encoder Nova generated.
extern crate alloc;

#[allow(dead_code)]
mod venus {
    #[path = "@REPO@/src/gpu/venus/cs.rs"]
    pub mod cs;
    #[path = "@REPO@/src/gpu/venus/protocol.rs"]
    pub mod protocol;
}

use venus::cs::Enc;
use venus::protocol::*;

static mut COUNTER: u64 = 1;

fn next_value() -> u64 {
    unsafe {
        let value = COUNTER * 0x9E3779B1 % 1000003 + 1;
        COUNTER += 1;
        value
    }
}

fn next_string() -> &'static str {
    Box::leak(format!("s{}", next_value()).into_boxed_str())
}

fn leak<T>(items: Vec<T>) -> &'static [T] {
    Box::leak(items.into_boxed_slice())
}

fn leak_one<T>(item: T) -> &'static T {
    Box::leak(Box::new(item))
}

fn dump(name: &str, e: &Enc) {
    assert!(!e.error(), "{}: encodeur en erreur", name);
    print!("{} ", name);
    for byte in e.bytes() {
        print!("{:02x}", byte);
    }
    println!();
}
