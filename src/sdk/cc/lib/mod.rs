//! The C library compiled with every program: its headers, found by
//! `#include <...>`, and its sources, compiled as translation units of
//! their own (see `libc.c` for what it holds and why it is written in C).

/// Streams, memory, strings, conversions, sorting, time.
pub const LIBC: &str = include_str!("libc.c");
/// `<math.h>`.
pub const LIBM: &str = include_str!("libm.c");

/// A header of the library, by the name `#include <...>` gives.
pub fn header(name: &str) -> Option<&'static str> {
    Some(match name {
        "assert.h" => include_str!("include/assert.h"),
        "ctype.h" => include_str!("include/ctype.h"),
        "errno.h" => include_str!("include/errno.h"),
        "float.h" => include_str!("include/float.h"),
        "inttypes.h" => include_str!("include/inttypes.h"),
        "limits.h" => include_str!("include/limits.h"),
        "math.h" => include_str!("include/math.h"),
        "stdarg.h" => include_str!("include/stdarg.h"),
        "stdbool.h" => include_str!("include/stdbool.h"),
        "stddef.h" => include_str!("include/stddef.h"),
        "stdint.h" => include_str!("include/stdint.h"),
        "stdio.h" => include_str!("include/stdio.h"),
        "stdlib.h" => include_str!("include/stdlib.h"),
        "string.h" => include_str!("include/string.h"),
        "time.h" => include_str!("include/time.h"),
        _ => return None,
    })
}
