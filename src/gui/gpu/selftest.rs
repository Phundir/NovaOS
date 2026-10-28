//! Self-test of the GPU compositor: the same scene painted by the
//! processor and composed by the GPU must give the same pixels, every
//! one of them.
//!
//! The scene gathers what the painter knows how to do and every edge case
//! of its clipping: a decorated window (rounded bar, border, title in a
//! transparent background, close cross in lines), text on an opaque
//! background, icons, a frame, a diagonal line, a window half off the
//! screen (glyphs and icons cut by its edge), and a canvas whose corner
//! lies off the screen. It is drawn whole, then a second time over a
//! damaged rectangle only -- the path of every frame after the first,
//! where the GPU loads the composed image and draws inside a scissor.

use alloc::string::String;
use alloc::vec::Vec;

use super::Renderer;
use crate::gui::canvas::{self, Canvas};
use crate::gui::compositor::{self, Compositor};
use crate::gui::painter::Image;
use crate::gui::surface::{Command, Layer, Rect};
use crate::gui::theme::Theme;

const WIDTH: u32 = 320;
const HEIGHT: u32 = 200;

/// Verdicts, in the order they were reached.
pub type Checks = Vec<(&'static str, bool)>;

fn scene(comp: &mut Compositor, canvas: &canvas::CanvasRef) {
    let owner = crate::scheduler::current_pid();
    let window = comp.stack.create(owner, Layer::Window, Rect::new(20, 40, 160, 90), String::from("Essai du GPU"));
    let off_screen = comp.stack.create(owner, Layer::Window, Rect::new(260, 150, 120, 80), String::from("Hors champ"));
    let shell = comp.stack.create(owner, Layer::Shell, Rect::new(0, 0, WIDTH as i32, 24), String::new());
    let icon: [u8; 32] = [
        0x80, 0x01, 0x40, 0x02, 0x20, 0x04, 0x10, 0x08, 0x08, 0x10, 0x04, 0x20, 0x02, 0x40, 0x01, 0x80, 0x01, 0x80,
        0x02, 0x40, 0x04, 0x20, 0x08, 0x10, 0x10, 0x08, 0x20, 0x04, 0x40, 0x02, 0xFF, 0xFF,
    ];
    let commands: [(Option<u32>, Vec<Command>); 3] = [
        (
            window,
            alloc::vec![
                Command::Fill { color: 0x0020_2833 },
                Command::Rect { rect: Rect::new(4, 4, 60, 20), color: 0x00AA_3344 },
                Command::Frame { rect: Rect::new(70, 4, 50, 30), color: 0x0011_CC66, thickness: 3 },
                Command::Line { x0: 2, y0: 80, x1: 150, y1: 30, color: 0x00FF_FF00 },
                Command::Text { x: 6, y: 40, color: 0x00FF_FFFF, background: None, text: String::from("Nova GPU !") },
                Command::Text { x: 6, y: 60, color: 0x0000_0000, background: Some(0x00CC_CCCC), text: String::from("fond opaque") },
                Command::Icon { x: 130, y: 60, bits: icon, color: 0x0000_FFFF, background: None },
                Command::Icon { x: 150, y: 70, bits: icon, color: 0x00FF_00FF, background: Some(0x0033_3333) },
                Command::Canvas { x: -30, y: 20, canvas: canvas.clone() },
            ],
        ),
        (
            off_screen,
            alloc::vec![
                Command::Fill { color: 0x0044_2200 },
                Command::Text { x: 30, y: 10, color: 0x00FF_8800, background: Some(0x0000_0044), text: String::from("coupe au bord") },
                Command::Icon { x: 50, y: 30, bits: icon, color: 0x0000_FF00, background: None },
            ],
        ),
        (
            shell,
            alloc::vec![
                Command::Fill { color: 0x0010_1010 },
                Command::Text { x: 4, y: 4, color: 0x0088_CCFF, background: None, text: String::from("Barre 12:34") },
            ],
        ),
    ];
    for (id, list) in commands {
        let Some(id) = id else { continue };
        if let Some(surface) = comp.stack.get_mut(id) {
            for command in list {
                surface.push(command);
            }
            surface.commit();
        }
    }
}

/// The processor's picture of `damage`, over what `image` already holds.
fn reference(comp: &Compositor, image: &mut Image, damage: Rect) {
    compositor::paint(comp, image, damage);
}

fn compare(renderer: &Renderer, image: &Image) -> (usize, Option<(u32, u32, u32, u32)>) {
    let mut wrong = 0;
    let mut first = None;
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let expected = image.pixels[(y * WIDTH + x) as usize] & 0x00FF_FFFF;
            let got = renderer.output_pixel(x, y).unwrap_or(u32::MAX);
            if expected != got {
                wrong += 1;
                if first.is_none() {
                    first = Some((x, y, expected, got));
                }
            }
        }
    }
    (wrong, first)
}

pub fn run() -> Checks {
    let mut checks: Checks = Vec::new();
    let mut renderer = match Renderer::offscreen(WIDTH, HEIGHT) {
        Ok(renderer) => renderer,
        Err(reason) => {
            crate::serial_println!("[gpu] compositeur d'essai refuse : {}", reason);
            checks.push(("le compositeur GPU s'ouvre hors ecran", false));
            return checks;
        }
    };
    checks.push(("le compositeur GPU s'ouvre hors ecran", true));

    // A canvas of 64 x 48: a gradient, so a copy shifted by one pixel shows.
    let mut canvas = Canvas::new(64, 48);
    for y in 0..48 {
        for x in 0..64 {
            canvas.pixels[y * 64 + x] = ((x as u32 * 4) << 16) | ((y as u32 * 5) << 8) | ((x + y) as u32 & 0xFF);
        }
    }
    canvas.publish();
    let Some(canvas_ref) = canvas::lookup(canvas.id, crate::scheduler::current_pid()) else {
        checks.push(("la toile d'essai existe", false));
        renderer.teardown();
        return checks;
    };

    let mut comp = Compositor::new(WIDTH as i32, HEIGHT as i32, Theme::default());
    scene(&mut comp, &canvas_ref);
    let mut image = Image::new(WIDTH as usize, HEIGHT as usize, false);
    let whole = Rect::new(0, 0, WIDTH as i32, HEIGHT as i32);

    // --- the whole screen ---
    reference(&comp, &mut image, whole);
    let frame = renderer.record(whole, |recorder| compositor::paint(&comp, recorder, whole));
    let instances = frame.instances.len();
    let drawn = renderer.draw(frame);
    let (wrong, first) = compare(&renderer, &image);
    if let Some((x, y, expected, got)) = first {
        crate::serial_println!("[gpu] composition : {} pixel(s) differents, le premier en ({}, {}) : {:06x} attendu, {:06x} lu", wrong, x, y, expected, got);
    }
    crate::serial_println!("[gpu]   composition {}x{} : {} instances, {} pixel(s) differents du processeur", WIDTH, HEIGHT, instances, wrong);
    checks.push(("le GPU compose une image entiere", drawn.is_ok()));
    checks.push(("image entiere : chaque pixel egal a celui du processeur", drawn.is_ok() && wrong == 0));

    // --- a damaged rectangle, over the previous frame ---
    canvas.pixels.iter_mut().for_each(|pixel| *pixel ^= 0x00FF_FFFF);
    canvas.publish();
    let moved = comp.stack.iter().find(|s| s.title == "Hors champ").map(|s| s.id);
    if let Some(surface) = moved.and_then(|id| comp.stack.get_mut(id)) {
        surface.rect.x -= 40;
    }
    let damage = Rect::new(10, 30, 290, 140);
    reference(&comp, &mut image, damage);
    let frame = renderer.record(damage, |recorder| compositor::paint(&comp, recorder, damage));
    let drawn = renderer.draw(frame);
    let (wrong, first) = compare(&renderer, &image);
    if let Some((x, y, expected, got)) = first {
        crate::serial_println!("[gpu] zone : {} pixel(s) differents, le premier en ({}, {}) : {:06x} attendu, {:06x} lu", wrong, x, y, expected, got);
    }
    checks.push(("zone endommagee seule : chaque pixel egal a celui du processeur", drawn.is_ok() && wrong == 0));
    // --- the accessibility transforms, on the processor after the GPU:
    // a colour filter and the magnifier, against the software path's own
    // mapping of the reference ---
    let transforms = super::Transforms {
        zoom_on: true,
        zoom: crate::access::zoom::Settings { percent: 200, ..Default::default() },
        centre: (120, 90),
        mode: crate::access::contrast::Mode::Achromatopsia,
        matrix: crate::access::contrast::Matrix([256, 0, 0, 0, 256, 0, 0, 0, 256]),
    };
    let frame = renderer.record(whole, |recorder| compositor::paint(&comp, recorder, whole));
    let drawn = renderer.draw_with(frame, Some(transforms));
    let sample = |x: usize, y: usize| image.pixels[y.min(HEIGHT as usize - 1) * WIDTH as usize + x.min(WIDTH as usize - 1)] & 0x00FF_FFFF;
    let mut mapped = Image::new(WIDTH as usize, HEIGHT as usize, false);
    for y in 0..HEIGHT as usize {
        for x in 0..WIDTH as usize {
            mapped.pixels[y * WIDTH as usize + x] = crate::access::map_pixel(
                &sample,
                WIDTH as i32,
                HEIGHT as i32,
                x,
                y,
                transforms.zoom_on,
                &transforms.zoom,
                transforms.centre,
                transforms.mode,
                &transforms.matrix,
            );
        }
    }
    let (wrong, first) = compare(&renderer, &mapped);
    if let Some((x, y, expected, got)) = first {
        crate::serial_println!("[gpu] transformations : {} pixel(s) differents, le premier en ({}, {}) : {:06x} attendu, {:06x} lu", wrong, x, y, expected, got);
    }
    checks.push(("loupe et filtre de couleur : chaque pixel egal au chemin processeur", drawn.is_ok() && wrong == 0));

    let (textures, icons) = renderer.held();
    checks.push(("une toile et une seule case d icone pour deux icones identiques", textures == 1 && icons == 1));

    drop(canvas);
    renderer.teardown();
    checks
}

/// Waits, a few seconds at most, for `ready`.
fn wait_for(ready: impl Fn() -> bool, ms: u64) -> bool {
    let deadline = crate::idt::fine_ticks() + ms;
    while !ready() {
        if crate::idt::fine_ticks() >= deadline {
            return false;
        }
        let _ = crate::syscall::invoke(crate::syscall::domain_time::SYS_TIME_SLEEP_MS, [2, 0, 0, 0, 0, 0]);
    }
    true
}

/// One run of the measure: `frames` new images of the canvas, each shown
/// before the next. Returns (frames timed, compositor ns, mean delay from
/// publish to screen in us).
fn run_frames(canvas: &mut Canvas, surface: u32, frames: u32, gpu: bool) -> (u64, u64, u64) {
    let before = *crate::gui::FRAME_TIME.lock();
    let _ = canvas.take_pacing();
    for index in 0..frames {
        let shade = (index * 2) & 0xFF;
        canvas.pixels.fill((shade << 16) | ((255 - shade) << 8) | 0x40);
        canvas.publish();
        let seen = crate::gui::with(|comp| {
            comp.damage_surface(surface);
            comp.frames
        })
        .unwrap_or(0);
        wait_for(|| crate::gui::with(|comp| comp.frames).unwrap_or(0) > seen, 500);
    }
    let after = *crate::gui::FRAME_TIME.lock();
    let pacing = canvas.take_pacing();
    let delay = pacing.latency_sum_ns / pacing.frames.max(1) / 1000;
    if gpu {
        (after.gpu_frames - before.gpu_frames, after.gpu_ns - before.gpu_ns, delay)
    } else {
        (after.processor_frames - before.processor_frames, after.processor_ns - before.processor_ns, delay)
    }
}

/// One run of whole-screen frames: every surface repainted, nothing
/// uploaded. Returns (frames timed, compositor ns).
fn run_screens(frames: u32, gpu: bool) -> (u64, u64) {
    let before = *crate::gui::FRAME_TIME.lock();
    for _ in 0..frames {
        let seen = crate::gui::with(|comp| {
            comp.damage_all();
            comp.frames
        })
        .unwrap_or(0);
        wait_for(|| crate::gui::with(|comp| comp.frames).unwrap_or(0) > seen, 500);
    }
    let after = *crate::gui::FRAME_TIME.lock();
    if gpu {
        (after.gpu_frames - before.gpu_frames, after.gpu_ns - before.gpu_ns)
    } else {
        (after.processor_frames - before.processor_frames, after.processor_ns - before.processor_ns)
    }
}

/// The 13.8.3 measure: a 1024 x 640 canvas republished a hundred times,
/// composed on the GPU, then on the processor. Printed, not judged: the
/// numbers depend on the host.
pub fn measure() {
    let composing = || crate::gui::gpu::STATS.lock().is_some() && crate::gpu::screen::shown();
    if !crate::gui::is_active() || !wait_for(composing, 5000) {
        crate::serial_println!("[gui] mesure 13.8.3 ignoree - le compositeur ne compose pas sur le GPU");
        return;
    }
    const FRAMES: u32 = 100;
    let owner = crate::scheduler::current_pid();
    let mut canvas = Canvas::new(1024, 640);
    let Some(canvas_ref) = canvas::lookup(canvas.id, owner) else { return };
    let surface = crate::gui::with(|comp| {
        let id = comp.stack.create(owner, Layer::Window, Rect::new(100, 60, 1024, 640), String::from("mesure 13.8.3"))?;
        if let Some(surface) = comp.stack.get_mut(id) {
            surface.push(Command::Canvas { x: 0, y: 0, canvas: canvas_ref.clone() });
            surface.commit();
        }
        comp.damage_surface(id);
        Some(id)
    })
    .flatten();
    let Some(surface) = surface else { return };

    let (gpu_screens, gpu_screen_ns) = run_screens(FRAMES, true);
    let stats_before = crate::gui::gpu::STATS.lock().map(|(s, _)| s).unwrap_or_default();
    let (gpu_frames, gpu_ns, gpu_delay) = run_frames(&mut canvas, surface, FRAMES, true);
    let stats_after = crate::gui::gpu::STATS.lock().map(|(s, _)| s).unwrap_or_default();
    let frames = (stats_after.frames - stats_before.frames).max(1);
    let gpu_cpu = (stats_after.cpu_ns - stats_before.cpu_ns) / frames / 1000;
    let gpu_wait = (stats_after.wait_ns - stats_before.wait_ns) / frames / 1000;

    super::drive::prefer_processor(true);
    wait_for(|| !crate::gpu::screen::shown(), 3000);
    let (cpu_frames, cpu_ns, cpu_delay) = run_frames(&mut canvas, surface, FRAMES, false);
    let (cpu_screens, cpu_screen_ns) = run_screens(FRAMES, false);
    super::drive::prefer_processor(false);
    wait_for(composing, 5000);

    crate::gui::with(|comp| {
        comp.damage_surface(surface);
        comp.stack.destroy(surface);
    });
    drop(canvas_ref);
    drop(canvas);
    crate::serial_println!(
        "[gui] mesure 13.8.3 (toile 1024x640, {} images) : GPU {} us/image dont {} us de processeur et {} us d'attente du GPU, delai toile-ecran {} us ({} images) - processeur {} us/image, delai {} us ({} images)",
        FRAMES,
        gpu_ns / gpu_frames.max(1) / 1000,
        gpu_cpu,
        gpu_wait,
        gpu_delay,
        gpu_frames,
        cpu_ns / cpu_frames.max(1) / 1000,
        cpu_delay,
        cpu_frames
    );
    crate::serial_println!(
        "[gui] mesure 13.8.3 (ecran entier, {} images) : GPU {} us/image ({} images) - processeur {} us/image ({} images)",
        FRAMES,
        gpu_screen_ns / gpu_screens.max(1) / 1000,
        gpu_screens,
        cpu_screen_ns / cpu_screens.max(1) / 1000,
        cpu_screens
    );
}
