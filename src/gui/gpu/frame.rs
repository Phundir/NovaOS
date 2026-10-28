//! Drawing a frame: what the painter recorded, the canvases and icons
//! copied in, the pass over the damaged rectangle, the copy to the screen
//! -- and, while the GPU is busy, the same frame painted on the processor.

use super::*;

impl Renderer {
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Records a frame: `paint` runs the painter on the recorder. Called
    /// under the compositor's lock -- it only builds lists.
    pub fn record(&mut self, damage: Rect, paint: impl FnOnce(&mut Recorder)) -> Frame {
        let started = crate::posix::time::monotonic_ns();
        self.icons.trim();
        let mut recorder = Recorder::new(self.width as usize, self.height as usize, &mut self.icons);
        paint(&mut recorder);
        let frame = Frame { damage, instances: recorder.instances, batches: recorder.batches, canvases: recorder.canvases };
        self.stats.cpu_ns += crate::posix::time::monotonic_ns() - started;
        frame
    }

    /// The canvas's texture, made or remade to its current size. Returns
    /// its index in `textures` and whether its pixels must be copied in.
    fn texture_of(&mut self, canvas: &CanvasRef) -> Result<(usize, bool), &'static str> {
        let (width, height, stamp) = {
            let pixels = canvas.0.lock();
            (pixels.width as u32, pixels.height as u32, pixels.published_ns)
        };
        if let Some(index) = self.textures.iter().position(|t| alloc::sync::Arc::ptr_eq(&t.canvas.0, &canvas.0)) {
            let texture = &self.textures[index];
            if texture.width == width && texture.height == height {
                return Ok((index, texture.stamp != stamp || stamp == 0));
            }
            let old = self.textures.swap_remove(index);
            let _ = gpu::object_destroy(&self.owner, self.context, old.image);
        }
        let image = gpu::object_create(
            &self.owner,
            self.context,
            Create::Image {
                desc: ImageDesc { format: Format::Bgra8, width, height, usage: backend::image_usage::SAMPLED, shareable: false },
                memory: 0,
                offset: 0,
            },
        )
        .map_err(fail("image de toile refusee"))?;
        self.textures.push(Texture { canvas: canvas.clone(), image, width, height, stamp: 0 });
        Ok((self.textures.len() - 1, true))
    }

    /// Drops the textures of canvases nobody holds any more.
    fn collect_textures(&mut self) {
        let mut index = 0;
        while index < self.textures.len() {
            if alloc::sync::Arc::strong_count(&self.textures[index].canvas.0) == 1 {
                let texture = self.textures.swap_remove(index);
                let _ = gpu::object_destroy(&self.owner, self.context, texture.image);
            } else {
                index += 1;
            }
        }
    }

    /// Draws a recorded frame and puts its damage on the screen. Called
    /// with no lock held: it copies canvases, submits, and sleeps until
    /// the GPU is done. `Err` means the GPU is lost for the compositor.
    pub fn draw(&mut self, frame: Frame) -> Result<(), DrawError> {
        self.draw_with(frame, Transforms::current())
    }

    /// `draw`, the transforms given: the self-test's way to check them
    /// without changing the screen's.
    pub fn draw_with(&mut self, frame: Frame, transforms: Option<Transforms>) -> Result<(), DrawError> {
        self.draw_frame(frame, transforms).map_err(|error| match error {
            Failure::Draw(error) => error,
            Failure::Setup(reason) => DrawError::Lost(reason),
        })
    }

    fn draw_frame(&mut self, frame: Frame, transforms: Option<Transforms>) -> Result<(), Failure> {
        let started = crate::posix::time::monotonic_ns();
        let damage = frame.damage.intersect(&Rect::new(0, 0, self.width as i32, self.height as i32));
        if damage.is_empty() {
            return Ok(());
        }
        self.collect_textures();

        // --- what must be copied in: canvases changed, icons new ---
        let mut canvas_images: Vec<u32> = Vec::with_capacity(frame.canvases.len());
        let mut copies: Vec<(usize, u64)> = Vec::new();
        let mut upload_bytes = 0u64;
        for canvas in &frame.canvases {
            let (index, stale) = self.texture_of(canvas)?;
            canvas_images.push(self.textures[index].image);
            if stale {
                copies.push((index, upload_bytes));
                let texture = &self.textures[index];
                upload_bytes += (texture.width as u64 * texture.height as u64 * 4).div_ceil(ALIGN) * ALIGN;
            }
        }
        let icons_at = upload_bytes;
        upload_bytes += self.icons.pending.len() as u64 * 256;
        if upload_bytes > 0 {
            self.ensure(|r| &mut r.upload, upload_bytes, MemKind::Visible)?;
        }
        let instance_bytes = (frame.instances.len().max(1) * 32) as u64;
        self.ensure(|r| &mut r.instances, instance_bytes, MemKind::Visible)?;

        let mut words = Words::default();
        if let Some(upload) = self.upload.as_ref() {
            let (upload_buffer, upload_address) = (upload.buffer, upload.address);
            for (index, offset) in copies.iter() {
                let texture = &mut self.textures[*index];
                let pixels = texture.canvas.0.lock();
                let count = (texture.width as usize * texture.height as usize).min(pixels.data.len());
                // SAFETY: the upload area holds `upload_bytes`, of which this
                // canvas has its own aligned range from `offset`.
                unsafe {
                    core::ptr::copy_nonoverlapping(pixels.data.as_ptr() as *const u8, (upload_address + offset) as *mut u8, count * 4)
                };
                texture.stamp = pixels.published_ns;
                drop(pixels);
                self.stats.uploaded += count as u64 * 4;
                words.copy_to_image(
                    upload_buffer,
                    *offset,
                    texture.width,
                    texture.image,
                    Rect::new(0, 0, texture.width as i32, texture.height as i32),
                );
            }
            for (rank, (slot, bits)) in self.icons.pending.iter().enumerate() {
                let offset = icons_at + rank as u64 * 256;
                for row in 0..16 {
                    let pattern = u16::from_be_bytes([bits[row * 2], bits[row * 2 + 1]]);
                    for column in 0..16 {
                        let lit = pattern & (0x8000 >> column) != 0;
                        let at = upload_address + offset + (row * 16 + column) as u64;
                        // SAFETY: inside this icon's 256 bytes of the area.
                        unsafe { (at as *mut u8).write_volatile(if lit { 255 } else { 0 }) };
                    }
                }
                let cell = Rect::new((slot % 64 * 16) as i32, (slot / 64 * 16) as i32, 16, 16);
                words.copy_to_image(upload_buffer, offset, 16, self.icon_atlas, cell);
            }
        }
        self.icons.pending.clear();

        // --- the instances ---
        let Some(instances) = self.instances.as_ref() else { return Err("zone des instances absente".into()) };
        let (instance_buffer, instance_address) = (instances.buffer, instances.address);
        for (index, instance) in frame.instances.iter().enumerate() {
            for (word, value) in instance.iter().enumerate() {
                // SAFETY: the area holds at least 32 bytes per instance.
                unsafe { ((instance_address + index as u64 * 32 + word as u64 * 4) as *mut u32).write_volatile(*value) };
            }
        }

        // --- the pass, over the damaged rectangle only ---
        let load = if self.composed_valid { 1 } else { 0 };
        let one = float_bits(1);
        words.cmd(
            op::BEGIN_RENDER,
            &[self.composed, load, 0, 0, 0, one, 0, 2, 0, damage.x as u32, damage.y as u32, damage.width as u32, damage.height as u32],
        );
        words.cmd(op::BIND_PIPELINE, &[self.pipeline]);
        words.cmd(op::VIEWPORT, &[0, 0, float_bits(self.width), float_bits(self.height), 0, one]);
        words.cmd(op::SCISSOR, &[damage.x as u32, damage.y as u32, damage.width as u32, damage.height as u32]);
        words.cmd(op::BIND_VERTEX, &[0, instance_buffer, 0, 0]);
        words.cmd(op::PUSH_CONSTANTS, &[0, self.width, self.height]);
        for batch in frame.batches.iter() {
            let canvas = batch.canvas.and_then(|index| canvas_images.get(index).copied()).unwrap_or(self.blank);
            words.cmd(op::BIND_RESOURCE, &[0, self.glyphs, self.sampler, 0, 0, 0, 0]);
            words.cmd(op::BIND_RESOURCE, &[1, self.icon_atlas, self.sampler, 0, 0, 0, 0]);
            words.cmd(op::BIND_RESOURCE, &[2, canvas, self.sampler, 0, 0, 0, 0]);
            words.cmd(op::DRAW, &[6, batch.count, 0, batch.first]);
        }
        words.cmd(op::END_RENDER, &[]);

        // --- to the screen: straight into the output, or through the
        // processor transforms ---
        let offset = (damage.y as u64 * self.width as u64 + damage.x as u64) * 4;
        if transforms.is_some() {
            let bytes = self.output.size;
            self.ensure(|r| &mut r.back, bytes, MemKind::Readback)?;
            let back = self.back.as_ref().map(|b| b.buffer).ok_or("tampon arriere absent")?;
            words.copy_to_buffer(self.composed, damage, back, offset, self.width);
        } else {
            words.copy_to_buffer(self.composed, damage, self.output.buffer, offset, self.width);
        }
        self.stats.cpu_ns += crate::posix::time::monotonic_ns() - started;
        self.run(&words, FRAME_WAIT_MS).map_err(Failure::Draw)?;
        let finished = crate::posix::time::monotonic_ns();
        if let Some(transforms) = transforms {
            self.transform(damage, &transforms);
            self.stats.transformed += 1;
        }
        self.composed_valid = self.composed_valid || damage == Rect::new(0, 0, self.width as i32, self.height as i32);
        if self.on_screen {
            gpu::screen::flush(damage.x as u32, damage.y as u32, damage.width as u32, damage.height as u32);
        }
        self.stats.frames += 1;
        self.stats.instances += frame.instances.len() as u64;
        self.stats.cpu_ns += crate::posix::time::monotonic_ns() - finished;
        Ok(())
    }

    /// The accessibility transforms, from `back` into the output: the
    /// software path's own mapping (`access::map_pixel`).
    fn transform(&self, damage: Rect, transforms: &Transforms) {
        let Some(back) = self.back.as_ref() else { return };
        let (width, height) = (self.width as usize, self.height as usize);
        let back_address = back.address;
        let sample = move |x: usize, y: usize| -> u32 {
            let x = x.min(width - 1);
            let y = y.min(height - 1);
            // SAFETY: inside the back area, `width * height` pixels.
            unsafe { ((back_address + ((y * width + x) * 4) as u64) as *const u32).read_volatile() & 0x00FF_FFFF }
        };
        for y in damage.y as usize..damage.bottom() as usize {
            for x in damage.x as usize..damage.right() as usize {
                let colour = crate::access::map_pixel(
                    &sample,
                    width as i32,
                    height as i32,
                    x,
                    y,
                    transforms.zoom_on,
                    &transforms.zoom,
                    transforms.centre,
                    transforms.mode,
                    &transforms.matrix,
                );
                // SAFETY: inside the output, `width * height` pixels.
                unsafe { ((self.output.address + ((y * width + x) * 4) as u64) as *mut u32).write_volatile(colour) };
            }
        }
    }

    /// Paints a frame on the processor, straight into the output -- the
    /// screen's memory -- while the GPU finishes a late one. `paint` runs
    /// the painter on it; the transforms go through the back area, as on
    /// the GPU. Called under the compositor's lock, like the software
    /// render.
    pub fn paint_processor(&mut self, damage: Rect, paint: impl FnOnce(&mut crate::gui::painter::Raw)) {
        let damage = damage.intersect(&Rect::new(0, 0, self.width as i32, self.height as i32));
        if damage.is_empty() {
            return;
        }
        let transforms = Transforms::current();
        let target = match (transforms.as_ref(), self.back.as_ref()) {
            (Some(_), Some(back)) => back.address,
            _ => self.output.address,
        };
        // SAFETY: both areas hold `width * height` pixels, mapped until the
        // renderer goes; the GPU writes the output only in the late
        // frame's final copy, which the next whole frame overwrites.
        let mut raw = unsafe { crate::gui::painter::Raw::new(target, self.width as usize, self.height as usize) };
        paint(&mut raw);
        if let Some(transforms) = transforms
            && self.back.is_some()
        {
            self.transform(damage, &transforms);
        }
        // Not sent: sending touches the device, and while the host's GPU
        // is held a core that touches it waits in QEMU -- timer and all.
        // The screen goes out when the GPU is back (`invalidate`: the next
        // frame is whole).
    }

    /// The next frame must be whole: the composed image missed what the
    /// processor painted meanwhile.
    pub fn invalidate(&mut self) {
        self.composed_valid = false;
    }

}
