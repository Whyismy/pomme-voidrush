//! Vanilla `TheEndPortalRenderer`: end portal and end gateway blocks, drawn
//! as faces of `RenderType.endPortal` (`end_portal.frag`), whose stars are
//! projected from the screen rather than mapped onto the faces.
// TODO: the gateway's beam while it spawns or cools down.

use std::path::Path;

use crate::assets::{AssetIndex, resolve_asset_path};
use crate::position::BlockPos;
use crate::renderer::camera::CameraUniform;
use crate::renderer::{rhi, shader, util};

/// A portal block to draw and the faces it shows, a bit per `Direction`
/// (down, up, north, south, west, east).
#[derive(Clone, Copy, Debug)]
pub struct EndPortalBlock {
    pub pos: BlockPos,
    pub gateway: bool,
    pub faces: u8,
}

impl EndPortalBlock {
    /// `TheEndPortalBlockEntity.shouldRenderFace`: the end portal shows only
    /// its top and bottom.
    pub const PORTAL_FACES: u8 = 0b11;
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct PortalVertex {
    position: [f32; 3],
    layers: f32,
}

const MAX_VERTICES: usize = 6 * 6 * 4096;

pub struct EndPortalPipeline {
    pipeline: rhi::backend::RenderPipeline,
    pipeline_layout: rhi::backend::PipelineLayout,
    camera_layout: rhi::backend::BindGroupLayout,
    texture_layout: rhi::backend::BindGroupLayout,
    camera_sets: Vec<rhi::backend::BindGroup>,
    camera_buffers: Vec<rhi::backend::Buffer>,
    vertex_buffers: Vec<rhi::backend::Buffer>,
    sampler: rhi::backend::Sampler,
    textures: [rhi::backend::Texture; 2],
    texture_sets: [rhi::backend::BindGroup; 2],
    blocks: Vec<EndPortalBlock>,
    /// Vanilla `GameTime`.
    game_time: f32,
}

impl EndPortalPipeline {
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
            &[camera_layout, texture_layout, texture_layout],
            Some(rhi::PushConstants {
                visibility: rhi::ShaderStages::FRAGMENT,
                size: 4,
            }),
        );
        let pipeline = create_pipeline(device, targets, pipeline_layout);
        let (camera_sets, camera_buffers) = util::create_uniform_sets(
            device,
            camera_layout,
            size_of::<CameraUniform>() as u64,
            "end_portal_cam",
        );
        let vertex_buffers = util::create_per_frame_host_buffers(
            device,
            (MAX_VERTICES * size_of::<PortalVertex>()) as u64,
            rhi::BufferUsages::VERTEX,
            "end_portal_verts",
        );
        // `TheEndPortalRenderer.END_SKY_LOCATION` and `END_PORTAL_LOCATION`,
        // tiled across the screen.
        let sampler = rhi::backend::create_sampler(device, &rhi::SamplerDesc::NEAREST_REPEAT);
        // 26.1 moved the portal's texture into `entity/end_portal/`.
        let portal = [
            "minecraft/textures/entity/end_portal/end_portal.png",
            "minecraft/textures/entity/end_portal.png",
        ]
        .into_iter()
        .find(|key| resolve_asset_path(jar_assets_dir, asset_index, key).exists())
        .unwrap_or("minecraft/textures/entity/end_portal.png");
        let textures = ["minecraft/textures/environment/end_sky.png", portal]
            .map(|key| util::load_asset_texture(device, jar_assets_dir, asset_index, key));
        let texture_sets = [0, 1].map(|i| {
            util::create_texture_group(device, texture_layout, textures[i].handle(), sampler)
        });
        Self {
            pipeline,
            pipeline_layout,
            camera_layout,
            texture_layout,
            camera_sets,
            camera_buffers,
            vertex_buffers,
            sampler,
            textures,
            texture_sets,
            blocks: Vec::new(),
            game_time: 0.0,
        }
    }

    pub fn recreate_pipeline(
        &mut self,
        device: &rhi::backend::Device,
        targets: rhi::backend::PipelineTargets,
    ) {
        self.pipeline.destroy(device);
        self.pipeline = create_pipeline(device, targets, self.pipeline_layout);
    }

    pub fn update_camera(&mut self, frame: usize, uniform: &CameraUniform) {
        self.camera_buffers[frame].write(bytemuck::bytes_of(uniform));
    }

    /// This frame's portal blocks, and the game clock (`gameTime % 24000`
    /// plus the partial tick) their stars scroll by.
    pub fn set_blocks(&mut self, blocks: Vec<EndPortalBlock>, clock_ticks: f32) {
        self.blocks = blocks;
        self.game_time = clock_ticks / 24000.0;
    }

    pub fn draw(
        &mut self,
        pass: &mut rhi::backend::RenderPassEncoder,
        frame: usize,
        anchor: glam::DVec3,
    ) {
        if self.blocks.is_empty() {
            return;
        }
        let mut vertices = Vec::new();
        for block in &self.blocks {
            let origin = (glam::DVec3::new(
                f64::from(block.pos.x),
                f64::from(block.pos.y),
                f64::from(block.pos.z),
            ) - anchor)
                .as_vec3();
            push_cube(&mut vertices, origin, block);
        }
        vertices.truncate(MAX_VERTICES);
        if vertices.is_empty() {
            return;
        }
        self.vertex_buffers[frame].write(bytemuck::cast_slice(&vertices));
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, self.camera_sets[frame]);
        pass.set_bind_group(1, self.texture_sets[0]);
        pass.set_bind_group(2, self.texture_sets[1]);
        pass.set_push_constants(
            rhi::ShaderStages::FRAGMENT,
            0,
            bytemuck::bytes_of(&self.game_time),
        );
        pass.set_vertex_buffer(0, self.vertex_buffers[frame].handle());
        pass.draw(vertices.len() as u32, 1, 0, 0);
    }

    pub fn destroy(&mut self, device: &rhi::backend::Device) {
        util::destroy_buffers(device, &mut self.vertex_buffers);
        util::destroy_buffers(device, &mut self.camera_buffers);
        util::destroy_bind_groups(device, self.camera_sets.drain(..));
        util::destroy_bind_groups(device, self.texture_sets);
        for texture in &mut self.textures {
            std::mem::take(texture).destroy(device);
        }
        self.sampler.destroy(device);
        self.pipeline.destroy(device);
        self.pipeline_layout.destroy(device);
        self.camera_layout.destroy(device);
        self.texture_layout.destroy(device);
    }
}

/// `TheEndPortalRenderer.renderCube`: each shown face's four corners, the top
/// at `getOffsetUp` and the bottom at `getOffsetDown`.
fn push_cube(out: &mut Vec<PortalVertex>, origin: glam::Vec3, block: &EndPortalBlock) {
    let (down, up, layers) = if block.gateway {
        (0.0, 1.0, 16.0)
    } else {
        (0.375, 0.75, 15.0)
    };
    // (x0, x1, y0, y1, z0, z1, z2, z3) per `Direction`, as vanilla's calls.
    let faces: [[f32; 8]; 6] = [
        [0.0, 1.0, down, down, 0.0, 0.0, 1.0, 1.0],
        [0.0, 1.0, up, up, 1.0, 1.0, 0.0, 0.0],
        [0.0, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 1.0, 1.0, 1.0, 1.0, 1.0],
        [0.0, 0.0, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0],
        [1.0, 1.0, 1.0, 0.0, 0.0, 1.0, 1.0, 0.0],
    ];
    for (direction, [x0, x1, y0, y1, z0, z1, z2, z3]) in faces.into_iter().enumerate() {
        if block.faces & (1 << direction) == 0 {
            continue;
        }
        let corners = [[x0, y0, z0], [x1, y0, z1], [x1, y1, z2], [x0, y1, z3]];
        for i in [0, 1, 2, 0, 2, 3] {
            out.push(PortalVertex {
                position: (origin + glam::Vec3::from_array(corners[i])).to_array(),
                layers,
            });
        }
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
            depth: Some(rhi::DepthState::WRITE),
            ..rhi::RenderPipelineDesc::new(
                "end portal",
                shader::include_shader!("end_portal.vert"),
                shader::include_shader!("end_portal.frag"),
                &[rhi::vertex_buffer!(PortalVertex, [Float32x3, Float32])],
            )
        },
    )
}
