//! The system's version, with one source of truth: `Cargo.toml`.
//!
//! The release workflow tags `v{version}` from the same field, so the
//! number printed at boot, the one `uname` gives a ported program and the
//! one SPM compares against an OS update can never disagree with the ISO
//! they came from. Before this module, SPM carried its own constant
//! (`0.8.0.1`) that nothing kept in step with the code.
//!
//! The scheme is `0.{phase}.{patch}`: the minor number is the last
//! roadmap phase whose code is complete, the patch counts fixes and
//! additions inside it. SPM versions have four parts; the fourth is
//! always zero for a release.

/// The version as written in `Cargo.toml`, e.g. `"0.15.0"`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The same, as the four numbers SPM compares (`[0, 15, 0, 0]`).
pub const NUMBERS: [u16; 4] = parse(VERSION);

/// Short Git commit the kernel was built from, or `"local"` when the
/// build had no repository (an archive) -- set by the Makefile.
pub const COMMIT: &str = match option_env!("NOVA_COMMIT") {
    Some(commit) => commit,
    None => "local",
};

/// Parses `major.minor.patch[.build][-suffix]` at compile time. A part
/// that is not a number stops the parse: the rest stays zero, which a
/// malformed `Cargo.toml` would make visible at boot rather than as a
/// compile error far from its cause.
const fn parse(text: &str) -> [u16; 4] {
    let bytes = text.as_bytes();
    let mut out = [0u16; 4];
    let mut part = 0;
    let mut i = 0;
    while i < bytes.len() && part < 4 {
        let b = bytes[i];
        if b == b'.' {
            part += 1;
        } else if b >= b'0' && b <= b'9' {
            out[part] = out[part] * 10 + (b - b'0') as u16;
        } else {
            break;
        }
        i += 1;
    }
    out
}
