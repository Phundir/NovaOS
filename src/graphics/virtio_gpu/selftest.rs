//! Self-test of the virtio-gpu driver.
//!
//! The command layouts are checked against the sizes and offsets of the
//! virtio specification: a field one word off is accepted by the device
//! and answered with `ERR_INVALID_PARAMETER`, or worse, with a resource of
//! the wrong size -- and nothing on screen says which.
//!
//! The live part runs only when the device drives the screen: it checks
//! that the three places holding the screen size -- the driver, the
//! framebuffer and the pointer -- agree.

use super::proto::{self, Area};

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
            crate::serial_println!("[virtio-gpu] ECHEC : {}", label);
        }
    }
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([bytes[offset], bytes[offset + 1], bytes[offset + 2], bytes[offset + 3]])
}

pub fn run() -> (usize, usize) {
    let mut count = Count {
        passed: 0,
        failed: 0,
    };
    layouts(&mut count);
    backing(&mut count);
    answers(&mut count);
    format(&mut count);
    live(&mut count);
    (count.passed, count.failed)
}

fn layouts(count: &mut Count) {
    let area = Area::new(1, 2, 3, 4);
    count.check(
        "les commandes ont les tailles de la specification",
        proto::get_display_info().len() == 24
            && proto::resource_create_2d(7, 800, 600).len() == 40
            && proto::resource_unref(7).len() == 32
            && proto::set_scanout(0, 7, area).len() == 48
            && proto::resource_flush(7, area).len() == 48
            && proto::transfer_to_host_2d(7, area, 0).len() == 56
            && proto::attach_backing(7, 3).len() == 32
            && proto::detach_backing(7).len() == 32,
    );

    let create = proto::resource_create_2d(7, 800, 600);
    count.check(
        "RESOURCE_CREATE_2D : type, ressource, format, dimensions",
        u32_at(&create, 0) == proto::CMD_RESOURCE_CREATE_2D
            && u32_at(&create, 24) == 7
            && u32_at(&create, 28) == proto::FORMAT_B8G8R8X8
            && u32_at(&create, 32) == 800
            && u32_at(&create, 36) == 600,
    );

    let scanout = proto::set_scanout(0, 9, Area::new(0, 0, 1280, 720));
    count.check(
        "SET_SCANOUT : le rectangle precede l'ecran et la ressource",
        u32_at(&scanout, 32) == 1280
            && u32_at(&scanout, 36) == 720
            && u32_at(&scanout, 40) == 0
            && u32_at(&scanout, 44) == 9,
    );

    let transfer = proto::transfer_to_host_2d(5, area, 0x1_0000_0010);
    let offset = u64::from_le_bytes([
        transfer[40], transfer[41], transfer[42], transfer[43], transfer[44], transfer[45],
        transfer[46], transfer[47],
    ]);
    count.check(
        "TRANSFER_TO_HOST_2D : decalage sur 64 bits, puis la ressource",
        offset == 0x1_0000_0010 && u32_at(&transfer, 48) == 5,
    );
}

fn backing(count: &mut Count) {
    let runs = proto::coalesce(&[0x1000, 0x2000, 0x3000, 0x8000, 0x9000, 0x5000], 0x1000);
    count.check(
        "les cadres contigus fusionnent, les autres non",
        runs == [(0x1000, 0x3000), (0x8000, 0x2000), (0x5000, 0x1000)],
    );
    count.check("une liste vide reste vide", proto::coalesce(&[], 0x1000).is_empty());

    let entries = proto::backing_entries(&runs);
    count.check(
        "une entree de support fait seize octets",
        entries.len() == 3 * proto::BACKING_ENTRY_SIZE
            && u32_at(&entries, 16) == 0x8000
            && u32_at(&entries, 24) == 0x2000,
    );
}

/// A display-info answer for one scanout, as the device writes it.
fn display_info(kind: u32, width: u32, height: u32, enabled: u32) -> alloc::vec::Vec<u8> {
    let mut bytes = alloc::vec![0u8; proto::DISPLAY_INFO_SIZE];
    bytes[0..4].copy_from_slice(&kind.to_le_bytes());
    let base = proto::HEADER_SIZE;
    bytes[base + 8..base + 12].copy_from_slice(&width.to_le_bytes());
    bytes[base + 12..base + 16].copy_from_slice(&height.to_le_bytes());
    bytes[base + 16..base + 20].copy_from_slice(&enabled.to_le_bytes());
    bytes
}

fn answers(count: &mut Count) {
    count.check(
        "la taille de l'ecran 0 se lit",
        proto::scanout_size(&display_info(proto::RESP_OK_DISPLAY_INFO, 1920, 1080, 1))
            == Some((1920, 1080)),
    );
    count.check(
        "un ecran eteint n'a pas de taille",
        proto::scanout_size(&display_info(proto::RESP_OK_DISPLAY_INFO, 1920, 1080, 0)).is_none(),
    );
    count.check(
        "une reponse d'erreur n'est pas lue comme une taille",
        proto::scanout_size(&display_info(0x1200, 1920, 1080, 1)).is_none(),
    );
    count.check(
        "une reponse tronquee est refusee",
        proto::scanout_size(&display_info(proto::RESP_OK_DISPLAY_INFO, 800, 600, 1)[..30]).is_none(),
    );
    count.check(
        "la taille demandee est bornee des deux cotes",
        super::clamp_size(200, 100) == (super::MIN_WIDTH, super::MIN_HEIGHT)
            && super::clamp_size(9000, 9000) == (super::MAX_WIDTH, super::MAX_HEIGHT)
            && super::clamp_size(1366, 768) == (1366, 768),
    );
}

fn format(count: &mut Count) {
    // B8G8R8X8 read as a little-endian word: exactly the layout the
    // framebuffer is told about in `set_mode`.
    let info = crate::multiboot::FramebufferInfo {
        addr: 0,
        pitch: 4,
        width: 1,
        height: 1,
        bpp: 32,
        kind: crate::multiboot::FB_RGB,
        red_pos: 16,
        red_size: 8,
        green_pos: 8,
        green_size: 8,
        blue_pos: 0,
        blue_size: 8,
    };
    let word = super::super::framebuffer::Format::from_info(&info)
        .encode(super::super::framebuffer::rgb(0x12, 0x34, 0x56));
    count.check(
        "un pixel s'ecrit B, V, R, X en memoire",
        word.to_le_bytes() == [0x56, 0x34, 0x12, 0x00],
    );
}

fn live(count: &mut Count) {
    if !super::is_active() {
        crate::serial_println!("[virtio-gpu]   absent - framebuffer GRUB, pas de redimensionnement");
        return;
    }
    let screen = crate::graphics::resolution().map(|(w, h)| (w as i32, h as i32));
    count.check(
        "le framebuffer et le pointeur ont la meme taille",
        screen == Some(crate::drivers::mouse::surface()),
    );
    let compositor = crate::gui::with(|comp| (comp.width, comp.height));
    count.check(
        "le compositeur a la taille du framebuffer",
        compositor.is_none() || compositor == screen,
    );
}
