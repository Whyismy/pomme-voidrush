use std::path::Path;

use glam::camera::rh::proj;
use glam::{Mat3, Mat4, Vec3};

use crate::assets::AssetIndex;
use crate::renderer::camera::CameraUniform;
use crate::renderer::chunk::atlas::TextureAtlas;
use crate::renderer::chunk::block_lighter::UNLIT_LANE;
use crate::renderer::pipelines::item_entity::{self, ItemEntityPipeline, ItemPipelineShared};
use crate::renderer::pipelines::lightmap::Lightmap;
use crate::renderer::rhi;
use crate::world::block::model::DisplayTransform;

pub struct GuiItemPipeline {
    pipeline: rhi::backend::RenderPipeline,
    /// Vanilla `GLINT`, which a foil item's icon is drawn again with.
    glint: rhi::backend::RenderPipeline,
    shared: ItemPipelineShared,
    atlas_px: u32,
}

impl GuiItemPipeline {
    pub fn new(
        device: &rhi::backend::Device,
        atlas_targets: rhi::backend::PipelineTargets,
        atlas_px: u32,
        atlas: &TextureAtlas,
        lightmap: &Lightmap,
        jar_assets_dir: &Path,
        asset_index: &Option<AssetIndex>,
    ) -> Self {
        let shared = ItemPipelineShared::new(
            device,
            atlas,
            lightmap,
            jar_assets_dir,
            asset_index,
            "gui_item",
        );
        let pipeline = item_entity::create_pipeline_with_front_face(
            device,
            atlas_targets,
            shared.pipeline_layout,
            rhi::FrontFace::Cw,
        );
        Self {
            pipeline,
            glint: item_entity::create_glint_pipeline(
                device,
                atlas_targets,
                shared.pipeline_layout,
            ),
            shared,
            atlas_px,
        }
    }

    pub fn set_atlas_px(&mut self, atlas_px: u32) {
        self.atlas_px = atlas_px;
    }

    pub fn recreate_pipeline(
        &mut self,
        device: &rhi::backend::Device,
        atlas_targets: rhi::backend::PipelineTargets,
    ) {
        self.pipeline.destroy(device);
        self.pipeline = item_entity::create_pipeline_with_front_face(
            device,
            atlas_targets,
            self.shared.pipeline_layout,
            rhi::FrontFace::Cw,
        );
        self.glint.destroy(device);
        self.glint =
            item_entity::create_glint_pipeline(device, atlas_targets, self.shared.pipeline_layout);
    }

    /// Writes `frame`'s camera buffer for the atlas's size, with the
    /// enchantment glint's texture transform as it now stands.
    fn write_atlas_ortho(&mut self, frame: usize, glint: [f32; 4]) {
        let atlas_px = self.atlas_px as f32;
        // Bottom/top swapped to Y-invert the projection (matches vanilla's
        // `invertY=true`).
        let view_proj =
            proj::directx::orthographic(0.0, atlas_px, atlas_px, 0.0, -10000.0, 10000.0);
        self.shared.update_camera(
            frame,
            &CameraUniform::with_view_proj(view_proj).with_glint(glint, 0.5),
        );
    }

    pub fn rebind_atlas(&mut self, device: &rhi::backend::Device, atlas: &TextureAtlas) {
        self.shared.rebind_atlas(device, atlas);
    }

    /// Replaces the glint's texture, with a resource pack's own.
    pub fn set_glint(
        &mut self,
        device: &rhi::backend::Device,
        pixels: &[u8],
        width: u32,
        height: u32,
    ) {
        self.shared.set_glint(device, pixels, width, height);
    }

    /// Binds for `items` bakes with `colors` tints between them; `glint` is
    /// the enchantment glint's texture transform for the foil ones.
    pub fn bind_for_bake_pass(
        &mut self,
        device: &rhi::backend::Device,
        pass: &mut rhi::backend::RenderPassEncoder,
        frame: usize,
        items: usize,
        colors: usize,
        glint: [f32; 4],
    ) {
        self.shared.begin_frame(device, frame, items, colors);
        self.write_atlas_ortho(frame, glint);
        self.shared.bind(pass, frame, self.pipeline);
    }

    /// Records into a pass [`Self::bind_for_bake_pass`] has bound; a `foil`
    /// item is drawn again with the enchantment glint.
    #[allow(clippy::too_many_arguments)]
    pub fn bake_to_slot(
        &mut self,
        pass: &mut rhi::backend::RenderPassEncoder,
        frame: usize,
        item_entity: &ItemEntityPipeline,
        slot_x_px: u32,
        slot_y_px: u32,
        slot_size_px: u32,
        item_name: &str,
        display: DisplayTransform,
        item_tints: &[u32],
        foil: bool,
    ) {
        let Some((buffer, vertex_count)) = item_entity.mesh_handle(item_name) else {
            return;
        };

        let model = slot_model_matrix(
            slot_x_px as f32,
            slot_y_px as f32,
            slot_size_px as f32,
            slot_size_px as f32,
            display,
        );

        let instance = self.shared.push_item(
            frame,
            &model,
            Mat3::IDENTITY,
            UNLIT_LANE,
            item_tints,
            item_entity::glint_flags(item_name, foil),
        );
        pass.set_vertex_buffer(0, buffer);
        pass.draw(vertex_count, 1, 0, instance);
        if foil {
            self.shared.bind_glint(pass, frame, self.glint);
            pass.set_vertex_buffer(0, buffer);
            pass.draw(vertex_count, 1, 0, instance);
            self.shared.bind(pass, frame, self.pipeline);
        }
    }

    /// [`Self::bake_to_slot`] sampling `texture` (a texture group in the
    /// atlas's layout) in place of the atlas.
    #[allow(clippy::too_many_arguments)]
    pub fn bake_to_slot_with_texture(
        &mut self,
        pass: &mut rhi::backend::RenderPassEncoder,
        frame: usize,
        item_entity: &ItemEntityPipeline,
        slot_x_px: u32,
        slot_y_px: u32,
        slot_size_px: u32,
        item_name: &str,
        display: DisplayTransform,
        texture: rhi::backend::BindGroup,
    ) {
        pass.set_bind_group(1, texture);
        self.bake_to_slot(
            pass,
            frame,
            item_entity,
            slot_x_px,
            slot_y_px,
            slot_size_px,
            item_name,
            display,
            &[],
            false,
        );
        pass.set_bind_group(1, self.shared.atlas_set());
    }

    pub fn destroy(&mut self, device: &rhi::backend::Device) {
        self.pipeline.destroy(device);
        self.glint.destroy(device);
        self.shared.destroy(device);
    }
}

fn slot_model_matrix(x: f32, y: f32, w: f32, h: f32, display: DisplayTransform) -> Mat4 {
    let depth_scale = w.max(h);
    // Vertices are pre-centered to `[-0.5, +0.5]` by `build_item_mesh`, so no
    // T(-0.5) here.
    Mat4::from_translation(Vec3::new(x + w * 0.5, y + h * 0.5, 0.0))
        * Mat4::from_scale(Vec3::new(w, -h, depth_scale))
        * display.to_matrix()
}
