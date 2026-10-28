//! `/proc` and `/sys`: files whose content is computed when read.
//!
//! A program reads `/proc/self/exe` to find its own libraries,
//! `/proc/self/maps` to find its stack, `/sys/devices/system/cpu/present`
//! to size its thread pools. None of it is stored: every node here holds
//! a closure, and a read runs it. Only what the traces of Firefox and of
//! musl showed being read is provided -- a missing file answers ENOENT,
//! which every caller of these paths handles.

use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use alloc::{format, vec};
use core::any::Any;
use core::sync::atomic::{AtomicU64, Ordering};

use super::{DirEntry, Inode, S_IFDIR, S_IFLNK, S_IFREG, Stat};
use crate::posix::errno::*;

type Content = Arc<dyn Fn() -> Vec<u8> + Send + Sync>;
type Target = Arc<dyn Fn() -> Option<String> + Send + Sync>;
type Listing = Arc<dyn Fn() -> Vec<(String, Arc<dyn Inode>)> + Send + Sync>;

static NEXT_INO: AtomicU64 = AtomicU64::new(3_000_000);

fn ino() -> u64 {
    NEXT_INO.fetch_add(1, Ordering::Relaxed)
}

struct GenFile {
    ino: u64,
    content: Content,
}

struct GenLink {
    ino: u64,
    target: Target,
}

struct GenDir {
    ino: u64,
    list: Listing,
}

fn file(content: impl Fn() -> Vec<u8> + Send + Sync + 'static) -> Arc<dyn Inode> {
    Arc::new(GenFile { ino: ino(), content: Arc::new(content) })
}

fn text(s: String) -> Arc<dyn Inode> {
    let bytes = s.into_bytes();
    file(move || bytes.clone())
}

fn link(target: impl Fn() -> Option<String> + Send + Sync + 'static) -> Arc<dyn Inode> {
    Arc::new(GenLink { ino: ino(), target: Arc::new(target) })
}

fn dir(list: impl Fn() -> Vec<(String, Arc<dyn Inode>)> + Send + Sync + 'static) -> Arc<dyn Inode> {
    Arc::new(GenDir { ino: ino(), list: Arc::new(list) })
}

impl Inode for GenFile {
    fn stat(&self) -> Stat {
        // Size 0, like Linux: the content only exists when read, and
        // readers of /proc read until end of file rather than trusting it.
        Stat { dev: super::device_of("procfs"), ino: self.ino, mode: S_IFREG | 0o444, ..Default::default() }
    }
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> LxResult<usize> {
        let data = (self.content)();
        if offset >= data.len() as u64 {
            return Ok(0);
        }
        let n = (data.len() - offset as usize).min(buf.len());
        buf[..n].copy_from_slice(&data[offset as usize..offset as usize + n]);
        Ok(n)
    }
    fn write_at(&self, _offset: u64, buf: &[u8]) -> LxResult<usize> {
        // oom_score_adj and friends: accepted and ignored.
        Ok(buf.len())
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }
}

impl Inode for GenLink {
    fn stat(&self) -> Stat {
        Stat { dev: super::device_of("procfs"), ino: self.ino, mode: S_IFLNK | 0o777, ..Default::default() }
    }
    fn readlink(&self) -> LxResult<String> {
        (self.target)().ok_or(ENOENT)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }
}

impl Inode for GenDir {
    fn stat(&self) -> Stat {
        Stat { dev: super::device_of("procfs"), ino: self.ino, mode: S_IFDIR | 0o555, nlink: 2, ..Default::default() }
    }
    fn lookup(&self, name: &str) -> LxResult<Arc<dyn Inode>> {
        (self.list)().into_iter().find(|(n, _)| n == name).map(|(_, i)| i).ok_or(ENOENT)
    }
    fn readdir(&self) -> LxResult<Vec<DirEntry>> {
        Ok((self.list)()
            .into_iter()
            .map(|(name, node)| {
                let st = node.stat();
                DirEntry { name, ino: st.ino, kind: st.kind() }
            })
            .collect())
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }
}

// ============================================================
//  /proc
// ============================================================

fn cores() -> usize {
    crate::smp_core_count().max(1)
}

fn cpuinfo() -> Vec<u8> {
    let mut s = String::new();
    for cpu in 0..cores() {
        s.push_str(&format!(
            "processor\t: {}\nvendor_id\t: NovaOS\ncpu family\t: 6\nmodel\t\t: 0\nmodel name\t: Nova virtual CPU\n\
             cpu MHz\t\t: {}\ncache size\t: 4096 KB\nphysical id\t: 0\nsiblings\t: {}\ncore id\t\t: {}\n\
             cpu cores\t: {}\nflags\t\t: fpu tsc cx8 cmov mmx fxsr sse sse2 syscall nx lm\n\n",
            cpu,
            crate::posix::time::tsc_per_ms() / 1000,
            cores(),
            cpu,
            cores()
        ));
    }
    s.into_bytes()
}

fn meminfo() -> Vec<u8> {
    let (total, _, free) = crate::frame::stats();
    let total_kb = total as u64 * 4;
    let free_kb = free as u64 * 4;
    format!(
        "MemTotal:       {} kB\nMemFree:        {} kB\nMemAvailable:   {} kB\nBuffers:               0 kB\n\
         Cached:         {} kB\nSwapTotal:              0 kB\nSwapFree:               0 kB\nShmem:          {} kB\n",
        total_kb,
        free_kb,
        free_kb,
        super::tmpfs::resident_bytes() / 1024,
        super::tmpfs::resident_bytes() / 1024
    )
    .into_bytes()
}

fn process_dir(tgid: u64) -> Arc<dyn Inode> {
    dir(move || {
        let Some(p) = crate::posix::process(tgid) else {
            return Vec::new();
        };
        let exe = p.exe.clone();
        let fds = p.clone();
        let maps = p.clone();
        let status = p.clone();
        let tasks = p.clone();
        let cmd = p.clone();
        vec![
            (String::from("exe"), link(move || Some(exe.clone()))),
            (String::from("cwd"), {
                let q = p.clone();
                link(move || Some(q.cwd()))
            }),
            (String::from("maps"), file(move || maps.maps_text().into_bytes())),
            (String::from("status"), file(move || status.status_text().into_bytes())),
            (String::from("stat"), {
                let q = p.clone();
                file(move || q.stat_text().into_bytes())
            }),
            (String::from("statm"), text(String::from("0 0 0 0 0 0 0\n"))),
            (String::from("cmdline"), file(move || cmd.cmdline.clone())),
            (String::from("environ"), text(String::new())),
            (String::from("comm"), text(format!("{}\n", p.name))),
            (String::from("oom_score_adj"), text(String::from("0\n"))),
            (String::from("cgroup"), text(String::from("0::/\n"))),
            (String::from("mounts"), file(mounts_text)),
            (String::from("mountinfo"), file(mounts_text)),
            (String::from("fd"), dir(move || fd_entries(&fds))),
            (String::from("task"), dir(move || {
                tasks.thread_ids().into_iter().map(|tid| (tid.to_string(), task_dir(tgid, tid))).collect()
            })),
        ]
    })
}

fn task_dir(tgid: u64, tid: u64) -> Arc<dyn Inode> {
    dir(move || {
        let Some(p) = crate::posix::process(tgid) else {
            return Vec::new();
        };
        let q = p.clone();
        vec![
            (String::from("stat"), file(move || q.stat_text().into_bytes())),
            (String::from("comm"), text(format!("{}\n", p.name))),
            (String::from("status"), {
                let r = p.clone();
                file(move || r.status_text().into_bytes())
            }),
            (String::from("tid"), text(format!("{}\n", tid))),
        ]
    })
}

fn fd_entries(p: &Arc<crate::posix::Process>) -> Vec<(String, Arc<dyn Inode>)> {
    p.fd_descriptions()
        .into_iter()
        .map(|(fd, description)| (fd.to_string(), link(move || Some(description.clone()))))
        .collect()
}

fn mounts_text() -> Vec<u8> {
    let mut s = String::new();
    for (path, name) in super::mounts() {
        s.push_str(&format!("{} {} {} rw 0 0\n", name, path, name));
    }
    s.into_bytes()
}

pub fn proc_root() -> Arc<dyn Inode> {
    dir(|| {
        let mut entries: Vec<(String, Arc<dyn Inode>)> = vec![
            (String::from("self"), link(|| crate::posix::current_tgid().map(|t| t.to_string()))),
            (String::from("thread-self"), link(|| {
                let tgid = crate::posix::current_tgid()?;
                Some(format!("{}/task/{}", tgid, crate::scheduler::current_pid()))
            })),
            (String::from("cpuinfo"), file(cpuinfo)),
            (String::from("meminfo"), file(meminfo)),
            (String::from("mounts"), file(mounts_text)),
            (String::from("uptime"), file(|| {
                let ns = crate::posix::time::monotonic_ns();
                format!("{}.{:02} {}.{:02}\n", ns / 1_000_000_000, (ns / 10_000_000) % 100, ns / 1_000_000_000, 0).into_bytes()
            })),
            (String::from("version"), text(String::from("Linux version 6.6.0-nova (nova@nova) #1 SMP\n"))),
            (String::from("filesystems"), text(String::from("nodev\ttmpfs\nnodev\tproc\nnodev\tsysfs\n\tnovafs\n"))),
            (String::from("stat"), file(|| {
                let mut s = String::from("cpu  0 0 0 0 0 0 0 0 0 0\n");
                for c in 0..cores() {
                    s.push_str(&format!("cpu{} 0 0 0 0 0 0 0 0 0 0\n", c));
                }
                s.push_str("btime 0\nprocesses 1\nprocs_running 1\n");
                s.into_bytes()
            })),
            (String::from("sys"), sys_tree()),
        ];
        for tgid in crate::posix::tgids() {
            entries.push((tgid.to_string(), process_dir(tgid)));
        }
        entries
    })
}

fn sys_tree() -> Arc<dyn Inode> {
    dir(|| {
        vec![
            (String::from("kernel"), dir(|| {
                vec![
                    (String::from("osrelease"), text(String::from("6.6.0-nova\n"))),
                    (String::from("ostype"), text(String::from("Linux\n"))),
                    (String::from("hostname"), text(String::from("nova\n"))),
                    (String::from("pid_max"), text(String::from("4194304\n"))),
                    (String::from("threads-max"), text(String::from("65536\n"))),
                    (String::from("random"), dir(|| {
                        vec![(String::from("uuid"), file(|| {
                            let mut b = [0u8; 16];
                            crate::posix::misc::fill_random(&mut b);
                            format!(
                                "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-4{:01x}{:02x}-a{:01x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}\n",
                                b[0], b[1], b[2], b[3], b[4], b[5], b[6] & 15, b[7], b[8] & 15, b[9], b[10], b[11], b[12], b[13], b[14], b[15]
                            )
                            .into_bytes()
                        }))]
                    })),
                ]
            })),
            (String::from("vm"), dir(|| {
                vec![
                    (String::from("overcommit_memory"), text(String::from("0\n"))),
                    (String::from("max_map_count"), text(String::from("65530\n"))),
                ]
            })),
        ]
    })
}

// ============================================================
//  /sys
// ============================================================

pub fn sys_root() -> Arc<dyn Inode> {
    fn range() -> Vec<u8> {
        format!("0-{}\n", cores() - 1).into_bytes()
    }
    dir(|| {
        vec![(String::from("devices"), dir(|| {
            vec![(String::from("system"), dir(|| {
                vec![(String::from("cpu"), dir(|| {
                    let mut v: Vec<(String, Arc<dyn Inode>)> = vec![
                        (String::from("present"), file(range)),
                        (String::from("online"), file(range)),
                        (String::from("possible"), file(range)),
                    ];
                    for c in 0..cores() {
                        v.push((format!("cpu{}", c), dir(|| Vec::new())));
                    }
                    v
                }))]
            }))]
        }))]
    })
}

/// `/etc/resolv.conf`, written from the network configuration of the
/// moment: the settings may have changed the server since boot.
pub fn resolv_conf() -> Arc<dyn Inode> {
    file(|| {
        let mut text = String::new();
        for server in crate::net::dns_servers().into_iter().flatten() {
            text.push_str(&format!("nameserver {}\n", server.text()));
        }
        text.push_str("options timeout:2 attempts:2\n");
        text.into_bytes()
    })
}

/// `/etc/localtime`: a link into the image's zoneinfo (package `tzdata`)
/// for the session's time zone. A link rather than a copy because ICU
/// reads the zone's *name* from the link's target, after "zoneinfo/";
/// musl follows it to the TZif file.
pub fn localtime() -> Arc<dyn Inode> {
    link(|| Some(format!("/usr/share/zoneinfo/{}", crate::i18n::settings().zone().linux_id())))
}

/// `/etc/timezone`: the same name, as text (Debian's convention, which
/// some programs read first).
pub fn timezone() -> Arc<dyn Inode> {
    file(|| format!("{}\n", crate::i18n::settings().zone().linux_id()).into_bytes())
}

/// `/etc/hostname`.
pub fn hostname() -> Arc<dyn Inode> {
    text(String::from("nova\n"))
}
