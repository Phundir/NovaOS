//! Creating the host objects behind Nova's: buffers, images, samplers,
//! shaders, layouts and pipelines, each one a round trip or two to the
//! host. Kept out of the frame path on purpose (see the parent module).

use super::*;
use crate::gpu::backend::Context as _;

impl VenusContext {
    pub(super) fn make_buffer(&mut self, desc: &backend::BufferDesc, mem: Id, offset: u64) -> Result<Id> {
        let type_index = self.memories.get(&mem).map(|m| m.type_index).ok_or(Error::Device("memoire inconnue"))?;
        let mut usage = vk::VK_BUFFER_USAGE_TRANSFER_SRC_BIT | vk::VK_BUFFER_USAGE_TRANSFER_DST_BIT;
        if desc.usage & backend::buffer_usage::VERTEX != 0 {
            usage |= vk::VK_BUFFER_USAGE_VERTEX_BUFFER_BIT;
        }
        if desc.usage & backend::buffer_usage::INDEX != 0 {
            usage |= vk::VK_BUFFER_USAGE_INDEX_BUFFER_BIT;
        }
        if desc.usage & backend::buffer_usage::UNIFORM != 0 {
            usage |= vk::VK_BUFFER_USAGE_UNIFORM_BUFFER_BIT;
        }
        if desc.usage & backend::buffer_usage::STORAGE != 0 {
            usage |= vk::VK_BUFFER_USAGE_STORAGE_BUFFER_BIT;
        }
        let id = self.session.id();
        let info = VkBufferCreateInfo { size: desc.size, usage, sharing_mode: vk::VK_SHARING_MODE_EXCLUSIVE, ..Default::default() };
        let mut enc = Enc::new();
        create_buffer(&mut enc, GENERATE_REPLY, self.session.device, &info, id);
        get_buffer_memory_requirements(&mut enc, GENERATE_REPLY, self.session.device, id);
        let reply = self.session.call(&enc)?;
        let mut dec = Dec::new(&reply);
        let created = reply_create_buffer(&mut dec).ok_or(Error::Device("reponse illisible"))?;
        check(created.ret)?;
        let requirements = reply_get_buffer_memory_requirements(&mut dec)
            .and_then(|r| r.memory_requirements)
            .ok_or(Error::Device("reponse illisible"))?;
        let fits = requirements.memory_type_bits & (1 << type_index) != 0
            && offset % requirements.alignment.max(1) == 0
            && offset.checked_add(requirements.size).is_some_and(|end| end <= self.memories[&mem].size);
        let mut enc = Enc::new();
        if !fits {
            destroy_buffer(&mut enc, 0, self.session.device, id);
            let _ = self.session.send(&enc);
            return Err(Error::Device("tampon incompatible avec sa memoire (type, alignement ou taille)"));
        }
        bind_buffer_memory(&mut enc, GENERATE_REPLY, self.session.device, id, mem, offset);
        let reply = self.session.call(&enc)?;
        let bound = reply_bind_buffer_memory(&mut Dec::new(&reply)).ok_or(Error::Device("reponse illisible"))?;
        if let Err(error) = check(bound.ret) {
            let mut enc = Enc::new();
            destroy_buffer(&mut enc, 0, self.session.device, id);
            let _ = self.session.send(&enc);
            return Err(error.into());
        }
        Ok(id)
    }

    pub(super) fn make_image(&mut self, desc: &ImageDesc, memory: Option<(Id, u64)>) -> Result<(Id, u64)> {
        let shareable = desc.shareable && self.session.info.dma_buf && memory.is_none();
        static EXTERNAL: [VkNext<'static>; 1] = [VkNext::ExternalMemoryImageCreateInfo(VkExternalMemoryImageCreateInfo {
            handle_types: vk::VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT,
        })];
        let next: &'static [VkNext<'static>] = if shareable { &EXTERNAL } else { &[] };
        let info = Self::image_create_info(desc, next);
        let (id, requirements) = self.new_image(&info)?;
        let destroy = |this: &mut Self| {
            let mut enc = Enc::new();
            destroy_image(&mut enc, 0, this.session.device, id);
            let _ = this.session.send(&enc);
        };
        let (memory_id, own, offset) = match memory {
            Some((mem, offset)) => {
                let Some(info) = self.memories.get(&mem) else {
                    destroy(self);
                    return Err(Error::Device("memoire inconnue"));
                };
                let fits = requirements.memory_type_bits & (1 << info.type_index) != 0
                    && offset % requirements.alignment.max(1) == 0
                    && offset.checked_add(requirements.size).is_some_and(|end| end <= info.size);
                if !fits {
                    destroy(self);
                    return Err(Error::Device("image incompatible avec sa memoire (type, alignement ou taille)"));
                }
                (mem, false, offset)
            }
            None => {
                let Some(type_index) = self.memory_type(requirements.memory_type_bits, MemKind::Local) else {
                    destroy(self);
                    return Err(Error::Unsupported);
                };
                let dedicated = VkNext::MemoryDedicatedAllocateInfo(VkMemoryDedicatedAllocateInfo { image: id, buffer: 0 });
                let export = VkNext::ExportMemoryAllocateInfo(VkExportMemoryAllocateInfo {
                    handle_types: vk::VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT,
                });
                let chain_shared = [dedicated, export];
                let chain_plain = [dedicated];
                let chain: &[VkNext] = if shareable { &chain_shared } else { &chain_plain };
                match self.allocate(requirements.size, type_index, chain) {
                    Ok(memory) => (memory, true, 0),
                    Err(error) => {
                        destroy(self);
                        return Err(error);
                    }
                }
            }
        };
        let view = self.session.id();
        let view_info = self.view_info(id, desc.format);
        let mut enc = Enc::new();
        bind_image_memory(&mut enc, GENERATE_REPLY, self.session.device, id, memory_id, offset);
        create_image_view(&mut enc, GENERATE_REPLY, self.session.device, &view_info, view);
        let reply = self.session.call(&enc)?;
        let mut dec = Dec::new(&reply);
        let bound = reply_bind_image_memory(&mut dec).map(|r| r.ret).unwrap_or(vk::VK_ERROR_UNKNOWN);
        let viewed = reply_create_image_view(&mut dec).map(|r| r.ret).unwrap_or(vk::VK_ERROR_UNKNOWN);
        let own_memory = own.then_some(memory_id);
        self.images.insert(id, ImageInfo { view, memory: own_memory });
        if bound < 0 || viewed < 0 {
            self.destroy_image_now(id);
            return Err(VenusError::Vulkan(bound.min(viewed)).into());
        }
        Ok((id, if own { requirements.size } else { 0 }))
    }

    pub(super) fn make_sampler(&mut self, desc: &backend::SamplerDesc) -> Result<Id> {
        let filter = if desc.linear { vk::VK_FILTER_LINEAR } else { vk::VK_FILTER_NEAREST };
        let address = match desc.address {
            1 => vk::VK_SAMPLER_ADDRESS_MODE_REPEAT,
            2 => vk::VK_SAMPLER_ADDRESS_MODE_MIRRORED_REPEAT,
            _ => vk::VK_SAMPLER_ADDRESS_MODE_CLAMP_TO_EDGE,
        };
        let info = VkSamplerCreateInfo {
            mag_filter: filter,
            min_filter: filter,
            mipmap_mode: vk::VK_SAMPLER_MIPMAP_MODE_NEAREST,
            address_mode_u: address,
            address_mode_v: address,
            address_mode_w: address,
            border_color: vk::VK_BORDER_COLOR_FLOAT_TRANSPARENT_BLACK,
            ..Default::default()
        };
        let id = self.session.id();
        let mut enc = Enc::new();
        create_sampler(&mut enc, GENERATE_REPLY, self.session.device, &info, id);
        let reply = self.session.call(&enc)?;
        check(reply_create_sampler(&mut Dec::new(&reply)).map(|r| r.ret).unwrap_or(vk::VK_ERROR_UNKNOWN))?;
        Ok(id)
    }

    pub(super) fn make_shader(&mut self, code: &[u32]) -> Result<Id> {
        let info = VkShaderModuleCreateInfo { code_size: code.len() as u64 * 4, code: Some(code), ..Default::default() };
        let id = self.session.id();
        let mut enc = Enc::new();
        create_shader_module(&mut enc, GENERATE_REPLY, self.session.device, &info, id);
        if enc.len() > transport::max_stream() {
            return Err(Error::Unsupported);
        }
        let reply = self.session.call(&enc)?;
        check(reply_create_shader_module(&mut Dec::new(&reply)).map(|r| r.ret).unwrap_or(vk::VK_ERROR_UNKNOWN))?;
        Ok(id)
    }

    pub(super) fn make_layout(&mut self, desc: &backend::LayoutDesc) -> Result<Id> {
        let bindings: Vec<VkDescriptorSetLayoutBinding> = desc
            .bindings
            .iter()
            .map(|b| VkDescriptorSetLayoutBinding {
                binding: b.binding,
                descriptor_type: match b.kind {
                    BindingKind::SampledImage => vk::VK_DESCRIPTOR_TYPE_COMBINED_IMAGE_SAMPLER,
                    BindingKind::UniformBuffer => vk::VK_DESCRIPTOR_TYPE_UNIFORM_BUFFER,
                    BindingKind::StorageBuffer => vk::VK_DESCRIPTOR_TYPE_STORAGE_BUFFER,
                },
                descriptor_count: 1,
                stage_flags: stages(b.stages),
                immutable_samplers: None,
            })
            .collect();
        let set_layout = self.session.id();
        let pipeline_layout = self.session.id();
        let set_info = VkDescriptorSetLayoutCreateInfo {
            flags: vk::VK_DESCRIPTOR_SET_LAYOUT_CREATE_PUSH_DESCRIPTOR_BIT,
            bindings: &bindings,
            ..Default::default()
        };
        let set_layouts = [set_layout];
        let ranges = [VkPushConstantRange { stage_flags: stages(backend::stage::ALL), offset: 0, size: desc.push_constants }];
        let layout_info = VkPipelineLayoutCreateInfo {
            set_layouts: if bindings.is_empty() { &[] } else { &set_layouts },
            push_constant_ranges: if desc.push_constants == 0 { &[] } else { &ranges },
            ..Default::default()
        };
        let mut enc = Enc::new();
        create_descriptor_set_layout(&mut enc, GENERATE_REPLY, self.session.device, &set_info, set_layout);
        create_pipeline_layout(&mut enc, GENERATE_REPLY, self.session.device, &layout_info, pipeline_layout);
        let reply = self.session.call(&enc)?;
        let mut dec = Dec::new(&reply);
        let first = reply_create_descriptor_set_layout(&mut dec).map(|r| r.ret).unwrap_or(vk::VK_ERROR_UNKNOWN);
        let second = reply_create_pipeline_layout(&mut dec).map(|r| r.ret).unwrap_or(vk::VK_ERROR_UNKNOWN);
        self.layouts.insert(pipeline_layout, LayoutInfo { set_layout, pipeline_layout });
        if first < 0 || second < 0 {
            self.destroy(ObjectKind::Layout, pipeline_layout);
            return Err(VenusError::Vulkan(first.min(second)).into());
        }
        Ok(pipeline_layout)
    }

    pub(super) fn make_pipeline(&mut self, desc: &backend::PipelineDesc) -> Result<Id> {
        let stages_info = [
            VkPipelineShaderStageCreateInfo {
                stage: vk::VK_SHADER_STAGE_VERTEX_BIT as i32,
                module: desc.vertex,
                name: Some("main"),
                ..Default::default()
            },
            VkPipelineShaderStageCreateInfo {
                stage: vk::VK_SHADER_STAGE_FRAGMENT_BIT as i32,
                module: desc.fragment,
                name: Some("main"),
                ..Default::default()
            },
        ];
        let bindings: Vec<VkVertexInputBindingDescription> = desc
            .bindings
            .iter()
            .enumerate()
            .map(|(index, b)| VkVertexInputBindingDescription {
                binding: index as u32,
                stride: b.stride,
                input_rate: if b.per_instance { vk::VK_VERTEX_INPUT_RATE_INSTANCE } else { vk::VK_VERTEX_INPUT_RATE_VERTEX },
            })
            .collect();
        let attributes: Vec<VkVertexInputAttributeDescription> = desc
            .attributes
            .iter()
            .map(|a| VkVertexInputAttributeDescription {
                location: a.location,
                binding: a.binding,
                format: match a.format {
                    VertexFormat::Float1 => vk::VK_FORMAT_R32_SFLOAT,
                    VertexFormat::Float2 => vk::VK_FORMAT_R32G32_SFLOAT,
                    VertexFormat::Float3 => vk::VK_FORMAT_R32G32B32_SFLOAT,
                    VertexFormat::Float4 => vk::VK_FORMAT_R32G32B32A32_SFLOAT,
                    VertexFormat::Ubyte4Norm => vk::VK_FORMAT_R8G8B8A8_UNORM,
                    VertexFormat::Uint1 => vk::VK_FORMAT_R32_UINT,
                },
                offset: a.offset,
            })
            .collect();
        let vertex_input = VkPipelineVertexInputStateCreateInfo {
            vertex_binding_descriptions: &bindings,
            vertex_attribute_descriptions: &attributes,
            ..Default::default()
        };
        let assembly = VkPipelineInputAssemblyStateCreateInfo {
            topology: match desc.topology {
                Topology::Triangles => vk::VK_PRIMITIVE_TOPOLOGY_TRIANGLE_LIST,
                Topology::TriangleStrip => vk::VK_PRIMITIVE_TOPOLOGY_TRIANGLE_STRIP,
                Topology::Lines => vk::VK_PRIMITIVE_TOPOLOGY_LINE_LIST,
                Topology::Points => vk::VK_PRIMITIVE_TOPOLOGY_POINT_LIST,
            },
            ..Default::default()
        };
        let viewport = VkPipelineViewportStateCreateInfo { viewport_count: 1, scissor_count: 1, ..Default::default() };
        let raster = VkPipelineRasterizationStateCreateInfo {
            polygon_mode: vk::VK_POLYGON_MODE_FILL,
            cull_mode: match desc.cull {
                1 => vk::VK_CULL_MODE_BACK_BIT,
                2 => vk::VK_CULL_MODE_FRONT_BIT,
                _ => vk::VK_CULL_MODE_NONE,
            },
            front_face: if desc.front_ccw { vk::VK_FRONT_FACE_COUNTER_CLOCKWISE } else { vk::VK_FRONT_FACE_CLOCKWISE },
            line_width: 0x3F80_0000,
            ..Default::default()
        };
        let multisample = VkPipelineMultisampleStateCreateInfo { rasterization_samples: vk::VK_SAMPLE_COUNT_1_BIT as i32, ..Default::default() };
        let depth = VkPipelineDepthStencilStateCreateInfo {
            depth_test_enable: desc.depth_test as u32,
            depth_write_enable: desc.depth_write as u32,
            depth_compare_op: vk::VK_COMPARE_OP_LESS_OR_EQUAL,
            ..Default::default()
        };
        let (blend_enable, src, dst, src_alpha, dst_alpha) = match desc.blend {
            Blend::None => (0, vk::VK_BLEND_FACTOR_ONE, vk::VK_BLEND_FACTOR_ZERO, vk::VK_BLEND_FACTOR_ONE, vk::VK_BLEND_FACTOR_ZERO),
            Blend::Premultiplied => (
                1,
                vk::VK_BLEND_FACTOR_ONE,
                vk::VK_BLEND_FACTOR_ONE_MINUS_SRC_ALPHA,
                vk::VK_BLEND_FACTOR_ONE,
                vk::VK_BLEND_FACTOR_ONE_MINUS_SRC_ALPHA,
            ),
            Blend::Alpha => (
                1,
                vk::VK_BLEND_FACTOR_SRC_ALPHA,
                vk::VK_BLEND_FACTOR_ONE_MINUS_SRC_ALPHA,
                vk::VK_BLEND_FACTOR_ONE,
                vk::VK_BLEND_FACTOR_ONE_MINUS_SRC_ALPHA,
            ),
            Blend::Additive => (1, vk::VK_BLEND_FACTOR_ONE, vk::VK_BLEND_FACTOR_ONE, vk::VK_BLEND_FACTOR_ONE, vk::VK_BLEND_FACTOR_ONE),
        };
        let attachment = [VkPipelineColorBlendAttachmentState {
            blend_enable,
            src_color_blend_factor: src,
            dst_color_blend_factor: dst,
            color_blend_op: vk::VK_BLEND_OP_ADD,
            src_alpha_blend_factor: src_alpha,
            dst_alpha_blend_factor: dst_alpha,
            alpha_blend_op: vk::VK_BLEND_OP_ADD,
            color_write_mask: 0xF,
        }];
        let blend = VkPipelineColorBlendStateCreateInfo { attachment_count: 1, attachments: Some(&attachment), ..Default::default() };
        let dynamic_states = [vk::VK_DYNAMIC_STATE_VIEWPORT, vk::VK_DYNAMIC_STATE_SCISSOR];
        let dynamic = VkPipelineDynamicStateCreateInfo { dynamic_states: &dynamic_states, ..Default::default() };
        let color_formats = [vk_format(desc.color_format)];
        let rendering = [VkNext::PipelineRenderingCreateInfo(VkPipelineRenderingCreateInfo {
            view_mask: 0,
            color_attachment_count: 1,
            color_attachment_formats: Some(&color_formats),
            depth_attachment_format: desc.depth_format.map(vk_format).unwrap_or(vk::VK_FORMAT_UNDEFINED),
            stencil_attachment_format: vk::VK_FORMAT_UNDEFINED,
        })];
        let info = [VkGraphicsPipelineCreateInfo {
            next: &rendering,
            stage_count: 2,
            stages: Some(&stages_info),
            vertex_input_state: Some(&vertex_input),
            input_assembly_state: Some(&assembly),
            viewport_state: Some(&viewport),
            rasterization_state: Some(&raster),
            multisample_state: Some(&multisample),
            depth_stencil_state: if desc.depth_format.is_some() { Some(&depth) } else { None },
            color_blend_state: Some(&blend),
            dynamic_state: Some(&dynamic),
            layout: desc.layout,
            base_pipeline_index: -1,
            ..Default::default()
        }];
        let id = self.session.id();
        let mut enc = Enc::new();
        create_graphics_pipelines(&mut enc, GENERATE_REPLY, self.session.device, 0, &info, Some(&[id]));
        let reply = self.session.call(&enc)?;
        check(reply_create_graphics_pipelines(&mut Dec::new(&reply)).map(|r| r.ret).unwrap_or(vk::VK_ERROR_UNKNOWN))?;
        Ok(id)
    }
}
