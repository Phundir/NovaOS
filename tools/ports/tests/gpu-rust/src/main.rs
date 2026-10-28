//! gpu-rust -- a native Nova program in Rust drawing on the GPU (phase
//! 13.6.3), through `nova-gfx` alone: a context, memory mapped and
//! written, a pipeline from the kernel self-test's SPIR-V, a triangle,
//! the image read back and checked; then what must fail, and everything
//! given back by `Drop`.
//!
//! `#![no_std]`: the process gets its start, `write` and `exit` from the
//! Nova C library, its GPU from the crate -- the smallest Rust program
//! the Nova interface allows. Exit code 0 when every check held, 77 with
//! no GPU, otherwise the number of the first check that failed.

#![no_std]
#![no_main]

use nova_gfx::{
    buffer_usage, image_usage, Blend, Commands, Context, Error, Format, Load, MemKind, PipelineDesc, Rect, Stage, Topology,
    VertexAttribute, VertexBinding, VertexFormat, WAIT_FOREVER,
};

#[link(name = "c")]
extern "C" {
    fn write(fd: i32, buffer: *const u8, count: usize) -> isize;
    fn close(fd: i32) -> i32;
}

static VERTEX: &[u8] = include_bytes!("../../../../../src/gpu/shaders/triangle.vert.spv");
static FRAGMENT: &[u8] = include_bytes!("../../../../../src/gpu/shaders/triangle.frag.spv");

const W: u32 = 128;
const H: u32 = 96;

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    say(b"gpu-rust: panique\n");
    loop {
        core::hint::spin_loop();
    }
}

fn say(text: &[u8]) {
    // SAFETY: the bytes are readable for their length.
    unsafe { write(1, text.as_ptr(), text.len()) };
}

/// A line of text and numbers, without `alloc`: a small buffer written in
/// place.
struct Line {
    bytes: [u8; 160],
    len: usize,
}

impl Line {
    fn new(text: &str) -> Line {
        let mut line = Line { bytes: [0; 160], len: 0 };
        line.text(text);
        line
    }

    fn text(&mut self, text: &str) -> &mut Line {
        for byte in text.bytes() {
            if self.len < self.bytes.len() {
                self.bytes[self.len] = byte;
                self.len += 1;
            }
        }
        self
    }

    fn number(&mut self, mut value: u64) -> &mut Line {
        let mut digits = [0u8; 20];
        let mut count = 0;
        loop {
            digits[count] = b'0' + (value % 10) as u8;
            count += 1;
            value /= 10;
            if value == 0 {
                break;
            }
        }
        for index in (0..count).rev() {
            if self.len < self.bytes.len() {
                self.bytes[self.len] = digits[index];
                self.len += 1;
            }
        }
        self
    }

    fn say(&mut self) {
        self.text("\n");
        say(&self.bytes[..self.len]);
    }
}

struct Checks {
    count: u32,
    first_failure: u32,
}

impl Checks {
    fn ok(&mut self, condition: bool, what: &str) -> bool {
        self.count += 1;
        if !condition {
            Line::new("gpu-rust: ECHEC ").number(self.count as u64).text(" - ").text(what).say();
            if self.first_failure == 0 {
                self.first_failure = self.count;
            }
        }
        condition
    }
}

#[no_mangle]
pub extern "C" fn main(_argc: i32, _argv: *const *const u8) -> i32 {
    let mut checks = Checks { count: 0, first_failure: 0 };
    let info = match nova_gfx::info() {
        Ok(info) => info,
        Err(Error::NoDevice) => {
            say(b"gpu-rust: pas de GPU - essai ignore\n");
            return 77;
        }
        Err(_) => {
            checks.ok(false, "SYS_GPU_INFO");
            return 1;
        }
    };
    checks.ok(info.interface == nova_gfx::INTERFACE, "interface du noyau");
    Line::new("gpu-rust: ").text(info.name()).text(" - quota ").number(info.quota >> 20).text(" Mio").say();

    let status = triangle(&mut checks);
    Line::new("gpu-rust: ").number(checks.count as u64).text(" verifications").say();
    if checks.first_failure != 0 { checks.first_failure as i32 } else { status }
}

fn triangle(checks: &mut Checks) -> i32 {
    let Ok(context) = Context::create() else {
        checks.ok(false, "contexte");
        return 1;
    };
    let (Ok(mut vertices_memory), Ok(mut readback_memory)) =
        (context.memory(65536, MemKind::Visible), context.memory((W * H * 4) as u64, MemKind::Readback))
    else {
        checks.ok(false, "memoire");
        return 1;
    };
    // The vertices, written through the mapping.
    let triangle: [f32; 6] = [0.0, -0.8, 0.8, 0.8, -0.8, 0.8];
    match vertices_memory.map() {
        Ok(bytes) => {
            for (index, value) in triangle.iter().enumerate() {
                bytes[index * 4..index * 4 + 4].copy_from_slice(&value.to_le_bytes());
            }
            checks.ok(true, "memoire projetee et ecrite");
        }
        Err(_) => {
            checks.ok(false, "memoire projetee et ecrite");
            return 1;
        }
    }
    checks.ok(context.memory(4096, MemKind::Local).map(|mut m| m.map().is_err()).unwrap_or(false), "la memoire locale ne se projette pas");

    let vertex_buffer = context.buffer(&vertices_memory, 0, 65536, buffer_usage::VERTEX);
    let readback = context.buffer(&readback_memory, 0, (W * H * 4) as u64, 0);
    let image = context.image(Format::Rgba8, W, H, image_usage::RENDER, None, false);
    let vertex = context.shader(VERTEX, Stage::Vertex);
    let fragment = context.shader(FRAGMENT, Stage::Fragment);
    let layout = context.layout(&[], 16);
    let fence = context.fence();
    let (Ok(vertex_buffer), Ok(readback), Ok(image), Ok(vertex), Ok(fragment), Ok(layout), Ok(fence)) =
        (vertex_buffer, readback, image, vertex, fragment, layout, fence)
    else {
        checks.ok(false, "objets crees");
        return 1;
    };
    checks.ok(context.shader(&VERTEX[..4], Stage::Vertex).err() == Some(Error::Invalid), "un SPIR-V tronque est refuse");
    let bindings = [VertexBinding { stride: 8, per_instance: false }];
    let attributes = [VertexAttribute { location: 0, binding: 0, format: VertexFormat::Float2, offset: 0 }];
    let pipeline = context.pipeline(&PipelineDesc {
        layout: &layout,
        vertex: &vertex,
        fragment: &fragment,
        topology: Topology::Triangles,
        cull: 0,
        front_ccw: false,
        blend: Blend::None,
        color_format: Format::Rgba8,
        depth_format: None,
        depth_test: false,
        depth_write: false,
        bindings: &bindings,
        attributes: &attributes,
    });
    let Ok(pipeline) = pipeline else {
        checks.ok(false, "pipeline");
        return 1;
    };
    checks.ok(true, "objets crees");

    // --- what must fail ---
    let mut words = [0u32; 256];
    let mut commands = Commands::new(&mut words);
    commands.draw(3, 1, 0, 0);
    checks.ok(context.submit(&commands, None, &[]) == Err(Error::Invalid), "un dessin hors d'une passe est refuse");

    // --- the triangle ---
    let green: [f32; 4] = [0.0, 1.0, 0.0, 1.0];
    let mut constants = [0u8; 16];
    for (index, value) in green.iter().enumerate() {
        constants[index * 4..index * 4 + 4].copy_from_slice(&value.to_le_bytes());
    }
    commands.clear();
    commands
        .begin_render(&image, Load::Clear, [0.0, 0.0, 0.5, 1.0], Rect::new(0, 0, W, H))
        .bind_pipeline(&pipeline)
        .bind_vertex(0, &vertex_buffer, 0)
        .push_constants(0, &constants)
        .draw(3, 1, 0, 0)
        .end_render()
        .copy_image_to_buffer(&image, Rect::new(0, 0, W, H), &readback, 0, 0);
    checks.ok(!commands.overflowed() && context.submit(&commands, Some(&fence), &[]).is_ok(), "le triangle est soumis");
    let descriptor = fence.export();
    checks.ok(fence.wait(WAIT_FOREVER).is_ok(), "la barriere est levee");
    if let Ok(fd) = descriptor {
        // SAFETY: the descriptor is this process's, exported just above.
        unsafe { close(fd) };
    }
    let pixels = match readback_memory.map() {
        Ok(bytes) => bytes,
        Err(_) => {
            checks.ok(false, "relecture projetee");
            return 1;
        }
    };
    let at = |x: u32, y: u32| {
        let i = ((y * W + x) * 4) as usize;
        [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
    };
    let centre = at(W / 2, H / 2 + 4);
    let corner = at(1, 1);
    Line::new("gpu-rust: centre ")
        .number(centre[0] as u64)
        .text(" ")
        .number(centre[1] as u64)
        .text(" ")
        .number(centre[2] as u64)
        .text(" ")
        .number(centre[3] as u64)
        .text(", coin ")
        .number(corner[0] as u64)
        .text(" ")
        .number(corner[1] as u64)
        .text(" ")
        .number(corner[2] as u64)
        .text(" ")
        .number(corner[3] as u64)
        .say();
    checks.ok(centre == [0, 255, 0, 255], "le centre du triangle est vert");
    checks.ok(corner[0] == 0 && corner[1] == 0 && (corner[2] == 127 || corner[2] == 128) && corner[3] == 255, "le coin a la couleur d'effacement");
    // Everything goes back by `Drop`, the context last.
    0
}
