//! The Linux personality (spec 1: the compatibility layer): runs Linux
//! x86-64 programs unchanged, by translating their system calls.
//!
//! A Linux program speaks Linux call numbers at its `syscall`
//! instruction. `syscall` turns each one into a call on the kernel's
//! POSIX objects (`crate::posix`) -- the same objects a ported program
//! reaches through the Nova interface (spec 6.7). That translation is
//! all this module is: it owns no object, no process, no file. The Web
//! Browser does not come here; it is a Nova program (`posix::
//! launch_web_browser`), and the monitor's `sous-systeme Linux` counter
//! stays flat while it runs.
//!
//! What remains Linux-specific besides the numbering: starting a
//! program in the Linux personality (`spawn`), the self-test of the
//! statically linked Linux programs, and the root image's startup
//! script, which is run by the image's shell -- a Linux program.

pub mod selftest;
pub mod syscall;

use core::sync::atomic::Ordering;

use crate::posix::errno::LxResult;
use crate::posix::{Abi, BROWSER_LAUNCH_NS, mount_home, spawn_with, time, vfs, wait_exit};

/// Starts `path` with `argv` and `envp` as a Linux program. Returns its
/// PID once its first thread is ready to run.
pub fn spawn(path: &str, argv: &[&str], envp: &[&str]) -> LxResult<u64> {
    spawn_with(path, argv, envp, Abi::Linux, crate::process::BinaryType::Cus)
}

/// Opens the browser of a test image, or runs `/nova/autostart.sh` of the
/// root image with the image's shell, and waits for it (at most
/// `timeout_ticks`). Returns its status, or None when there is nothing to
/// run.
pub fn autostart(timeout_ticks: u64) -> Option<i64> {
    const SCRIPT: &str = "/nova/autostart.sh";
    if let Some(status) = crate::posix::autostart_browser(timeout_ticks) {
        return Some(status);
    }
    vfs::walk("/", SCRIPT, true).ok()?;
    let argv = ["/bin/sh", SCRIPT];
    let envp = ["PATH=/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin", "HOME=/home/nova", "LANG=C.UTF-8", "TERM=dumb"];
    crate::serial_println!("[linux] demarrage de {}", SCRIPT);
    // The script may start programs that write in the session's home:
    // on the disk, like the one the menu gives the browser.
    mount_home();
    BROWSER_LAUNCH_NS.store(time::monotonic_ns(), Ordering::Relaxed);
    let pid = match spawn("/bin/sh", &argv, &envp) {
        Ok(pid) => pid,
        Err(e) => {
            crate::serial_println!("[linux] {} : lancement refuse, errno {}", SCRIPT, e);
            return Some(-e);
        }
    };
    let status = wait_exit(pid, Some(timeout_ticks));
    crate::serial_println!("[linux] {} : fin, statut {:?}", SCRIPT, status);
    Some(status.unwrap_or(-1))
}
