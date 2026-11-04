//! Vanilla `LevelRenderer.renderHitOutline`: the targeted block's outline
//! shape, its edges drawn as `RenderType.lines` (`hit_outline.vert`) in
//! translucent black.

use glam::DVec3;

use crate::physics::block_shape::LocalBox;
use crate::renderer::camera::CameraUniform;
use crate::renderer::{rhi, shader, util};

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct LineVertex {
    position: [f32; 3],
    direction: [f32; 3],
    side: f32,
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct Push {
    screen_size: [f32; 2],
    line_width: f32,
    _pad: f32,
    color: [f32; 4],
}

/// `ARGB.color(102, 0, 0, 0)`.
const OUTLINE_COLOR: [f32; 4] = [0.0, 0.0, 0.0, 0.4];
/// A fishing line's colour (`FishingHookRenderer.stringVertex`'s -16777216).
const LINE_COLOR: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
/// Far more than any outline shape's edges, and a few fishing lines, need.
const MAX_VERTICES: usize = 6 * 512;

pub struct HitOutlinePipeline {
    pipeline: rhi::backend::RenderPipeline,
    pipeline_layout: rhi::backend::PipelineLayout,
    camera_layout: rhi::backend::BindGroupLayout,
    camera_sets: Vec<rhi::backend::BindGroup>,
    camera_buffers: Vec<rhi::backend::Buffer>,
    vertex_buffers: Vec<rhi::backend::Buffer>,
    /// The outlined shape's edges (block-local) and where the block sits.
    target: Option<(DVec3, Vec<(DVec3, DVec3)>)>,
    /// Fishing lines' segments, in world space.
    lines: Vec<(DVec3, DVec3)>,
    vertices: Vec<LineVertex>,
}

impl HitOutlinePipeline {
    pub fn new(device: &rhi::backend::Device, targets: rhi::backend::PipelineTargets) -> Self {
        let camera_layout = rhi::backend::create_bind_group_layout(device, util::VERTEX_UNIFORM);
        let pipeline_layout = rhi::backend::create_pipeline_layout(
            device,
            &[camera_layout],
            Some(rhi::PushConstants {
                visibility: rhi::ShaderStages::VERTEX,
                size: size_of::<Push>() as u32,
            }),
        );
        let pipeline = create_pipeline(device, targets, pipeline_layout);
        let (camera_sets, camera_buffers) = util::create_uniform_sets(
            device,
            camera_layout,
            size_of::<CameraUniform>() as u64,
            "hit_outline_cam",
        );
        let vertex_buffers = util::create_per_frame_host_buffers(
            device,
            (MAX_VERTICES * size_of::<LineVertex>()) as u64,
            rhi::BufferUsages::VERTEX,
            "hit_outline_verts",
        );
        Self {
            pipeline,
            pipeline_layout,
            camera_layout,
            camera_sets,
            camera_buffers,
            vertex_buffers,
            target: None,
            lines: Vec::new(),
            vertices: Vec::new(),
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

    /// The outline to draw: `boxes` (block-local) at world position
    /// `origin`; `None` draws nothing.
    pub fn set_target(&mut self, target: Option<(DVec3, &[LocalBox])>) {
        self.target = target.map(|(origin, boxes)| (origin, shape_edges(boxes)));
    }

    /// This frame's fishing lines, as world-space segments.
    pub fn set_lines(&mut self, lines: Vec<(DVec3, DVec3)>) {
        self.lines = lines;
    }

    /// This frame's quads, relative to the render anchor: the outline's,
    /// then the fishing lines'. Returns how many are the outline's.
    fn build(&mut self, anchor: DVec3) -> usize {
        self.vertices.clear();
        if let Some((origin, edges)) = &self.target {
            let base = *origin - anchor;
            for &(from, to) in edges {
                push_segment(&mut self.vertices, base + from, base + to);
            }
        }
        let outline = self.vertices.len();
        for &(from, to) in &self.lines {
            push_segment(&mut self.vertices, from - anchor, to - anchor);
        }
        self.vertices.truncate(MAX_VERTICES);
        outline.min(self.vertices.len())
    }

    /// `screen` is the framebuffer size in pixels.
    pub fn draw(
        &mut self,
        pass: &mut rhi::backend::RenderPassEncoder,
        frame: usize,
        anchor: DVec3,
        screen: [f32; 2],
    ) {
        let outline = self.build(anchor);
        if self.vertices.is_empty() {
            return;
        }
        self.vertex_buffers[frame].write(bytemuck::cast_slice(&self.vertices));
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, self.camera_sets[frame]);
        pass.set_vertex_buffer(0, self.vertex_buffers[frame].handle());
        // Both at `Window.getAppropriateLineWidth`.
        let line_width = (screen[0] / 1920.0 * 2.5).max(2.5);
        for (color, range) in [
            (OUTLINE_COLOR, 0..outline),
            (LINE_COLOR, outline..self.vertices.len()),
        ] {
            if range.is_empty() {
                continue;
            }
            let push = Push {
                screen_size: screen,
                line_width,
                _pad: 0.0,
                color,
            };
            pass.set_push_constants(rhi::ShaderStages::VERTEX, 0, bytemuck::bytes_of(&push));
            pass.draw(range.len() as u32, 1, range.start as u32, 0);
        }
    }

    pub fn destroy(&mut self, device: &rhi::backend::Device) {
        util::destroy_buffers(device, &mut self.vertex_buffers);
        util::destroy_buffers(device, &mut self.camera_buffers);
        util::destroy_bind_groups(device, self.camera_sets.drain(..));
        self.pipeline.destroy(device);
        self.pipeline_layout.destroy(device);
        self.camera_layout.destroy(device);
    }
}

/// One segment's two triangles, each end's two sides.
fn push_segment(vertices: &mut Vec<LineVertex>, from: DVec3, to: DVec3) {
    let start = from.as_vec3();
    let direction = (to - from).as_vec3();
    let end = start + direction;
    for (point, side) in [
        (start, 1.0),
        (start, -1.0),
        (end, 1.0),
        (start, -1.0),
        (end, -1.0),
        (end, 1.0),
    ] {
        vertices.push(LineVertex {
            position: point.to_array(),
            direction: direction.to_array(),
            side,
        });
    }
}

/// Vanilla `VoxelShape.forAllEdges`: the shape's outer edges, joined where
/// they run on. On the grid of every box face, a line between four cells is
/// an edge when one or three of them are filled, or two diagonally opposite.
pub(crate) fn shape_edges(boxes: &[LocalBox]) -> Vec<(DVec3, DVec3)> {
    let coords = |axis: usize| {
        let mut values: Vec<f64> = boxes.iter().flat_map(|b| [b[axis], b[axis + 3]]).collect();
        values.sort_by(f64::total_cmp);
        values.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
        values
    };
    let grid = [coords(0), coords(1), coords(2)];
    let size: [usize; 3] = std::array::from_fn(|a| grid[a].len().saturating_sub(1));
    let filled = |cell: [i64; 3]| -> bool {
        if (0..3).any(|a| cell[a] < 0 || cell[a] as usize >= size[a]) {
            return false;
        }
        let center: [f64; 3] = std::array::from_fn(|a| {
            let i = cell[a] as usize;
            (grid[a][i] + grid[a][i + 1]) * 0.5
        });
        boxes
            .iter()
            .any(|b| (0..3).all(|a| b[a] < center[a] && center[a] < b[a + 3]))
    };
    let mut edges = Vec::new();
    for axis in 0..3 {
        let (u, v) = ((axis + 1) % 3, (axis + 2) % 3);
        for j in 0..grid[u].len() {
            for k in 0..grid[v].len() {
                let mut run: Option<usize> = None;
                for i in 0..=size[axis] {
                    let is_edge = i < size[axis] && {
                        let cell = |dj: i64, dk: i64| {
                            let mut c = [0i64; 3];
                            c[axis] = i as i64;
                            c[u] = j as i64 + dj;
                            c[v] = k as i64 + dk;
                            filled(c)
                        };
                        let (a, b, c, d) = (cell(-1, -1), cell(0, -1), cell(-1, 0), cell(0, 0));
                        let count = [a, b, c, d].iter().filter(|f| **f).count();
                        count == 1 || count == 3 || (count == 2 && a == d)
                    };
                    match (is_edge, run) {
                        (true, None) => run = Some(i),
                        (false, Some(start)) => {
                            let point = |at: usize| {
                                let mut p = [0.0; 3];
                                p[axis] = grid[axis][at];
                                p[u] = grid[u][j];
                                p[v] = grid[v][k];
                                DVec3::from_array(p)
                            };
                            edges.push((point(start), point(i)));
                            run = None;
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    edges
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
            depth: Some(rhi::DepthState::READ_ONLY),
            blend: Some(rhi::BlendState::OVER_REPLACE_ALPHA),
            ..rhi::RenderPipelineDesc::new(
                "hit outline",
                shader::include_shader!("hit_outline.vert"),
                shader::include_shader!("hit_outline.frag"),
                &[rhi::vertex_buffer!(
                    LineVertex,
                    [Float32x3, Float32x3, Float32]
                )],
            )
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cube_has_twelve_edges() {
        assert_eq!(shape_edges(&[[0.0, 0.0, 0.0, 1.0, 1.0, 1.0]]).len(), 12);
    }

    /// A stair's two boxes outline as one shape: no seam where they meet,
    /// and the step's inner corner drawn once.
    #[test]
    fn a_stair_outlines_without_seams() {
        let stair = [
            [0.0, 0.0, 0.0, 1.0, 0.5, 1.0],
            [0.0, 0.5, 0.5, 1.0, 1.0, 1.0],
        ];
        let edges = shape_edges(&stair);
        // An L prism: 6 corners on each end face, joined by 6 lengthwise
        // edges.
        assert_eq!(edges.len(), 18, "{edges:?}");
        // The bottom slab's top-back edge is inside the step, so it's gone.
        let hidden = (DVec3::new(0.0, 0.5, 1.0), DVec3::new(1.0, 0.5, 1.0));
        assert!(!edges.contains(&hidden));
    }

    #[test]
    fn no_shape_has_no_edges() {
        assert!(shape_edges(&[]).is_empty());
    }
}
