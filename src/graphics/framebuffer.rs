//! 7.1.2 -- Low-level framebuffer driver.
//!
//! ## What this module knows, and what it ignores
//!
//! It knows where the first pixel is, how many bytes separate two rows,
//! and how a colour is encoded in a hardware pixel. It knows nothing of
//! characters, windows or surfaces: it is the console of `console.rs`
//! and, later, the compositor of 8.1 that stack meaning on top. The
//! boundary is sharp -- everything handling pixels is here, everything
//! handling cells is elsewhere.
//!
//! ## Two buffers, and why
//!
//! Drawing straight on the hardware framebuffer gives a display built
//! under the user's eyes: a console scroll is seen row by row, a moved
//! window leaves a trail. The back buffer lives in ordinary RAM -- hence
//! cached, hence fast to read as to write -- and `present` copies it in
//! one block to the hardware. The image then changes all at once.
//!
//! The back buffer does not exist from boot: it demands frames, hence the
//! allocator of phase 2.3. Before it, the driver draws straight on the
//! hardware. That is what lets the console display the very first lines
//! of the kernel, the ones that precede the allocator.
//!
//! ## Cache types
//!
//! The framebuffer is not a register bank: no write has a side effect,
//! only the final image counts. Decorating it `NO_CACHE` like ordinary
//! MMIO would cost one bus access per pixel. So it is mapped
//! **write-combining** when the processor allows it (see
//! `memory::init_pat`): the writes accumulate and leave in bursts. For
//! want of a PAT, we fall back on the physical window, which is
//! write-back -- correct, only less predictable.
//!
//! It is also why `present` copies with `copy_nonoverlapping` and not
//! with a run of `write_volatile`: there is no register to touch in a
//! precise order, there are three mebibytes to move.

use crate::multiboot::FramebufferInfo;

/// Internal colour of the driver: eight bits per component, `0x00RRGGBB`.
///
/// A single internal format avoids every caller having to know the real
/// depth of the mode. The conversion to the hardware pixel happens at the
/// last moment, through `Format::encode`.
pub type Color = u32;

pub const fn rgb(red: u8, green: u8, blue: u8) -> Color {
    ((red as u32) << 16) | ((green as u32) << 8) | blue as u32
}

pub const fn red_of(color: Color) -> u8 {
    (color >> 16) as u8
}

pub const fn green_of(color: Color) -> u8 {
    (color >> 8) as u8
}

pub const fn blue_of(color: Color) -> u8 {
    color as u8
}

/// Mixes two colours, component by component.
///
/// `weight` is the share of `b`, from 0 (pure `a`) to 255 (pure `b`).
///
/// ## Why integers and not a ratio
///
/// No `f32` or `f64` anywhere in the kernel as long as SSE is off in
/// `CR0`/`CR4`: a single floating-point instruction on a soft-float target
/// either does not link or faults. `(a * (255 - w) + b * w) / 255` per
/// component holds in a `u32` -- 255 * 255 is 65025 -- and rounds down,
/// which for a shade is invisible.
///
/// This exists because a theme declares `surface` and `surface_alt` and
/// nothing in between, while a hover needs a shade **between** the two:
/// an element under the pointer that took the colour of a selected
/// element would say "selected" when it only means "the mouse is here".
pub const fn mix(a: Color, b: Color, weight: u8) -> Color {
    let inverse = 255 - weight as u32;
    let weight = weight as u32;
    let red = (red_of(a) as u32 * inverse + red_of(b) as u32 * weight) / 255;
    let green = (green_of(a) as u32 * inverse + green_of(b) as u32 * weight) / 255;
    let blue = (blue_of(a) as u32 * inverse + blue_of(b) as u32 * weight) / 255;
    rgb(red as u8, green as u8, blue as u8)
}

// ============================================================
//  Pixel format
// ============================================================

/// Position and width of each component in a hardware pixel.
///
/// Nothing is assumed: a 16-bit 5-6-5 mode, a 24-bit BGR mode and a
/// 32-bit XRGB mode are all described by these six numbers, which the
/// bootloader supplies. Hard-coding `0x00RRGGBB` would work under QEMU
/// and give wrong colours on the first real machine whose firmware
/// chooses BGR.
#[derive(Debug, Clone, Copy)]
pub struct Format {
    pub red_pos: u8,
    pub red_size: u8,
    pub green_pos: u8,
    pub green_size: u8,
    pub blue_pos: u8,
    pub blue_size: u8,
}

impl Format {
    pub fn from_info(info: &FramebufferInfo) -> Self {
        Format {
            red_pos: info.red_pos,
            red_size: info.red_size,
            green_pos: info.green_pos,
            green_size: info.green_size,
            blue_pos: info.blue_pos,
            blue_size: info.blue_size,
        }
    }

    /// Converts an internal colour into a hardware pixel pattern.
    ///
    /// A component narrower than eight bits keeps its **high** bits:
    /// truncating from the bottom preserves the relative brightness,
    /// truncating from the top would destroy it.
    pub fn encode(&self, color: Color) -> u32 {
        fn place(value: u8, position: u8, size: u8) -> u32 {
            if size == 0 {
                return 0;
            }
            let size = size.min(8);
            ((value >> (8 - size)) as u32) << position
        }

        place(red_of(color), self.red_pos, self.red_size)
            | place(green_of(color), self.green_pos, self.green_size)
            | place(blue_of(color), self.blue_pos, self.blue_size)
    }
}

// ============================================================
//  Surface
// ============================================================

/// The framebuffer and its optional back buffer.
pub struct Framebuffer {
    /// Virtual address of the hardware framebuffer.
    front: *mut u8,
    /// Virtual address of the back buffer, null while it does not exist.
    back: *mut u8,
    pitch: usize,
    width: usize,
    height: usize,
    bytes_per_pixel: usize,
    format: Format,
    /// Physical address of the hardware, kept for diagnostics and for the
    /// write-combining remap.
    phys: u64,
    bpp: u8,
    /// Called with every rectangle just published, when the pixels only
    /// reach the screen through a further step -- virtio-gpu's transfer
    /// to the host. `None` for a framebuffer the hardware scans directly.
    flush: Option<fn(usize, usize, usize, usize)>,
}

// Safe: both pointers name mappings global to the machine, and every
// access goes through the lock protecting the structure.
unsafe impl Send for Framebuffer {}

impl Framebuffer {
    /// Builds the surface from the bootloader description and an already
    /// mapped virtual address.
    ///
    /// Refuses rather than guesses: a mode whose stride is not a multiple
    /// of the pixel size, or whose base is misaligned, would make every
    /// pixel write incorrect from a certain row on. Better to stay in text
    /// mode than to paint wrong.
    ///
    /// # Safety
    /// `virt` must point at a writable mapping covering at least
    /// `pitch * height` bytes, really describing the mode `info`.
    pub unsafe fn new(info: &FramebufferInfo, virt: u64) -> Result<Self, &'static str> {
        if !info.is_graphic() {
            return Err("mode non graphique");
        }

        let bytes_per_pixel = info.bytes_per_pixel();
        if !(2..=4).contains(&bytes_per_pixel) {
            return Err("profondeur non geree");
        }
        if info.pitch as usize % bytes_per_pixel != 0 {
            return Err("pas de ligne non multiple d'un pixel");
        }
        // The 3-byte case has no constraint: it is written byte by byte,
        // precisely because a 24-bit pixel cannot be aligned on a word
        // boundary.
        if bytes_per_pixel != 3 && (virt as usize % bytes_per_pixel != 0) {
            return Err("base desalignee");
        }
        if (info.width as usize) * bytes_per_pixel > info.pitch as usize {
            return Err("pas de ligne plus court qu'une ligne de pixels");
        }

        Ok(Framebuffer {
            front: virt as *mut u8,
            back: core::ptr::null_mut(),
            pitch: info.pitch as usize,
            width: info.width as usize,
            height: info.height as usize,
            bytes_per_pixel,
            format: Format::from_info(info),
            phys: info.addr,
            bpp: info.bpp,
            flush: None,
        })
    }

    /// Installs the hook called after each publication.
    pub fn set_flush(&mut self, flush: fn(usize, usize, usize, usize)) {
        self.flush = Some(flush);
    }

    fn flushed(&self, x: usize, y: usize, width: usize, height: usize) {
        if let Some(flush) = self.flush {
            flush(x, y, width, height);
        }
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn height(&self) -> usize {
        self.height
    }

    pub fn pitch(&self) -> usize {
        self.pitch
    }

    pub fn bpp(&self) -> u8 {
        self.bpp
    }

    pub fn bytes_per_pixel(&self) -> usize {
        self.bytes_per_pixel
    }

    pub fn phys_addr(&self) -> u64 {
        self.phys
    }

    pub fn format(&self) -> Format {
        self.format
    }

    /// Bytes occupied by a complete image.
    pub fn frame_bytes(&self) -> usize {
        self.pitch * self.height
    }

    pub fn is_double_buffered(&self) -> bool {
        !self.back.is_null()
    }

    /// Replaces the virtual address of the hardware -- used when the
    /// framebuffer is remapped write-combining after the fact.
    ///
    /// # Safety
    /// `virt` must cover the same physical memory as the previous
    /// mapping, with the same dimensions.
    pub unsafe fn rebind_front(&mut self, virt: u64) {
        self.front = virt as *mut u8;
    }

    /// Installs the back buffer and copies the displayed image into it.
    ///
    /// The copy is not a decorative precaution: without it, the first
    /// `present` would erase everything the console displayed before the
    /// double buffer existed.
    ///
    /// # Safety
    /// `virt` must point at `pitch * height` bytes readable and writable,
    /// disjoint from the hardware framebuffer.
    pub unsafe fn attach_back_buffer(&mut self, virt: u64) {
        let back = virt as *mut u8;
        unsafe { core::ptr::copy_nonoverlapping(self.front, back, self.frame_bytes()) };
        self.back = back;
    }

    /// Surface the drawing acts on: the back buffer if it exists, the
    /// hardware otherwise.
    #[inline]
    fn target(&self) -> *mut u8 {
        if self.back.is_null() { self.front } else { self.back }
    }

    #[inline]
    fn pixel_ptr(&self, x: usize, y: usize) -> *mut u8 {
        unsafe { self.target().add(y * self.pitch + x * self.bytes_per_pixel) }
    }

    /// Drops an already encoded pixel.
    ///
    /// # Safety
    /// `ptr` must name a whole pixel inside the surface.
    #[inline]
    unsafe fn store(&self, ptr: *mut u8, value: u32) {
        unsafe {
            match self.bytes_per_pixel {
                4 => (ptr as *mut u32).write_volatile(value),
                2 => (ptr as *mut u16).write_volatile(value as u16),
                // 24 bits: three bytes, with no alignment constraint.
                _ => {
                    ptr.write_volatile(value as u8);
                    ptr.add(1).write_volatile((value >> 8) as u8);
                    ptr.add(2).write_volatile((value >> 16) as u8);
                }
            }
        }
    }

    pub fn encode(&self, color: Color) -> u32 {
        self.format.encode(color)
    }

    /// One pixel, coordinates outside the surface ignored.
    pub fn put_pixel(&mut self, x: usize, y: usize, color: Color) {
        if x >= self.width || y >= self.height {
            return;
        }
        let value = self.encode(color);
        unsafe { self.store(self.pixel_ptr(x, y), value) };
    }

    /// A row of pixels from `(x, y)`, clipped to the surface.
    ///
    /// The common screen -- 32 bits, red above green above blue -- is the
    /// layout of `Color` itself, and takes one copy. Any other format
    /// encodes pixel by pixel.
    pub fn write_row(&mut self, x: usize, y: usize, pixels: &[Color]) {
        if y >= self.height || x >= self.width {
            return;
        }
        let count = pixels.len().min(self.width - x);
        let native = self.bytes_per_pixel == 4
            && self.format.red_pos == 16
            && self.format.green_pos == 8
            && self.format.blue_pos == 0
            && self.format.red_size == 8
            && self.format.green_size == 8
            && self.format.blue_size == 8;
        let destination = self.pixel_ptr(x, y);
        if native {
            // Safe: `count` pixels from `(x, y)` are inside the row.
            unsafe {
                core::ptr::copy_nonoverlapping(pixels.as_ptr() as *const u8, destination, count * 4)
            };
            return;
        }
        let mut pointer = destination;
        for pixel in &pixels[..count] {
            let value = self.encode(*pixel);
            unsafe {
                self.store(pointer, value);
                pointer = pointer.add(self.bytes_per_pixel);
            }
        }
    }

    /// Filled rectangle, clipped to the surface.
    ///
    /// The first row is painted pixel by pixel, the following ones are
    /// copies of it. A full-screen clear thus goes from half a million
    /// pixel writes to a thousand row copies -- the difference between a
    /// console scroll one sees go by and one that is not seen. The colour
    /// itself is encoded only once for the whole rectangle.
    pub fn fill_rect(&mut self, x: usize, y: usize, width: usize, height: usize, color: Color) {
        let x_end = (x + width).min(self.width);
        let y_end = (y + height).min(self.height);
        if x >= x_end || y >= y_end {
            return;
        }

        let value = self.encode(color);
        let span = (x_end - x) * self.bytes_per_pixel;

        let first = self.pixel_ptr(x, y);
        let mut ptr = first;
        for _ in x..x_end {
            unsafe {
                self.store(ptr, value);
                ptr = ptr.add(self.bytes_per_pixel);
            }
        }

        for row in (y + 1)..y_end {
            let destination = self.pixel_ptr(x, row);
            unsafe { core::ptr::copy_nonoverlapping(first, destination, span) };
        }
    }

    pub fn clear(&mut self, color: Color) {
        self.fill_rect(0, 0, self.width, self.height, color);
    }

    /// Draws a monochrome glyph: bit at 1 in `foreground`, bit at 0 in
    /// `background`.
    ///
    /// The background is painted at the same time as the stroke, and not
    /// by a prior rectangle: two passes over the same area double the cost
    /// of a console that writes one character at a time.
    pub fn draw_glyph(
        &mut self,
        x: usize,
        y: usize,
        glyph: &[u8],
        foreground: Color,
        background: Color,
    ) {
        let front = self.encode(foreground);
        let behind = self.encode(background);
        let columns = 8usize.min(self.width.saturating_sub(x));

        for (line, bits) in glyph.iter().enumerate() {
            let row = y + line;
            if row >= self.height {
                break;
            }
            let mut ptr = self.pixel_ptr(x, row);
            for column in 0..columns {
                let lit = bits & (0x80 >> column) != 0;
                unsafe {
                    self.store(ptr, if lit { front } else { behind });
                    ptr = ptr.add(self.bytes_per_pixel);
                }
            }
        }
    }

    /// Moves the content up by `pixels` rows and paints the freed band at
    /// the bottom.
    ///
    /// The shift is done with `copy` -- overlapping by construction, since
    /// source and destination are the same surface -- and not with a pixel
    /// loop: moving three mebibytes by individual read-write would make
    /// the console scroll visible to the eye.
    pub fn scroll_up(&mut self, pixels: usize, fill: Color) {
        if pixels == 0 {
            return;
        }
        if pixels >= self.height {
            self.clear(fill);
            return;
        }

        let moved = (self.height - pixels) * self.pitch;
        let base = self.target();
        unsafe { core::ptr::copy(base.add(pixels * self.pitch), base, moved) };
        self.fill_rect(0, self.height - pixels, self.width, pixels, fill);
    }

    /// Publishes the complete image on the hardware.
    pub fn present(&mut self) {
        self.present_rows(0, self.height);
    }

    /// Publishes a horizontal band.
    ///
    /// The back buffer shares the stride of the hardware, which reduces
    /// publishing a band to a single contiguous copy.
    pub fn present_rows(&mut self, first: usize, count: usize) {
        if self.back.is_null() || count == 0 || first >= self.height {
            return;
        }
        let count = count.min(self.height - first);
        let offset = first * self.pitch;
        let bytes = count * self.pitch;

        unsafe {
            core::ptr::copy_nonoverlapping(self.back.add(offset), self.front.add(offset), bytes)
        };
        self.flushed(0, first, self.width, count);
    }

    /// Publishes a rectangle, columns included.
    ///
    /// ## Why not always publish whole rows
    ///
    /// `present_rows` copies the **complete stride** of each row, hence
    /// the whole width of the screen. Moving a cursor by sixteen pixels
    /// dirties eighteen rows: publishing would then transfer
    /// 18 x 1024 x 4 bytes, that is about 74 KiB, for 1.3 KiB really
    /// modified. A factor of fifty-seven, paid on every frame, on a
    /// write-combining mapping where only the number of bytes written counts.
    ///
    /// Splitting by columns replaces one contiguous copy with a short copy
    /// per row. The extra call per row weighs nothing against the writes
    /// avoided -- except when the rectangle occupies nearly the whole
    /// width, where contiguity takes the advantage back: `present`
    /// arbitrates between the two.
    pub fn present_rect(&mut self, x: usize, y: usize, width: usize, height: usize) {
        if self.back.is_null() || width == 0 || height == 0 {
            return;
        }
        if x >= self.width || y >= self.height {
            return;
        }
        let width = width.min(self.width - x);
        let height = height.min(self.height - y);
        let bytes = width * self.bytes_per_pixel;

        for row in 0..height {
            let offset = (y + row) * self.pitch + x * self.bytes_per_pixel;
            unsafe {
                core::ptr::copy_nonoverlapping(
                    self.back.add(offset),
                    self.front.add(offset),
                    bytes,
                )
            };
        }
        self.flushed(x, y, width, height);
    }

    /// Publishes an area choosing the least costly strategy.
    ///
    /// The threshold is at three quarters of the width: beyond it, what
    /// would be saved in columns no longer makes up for the loss of
    /// contiguity, and a single copy of several rows is worth more than
    /// thirty copies nearly as long.
    pub fn present_area(&mut self, x: usize, y: usize, width: usize, height: usize) {
        if width * 4 >= self.width * 3 {
            self.present_rows(y, height);
        } else {
            self.present_rect(x, y, width, height);
        }
    }

    /// Publishes an area, passing every pixel through a mapping.
    ///
    /// This is the publication stage step 10.1 needs: the accessibility
    /// magnifier and the colour transforms act **between the back buffer
    /// and the screen**, never on the drawing. An application draws what
    /// it draws; nothing above this line knows the screen is magnified or
    /// recoloured. 8.1 holds unchanged -- the compositor is still the
    /// only writer of the framebuffer, and this is a stage of its
    /// publication.
    ///
    /// `map` receives the destination coordinates and returns the colour
    /// to put there. It is given a reader for the back buffer rather than
    /// a colour, because the magnifier needs to sample a pixel that is
    /// **somewhere else**: a mapping that only received the pixel under
    /// the destination could recolour but never move anything.
    ///
    /// Does nothing with no back buffer. Reading and writing the same
    /// memory would apply the transform to its own output, once per
    /// frame, and the screen would drift a little further from the truth
    /// every time.
    pub fn present_area_mapped(
        &mut self,
        x: usize,
        y: usize,
        width: usize,
        height: usize,
        map: &dyn Fn(&dyn Fn(usize, usize) -> Color, usize, usize) -> Color,
    ) {
        if self.back.is_null() || width == 0 || height == 0 {
            return;
        }
        if x >= self.width || y >= self.height {
            return;
        }
        let width = width.min(self.width - x);
        let height = height.min(self.height - y);

        // The sampler reads the **back** buffer, always: `read_pixel`
        // reads the drawing surface, which is the same thing here but
        // would silently become the hardware if the back buffer went
        // away, and reading a write-combining mapping per pixel is
        // measured in milliseconds per frame.
        let back = self.back;
        let pitch = self.pitch;
        let bytes_per_pixel = self.bytes_per_pixel;
        let format = self.format;
        let (limit_x, limit_y) = (self.width, self.height);

        let sample = move |sx: usize, sy: usize| -> Color {
            let sx = sx.min(limit_x.saturating_sub(1));
            let sy = sy.min(limit_y.saturating_sub(1));
            let ptr = unsafe { back.add(sy * pitch + sx * bytes_per_pixel) };
            let raw = unsafe {
                match bytes_per_pixel {
                    4 => (ptr as *const u32).read_volatile(),
                    2 => (ptr as *const u16).read_volatile() as u32,
                    _ => {
                        ptr.read_volatile() as u32
                            | (ptr.add(1).read_volatile() as u32) << 8
                            | (ptr.add(2).read_volatile() as u32) << 16
                    }
                }
            };
            decode_with(&format, raw)
        };

        for row in 0..height {
            for column in 0..width {
                let (dx, dy) = (x + column, y + row);
                let colour = map(&sample, dx, dy);
                let value = format.encode(colour);
                // Straight to the hardware: the back buffer must keep the
                // untransformed image, or the next frame would transform
                // an already transformed pixel.
                let ptr = unsafe { self.front.add(dy * pitch + dx * bytes_per_pixel) };
                unsafe { self.store(ptr, value) };
            }
        }
        self.flushed(x, y, width, height);
    }

    /// Reads a pixel of the drawing surface back, as an internal colour.
    ///
    /// Used by the self-test: establishing that a written pixel reads back
    /// is the only check possible with no human eye. The read-back is done
    /// on the drawing surface, never on the hardware -- a write-combining
    /// framebuffer reads back slowly, and nothing guarantees a given mode
    /// is readable.
    pub fn read_pixel(&self, x: usize, y: usize) -> Option<Color> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let ptr = self.pixel_ptr(x, y);
        let raw = unsafe {
            match self.bytes_per_pixel {
                4 => (ptr as *const u32).read_volatile(),
                2 => (ptr as *const u16).read_volatile() as u32,
                _ => {
                    ptr.read_volatile() as u32
                        | (ptr.add(1).read_volatile() as u32) << 8
                        | (ptr.add(2).read_volatile() as u32) << 16
                }
            }
        };
        Some(self.decode(raw))
    }

    /// Inverse operation of `Format::encode`.
    ///
    /// The bits lost at encoding do not come back: a five-bit component
    /// reads back with its three low bits at zero. So the comparison of a
    /// self-test must bear on colours already passed through this pair,
    /// not on the original value.
    fn decode(&self, raw: u32) -> Color {
        decode_with(&self.format, raw)
    }
}

/// The same decoding, against a format passed by value.
///
/// Free function because `present_area_mapped` builds a sampler closure
/// that outlives the borrow of `self`: a method would keep that borrow
/// alive and stop the front buffer being written in the same loop.
fn decode_with(format: &Format, raw: u32) -> Color {
    fn take(raw: u32, position: u8, size: u8) -> u8 {
        if size == 0 {
            return 0;
        }
        let size = size.min(8);
        let mask = (1u32 << size) - 1;
        (((raw >> position) & mask) << (8 - size)) as u8
    }

    rgb(
        take(raw, format.red_pos, format.red_size),
        take(raw, format.green_pos, format.green_size),
        take(raw, format.blue_pos, format.blue_size),
    )
}
