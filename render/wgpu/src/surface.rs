mod commands;
pub mod target;

use crate::blend::ComplexBlend;
use crate::buffer_pool::TexturePool;
use crate::mesh::Mesh;
use crate::surface::commands::{chunk_blends, Chunk, CommandRenderer};
use crate::uniform_buffer::BufferStorage;
use crate::utils::{remove_srgb, supported_sample_count};
use crate::{
    ColorAdjustments, Descriptors, MaskState, Pipelines, PushConstants, Texture, TextureTransforms,
    Transforms, UniformBuffer, DEFAULT_COLOR_ADJUSTMENTS,
};
use ruffle_render::commands::CommandList;
use ruffle_render::filters::{BlurFilter, ColorMatrixFilter, Filter};
use ruffle_render::quality::StageQuality;
use std::sync::Arc;
use target::CommandTarget;
use tracing::instrument;
use wgpu::util::DeviceExt;

#[derive(Debug)]
pub struct Surface {
    size: wgpu::Extent3d,
    quality: StageQuality,
    sample_count: u32,
    pipelines: Arc<Pipelines>,
    format: wgpu::TextureFormat,
    actual_surface_format: wgpu::TextureFormat,
}

impl Surface {
    pub fn new(
        descriptors: &Descriptors,
        quality: StageQuality,
        width: u32,
        height: u32,
        surface_format: wgpu::TextureFormat,
    ) -> Self {
        let size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let frame_buffer_format = remove_srgb(surface_format);

        let sample_count =
            supported_sample_count(&descriptors.adapter, quality, frame_buffer_format);
        let pipelines = descriptors.pipelines(sample_count, frame_buffer_format);
        Self {
            size,
            quality,
            sample_count,
            pipelines,
            format: frame_buffer_format,
            actual_surface_format: surface_format,
        }
    }

    #[allow(clippy::too_many_arguments)]
    #[instrument(level = "debug", skip_all)]
    pub fn draw_commands_to(
        &mut self,
        frame_view: &wgpu::TextureView,
        clear_color: Option<wgpu::Color>,
        descriptors: &Descriptors,
        uniform_buffers_storage: &mut BufferStorage<Transforms>,
        color_buffers_storage: &mut BufferStorage<ColorAdjustments>,
        meshes: &Vec<Mesh>,
        commands: CommandList,
        texture_pool: &mut TexturePool,
    ) -> Vec<wgpu::CommandBuffer> {
        let uniform_encoder_label = create_debug_label!("Uniform upload command encoder");
        let mut uniform_buffer = UniformBuffer::new(uniform_buffers_storage);
        let mut color_buffer = UniformBuffer::new(color_buffers_storage);
        let mut uniform_encoder =
            descriptors
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: uniform_encoder_label.as_deref(),
                });
        let label = create_debug_label!("Draw encoder");
        let mut draw_encoder =
            descriptors
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: label.as_deref(),
                });

        let target = self.draw_commands(
            clear_color.unwrap_or(wgpu::Color::TRANSPARENT),
            descriptors,
            meshes,
            commands,
            &mut uniform_buffer,
            &mut color_buffer,
            &mut uniform_encoder,
            &mut draw_encoder,
            None,
            texture_pool,
        );
        let mut buffers = vec![draw_encoder.finish()];

        let copy_bind_group = descriptors
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                layout: &descriptors.bind_layouts.bitmap,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: descriptors.quad.texture_transforms.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&target.color_view()),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(
                            &descriptors.bitmap_samplers.get_sampler(false, false),
                        ),
                    },
                ],
                label: create_debug_label!("Copy sRGB bind group").as_deref(),
            });

        let pipeline = if self.actual_surface_format == self.format {
            descriptors.copy_pipeline(self.format)
        } else {
            descriptors.copy_srgb_pipeline(self.actual_surface_format)
        };

        let mut copy_encoder =
            descriptors
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: create_debug_label!("Frame copy command encoder").as_deref(),
                });

        let load = match clear_color {
            Some(color) => wgpu::LoadOp::Clear(color),
            None => wgpu::LoadOp::Load,
        };

        let mut render_pass = copy_encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: frame_view,
                ops: wgpu::Operations { load, store: true },
                resolve_target: None,
            })],
            depth_stencil_attachment: None,
            label: create_debug_label!("Copy back to render target").as_deref(),
        });

        render_pass.set_pipeline(&pipeline);
        render_pass.set_bind_group(0, target.globals().bind_group(), &[]);

        if descriptors.limits.max_push_constant_size > 0 {
            render_pass.set_push_constants(
                wgpu::ShaderStages::VERTEX,
                0,
                bytemuck::cast_slice(&[Transforms {
                    world_matrix: [
                        [self.size.width as f32, 0.0, 0.0, 0.0],
                        [0.0, self.size.height as f32, 0.0, 0.0],
                        [0.0, 0.0, 1.0, 0.0],
                        [0.0, 0.0, 0.0, 1.0],
                    ],
                }]),
            );
            render_pass.set_bind_group(1, &copy_bind_group, &[]);
        } else {
            render_pass.set_bind_group(1, target.whole_frame_bind_group(descriptors), &[0]);
            render_pass.set_bind_group(2, &copy_bind_group, &[]);
        }

        render_pass.set_vertex_buffer(0, descriptors.quad.vertices_pos.slice(..));
        render_pass.set_index_buffer(
            descriptors.quad.indices.slice(..),
            wgpu::IndexFormat::Uint32,
        );

        render_pass.draw_indexed(0..6, 0, 0..1);
        drop(render_pass);

        buffers.push(copy_encoder.finish());
        buffers.insert(0, uniform_encoder.finish());
        uniform_buffer.finish();
        color_buffer.finish();

        buffers
    }

    #[allow(clippy::too_many_arguments)]
    #[instrument(level = "debug", skip_all)]
    pub fn draw_commands<'frame, 'global: 'frame>(
        &mut self,
        clear_color: wgpu::Color,
        descriptors: &'global Descriptors,
        meshes: &'global Vec<Mesh>,
        commands: CommandList,
        uniform_buffers: &'frame mut UniformBuffer<'global, Transforms>,
        color_buffers: &'frame mut UniformBuffer<'global, ColorAdjustments>,
        uniform_encoder: &'frame mut wgpu::CommandEncoder,
        draw_encoder: &'frame mut wgpu::CommandEncoder,
        nearest_layer: Option<&'frame CommandTarget>,
        texture_pool: &mut TexturePool,
    ) -> CommandTarget {
        let target = CommandTarget::new(
            &descriptors,
            texture_pool,
            self.size,
            self.format,
            self.sample_count,
            clear_color,
        );
        let mut num_masks = 0;
        let mut mask_state = MaskState::NoMask;
        let chunks = chunk_blends(
            commands.commands,
            descriptors,
            uniform_buffers,
            color_buffers,
            uniform_encoder,
            draw_encoder,
            meshes,
            self.quality,
            target.width(),
            target.height(),
            nearest_layer.unwrap_or(&target),
            texture_pool,
        );

        for chunk in chunks {
            match chunk {
                Chunk::Draw(chunk, needs_depth) => {
                    let mut render_pass =
                        draw_encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                            label: create_debug_label!(
                                "Chunked draw calls {}",
                                if needs_depth {
                                    "(with depth)"
                                } else {
                                    "(Depthless)"
                                }
                            )
                            .as_deref(),
                            color_attachments: &[target.color_attachments()],
                            depth_stencil_attachment: if needs_depth {
                                target.depth_attachment(&descriptors, texture_pool)
                            } else {
                                None
                            },
                        });
                    render_pass.set_bind_group(0, target.globals().bind_group(), &[]);
                    let mut renderer = CommandRenderer::new(
                        &self.pipelines,
                        &meshes,
                        &descriptors,
                        uniform_buffers,
                        color_buffers,
                        uniform_encoder,
                        render_pass,
                        num_masks,
                        mask_state,
                        needs_depth,
                    );

                    for command in &chunk {
                        renderer.execute(command);
                    }

                    num_masks = renderer.num_masks();
                    mask_state = renderer.mask_state();
                }
                Chunk::Blend(texture, blend_mode, needs_depth) => {
                    let parent = match blend_mode {
                        ComplexBlend::Alpha | ComplexBlend::Erase => {
                            nearest_layer.unwrap_or(&target)
                        }
                        _ => &target,
                    };

                    let parent_blend_buffer =
                        parent.update_blend_buffer(&descriptors, texture_pool, draw_encoder);

                    let blend_bind_group =
                        descriptors
                            .device
                            .create_bind_group(&wgpu::BindGroupDescriptor {
                                label: create_debug_label!(
                                    "Complex blend binds {:?} {}",
                                    blend_mode,
                                    if needs_depth {
                                        "(with depth)"
                                    } else {
                                        "(Depthless)"
                                    }
                                )
                                .as_deref(),
                                layout: &descriptors.bind_layouts.blend,
                                entries: &[
                                    wgpu::BindGroupEntry {
                                        binding: 0,
                                        resource: wgpu::BindingResource::TextureView(
                                            parent_blend_buffer.view(),
                                        ),
                                    },
                                    wgpu::BindGroupEntry {
                                        binding: 1,
                                        resource: wgpu::BindingResource::TextureView(&texture.1),
                                    },
                                    wgpu::BindGroupEntry {
                                        binding: 2,
                                        resource: wgpu::BindingResource::Sampler(
                                            descriptors.bitmap_samplers.get_sampler(false, false),
                                        ),
                                    },
                                ],
                            });

                    let mut render_pass =
                        draw_encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                            label: create_debug_label!(
                                "Complex blend {:?} {}",
                                blend_mode,
                                if needs_depth {
                                    "(with depth)"
                                } else {
                                    "(Depthless)"
                                }
                            )
                            .as_deref(),
                            color_attachments: &[target.color_attachments()],
                            depth_stencil_attachment: if needs_depth {
                                target.depth_attachment(&descriptors, texture_pool)
                            } else {
                                None
                            },
                        });
                    render_pass.set_bind_group(0, target.globals().bind_group(), &[]);

                    if needs_depth {
                        match mask_state {
                            MaskState::NoMask => {}
                            MaskState::DrawMaskStencil => {
                                render_pass.set_stencil_reference(num_masks - 1);
                            }
                            MaskState::DrawMaskedContent => {
                                render_pass.set_stencil_reference(num_masks);
                            }
                            MaskState::ClearMaskStencil => {
                                render_pass.set_stencil_reference(num_masks);
                            }
                        }
                        render_pass.set_pipeline(
                            self.pipelines.complex_blends[blend_mode].pipeline_for(mask_state),
                        );
                    } else {
                        render_pass.set_pipeline(
                            self.pipelines.complex_blends[blend_mode].depthless_pipeline(),
                        );
                    }

                    if descriptors.limits.max_push_constant_size > 0 {
                        render_pass.set_push_constants(
                            wgpu::ShaderStages::VERTEX,
                            0,
                            bytemuck::cast_slice(&[Transforms {
                                world_matrix: [
                                    [self.size.width as f32, 0.0, 0.0, 0.0],
                                    [0.0, self.size.height as f32, 0.0, 0.0],
                                    [0.0, 0.0, 1.0, 0.0],
                                    [0.0, 0.0, 0.0, 1.0],
                                ],
                            }]),
                        );
                        render_pass.set_bind_group(1, &blend_bind_group, &[]);
                    } else {
                        render_pass.set_bind_group(
                            1,
                            target.whole_frame_bind_group(descriptors),
                            &[0],
                        );
                        render_pass.set_bind_group(2, &blend_bind_group, &[]);
                    }

                    render_pass.set_vertex_buffer(0, descriptors.quad.vertices_pos.slice(..));
                    render_pass.set_index_buffer(
                        descriptors.quad.indices.slice(..),
                        wgpu::IndexFormat::Uint32,
                    );

                    render_pass.draw_indexed(0..6, 0, 0..1);
                    drop(render_pass);
                }
            }
        }

        target
    }

    pub fn quality(&self) -> StageQuality {
        self.quality
    }

    pub fn sample_count(&self) -> u32 {
        self.sample_count
    }

    pub fn size(&self) -> wgpu::Extent3d {
        self.size
    }

    #[allow(clippy::too_many_arguments)]
    pub fn apply_filter(
        &self,
        descriptors: &Descriptors,
        draw_encoder: &mut wgpu::CommandEncoder,
        texture_pool: &mut TexturePool,
        source_texture: &Texture,
        source_point: (u32, u32),
        source_size: (u32, u32),
        dest_texture: &Texture,
        dest_point: (u32, u32),
        filter: Filter,
    ) {
        let target = match filter {
            Filter::ColorMatrixFilter(filter) => self.apply_color_matrix(
                descriptors,
                texture_pool,
                draw_encoder,
                source_texture,
                source_point,
                source_size,
                &filter,
            ),
            Filter::BlurFilter(filter) => self.apply_blur(
                descriptors,
                texture_pool,
                draw_encoder,
                source_texture,
                source_point,
                source_size,
                &filter,
            ),
        };

        draw_encoder.copy_texture_to_texture(
            wgpu::ImageCopyTexture {
                texture: target.color_texture(),
                mip_level: 0,
                origin: wgpu::Origin3d { x: 0, y: 0, z: 0 },
                aspect: Default::default(),
            },
            wgpu::ImageCopyTexture {
                texture: &dest_texture.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: dest_point.0,
                    y: dest_point.1,
                    z: 0,
                },
                aspect: Default::default(),
            },
            wgpu::Extent3d {
                width: (target.width()).min(dest_texture.width - dest_point.0),
                height: (target.height()).min(dest_texture.height - dest_point.1),
                depth_or_array_layers: 1,
            },
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn apply_color_matrix(
        &self,
        descriptors: &Descriptors,
        texture_pool: &mut TexturePool,
        draw_encoder: &mut wgpu::CommandEncoder,
        source_texture: &Texture,
        source_point: (u32, u32),
        source_size: (u32, u32),
        filter: &ColorMatrixFilter,
    ) -> CommandTarget {
        let target = CommandTarget::new(
            &descriptors,
            texture_pool,
            wgpu::Extent3d {
                width: source_size.0,
                height: source_size.1,
                depth_or_array_layers: 1,
            },
            self.format,
            self.sample_count,
            wgpu::Color::TRANSPARENT,
        );
        let texture_transform =
            descriptors
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: None,
                    contents: bytemuck::cast_slice(&[TextureTransforms {
                        u_matrix: [
                            [
                                1.0,
                                0.0,
                                0.0,
                                (source_point.0 as f32 / source_size.0 as f32),
                            ],
                            [
                                0.0,
                                1.0,
                                0.0,
                                (source_point.1 as f32 / source_size.1 as f32),
                            ],
                            [0.0, 0.0, 1.0, 0.0],
                            [0.0, 0.0, 0.0, 1.0],
                        ],
                    }]),
                    usage: wgpu::BufferUsages::UNIFORM,
                });
        let source_view = source_texture.texture.create_view(&Default::default());
        let bitmap_group = descriptors
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: create_debug_label!("Bitmap copy group").as_deref(),
                layout: &descriptors.bind_layouts.bitmap,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: texture_transform.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&source_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(
                            &descriptors.bitmap_samplers.get_sampler(false, false),
                        ),
                    },
                ],
            });
        let buffer = descriptors
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: create_debug_label!("Filter arguments").as_deref(),
                contents: &bytemuck::cast_slice(&filter.matrix),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let filter_group = descriptors
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: create_debug_label!("Filter group").as_deref(),
                layout: &descriptors.bind_layouts.color_matrix_filter,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: buffer.as_entire_binding(),
                }],
            });
        let mut render_pass = draw_encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: create_debug_label!("Color matrix filter").as_deref(),
            color_attachments: &[target.color_attachments()],
            depth_stencil_attachment: None,
        });
        render_pass.set_pipeline(&self.pipelines.color_matrix_filter);

        render_pass.set_bind_group(0, target.globals().bind_group(), &[]);
        if descriptors.limits.max_push_constant_size > 0 {
            render_pass.set_push_constants(
                wgpu::ShaderStages::VERTEX_FRAGMENT,
                0,
                bytemuck::cast_slice(&[PushConstants {
                    transforms: Transforms {
                        world_matrix: [
                            [self.size.width as f32, 0.0, 0.0, 0.0],
                            [0.0, self.size.height as f32, 0.0, 0.0],
                            [0.0, 0.0, 1.0, 0.0],
                            [0.0, 0.0, 0.0, 1.0],
                        ],
                    },
                    colors: DEFAULT_COLOR_ADJUSTMENTS,
                }]),
            );
            render_pass.set_bind_group(1, &bitmap_group, &[]);
            render_pass.set_bind_group(2, &filter_group, &[]);
        } else {
            render_pass.set_bind_group(1, target.whole_frame_bind_group(descriptors), &[0]);
            render_pass.set_bind_group(2, &descriptors.default_color_bind_group, &[0]);
            render_pass.set_bind_group(3, &bitmap_group, &[]);
            render_pass.set_bind_group(4, &filter_group, &[]);
        }

        render_pass.set_vertex_buffer(0, descriptors.quad.vertices_pos.slice(..));
        render_pass.set_index_buffer(
            descriptors.quad.indices.slice(..),
            wgpu::IndexFormat::Uint32,
        );
        render_pass.draw_indexed(0..6, 0, 0..1);
        drop(render_pass);
        target
    }

    #[allow(clippy::too_many_arguments)]
    pub fn apply_blur(
        &self,
        descriptors: &Descriptors,
        texture_pool: &mut TexturePool,
        draw_encoder: &mut wgpu::CommandEncoder,
        source_texture: &Texture,
        source_point: (u32, u32),
        source_size: (u32, u32),
        filter: &BlurFilter,
    ) -> CommandTarget {
        let targets = [
            CommandTarget::new(
                &descriptors,
                texture_pool,
                wgpu::Extent3d {
                    width: source_size.0,
                    height: source_size.1,
                    depth_or_array_layers: 1,
                },
                self.format,
                self.sample_count,
                wgpu::Color::TRANSPARENT,
            ),
            CommandTarget::new(
                &descriptors,
                texture_pool,
                wgpu::Extent3d {
                    width: source_size.0,
                    height: source_size.1,
                    depth_or_array_layers: 1,
                },
                self.format,
                self.sample_count,
                wgpu::Color::TRANSPARENT,
            ),
        ];
        let texture_transform =
            descriptors
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: None,
                    contents: bytemuck::cast_slice(&[TextureTransforms {
                        u_matrix: [
                            [
                                1.0,
                                0.0,
                                0.0,
                                (source_point.0 as f32 / source_size.0 as f32),
                            ],
                            [
                                0.0,
                                1.0,
                                0.0,
                                (source_point.1 as f32 / source_size.1 as f32),
                            ],
                            [0.0, 0.0, 1.0, 0.0],
                            [0.0, 0.0, 0.0, 1.0],
                        ],
                    }]),
                    usage: wgpu::BufferUsages::UNIFORM,
                });
        let source_view = source_texture.texture.create_view(&Default::default());
        for i in 0..2 {
            let blur_x = (filter.blur_x - 1.0).max(0.0);
            let blur_y = (filter.blur_y - 1.0).max(0.0);
            let current = &targets[i % 2];
            let (previous_view, previous_transform, previous_width, previous_height) = if i == 0 {
                (
                    &source_view,
                    texture_transform.as_entire_binding(),
                    source_texture.width as f32,
                    source_texture.height as f32,
                )
            } else {
                let previous = &targets[(i - 1) % 2];
                (
                    previous.color_view(),
                    descriptors.quad.texture_transforms.as_entire_binding(),
                    previous.width() as f32,
                    previous.height() as f32,
                )
            };
            let bitmap_group = descriptors
                .device
                .create_bind_group(&wgpu::BindGroupDescriptor {
                    label: create_debug_label!("Bitmap copy group").as_deref(),
                    layout: &descriptors.bind_layouts.bitmap,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: previous_transform,
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::TextureView(previous_view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: wgpu::BindingResource::Sampler(
                                &descriptors.bitmap_samplers.get_sampler(false, true),
                            ),
                        },
                    ],
                });
            let buffer = descriptors
                .device
                .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: create_debug_label!("Filter arguments").as_deref(),
                    contents: &bytemuck::cast_slice(&[
                        blur_x * ((i as u32) % 2) as f32,
                        blur_y * (((i as u32) % 2) + 1) as f32,
                        previous_width,
                        previous_height,
                    ]),
                    usage: wgpu::BufferUsages::UNIFORM,
                });
            let filter_group = descriptors
                .device
                .create_bind_group(&wgpu::BindGroupDescriptor {
                    label: create_debug_label!("Filter group").as_deref(),
                    layout: &descriptors.bind_layouts.blur_filter,
                    entries: &[wgpu::BindGroupEntry {
                        binding: 0,
                        resource: buffer.as_entire_binding(),
                    }],
                });
            let mut render_pass = draw_encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: create_debug_label!("Color matrix filter").as_deref(),
                color_attachments: &[current.color_attachments()],
                depth_stencil_attachment: None,
            });
            render_pass.set_pipeline(&self.pipelines.blur_filter);

            render_pass.set_bind_group(0, current.globals().bind_group(), &[]);
            if descriptors.limits.max_push_constant_size > 0 {
                render_pass.set_push_constants(
                    wgpu::ShaderStages::VERTEX_FRAGMENT,
                    0,
                    bytemuck::cast_slice(&[PushConstants {
                        transforms: Transforms {
                            world_matrix: [
                                [current.width() as f32, 0.0, 0.0, 0.0],
                                [0.0, current.height() as f32, 0.0, 0.0],
                                [0.0, 0.0, 1.0, 0.0],
                                [0.0, 0.0, 0.0, 1.0],
                            ],
                        },
                        colors: DEFAULT_COLOR_ADJUSTMENTS,
                    }]),
                );
                render_pass.set_bind_group(1, &bitmap_group, &[]);
                render_pass.set_bind_group(2, &filter_group, &[]);
            } else {
                render_pass.set_bind_group(1, current.whole_frame_bind_group(descriptors), &[0]);
                render_pass.set_bind_group(2, &descriptors.default_color_bind_group, &[0]);
                render_pass.set_bind_group(3, &bitmap_group, &[]);
                render_pass.set_bind_group(4, &filter_group, &[]);
            }

            render_pass.set_vertex_buffer(0, descriptors.quad.vertices_pos.slice(..));
            render_pass.set_index_buffer(
                descriptors.quad.indices.slice(..),
                wgpu::IndexFormat::Uint32,
            );
            render_pass.draw_indexed(0..6, 0, 0..1);
        }
        targets
            .into_iter()
            .last()
            .expect("Targets should not be empty")
    }
}
