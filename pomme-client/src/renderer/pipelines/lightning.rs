//! Lightning bolts (vanilla `LightningBoltRenderer` in the `lightning`
//! render type): plain coloured quads that add their light to what's
//! behind them.

use glam::DVec3;

use crate::entity::lightning::{BOLT_COLOR, bolt_quads};
use crate::renderer::camera::CameraUniform;
use crate::renderer::{rhi, shader, util};

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct BoltVertex {
    position: [f32; 3],
    color: [f32; 4],
}

/// The bolts drawn at once; more than a storm strikes in a frame.
const MAX_BOLTS: usize = 8;
/// `bolt_quads`' count, two triangles each.
const VERTICES_PER_BOLT: usize = 224 * 6;

pub struct LightningPipeline {
    pipeline: rhi::backend::RenderPipeline,
    pipeline_layout: rhi::backend::PipelineLayout,
    camera_layout: rhi::backend::BindGroupLayout,
    camera_sets: Vec<rhi::backend::BindGroup>,
    camera_buffers: Vec<rhi::backend::Buffer>,
    vertex_buffers: Vec<rhi::backend::Buffer>,
}

impl LightningPipeline {
    pub fn new(device: &rhi::backend::Device, targets: rhi::backend::PipelineTargets) -> Self {
        let camera_layout = rhi::backend::create_bind_group_layout(device, util::VERTEX_UNIFORM);
        let pipeline_layout = rhi::backend::create_pipeline_layout(device, &[camera_layout], None);
        let pipeline = create_pipeline(device, targets, pipeline_layout);
        let (camera_sets, camera_buffers) = util::create_uniform_sets(
            device,
            camera_layout,
            size_of::<CameraUniform>() as u64,
            "lightning_cam",
        );
        let vertex_buffers = util::create_per_frame_host_buffers(
            device,
            (MAX_BOLTS * VERTICES_PER_BOLT * size_of::<BoltVertex>()) as u64,
            rhi::BufferUsages::VERTEX,
            "lightning_verts",
        );
        Self {
            pipeline,
            pipeline_layout,
            camera_layout,
            camera_sets,
            camera_buffers,
            vertex_buffers,
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

    /// Draws `bolts` (where each strikes, and its shape's seed) as seen from
    /// `eye`: positions are taken eye-relative in f64, as the chunk borders'.
    pub fn draw(
        &mut self,
        pass: &mut rhi::backend::RenderPassEncoder,
        frame: usize,
        bolts: &[(DVec3, u64)],
        eye: DVec3,
    ) {
        if bolts.is_empty() {
            return;
        }
        let mut vertices = Vec::with_capacity(bolts.len().min(MAX_BOLTS) * VERTICES_PER_BOLT);
        for &(position, seed) in bolts.iter().take(MAX_BOLTS) {
            let origin = (position - eye).as_vec3();
            for quad in bolt_quads(seed) {
                vertices.extend([0, 1, 2, 2, 3, 0].map(|i| BoltVertex {
                    position: (origin + glam::Vec3::from_array(quad[i])).to_array(),
                    color: BOLT_COLOR,
                }));
            }
        }
        self.vertex_buffers[frame].write(bytemuck::cast_slice(&vertices));
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, self.camera_sets[frame]);
        pass.set_vertex_buffer(0, self.vertex_buffers[frame].handle());
        pass.draw(vertices.len() as u32, 1, 0, 0);
    }

    pub fn destroy(&mut self, device: &rhi::backend::Device) {
        util::destroy_buffers(device, &mut self.vertex_buffers);
        util::destroy_buffers(device, &mut self.camera_buffers);
        self.pipeline.destroy(device);
        self.pipeline_layout.destroy(device);
        util::destroy_bind_groups(device, self.camera_sets.drain(..));
        self.camera_layout.destroy(device);
    }
}

/// Vanilla `RenderPipelines.LIGHTNING`: the glow adds to the scene
/// (`BlendFunction.LIGHTNING`), back faces culled. Its layers are nested
/// tubes that must all add up, so nothing is written to the depth buffer
/// for one to hide another behind.
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
            cull_mode: rhi::CullMode::Back,
            front_face: rhi::FrontFace::Ccw,
            depth: Some(rhi::DepthState::READ_ONLY),
            blend: Some(rhi::BlendState::OVERLAY),
            ..rhi::RenderPipelineDesc::new(
                "lightning",
                shader::include_shader!("chunk_border.vert"),
                shader::include_shader!("chunk_border.frag"),
                &[rhi::vertex_buffer!(BoltVertex, [Float32x3, Float32x4])],
            )
        },
    )
}
