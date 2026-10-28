//! The C compiler of Nova, compiled for the host (`make cc-test`).
//!
//! `sdk::cc` and `sdk::asm` are plain `alloc` code: the same files
//! compile here with `std` underneath. What the kernel turns into a Nova
//! binary, this harness turns into a static Linux ELF -- same assembly,
//! same encoder, only the platform layer of the C library differs
//! (`__linux__` instead of `__nova__`) -- and runs it. The witness is gcc:
//! `tests/c/*.c` and the outputs gcc's build gave (`*.out`, written by
//! `tools/cchost/reference.sh`).
//!
//! Built by `rustc` directly, not by cargo: the repository's
//! `.cargo/config.toml` points every cargo build at the kernel target.
//!
//! Usage:
//!   cchost --asm                    encodings checked against GNU as
//!   cchost program.c [-S] [-o out]  compile (and run unless -o / -S)
//!   cchost --corpus directory       every test against its .out

#![allow(dead_code, unused_imports, unused_macros, unexpected_cfgs)]

extern crate alloc;

pub mod sdk;
pub mod web;

use std::process::Command;

use sdk::asm;
use sdk::cc;

/// Instructions whose encoding `sdk::asm` must share with GNU as.
const ENCODINGS: &[&str] = &[
    "movsx rax, byte [rcx]",
    "movsx rdx, byte [rbp - 8]",
    "movsx r9, word [r12 + 300]",
    "movzx rax, byte [rax]",
    "movzx r11, word [rsp + 8]",
    "movsxd rax, dword [rbp - 12]",
    "movsxd r15, dword [r13]",
    "movsx rax, al",
    "movsx rax, sil",
    "movsx rcx, ax",
    "movzx rax, dil",
    "movzx rax, r10b",
    "movzx rdx, cx",
    "movsxd rax, eax",
    "movsxd r8, r9d",
    "mov eax, dword [rcx]",
    "mov r10d, dword [rbp - 4]",
    "mov dword [rax], ecx",
    "mov dword [rbp - 20], r9d",
    "mov word [rax], cx",
    "mov word [r8 + 2], r11w",
    "mov byte [rax], cl",
    "mov byte [rdi], sil",
    "mov byte [r12], r8b",
    "mov al, byte [rbp - 1]",
    "mov qword [rax], rcx",
    "mov rax, qword [rcx + 16]",
    "mov eax, eax",
    "mov ecx, r9d",
    "lea rax, [rbp - 24]",
    "lea r12, [rsp + 4096]",
    "lea rdi, [r13]",
    "test rax, rax",
    "test r9, rcx",
    "call rax",
    "call r11",
    "jmp rcx",
    "rep movsb",
    "rep stosb",
    "sete al",
    "setne cl",
    "setl dl",
    "setle al",
    "setg al",
    "setge al",
    "setb al",
    "setbe al",
    "seta al",
    "setae al",
    "setp cl",
    "setnp dil",
    "sete r9b",
    "movq xmm0, rax",
    "movq xmm1, rcx",
    "movq xmm9, r10",
    "movq rax, xmm0",
    "movq r12, xmm15",
    "movd xmm0, eax",
    "movd eax, xmm0",
    "movd r8d, xmm3",
    "addsd xmm0, xmm1",
    "subsd xmm0, xmm1",
    "mulsd xmm2, xmm9",
    "divsd xmm0, xmm1",
    "sqrtsd xmm0, xmm0",
    "minsd xmm0, xmm1",
    "maxsd xmm0, xmm1",
    "ucomisd xmm0, xmm1",
    "ucomisd xmm8, xmm1",
    "cvtsd2ss xmm0, xmm0",
    "cvtss2sd xmm0, xmm0",
    "cvtsi2sd xmm0, rax",
    "cvtsi2sd xmm1, r9",
    "cvttsd2si rax, xmm0",
    "cvttsd2si r8, xmm12",
    // Unchanged forms, to be sure the extension did not take them over.
    "mov rax, rcx",
    "mov rax, [rbp - 8]",
    "mov [rbp - 8], rax",
    "add rax, rcx",
    "sub rsp, 4096",
    "imul rax, rcx",
    "idiv rcx",
    "cqo",
    "shl rax, cl",
    "sar rax, 3",
];

/// GNU as wants `byte ptr`; Nova's assembler takes both.
fn gnu_syntax(line: &str) -> String {
    let mut out = line.to_string();
    for width in ["qword", "dword", "word", "byte"] {
        let from = format!("{} [", width);
        if out.contains(&from) {
            out = out.replace(&from, &format!("{} ptr [", width));
            break;
        }
    }
    out
}

fn gnu_bytes(line: &str, work: &std::path::Path) -> Result<Vec<u8>, String> {
    let source = work.join("x.s");
    let object = work.join("x.o");
    let binary = work.join("x.bin");
    std::fs::write(&source, format!(".intel_syntax noprefix\n.text\n{}\n", gnu_syntax(line))).map_err(|e| e.to_string())?;
    let status = Command::new("as").arg("-o").arg(&object).arg(&source).status().map_err(|e| e.to_string())?;
    if !status.success() {
        return Err(String::from("as refuse"));
    }
    let status = Command::new("objcopy")
        .args(["-O", "binary", "-j", ".text"])
        .arg(&object)
        .arg(&binary)
        .status()
        .map_err(|e| e.to_string())?;
    if !status.success() {
        return Err(String::from("objcopy refuse"));
    }
    std::fs::read(&binary).map_err(|e| e.to_string())
}

fn check_encodings() -> bool {
    let work = std::env::temp_dir().join(format!("cchost-asm-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&work);
    let mut failed = 0;
    for line in ENCODINGS {
        let nova = match asm::assemble(&format!(".text\n_start:\n    {}\n", line)) {
            Ok(object) => object.text,
            Err(error) => {
                println!("[asm] {:<34} refuse par Nova : {}", line, error.message());
                failed += 1;
                continue;
            }
        };
        match gnu_bytes(line, &work) {
            Ok(gnu) if gnu == nova => {}
            Ok(gnu) => {
                println!("[asm] {:<34} Nova {:02x?} / GNU {:02x?}", line, nova, gnu);
                failed += 1;
            }
            Err(error) => {
                println!("[asm] {:<34} GNU : {}", line, error);
                failed += 1;
            }
        }
    }
    let _ = std::fs::remove_dir_all(&work);
    println!("[asm] {} encodages identiques a GNU as, {} en echec", ENCODINGS.len() - failed, failed);
    failed == 0
}

/// The disk of the host, as `#include "..."` sees it.
struct Disk;

impl cc::pp::Sources for Disk {
    fn read(&self, path: &str) -> Option<String> {
        std::fs::read_to_string(path).ok()
    }
}

const PAGE: usize = 4096;
/// Where the image is mapped: the text, then the data, then the bss, as
/// `sdk::emit` lays out a Nova binary.
const BASE: u64 = 0x40_1000;

fn page_up(n: usize) -> usize {
    n.div_ceil(PAGE) * PAGE
}

/// A static ELF of the object, laid out as `sdk::emit` lays out a Nova
/// binary (text, data, bss, each on its own pages), the RIP-relative
/// holes filled the same way.
fn link_elf(object: &asm::Object) -> Result<Vec<u8>, String> {
    let text_span = page_up(object.text.len().max(1));
    let data_span = page_up(object.data.len());
    let mut text = object.text.clone();
    for fixup in object.fixups.iter() {
        let symbol = object.symbol(&fixup.symbol).ok_or_else(|| format!("symbole {} absent", fixup.symbol))?;
        let base = match symbol.section {
            asm::Section::Text => 0i64,
            asm::Section::Data => text_span as i64,
            asm::Section::Bss => (text_span + data_span) as i64,
        };
        let displacement = base + symbol.offset as i64 - fixup.next as i64;
        text[fixup.at..fixup.at + 4].copy_from_slice(&(displacement as i32).to_le_bytes());
    }

    let mut file = vec![0u8; PAGE];
    let put16 = |f: &mut Vec<u8>, at: usize, v: u16| f[at..at + 2].copy_from_slice(&v.to_le_bytes());
    let put32 = |f: &mut Vec<u8>, at: usize, v: u32| f[at..at + 4].copy_from_slice(&v.to_le_bytes());
    let put64 = |f: &mut Vec<u8>, at: usize, v: u64| f[at..at + 8].copy_from_slice(&v.to_le_bytes());
    file[0..4].copy_from_slice(b"\x7fELF");
    file[4] = 2; // 64-bit
    file[5] = 1; // little endian
    file[6] = 1; // version
    put16(&mut file, 16, 2); // ET_EXEC
    put16(&mut file, 18, 0x3E); // x86_64
    put32(&mut file, 20, 1);
    put64(&mut file, 24, BASE + object.entry);
    put64(&mut file, 32, 64); // program headers
    put16(&mut file, 52, 64);
    put16(&mut file, 54, 56);
    put16(&mut file, 56, 2);
    put16(&mut file, 58, 64);
    // Text: read + execute.
    let ph = 64;
    put32(&mut file, ph, 1);
    put32(&mut file, ph + 4, 5);
    put64(&mut file, ph + 8, PAGE as u64);
    put64(&mut file, ph + 16, BASE);
    put64(&mut file, ph + 24, BASE);
    put64(&mut file, ph + 32, text.len() as u64);
    put64(&mut file, ph + 40, text_span as u64);
    put64(&mut file, ph + 48, PAGE as u64);
    // Data and bss: read + write.
    let ph = 64 + 56;
    let data_address = BASE + text_span as u64;
    put32(&mut file, ph, 1);
    put32(&mut file, ph + 4, 6);
    put64(&mut file, ph + 8, (PAGE + text_span) as u64);
    put64(&mut file, ph + 16, data_address);
    put64(&mut file, ph + 24, data_address);
    put64(&mut file, ph + 32, object.data.len() as u64);
    put64(&mut file, ph + 40, (data_span as u64 + object.bss_size).max(1));
    put64(&mut file, ph + 48, PAGE as u64);
    file.extend_from_slice(&text);
    file.resize(PAGE + text_span, 0);
    file.extend_from_slice(&object.data);
    Ok(file)
}

/// Compiles `files` into an executable at `out`.
fn build(files: &[&str], out: &std::path::Path, assembly: Option<&std::path::Path>) -> Result<(), String> {
    let text = cc::compile_to_assembly(&Disk, files, cc::Target::Linux).map_err(|e| e.render())?;
    if let Some(path) = assembly {
        let _ = std::fs::write(path, &text);
    }
    let object = asm::assemble(&text).map_err(|e| format!("assemblage : {}", e.message()))?;
    let image = link_elf(&object)?;
    std::fs::write(out, image).map_err(|e| e.to_string())?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(out, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    Ok(())
}

/// Runs a binary: its standard output, then `--- exit N`.
fn run(binary: &std::path::Path, input: Option<&std::path::Path>) -> String {
    let mut command = Command::new(binary);
    command.stderr(std::process::Stdio::null());
    match input.and_then(|p| std::fs::File::open(p).ok()) {
        Some(file) => {
            command.stdin(file);
        }
        None => {
            command.stdin(std::process::Stdio::null());
        }
    }
    match command.output() {
        Ok(output) => {
            let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
            let code = output.status.code().unwrap_or_else(|| {
                use std::os::unix::process::ExitStatusExt;
                128 + output.status.signal().unwrap_or(0)
            });
            text.push_str(&format!("--- exit {}\n", code));
            text
        }
        Err(error) => format!("--- lancement impossible : {}\n", error),
    }
}

fn work_dir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("cchost-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn corpus(directory: &str) -> bool {
    let mut tests: Vec<std::path::PathBuf> = std::fs::read_dir(directory)
        .map(|entries| entries.filter_map(|e| e.ok()).map(|e| e.path()).collect())
        .unwrap_or_default();
    tests.retain(|p| p.extension().is_some_and(|e| e == "c"));
    tests.sort();
    let work = work_dir();
    let start = std::time::Instant::now();
    let (mut passed, mut failed) = (0, 0);
    for test in tests.iter() {
        let name = test.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let expected = std::fs::read_to_string(test.with_extension("out")).unwrap_or_default();
        let binary = work.join(&name);
        let path = test.to_string_lossy().into_owned();
        let got = match build(&[&path], &binary, Some(&work.join(format!("{}.s", name)))) {
            Ok(()) => {
                let input = test.with_extension("in");
                run(&binary, input.exists().then_some(input.as_path()))
            }
            Err(error) => format!("--- compilation : {}\n", error),
        };
        if got == expected {
            passed += 1;
        } else {
            failed += 1;
            println!("[cc-test] {} : ECHEC", name);
            let (g, e): (Vec<&str>, Vec<&str>) = (got.lines().collect(), expected.lines().collect());
            for i in 0..g.len().max(e.len()) {
                let (a, b) = (g.get(i).copied().unwrap_or("<rien>"), e.get(i).copied().unwrap_or("<rien>"));
                if a != b {
                    println!("    ligne {} :\n      nova : {}\n      gcc  : {}", i + 1, a, b);
                    break;
                }
            }
        }
    }
    let _ = std::fs::remove_dir_all(&work);
    println!(
        "[cc-test] {} programmes conformes a gcc, {} en echec ({} ms)",
        passed,
        failed,
        start.elapsed().as_millis()
    );
    failed == 0
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let ok = match args.get(1).map(String::as_str) {
        Some("--asm") => check_encodings(),
        Some("--corpus") => corpus(args.get(2).map(String::as_str).unwrap_or("tests/c")),
        Some(_) => {
            // cchost a.c [b.c ...] [-S file.s] [-o out]: compile, and run
            // unless -o names an output.
            let mut files = Vec::new();
            let mut out = None;
            let mut assembly = None;
            let mut i = 1;
            while i < args.len() {
                match args[i].as_str() {
                    "-o" => {
                        out = args.get(i + 1).cloned();
                        i += 1;
                    }
                    "-S" => {
                        assembly = args.get(i + 1).cloned();
                        i += 1;
                    }
                    other => files.push(other.to_string()),
                }
                i += 1;
            }
            let refs: Vec<&str> = files.iter().map(String::as_str).collect();
            let target = match &out {
                Some(path) => std::path::PathBuf::from(path),
                None => work_dir().join("programme"),
            };
            match build(&refs, &target, assembly.as_deref().map(std::path::Path::new)) {
                Ok(()) if out.is_some() => true,
                Ok(()) => {
                    let status = Command::new(&target).status();
                    let _ = std::fs::remove_file(&target);
                    std::process::exit(status.ok().and_then(|s| s.code()).unwrap_or(1));
                }
                Err(error) => {
                    eprintln!("{}", error);
                    false
                }
            }
        }
        None => {
            eprintln!("usage : cchost --asm | --corpus dossier | fichier.c ... [-o sortie] [-S sortie.s]");
            false
        }
    };
    std::process::exit(if ok { 0 } else { 1 });
}
