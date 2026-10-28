//! Recording a validated command list into a Vulkan command buffer and
//! submitting it on the queue's timeline, with the fixed choices the parent
//! module lists: GENERAL layout, full barriers between passes, pushed
//! resources, dynamic rendering. Never waits for the host.

use super::*;

/// A barrier between everything before and everything after.
pub(super) fn full_barrier(enc: &mut Enc, command_buffer: u64, towards_host: bool) {
    use vk::*;
    let (dst_stage, dst_access) = if towards_host {
        (VK_PIPELINE_STAGE_2_HOST_BIT, VK_ACCESS_2_HOST_READ_BIT | VK_ACCESS_2_HOST_WRITE_BIT)
    } else {
        (VK_PIPELINE_STAGE_2_ALL_COMMANDS_BIT, VK_ACCESS_2_MEMORY_READ_BIT | VK_ACCESS_2_MEMORY_WRITE_BIT)
    };
    let barrier = [VkMemoryBarrier2 {
        src_stage_mask: VK_PIPELINE_STAGE_2_ALL_COMMANDS_BIT | VK_PIPELINE_STAGE_2_HOST_BIT,
        src_access_mask: VK_ACCESS_2_MEMORY_READ_BIT | VK_ACCESS_2_MEMORY_WRITE_BIT | VK_ACCESS_2_HOST_WRITE_BIT,
        dst_stage_mask: dst_stage,
        dst_access_mask: dst_access,
        ..Default::default()
    }];
    let dependency = VkDependencyInfo { memory_barriers: &barrier, ..Default::default() };
    cmd_pipeline_barrier2(enc, 0, command_buffer, &dependency);
}

/// Every image a command list uses for the first time.
pub(super) fn first_uses(commands: &[Command]) -> Vec<ImageRef> {
    let mut images: Vec<ImageRef> = Vec::new();
    let mut add = |image: &ImageRef| {
        if image.first_use && !images.iter().any(|i| i.id == image.id) {
            images.push(*image);
        }
    };
    for command in commands {
        match command {
            Command::BeginRender { color, depth, .. } => {
                add(color);
                if let Some((depth, _, _)) = depth {
                    add(depth);
                }
            }
            Command::CopyBufferToImage { image, .. } | Command::CopyImageToBuffer { image, .. } | Command::ClearImage { image, .. } => add(image),
            Command::CopyImage { src, dst, .. } | Command::Blit { src, dst, .. } => {
                add(src);
                add(dst);
            }
            _ => {}
        }
    }
    images
}

impl VenusContext {
    pub(super) fn record(&mut self, commands: &[Command]) -> Result<Work> {
        let device = self.session.device;
        if self.feedback.is_none() {
            self.feedback = self.make_feedback().ok();
        }
        self.collect_retired();
        // --- the command buffer: a recycled one, or a new one ---
        let mut enc = Enc::new();
        let buffer = match self.free_buffers.pop() {
            Some(buffer) => {
                reset_command_buffer(&mut enc, 0, buffer, 0);
                buffer
            }
            None => {
                let buffer = self.session.id();
                let info = VkCommandBufferAllocateInfo {
                    command_pool: self.session.pool,
                    level: vk::VK_COMMAND_BUFFER_LEVEL_PRIMARY,
                    command_buffer_count: 1,
                    ..Default::default()
                };
                allocate_command_buffers(&mut enc, 0, device, &info, Some(&[buffer]));
                buffer
            }
        };
        let begin = VkCommandBufferBeginInfo { flags: vk::VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT, ..Default::default() };
        begin_command_buffer(&mut enc, 0, buffer, &begin);
        full_barrier(&mut enc, buffer, false);

        // --- images used for the first time: to the general layout ---
        let fresh = first_uses(commands);
        if !fresh.is_empty() {
            let barriers: Vec<VkImageMemoryBarrier2> = fresh
                .iter()
                .map(|image| VkImageMemoryBarrier2 {
                    src_stage_mask: vk::VK_PIPELINE_STAGE_2_TOP_OF_PIPE_BIT,
                    src_access_mask: 0,
                    dst_stage_mask: vk::VK_PIPELINE_STAGE_2_ALL_COMMANDS_BIT,
                    dst_access_mask: vk::VK_ACCESS_2_MEMORY_READ_BIT | vk::VK_ACCESS_2_MEMORY_WRITE_BIT,
                    old_layout: vk::VK_IMAGE_LAYOUT_UNDEFINED,
                    new_layout: vk::VK_IMAGE_LAYOUT_GENERAL,
                    src_queue_family_index: u32::MAX,
                    dst_queue_family_index: u32::MAX,
                    image: image.id,
                    subresource_range: whole(image.format),
                    ..Default::default()
                })
                .collect();
            let dependency = VkDependencyInfo { image_memory_barriers: &barriers, ..Default::default() };
            cmd_pipeline_barrier2(&mut enc, 0, buffer, &dependency);
        }

        // Stream boundaries: a stream longer than one submission is cut
        // between two commands, never inside one.
        let mut cuts: Vec<usize> = Vec::new();
        let general = vk::VK_IMAGE_LAYOUT_GENERAL;
        for command in commands {
            cuts.push(enc.len());
            match command {
                Command::BeginRender { color, load: op, clear, depth, area } => {
                    full_barrier(&mut enc, buffer, false);
                    let color_attachment = [VkRenderingAttachmentInfo {
                        image_view: self.view_of(color.id),
                        image_layout: general,
                        load_op: load(*op),
                        store_op: vk::VK_ATTACHMENT_STORE_OP_STORE,
                        clear_value: *clear,
                        ..Default::default()
                    }];
                    let depth_attachment = depth.map(|(image, op, value)| VkRenderingAttachmentInfo {
                        image_view: self.view_of(image.id),
                        image_layout: general,
                        load_op: load(op),
                        store_op: vk::VK_ATTACHMENT_STORE_OP_STORE,
                        clear_value: [value, 0, 0, 0],
                        ..Default::default()
                    });
                    let info = VkRenderingInfo {
                        render_area: VkRect2D {
                            offset: VkOffset2D { x: area.x, y: area.y },
                            extent: VkExtent2D { width: area.width, height: area.height },
                        },
                        layer_count: 1,
                        color_attachments: &color_attachment,
                        depth_attachment: depth_attachment.as_ref(),
                        ..Default::default()
                    };
                    cmd_begin_rendering(&mut enc, 0, buffer, &info);
                }
                Command::EndRender => cmd_end_rendering(&mut enc, 0, buffer),
                Command::BindPipeline { pipeline } => {
                    cmd_bind_pipeline(&mut enc, 0, buffer, vk::VK_PIPELINE_BIND_POINT_GRAPHICS, *pipeline)
                }
                Command::BindVertex { slot, buffer: vertex, offset } => {
                    cmd_bind_vertex_buffers(&mut enc, 0, buffer, *slot, &[*vertex], &[*offset])
                }
                Command::BindIndex { buffer: index, offset, wide } => cmd_bind_index_buffer(
                    &mut enc,
                    0,
                    buffer,
                    *index,
                    *offset,
                    if *wide { vk::VK_INDEX_TYPE_UINT32 } else { vk::VK_INDEX_TYPE_UINT16 },
                ),
                Command::PushResources { layout, writes } => {
                    let images: Vec<[VkDescriptorImageInfo; 1]> = writes
                        .iter()
                        .map(|w| match w {
                            ResourceWrite::Image { image, sampler, .. } => {
                                [VkDescriptorImageInfo { sampler: *sampler, image_view: self.view_of(*image), image_layout: general }]
                            }
                            _ => [VkDescriptorImageInfo::default()],
                        })
                        .collect();
                    let buffers: Vec<[VkDescriptorBufferInfo; 1]> = writes
                        .iter()
                        .map(|w| match w {
                            ResourceWrite::Buffer { buffer, offset, range, .. } => {
                                [VkDescriptorBufferInfo { buffer: *buffer, offset: *offset, range: *range }]
                            }
                            _ => [VkDescriptorBufferInfo::default()],
                        })
                        .collect();
                    let sets: Vec<VkWriteDescriptorSet> = writes
                        .iter()
                        .enumerate()
                        .map(|(index, w)| match w {
                            ResourceWrite::Image { binding, .. } => VkWriteDescriptorSet {
                                dst_binding: *binding,
                                descriptor_count: 1,
                                descriptor_type: vk::VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER,
                                image_info: Some(&images[index]),
                                ..Default::default()
                            },
                            ResourceWrite::Buffer { binding, kind, .. } => VkWriteDescriptorSet {
                                dst_binding: *binding,
                                descriptor_count: 1,
                                descriptor_type: if *kind == BindingKind::UniformBuffer {
                                    vk::VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER
                                } else {
                                    vk::VK_DESCRIPTOR_TYPE_STORAGE_BUFFER
                                },
                                buffer_info: Some(&buffers[index]),
                                ..Default::default()
                            },
                        })
                        .collect();
                    cmd_push_descriptor_set(&mut enc, 0, buffer, vk::VK_PIPELINE_BIND_POINT_GRAPHICS, *layout, 0, &sets);
                }
                Command::Viewport { x, y, width, height, min_depth, max_depth } => {
                    let viewport = [VkViewport { x: *x, y: *y, width: *width, height: *height, min_depth: *min_depth, max_depth: *max_depth }];
                    cmd_set_viewport(&mut enc, 0, buffer, 0, &viewport);
                }
                Command::Scissor(rect) => {
                    let scissor = [VkRect2D {
                        offset: VkOffset2D { x: rect.x, y: rect.y },
                        extent: VkExtent2D { width: rect.width, height: rect.height },
                    }];
                    cmd_set_scissor(&mut enc, 0, buffer, 0, &scissor);
                }
                Command::PushConstants { layout, offset, data } => {
                    cmd_push_constants(&mut enc, 0, buffer, *layout, stages(backend::stage::ALL), *offset, data)
                }
                Command::Draw { vertices, instances, first_vertex, first_instance } => {
                    cmd_draw(&mut enc, 0, buffer, *vertices, *instances, *first_vertex, *first_instance)
                }
                Command::DrawIndexed { indices, instances, first_index, vertex_offset, first_instance } => {
                    cmd_draw_indexed(&mut enc, 0, buffer, *indices, *instances, *first_index, *vertex_offset, *first_instance)
                }
                Command::CopyBuffer { src, dst, src_offset, dst_offset, size } => {
                    full_barrier(&mut enc, buffer, false);
                    let region = [VkBufferCopy { src_offset: *src_offset, dst_offset: *dst_offset, size: *size }];
                    cmd_copy_buffer(&mut enc, 0, buffer, *src, *dst, &region);
                }
                Command::CopyBufferToImage { buffer: source, offset, row_pixels, image, rect } => {
                    full_barrier(&mut enc, buffer, false);
                    let region = [buffer_image(*offset, *row_pixels, image, *rect)];
                    cmd_copy_buffer_to_image(&mut enc, 0, buffer, *source, image.id, general, &region);
                }
                Command::CopyImageToBuffer { image, rect, buffer: target, offset, row_pixels } => {
                    full_barrier(&mut enc, buffer, false);
                    let region = [buffer_image(*offset, *row_pixels, image, *rect)];
                    cmd_copy_image_to_buffer(&mut enc, 0, buffer, image.id, general, *target, &region);
                }
                Command::CopyImage { src, dst, src_x, src_y, dst_x, dst_y, width, height } => {
                    full_barrier(&mut enc, buffer, false);
                    let region = [VkImageCopy {
                        src_subresource: layers(src.format),
                        src_offset: offset3(*src_x, *src_y),
                        dst_subresource: layers(dst.format),
                        dst_offset: offset3(*dst_x, *dst_y),
                        extent: extent3(*width, *height),
                    }];
                    cmd_copy_image(&mut enc, 0, buffer, src.id, general, dst.id, general, &region);
                }
                Command::Blit { src, src_rect, dst, dst_rect, linear } => {
                    full_barrier(&mut enc, buffer, false);
                    let corners = |r: &Rect| [offset3(r.x, r.y), VkOffset3D { x: r.x + r.width as i32, y: r.y + r.height as i32, z: 1 }];
                    let region = [VkImageBlit {
                        src_subresource: layers(src.format),
                        src_offsets: corners(src_rect),
                        dst_subresource: layers(dst.format),
                        dst_offsets: corners(dst_rect),
                    }];
                    let filter = if *linear { vk::VK_FILTER_LINEAR } else { vk::VK_FILTER_NEAREST };
                    cmd_blit_image(&mut enc, 0, buffer, src.id, general, dst.id, general, &region, filter);
                }
                Command::ClearImage { image, color } => {
                    full_barrier(&mut enc, buffer, false);
                    cmd_clear_color_image(&mut enc, 0, buffer, image.id, general, color, &[whole(image.format)]);
                }
                Command::FillBuffer { buffer: target, offset, size, value } => {
                    full_barrier(&mut enc, buffer, false);
                    cmd_fill_buffer(&mut enc, 0, buffer, *target, *offset, *size, *value);
                }
                Command::UpdateBuffer { buffer: target, offset, data } => {
                    full_barrier(&mut enc, buffer, false);
                    cmd_update_buffer(&mut enc, 0, buffer, *target, *offset, data);
                }
            }
        }
        cuts.push(enc.len());
        // The completion word: written by the GPU once everything above is
        // done, and seen by the kernel as soon as it is. The virtio fence
        // says the same thing later -- QEMU looks at the host's fences
        // every ten milliseconds, a whole frame.
        if let Some(feedback) = self.feedback.as_ref() {
            full_barrier(&mut enc, buffer, false);
            cmd_fill_buffer(&mut enc, 0, buffer, feedback.buffer, 0, 4, self.next_work as u32);
        }
        full_barrier(&mut enc, buffer, true);
        end_command_buffer(&mut enc, 0, buffer);
        let commands_buffers = [buffer];
        let submit = [VkSubmitInfo { command_buffers: &commands_buffers, ..Default::default() }];
        queue_submit(&mut enc, 0, self.session.queue, &submit, 0);
        if enc.error() {
            return Err(Error::Device("flux Venus incoherent"));
        }

        // --- out: every chunk but the last without a fence ---
        let bytes = enc.bytes();
        let limit = transport::max_stream();
        let mut start = 0;
        while bytes.len() - start > limit {
            let cut = cuts.iter().copied().filter(|c| *c > start && *c - start <= limit).max().ok_or(Error::Unsupported)?;
            let mut chunk = Enc::new();
            chunk.raw(&bytes[start..cut]);
            self.session.send(&chunk)?;
            start = cut;
        }
        let mut last = Enc::new();
        last.raw(&bytes[start..]);
        let submission = self.session.submit_work(&last, self.feedback.is_some())?;
        let work = self.next_work;
        self.next_work += 1;
        self.in_flight.insert(work, (submission, buffer));
        Ok(Work(work))
    }
}


pub(super) fn buffer_image(offset: u64, row_pixels: u32, image: &ImageRef, rect: Rect) -> VkBufferImageCopy {
    VkBufferImageCopy {
        buffer_offset: offset,
        buffer_row_length: row_pixels,
        buffer_image_height: 0,
        image_subresource: layers(image.format),
        image_offset: offset3(rect.x, rect.y),
        image_extent: extent3(rect.width, rect.height),
    }
}
