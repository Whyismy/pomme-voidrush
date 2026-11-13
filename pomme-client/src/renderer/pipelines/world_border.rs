//! The world border's walls (vanilla `WorldBorderRenderer` and
//! `RenderPipelines.WORLD_BORDER`): the forcefield texture, repeating, in
//! the border's colour, added over the world behind it.

use std::path::Path;

use crate::assets::{AssetIndex, resolve_asset_path};
use crate::renderer::camera::CameraUniform;
use crate::renderer::{MAX_FRAMES_IN_FLIGHT, rhi, shader, util};

/// Four walls of two triangles each.
const MAX_VERTS: usize = 24;

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
pub struct BorderVertex {
    /// Less the camera's anchor.
    pub position: [f32; 3],
    pub uv: [f32; 2],
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct BorderPush {
    tint: [f32; 4],
}

/// A frame's walls and their colour.
pub struct BorderWalls {
    pub vertices: Vec<BorderVertex>,
    pub color: [f32; 4],
}

pub struct WorldBorderPipeline {
    pipeline: rhi::backend::RenderPipeline,
    pipeline_layout: rhi::backend::PipelineLayout,
    camera_layout: rhi::backend::BindGroupLayout,
    texture_layout: rhi::backend::BindGroupLayout,
    camera_sets: Vec<rhi::backend::BindGroup>,
    texture_set: rhi::backend::BindGroup,
    camera_buffers: Vec<rhi::backend::Buffer>,
    vertex_buffers: Vec<rhi::backend::Buffer>,
    texture: rhi::backend::Texture,
    sampler: rhi::backend::Sampler,
}

impl WorldBorderPipeline {
    pub fn new(
        device: &rhi::backend::Device,
        targets: rhi::backend::PipelineTargets,
        jar_assets_dir: &Path,
        asset_index: &Option<AssetIndex>,
    ) -> Self {
        let camera_layout = rhi::backend::create_bind_group_layout(device, util::VERTEX_UNIFORM);
        let texture_layout = rhi::backend::create_bind_group_layout(device, util::FRAGMENT_TEXTURE);
        let pipeline_layout = rhi::backend::create_pipeline_layout(
            device,
            &[camera_layout, texture_layout],
            Some(rhi::PushConstants {
                visibility: rhi::ShaderStages::FRAGMENT,
                size: size_of::<BorderPush>() as u32,
            }),
        );
        let pipeline = create_pipeline(device, targets, pipeline_layout);

        let (camera_sets, camera_buffers) = util::create_uniform_sets(
            device,
            camera_layout,
            size_of::<CameraUniform>() as u64,
            "world_border_camera",
        );

        let path = resolve_asset_path(
            jar_assets_dir,
            asset_index,
            "minecraft/textures/misc/forcefield.png",
        );
        // A plain white wall where the texture is missing.
        let (pixels, width, height) = util::load_png(&path).unwrap_or_else(|| (vec![255; 4], 1, 1));
        let texture = util::create_sampled_texture(device, "forcefield", &pixels, width, height);
        let sampler = rhi::backend::create_sampler(device, &rhi::SamplerDesc::NEAREST_REPEAT);
        let texture_set =
            util::create_texture_group(device, texture_layout, texture.handle(), sampler);

        let placeholder = vec![
            BorderVertex {
                position: [0.0; 3],
                uv: [0.0; 2],
            };
            MAX_VERTS
        ];
        let bytes = bytemuck::cast_slice::<BorderVertex, u8>(&placeholder);
        let vertex_buffers = (0..MAX_FRAMES_IN_FLIGHT)
            .map(|_| {
                rhi::backend::Buffer::with_data(
                    device,
                    "world_border_vertices",
                    bytes,
                    rhi::BufferUsages::VERTEX,
                )
            })
            .collect();

        Self {
            pipeline,
            pipeline_layout,
            camera_layout,
            texture_layout,
            camera_sets,
            texture_set,
            camera_buffers,
            vertex_buffers,
            texture,
            sampler,
        }
    }

    pub fn update_camera(&mut self, frame: usize, uniform: &CameraUniform) {
        let bytes = bytemuck::bytes_of(uniform);
        self.camera_buffers[frame].write(bytes);
    }

    pub fn draw(
        &mut self,
        pass: &mut rhi::backend::RenderPassEncoder,
        frame: usize,
        walls: &BorderWalls,
    ) {
        let count = walls.vertices.len().min(MAX_VERTS);
        if count == 0 {
            return;
        }
        let bytes = bytemuck::cast_slice::<BorderVertex, u8>(&walls.vertices[..count]);
        self.vertex_buffers[frame].write(bytes);

        pass.set_pipeline(&self.pipeline);
        let push = BorderPush { tint: walls.color };
        pass.set_push_constants(rhi::ShaderStages::FRAGMENT, 0, bytemuck::bytes_of(&push));
        pass.set_bind_groups(0, &[self.camera_sets[frame], self.texture_set]);
        pass.set_vertex_buffer(0, self.vertex_buffers[frame].handle());
        pass.draw(count as u32, 1, 0, 0);
    }

    pub fn recreate_pipeline(
        &mut self,
        device: &rhi::backend::Device,
        targets: rhi::backend::PipelineTargets,
    ) {
        self.pipeline.destroy(device);
        self.pipeline = create_pipeline(device, targets, self.pipeline_layout);
    }

    pub fn destroy(&mut self, device: &rhi::backend::Device) {
        util::destroy_buffers(device, &mut self.camera_buffers);
        util::destroy_buffers(device, &mut self.vertex_buffers);

        self.sampler.destroy(device);
        std::mem::take(&mut self.texture).destroy(device);

        self.pipeline.destroy(device);
        self.pipeline_layout.destroy(device);
        util::destroy_bind_groups(device, self.camera_sets.drain(..));
        self.texture_set.destroy(device);
        self.camera_layout.destroy(device);
        self.texture_layout.destroy(device);
    }
}

fn create_pipeline(
    device: &rhi::backend::Device,
    targets: rhi::backend::PipelineTargets,
    layout: rhi::backend::PipelineLayout,
) -> rhi::backend::RenderPipeline {
    rhi::backend::create_render_pipeline(
        device,
        targets,
        layout,
        &rhi::RenderPipelineDesc {
            // Seen from both sides, over what is behind it, pulled a little
            // toward the camera off the blocks it runs through.
            depth: Some(rhi::DepthState {
                bias: Some(rhi::DepthBias {
                    constant: -3,
                    slope: -3.0,
                }),
                ..rhi::DepthState::READ_ONLY
            }),
            blend: Some(rhi::BlendState {
                color: rhi::BlendComponent {
                    src: rhi::BlendFactor::SrcAlpha,
                    dst: rhi::BlendFactor::One,
                },
                alpha: rhi::BlendComponent::REPLACE,
            }),
            ..rhi::RenderPipelineDesc::new(
                "world border",
                shader::include_shader!("block_overlay.vert"),
                shader::include_shader!("world_border.frag"),
                &[rhi::vertex_buffer!(BorderVertex, [Float32x3, Float32x2])],
            )
        },
    )
}
