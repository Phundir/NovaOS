//! Self-test of the GPU stack (13.3.4, 13.7.2).
//!
//! ## Two halves
//!
//! The first half needs no GPU: the SPIR-V validator, the command buffer
//! validation against a table of objects made up for it, the layouts of
//! the new virtio-gpu commands, the float patterns. It runs everywhere,
//! and it is the half that says the kernel refuses what it must.
//!
//! The second half runs when a GPU is present, and goes through the
//! system calls exactly as a program would (`syscall::invoke`, which
//! differs from a ring 3 call only by the provenance of its pointers): a
//! context, memory, a vertex buffer, two shaders, a pipeline, a triangle
//! drawn into an image, the image copied to memory the CPU reads -- and the
//! pixels checked. Then what must fail: a command outside a pass, a handle
//! of the wrong kind, the quota. Then everything destroyed, and the
//! accounting back to zero. Without a GPU the second half is skipped and
//! says so: "absent", not a failure.

mod programs;

use programs::{compositor, native_program, rust_program};

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use super::api::{self, kind, nr};
use super::backend::{Format, LayoutBinding, BindingKind};
use super::cmd::{self, op, Info, Object};
use crate::graphics::virtio_gpu::proto;

static TRIANGLE_VERT: &[u8] = include_bytes!("../shaders/triangle.vert.spv");
static TRIANGLE_FRAG: &[u8] = include_bytes!("../shaders/triangle.frag.spv");

struct Count {
    passed: usize,
    failed: usize,
}

impl Count {
    fn check(&mut self, label: &str, condition: bool) {
        if condition {
            self.passed += 1;
        } else {
            self.failed += 1;
            crate::serial_println!("[gpu] ECHEC : {}", label);
        }
    }
}

pub fn run() -> (usize, usize, bool) {
    let mut count = Count { passed: 0, failed: 0 };
    spirv(&mut count);
    floats(&mut count);
    commands(&mut count);
    transport(&mut count);
    modules(&mut count);
    let live = super::present();
    if live {
        let batches = super::venus::RING_BATCHES.load(core::sync::atomic::Ordering::Relaxed);
        live_tests(&mut count);
        count.check(
            "les lots Venus passent par l'anneau de chaque session",
            super::venus::RING_BATCHES.load(core::sync::atomic::Ordering::Relaxed) > batches
                && super::venus::RING_REFUSED.load(core::sync::atomic::Ordering::Relaxed) == 0,
        );
        native_program(&mut count);
        rust_program(&mut count);
        compositor(&mut count);
    } else {
        crate::serial_println!("[gpu]   GPU absent - essais sur le materiel ignores");
    }
    (count.passed, count.failed, live)
}

fn words(bytes: &[u8]) -> Vec<u32> {
    super::spirv::words(bytes).unwrap_or_default()
}

fn spirv(count: &mut Count) {
    use super::spirv::{validate, Stage};
    let vert = words(TRIANGLE_VERT);
    let frag = words(TRIANGLE_FRAG);
    count.check("SPIR-V : le shader de sommets est accepte", validate(&vert, Stage::Vertex).is_ok());
    count.check("SPIR-V : le shader de fragments est accepte", validate(&frag, Stage::Fragment).is_ok());
    count.check("SPIR-V : un shader de sommets n'est pas un shader de fragments", validate(&vert, Stage::Fragment).is_err());
    let mut bad = vert.clone();
    bad[0] ^= 1;
    count.check("SPIR-V : un nombre magique faux est refuse", validate(&bad, Stage::Vertex).is_err());
    count.check("SPIR-V : un module tronque est refuse", validate(&vert[..4], Stage::Vertex).is_err());
    // The first instruction after the header is OpCapability Shader: make
    // it Float64 (10), which the device was not created with.
    let mut float64 = vert.clone();
    if float64.get(5).is_some_and(|w| w & 0xFFFF == 17) {
        float64[6] = 10;
    }
    count.check("SPIR-V : une capacite non offerte est refusee", validate(&float64, Stage::Vertex).is_err());
    let mut extension = vert.clone();
    // An OpExtension "x" inserted after the header.
    extension.splice(5..5, [(2 << 16) | 10, u32::from_le_bytes(*b"x\0\0\0")]);
    count.check("SPIR-V : une extension est refusee", validate(&extension, Stage::Vertex).is_err());
    let mut runaway = vert.clone();
    let last = runaway.len() - 1;
    runaway[last] = (0x7FFF << 16) | 1;
    count.check("SPIR-V : une instruction qui deborde du module est refusee", validate(&runaway, Stage::Vertex).is_err());
}

fn floats(count: &mut Count) {
    use super::cmd::float_bits;
    count.check(
        "les tailles deviennent des flottants exacts sans instruction flottante",
        float_bits(0) == 0
            && float_bits(1) == 0x3F80_0000
            && float_bits(3) == 0x4040_0000
            && float_bits(64) == 0x4280_0000
            && float_bits(1280) == 0x44A0_0000
            && float_bits(16_777_215) == 0x4B7F_FFFF,
    );
}

/// A table of objects for the validation, made up: handles 1 to 8.
fn table() -> BTreeMap<u32, Object> {
    let mut objects = BTreeMap::new();
    let usage = super::backend::image_usage::RENDER | super::backend::image_usage::SAMPLED;
    objects.insert(1, Object { backend: 101, info: Info::Image { format: Format::Rgba8, width: 64, height: 64, usage, initialized: false } });
    objects.insert(2, Object { backend: 102, info: Info::Buffer { size: 4096, usage: super::backend::buffer_usage::VERTEX } });
    objects.insert(3, Object { backend: 103, info: Info::Layout { bindings: Vec::new(), push_constants: 16 } });
    objects.insert(4, Object { backend: 104, info: Info::Pipeline { layout: 3, color: Format::Rgba8, depth: None, vertex_bindings: 1 } });
    objects.insert(5, Object { backend: 105, info: Info::Pipeline { layout: 3, color: Format::Bgra8, depth: None, vertex_bindings: 1 } });
    objects.insert(6, Object { backend: 106, info: Info::Buffer { size: 64 * 64 * 4, usage: 0 } });
    objects.insert(7, Object { backend: 107, info: Info::Sampler });
    let bindings = alloc::vec![LayoutBinding { binding: 0, kind: BindingKind::SampledImage, stages: 2 }];
    objects.insert(8, Object { backend: 108, info: Info::Layout { bindings, push_constants: 0 } });
    objects
}

fn header(opcode: u32, operands: usize) -> u32 {
    opcode | ((operands as u32 + 1) << 16)
}

fn begin(out: &mut Vec<u32>, image: u32, width: u32, height: u32) {
    out.push(header(op::BEGIN_RENDER, 13));
    out.extend_from_slice(&[image, 0, 0, 0, 0x3F80_0000, 0x3F80_0000, 0, 0, 0, 0, 0, width, height]);
}

fn draw(out: &mut Vec<u32>, pipeline: u32, vertex: u32) {
    out.push(header(op::BIND_PIPELINE, 1));
    out.push(pipeline);
    out.push(header(op::BIND_VERTEX, 4));
    out.extend_from_slice(&[0, vertex, 0, 0]);
    out.push(header(op::PUSH_CONSTANTS, 5));
    out.extend_from_slice(&[0, 0x3F80_0000, 0, 0, 0x3F80_0000]);
    out.push(header(op::DRAW, 4));
    out.extend_from_slice(&[3, 1, 0, 0]);
}

fn end(out: &mut Vec<u32>) {
    out.push(header(op::END_RENDER, 0));
}

fn copy_out(out: &mut Vec<u32>, image: u32, buffer: u32) {
    out.push(header(op::COPY_IMAGE_TO_BUFFER, 9));
    out.extend_from_slice(&[image, 0, 0, 64, 64, buffer, 0, 0, 0]);
}

fn commands(count: &mut Count) {
    let objects = table();
    let mut good = Vec::new();
    begin(&mut good, 1, 64, 64);
    draw(&mut good, 4, 2);
    end(&mut good);
    copy_out(&mut good, 1, 6);
    let accepted = cmd::translate(&good, &objects);
    count.check(
        "un tampon de commandes valide est traduit",
        accepted.as_ref().is_ok_and(|(commands, first)| commands.len() == 9 && first == &[1]),
    );

    let refused = |words: &[u32]| cmd::translate(words, &objects).is_err();
    let mut outside = Vec::new();
    draw(&mut outside, 4, 2);
    count.check("un dessin hors d'une passe est refuse", refused(&outside));

    let mut inside = Vec::new();
    begin(&mut inside, 1, 64, 64);
    copy_out(&mut inside, 1, 6);
    end(&mut inside);
    count.check("une copie dans une passe est refusee", refused(&inside));

    let mut mismatch = Vec::new();
    begin(&mut mismatch, 1, 64, 64);
    draw(&mut mismatch, 5, 2);
    end(&mut mismatch);
    count.check("un pipeline d'un autre format que la passe est refuse", refused(&mismatch));

    let mut unknown = Vec::new();
    begin(&mut unknown, 1, 64, 64);
    draw(&mut unknown, 4, 99);
    end(&mut unknown);
    count.check("une poignee inconnue est refusee", refused(&unknown));

    let mut wrong_kind = Vec::new();
    begin(&mut wrong_kind, 1, 64, 64);
    draw(&mut wrong_kind, 4, 6);
    end(&mut wrong_kind);
    count.check("un tampon sans usage de sommets est refuse comme tel", refused(&wrong_kind));

    let mut too_big = Vec::new();
    begin(&mut too_big, 1, 65, 64);
    end(&mut too_big);
    count.check("une zone de rendu hors de l'image est refusee", refused(&too_big));

    let mut open = Vec::new();
    begin(&mut open, 1, 64, 64);
    count.check("une passe non terminee est refusee", refused(&open));

    let mut overflow = Vec::new();
    overflow.push(header(op::COPY_BUFFER, 8));
    overflow.extend_from_slice(&[6, 2, 0xFFFF_FFF0, 0xFFFF_FFFF, 0, 0, 0x100, 0]);
    count.check("un decalage qui deborde en 64 bits est refuse", refused(&overflow));

    count.check("un en-tete de longueur nulle est refuse", refused(&[op::END_RENDER]));
    count.check("un en-tete plus long que le tampon est refuse", refused(&[header(op::DRAW, 40), 1, 2]));
    count.check("une commande inconnue est refusee", refused(&[header(999, 0)]));

    let mut resource = Vec::new();
    begin(&mut resource, 1, 64, 64);
    resource.push(header(op::BIND_PIPELINE, 1));
    resource.push(4);
    resource.push(header(op::BIND_RESOURCE, 7));
    resource.extend_from_slice(&[0, 1, 7, 0, 0, 0, 0]);
    end(&mut resource);
    count.check("une ressource hors de la disposition du pipeline est refusee", refused(&resource));
}

fn transport(count: &mut Count) {
    count.check(
        "les commandes 3D ont les tailles de la specification virtio",
        proto::ctx_create(1, proto::CAPSET_VENUS, "x").len() == 96
            && proto::ctx_destroy(1).len() == 24
            && proto::resource_create_blob(1, 2, 1, 0, 4096).len() == 56
            && proto::resource_map_blob(1, 2, 0).len() == 40
            && proto::submit_3d(1, 16, None).len() == 32
            && proto::get_capset_info(0).len() == 32,
    );
    // The cursor queue's two commands (virtio 1.2, 5.7.6.10): 56 bytes,
    // position at 28 and 32, resource at 40, hot spot at 44 and 48.
    let update = proto::update_cursor(7, 100, 200, 3, 4);
    let field = |bytes: &[u8], at: usize| u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
    count.check(
        "les commandes du curseur ont la disposition de la specification",
        update.len() == 56
            && field(&update, 0) == proto::CMD_UPDATE_CURSOR
            && field(&update, 28) == 100
            && field(&update, 32) == 200
            && field(&update, 40) == 7
            && field(&update, 44) == 3
            && field(&update, 48) == 4
            && proto::move_cursor(5, 6).len() == 56
            && field(&proto::move_cursor(5, 6), 0) == proto::CMD_MOVE_CURSOR,
    );
    let fenced = proto::submit_3d(3, 64, Some(proto::Fenced { fence_id: 0x1_0000_0002, ring: 1 }));
    count.check(
        "une soumission gardee porte sa barriere, son contexte et sa timeline",
        u32::from_le_bytes([fenced[4], fenced[5], fenced[6], fenced[7]]) == proto::FLAG_FENCE | proto::FLAG_INFO_RING_IDX
            && u64::from_le_bytes([fenced[8], fenced[9], fenced[10], fenced[11], fenced[12], fenced[13], fenced[14], fenced[15]]) == 0x1_0000_0002
            && u32::from_le_bytes([fenced[16], fenced[17], fenced[18], fenced[19]]) == 3
            && fenced[20] == 1
            && u32::from_le_bytes([fenced[24], fenced[25], fenced[26], fenced[27]]) == 64,
    );
    let ranges = crate::graphics::virtio_gpu::context::hostmem_selftest();
    count.check(
        "la region partagee rend ce qu'elle prete et recolle ses trous",
        ranges[0] == (0, 4096) && ranges[1] == (16384, 4096) && ranges[2].0 == ranges[2].1,
    );
}

// ============================================================
//  On the GPU
// ============================================================

fn call(number: u64, args: [u64; 6]) -> i64 {
    match crate::syscall::invoke(number as u16, args) {
        Ok(value) => value as i64,
        Err(_) => -38,
    }
}

fn create(context: i64, kind: u64, desc: &[u8]) -> i64 {
    call(nr::OBJECT_CREATE, [context as u64, kind, desc.as_ptr() as u64, desc.len() as u64, 0, 0])
}

fn le32(values: &[u32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_le_bytes()).collect()
}

fn submit(context: i64, words: &[u32], fence: i64) -> i64 {
    call(nr::SUBMIT, [context as u64, words.as_ptr() as u64, words.len() as u64 * 4, fence.max(0) as u64, 0, 0])
}

fn wait(fence: i64) -> i64 {
    let fences = [fence as u32];
    call(nr::FENCE_WAIT, [fences.as_ptr() as u64, 1, 3_000_000_000, 1, 0, 0])
}

/// The triangle, and the checks around it.
fn live_tests(count: &mut Count) {
    let owner = super::Owner::current();
    let before = super::usage_of(&owner);
    let started = crate::idt::fine_ticks();

    let mut info = [0u8; api::INFO_SIZE];
    let answer = call(nr::INFO, [info.as_mut_ptr() as u64, info.len() as u64, 0, 0, 0, 0]);
    count.check(
        "SYS_GPU_INFO decrit le GPU",
        answer == 0 && info[4] == 1 && info[64] != 0 && u32::from_le_bytes([info[28], info[29], info[30], info[31]]) & (1 << 2) != 0,
    );

    let context = call(nr::CONTEXT_CREATE, [0; 6]);
    count.check("un contexte s'ouvre", context > 0);
    if context <= 0 {
        return;
    }
    let opened = crate::idt::fine_ticks() - started;
    let c = context as u64;

    // --- memory: vertices (visible), and a read-back area ---
    let vertices_mem = call(nr::MEM_ALLOC, [c, 4096, 1, 0, 0, 0]);
    let readback_mem = call(nr::MEM_ALLOC, [c, 64 * 64 * 4, 2, 0, 0, 0]);
    count.check("la memoire visible et la memoire de relecture s'allouent", vertices_mem > 0 && readback_mem > 0);
    let vertices_at = call(nr::MEM_MAP, [c, vertices_mem as u64, 0, 0, 0, 0]);
    let readback_at = call(nr::MEM_MAP, [c, readback_mem as u64, 0, 0, 0, 0]);
    // An address of the kernel window reads negative as an i64: an error
    // is only ever -1 to -4095, as under Linux.
    let mapped = |at: i64| !(-4095..0).contains(&at) && at != 0;
    count.check("la memoire visible se projette", mapped(vertices_at) && mapped(readback_at));
    count.check("une memoire locale ne se projette pas", {
        let local = call(nr::MEM_ALLOC, [c, 65536, 0, 0, 0, 0]);
        let refused = call(nr::MEM_MAP, [c, local as u64, 0, 0, 0, 0]) == -22;
        call(nr::MEM_FREE, [c, local as u64, 0, 0, 0, 0]);
        local > 0 && refused
    });
    if !mapped(vertices_at) || !mapped(readback_at) {
        return;
    }
    // (0, -0.8), (0.8, 0.8), (-0.8, 0.8): the centre inside, the corners
    // outside.
    let triangle: [u32; 6] = [0, 0xBF4C_CCCD, 0x3F4C_CCCD, 0x3F4C_CCCD, 0xBF4C_CCCD, 0x3F4C_CCCD];
    // SAFETY: MEM_MAP returned a kernel mapping of 4096 bytes.
    unsafe { core::ptr::copy_nonoverlapping(triangle.as_ptr(), vertices_at as *mut u32, triangle.len()) };
    unsafe { core::ptr::write_bytes(readback_at as *mut u8, 0x55, 64 * 64 * 4) };

    // --- objects ---
    let mut buffer = le32(&[4096, 0, 1]);
    buffer.extend_from_slice(&(vertices_mem as u32).to_le_bytes());
    buffer.extend_from_slice(&0u64.to_le_bytes());
    let vertex = create(context, kind::BUFFER, &buffer);
    let mut readback_desc = le32(&[64 * 64 * 4, 0, 0]);
    readback_desc.extend_from_slice(&(readback_mem as u32).to_le_bytes());
    readback_desc.extend_from_slice(&0u64.to_le_bytes());
    let readback = create(context, kind::BUFFER, &readback_desc);
    let mut image_desc = le32(&[Format::Rgba8 as u32, 64, 64, 1, 0, 0]);
    image_desc.extend_from_slice(&0u64.to_le_bytes());
    let image = create(context, kind::IMAGE, &image_desc);
    count.check("tampons et image se creent", vertex > 0 && readback > 0 && image > 0);

    let shader = |code: &[u8], stage: u32| {
        let mut desc = (code.as_ptr() as u64).to_le_bytes().to_vec();
        desc.extend_from_slice(&le32(&[code.len() as u32, stage]));
        create(context, kind::SHADER, &desc)
    };
    let vert = shader(TRIANGLE_VERT, 1);
    let frag = shader(TRIANGLE_FRAG, 2);
    count.check("les shaders valides se creent", vert > 0 && frag > 0);
    count.check("un shader d'un autre etage est refuse par le noyau", shader(TRIANGLE_VERT, 2) == -22);
    let layout = create(context, kind::LAYOUT, &le32(&[0, 16]));
    let mut pipeline = le32(&[layout as u32, vert as u32, frag as u32, 0, 0, 0, 0, Format::Rgba8 as u32, 0, 0, 0, 1, 8, 0]);
    pipeline.extend_from_slice(&le32(&[0; 6]));
    pipeline.extend_from_slice(&le32(&[1, 0, 0, 2, 0]));
    pipeline.resize(api::PIPELINE_DESC, 0);
    let pipe = create(context, kind::PIPELINE, &pipeline);
    count.check("la disposition et le pipeline se creent", layout > 0 && pipe > 0);
    let fence = call(nr::FENCE_CREATE, [c, 0, 0, 0, 0, 0]);
    if vertex <= 0 || readback <= 0 || image <= 0 || pipe <= 0 || fence <= 0 {
        return;
    }

    // --- what must fail ---
    let mut outside = Vec::new();
    draw(&mut outside, pipe as u32, vertex as u32);
    count.check("un dessin hors d'une passe est refuse par SYS_GPU_SUBMIT", submit(context, &outside, 0) == -22);
    count.check("une memoire n'est pas un objet", call(nr::OBJECT_DESTROY, [c, vertices_mem as u64, 0, 0, 0, 0]) == -9);
    count.check("une poignee jamais donnee est inconnue", call(nr::OBJECT_DESTROY, [c, 0x7FFF_FFF0, 0, 0, 0, 0]) == -9);
    count.check("le quota refuse ce qui le depasse", call(nr::MEM_ALLOC, [c, super::QUOTA, 0, 0, 0, 0]) == -api::EDQUOT);

    // --- the triangle ---
    let mut words = Vec::new();
    begin(&mut words, image as u32, 64, 64);
    // Clear blue: the colour words of BEGIN_RENDER.
    words[3] = 0;
    words[4] = 0;
    words[5] = 0x3F80_0000;
    words[6] = 0x3F80_0000;
    draw(&mut words, pipe as u32, vertex as u32);
    end(&mut words);
    copy_out(&mut words, image as u32, readback as u32);
    let render_started = crate::idt::fine_ticks();
    let submitted = submit(context, &words, fence);
    // Destroyed while the GPU may still read it: the core must defer it.
    let destroyed = call(nr::OBJECT_DESTROY, [c, vertex as u64, 0, 0, 0, 0]);
    let waited = wait(fence);
    let rendered = crate::idt::fine_ticks() - render_started;
    count.check("le triangle est soumis et sa barriere se leve", submitted == 0 && waited == 0);
    count.check("un tampon detruit pendant le rendu l'est sans erreur", destroyed == 0);
    let pixel = |x: usize, y: usize| -> [u8; 4] {
        let mut out = [0u8; 4];
        // SAFETY: the read-back mapping covers 64 x 64 x 4 bytes.
        unsafe { core::ptr::copy_nonoverlapping((readback_at as *const u8).add((y * 64 + x) * 4), out.as_mut_ptr(), 4) };
        out
    };
    let centre = pixel(32, 36);
    let corner = pixel(1, 1);
    count.check("le centre du triangle est rouge", centre == [255, 0, 0, 255]);
    count.check("le coin de l'image a la couleur d'effacement", corner == [0, 0, 255, 255]);
    crate::serial_println!(
        "[gpu]   triangle 64x64 : centre {:?}, coin {:?} - contexte ouvert en {} ms, rendu et relu en {} ms",
        centre,
        corner,
        opened,
        rendered
    );

    // --- a second submission on the same context, after the refusal ---
    let mut again = Vec::new();
    again.push(header(op::FILL_BUFFER, 6));
    again.extend_from_slice(&[readback as u32, 0, 0, 16, 0, 0x0102_0304]);
    let refilled = submit(context, &again, fence) == 0 && wait(fence) == 0;
    // SAFETY: as for `pixel`.
    let word = unsafe { (readback_at as *const u32).read_volatile() };
    count.check("le contexte sert encore apres une soumission refusee", refilled && word == 0x0102_0304);

    // --- everything goes, and the accounting with it ---
    for object in [readback, image, pipe, layout, vert, frag] {
        call(nr::OBJECT_DESTROY, [c, object as u64, 0, 0, 0, 0]);
    }
    call(nr::OBJECT_DESTROY, [c, fence as u64, 0, 0, 0, 0]);
    let freed = call(nr::MEM_FREE, [c, vertices_mem as u64, 0, 0, 0, 0]) == 0 && call(nr::MEM_FREE, [c, readback_mem as u64, 0, 0, 0, 0]) == 0;
    count.check("les memoires se liberent une fois leurs objets detruits", freed);
    let closed = call(nr::CONTEXT_DESTROY, [c, 0, 0, 0, 0, 0]) == 0;
    count.check("le contexte se ferme", closed);
    count.check("une poignee d'un contexte ferme est inconnue", call(nr::MEM_MAP, [c, vertices_mem as u64, 0, 0, 0, 0]) == -9);
    count.check("toute la memoire GPU comptee est rendue", super::usage_of(&owner) == before);

    sharing(count);
}

/// An image drawn in one context, read back in another (13.4.2).
fn sharing(count: &mut Count) {
    if !super::description().is_some_and(|d| d.sharing) {
        crate::serial_println!("[gpu]   partage d'images non offert par l'hote - essai ignore");
        return;
    }
    let a = call(nr::CONTEXT_CREATE, [0; 6]);
    let b = call(nr::CONTEXT_CREATE, [0; 6]);
    if a <= 0 || b <= 0 {
        count.check("deux contextes s'ouvrent pour le partage", false);
        return;
    }
    let mut image_desc = le32(&[Format::Rgba8 as u32, 64, 64, 1, 0, 1]);
    image_desc.extend_from_slice(&0u64.to_le_bytes());
    let image = create(a, kind::IMAGE, &image_desc);
    let fence_a = call(nr::FENCE_CREATE, [a as u64, 0, 0, 0, 0, 0]);
    let mut clear = Vec::new();
    clear.push(header(op::CLEAR_IMAGE, 5));
    clear.extend_from_slice(&[image as u32, 0, 0x3F80_0000, 0, 0x3F80_0000]);
    let cleared = submit(a, &clear, fence_a) == 0 && wait(fence_a) == 0;
    let token = call(nr::SHARE, [a as u64, image as u64, 0, 0, 0, 0]);
    let imported = call(nr::IMPORT, [b as u64, token as u64, 0, 0, 0, 0]);
    count.check("une image se partage entre deux contextes", cleared && token != 0 && imported > 0);
    let memory = call(nr::MEM_ALLOC, [b as u64, 64 * 64 * 4, 2, 0, 0, 0]);
    let at = call(nr::MEM_MAP, [b as u64, memory as u64, 0, 0, 0, 0]);
    let mut desc = le32(&[64 * 64 * 4, 0, 0]);
    desc.extend_from_slice(&(memory as u32).to_le_bytes());
    desc.extend_from_slice(&0u64.to_le_bytes());
    let buffer = create(b, kind::BUFFER, &desc);
    let fence_b = call(nr::FENCE_CREATE, [b as u64, 0, 0, 0, 0, 0]);
    let mut copy = Vec::new();
    copy_out(&mut copy, imported as u32, buffer as u32);
    let mapped = !(-4095..0).contains(&at) && at != 0;
    let copied = imported > 0 && buffer > 0 && mapped && submit(b, &copy, fence_b) == 0 && wait(fence_b) == 0;
    let mut pixel = [0u8; 4];
    if copied {
        // SAFETY: the mapping covers 64 x 64 x 4 bytes.
        unsafe { core::ptr::copy_nonoverlapping((at as *const u8).add((10 * 64 + 10) * 4), pixel.as_mut_ptr(), 4) };
    }
    count.check("le second contexte lit ce que le premier a dessine", pixel == [0, 255, 0, 255]);
    call(nr::CONTEXT_DESTROY, [b as u64, 0, 0, 0, 0, 0]);
    call(nr::CONTEXT_DESTROY, [a as u64, 0, 0, 0, 0, 0]);
}

// ============================================================
//  Signed modules (13.4.1b)
// ============================================================

static TEST_MODULE: &[u8] = include_bytes!("../module/test-gpu.kmd");

/// The test module (tools/kmd/test-gpu.c): refused while unknown, refused
/// altered, refused of another interface, refused self-signed; accepted
/// once its digest is official -- then driven through the adapter, a GPU
/// made of CPU: memory, a buffer, three commands, the bytes read back.
fn modules(count: &mut Count) {
    use super::backend::{Command, Device as _, MemKind, WorkState};
    use super::module;

    // The sizes gcc gives the structures of tools/kmd/nova_kmd.h.
    count.check("les structures de nova_kmd.h ont la meme taille en C et en Rust", module::abi_sizes() == [104, 168, 104, 224, 112, 40]);
    count.check("les champs de nova_kmd_command sont aux memes places en C et en Rust", module::abi_offsets() == [32, 32, 40, 88, 24, 24]);

    let refused = |data: &[u8], wanted: &str| matches!(module::open(data), Err(reason) if reason.contains(wanted));
    count.check("un module SYSTEM hors de la table du noyau est refuse", refused(TEST_MODULE, "SYSTEM"));
    let mut altered = TEST_MODULE.to_vec();
    let last = altered.len() - 1;
    altered[last] ^= 0x40;
    count.check("un module altere est refuse", refused(&altered, "alteree"));
    let mut other = TEST_MODULE.to_vec();
    other[10] = 2;
    count.check("un module d'une autre interface est refuse", refused(&other, "interface"));
    let mut own = TEST_MODULE.to_vec();
    own[12] = 1;
    count.check("un module auto-signe non autorise est refuse", refused(&own, "SELF"));
    count.check("un fichier qui n'est pas un module est refuse", refused(b"pas un module", "pas un module"));

    let Ok(container) = module::parse(TEST_MODULE) else {
        count.check("le conteneur du module d'essai se lit", false);
        return;
    };
    let digest = module::authenticity_digest(TEST_MODULE, &container);
    crate::binfmt::signature::register_system_digest(digest);
    let loaded = match module::open(TEST_MODULE) {
        Ok(loaded) => loaded,
        Err(reason) => {
            crate::serial_println!("[gpu]   module d'essai : {}", reason);
            count.check("un module SYSTEM connu se charge", false);
            return;
        }
    };
    count.check("un module SYSTEM connu se charge et s'initialise", loaded.name == "test-gpu");
    count.check("le module ne trouve pas sa carte et le dit", !loaded.probe());

    // Its software GPU, through the same calls as the core's.
    let device = module::ModuleDevice::new(loaded);
    let description = device.describe();
    count.check("le module decrit son peripherique", description.vendor_id == 0x4E4F && description.name.starts_with("GPU d'essai"));
    let mut read_back = [0u32; 8];
    let worked = (|| -> Option<bool> {
        let mut context = device.open("essai-module").ok()?;
        let (memory, size) = context.alloc(8192, MemKind::Visible).ok()?;
        let window = context.map(memory).ok()?;
        let buffer = context.create_buffer(&super::backend::BufferDesc { size: 4096, usage: 0 }, memory, 0).ok()?;
        let second = context.create_buffer(&super::backend::BufferDesc { size: 4096, usage: 0 }, memory, 4096).ok()?;
        let update: alloc::vec::Vec<u8> = [0x0102_0304u32, 0x0506_0708].iter().flat_map(|w| w.to_le_bytes()).collect();
        let work = context
            .submit(&[
                Command::FillBuffer { buffer, offset: 0, size: 4096, value: 0xA5A5_A5A5 },
                Command::UpdateBuffer { buffer, offset: 8, data: update },
                Command::CopyBuffer { src: buffer, dst: second, src_offset: 0, dst_offset: 0, size: 32 },
            ])
            .ok()?;
        let done = context.poll(work) == WorkState::Done;
        // The module's pages are RAM: read through the physical window.
        let base = crate::memory::phys_to_virt(window.phys + 4096) as *const u32;
        for (index, word) in read_back.iter_mut().enumerate() {
            // SAFETY: the window covers 8192 bytes the module lent.
            *word = unsafe { base.add(index).read_volatile() };
        }
        context.destroy(super::backend::ObjectKind::Buffer, buffer);
        context.destroy(super::backend::ObjectKind::Buffer, second);
        context.unmap(memory);
        context.free(memory);
        context.close();
        Some(done && size == 8192)
    })();
    count.check(
        "le GPU du module remplit, met a jour et copie a travers l'interface",
        worked == Some(true) && read_back == [0xA5A5_A5A5, 0xA5A5_A5A5, 0x0102_0304, 0x0506_0708, 0xA5A5_A5A5, 0xA5A5_A5A5, 0xA5A5_A5A5, 0xA5A5_A5A5],
    );
    device.unload();
}

