//! Maps: vanilla `MapRenderer` and `MapTextureManager`. Each map the game
//! shows has its picture in a texture of its own; the paper behind it and
//! the markers on it are textures here too. They're drawn as quads through
//! the item shaders by whoever shows a map (the first-person hands).

use std::collections::HashMap;
use std::path::Path;

use glam::{Mat4, Vec3};
use pomme_protocol::map::MAP_SIZE;

use super::item_entity;
use crate::assets::{AssetIndex, resolve_asset_path};
use crate::renderer::chunk::block_lighter::LightLevels;
use crate::renderer::{MAX_FRAMES_IN_FLIGHT, rhi, util};
use crate::world::map::{SavedMap, decoration_textures};

/// One quad of a drawn map: six vertices of [`MapPipeline::vertex_buffer`]
/// from `first_vertex`, in `model`'s space with `texture` as set 1.
pub struct MapQuad {
    pub model: Mat4,
    pub first_vertex: u32,
    pub texture: rhi::backend::BindGroup,
}

/// A map to draw on an item frame: where its 128-unit space sits in the
/// world (anchor-relative), and its light.
pub struct FramedMap<'a> {
    pub id: i32,
    /// `None` until the server has sent the map.
    pub map: Option<&'a SavedMap>,
    pub pose: Mat4,
    pub light: LightLevels,
}

/// A prepared [`FramedMap`].
pub struct WorldMap {
    pub quads: Vec<MapQuad>,
    pub pose: Mat4,
    pub light: LightLevels,
}

/// A map's picture on the GPU, as of `revision`.
struct MapTexture {
    revision: u64,
    texture: rhi::backend::Texture,
    set: rhi::backend::BindGroup,
}

/// A marker sprite's cell in the sheet has a clear pixel all round, so a
/// turned marker's edge never picks up its neighbour.
const GUTTER: u32 = 1;

pub struct MapPipeline {
    layout: rhi::backend::BindGroupLayout,
    sampler: rhi::backend::Sampler,
    /// The paper behind a map with no data yet, then behind one with.
    backgrounds: [(rhi::backend::Texture, rhi::backend::BindGroup); 2],
    decorations: (rhi::backend::Texture, rhi::backend::BindGroup),
    /// The paper's quad, the picture's, then one per marker type.
    vertex_buffer: rhi::backend::Buffer,
    maps: HashMap<i32, MapTexture>,
    /// Pictures replaced while a frame in flight may still read them, and
    /// the frames left until they can go.
    retired: Vec<(usize, MapTexture)>,
}

const VERTICES_PER_QUAD: u32 = 6;
const BACKGROUND_QUAD: u32 = 0;
const PICTURE_QUAD: u32 = 1;
const FIRST_DECORATION_QUAD: u32 = 2;

impl MapPipeline {
    pub fn new(
        device: &rhi::backend::Device,
        jar_assets_dir: &Path,
        asset_index: &Option<AssetIndex>,
    ) -> Self {
        let layout = rhi::backend::create_bind_group_layout(device, util::FRAGMENT_TEXTURE);
        let sampler = rhi::backend::create_sampler(device, &rhi::SamplerDesc::NEAREST);
        let backgrounds = ["map_background", "map_background_checkerboard"].map(|name| {
            let key = format!("minecraft/textures/map/{name}.png");
            let texture = util::load_asset_texture(device, jar_assets_dir, asset_index, &key);
            let set = util::create_texture_group(device, layout, texture.handle(), sampler);
            (texture, set)
        });

        let sprites: Vec<_> = decoration_textures()
            .map(|name| {
                let key = format!("minecraft/textures/map/decorations/{name}.png");
                util::load_png(&resolve_asset_path(jar_assets_dir, asset_index, &key))
            })
            .collect();
        let (sheet, sheet_size, cells) = decoration_sheet(&sprites);
        let texture = util::create_sampled_texture(
            device,
            "map_decorations",
            &sheet,
            sheet_size.0,
            sheet_size.1,
        );
        let set = util::create_texture_group(device, layout, texture.handle(), sampler);

        // Vanilla `ItemInHandRenderer.renderMap`'s paper, `MapRenderer`'s
        // picture just in front of it, and a marker's quad.
        let size = MAP_SIZE as f32;
        let whole = |min: f32, max: f32, z: f32| {
            [
                ([min, max, z], [0.0, 1.0]),
                ([max, max, z], [1.0, 1.0]),
                ([max, min, z], [1.0, 0.0]),
                ([min, min, z], [0.0, 0.0]),
            ]
        };
        let mut quads = vec![whole(-7.0, size + 7.0, 0.0), whole(0.0, size, -0.01)];
        quads.extend(cells.iter().map(|&[u0, v0, u1, v1]| {
            [
                ([-1.0, 1.0, 0.0], [u0, v0]),
                ([1.0, 1.0, 0.0], [u1, v0]),
                ([1.0, -1.0, 0.0], [u1, v1]),
                ([-1.0, -1.0, 0.0], [u0, v1]),
            ]
        }));
        let vertex_buffer = item_entity::create_quad_buffer(device, "map_quads", &quads);

        Self {
            layout,
            sampler,
            backgrounds,
            decorations: (texture, set),
            vertex_buffer,
            maps: HashMap::new(),
            retired: Vec::new(),
        }
    }

    pub fn vertex_buffer(&self) -> rhi::backend::BufferHandle {
        self.vertex_buffer.handle()
    }

    /// Once a frame, before any map is prepared: lets go of the pictures no
    /// frame can still be reading.
    pub fn begin_frame(&mut self, device: &rhi::backend::Device) {
        self.retired.retain_mut(|(frames, map)| {
            if *frames > 0 {
                *frames -= 1;
                return true;
            }
            std::mem::take(&mut map.texture).destroy(device);
            map.set.destroy(device);
            false
        });
    }

    /// A held or framed map's quads in the map's own space (128 units a
    /// side, y down, facing -z), back to front: vanilla `renderMap`'s paper
    /// for a held one, then `MapRenderer.render`. `map` is `None` until the
    /// server has sent the map. Call outside a render pass: a changed
    /// picture is uploaded here.
    pub fn prepare(
        &mut self,
        device: &rhi::backend::Device,
        id: i32,
        map: Option<&SavedMap>,
        in_item_frame: bool,
    ) -> Vec<MapQuad> {
        let mut quads = Vec::new();
        if !in_item_frame {
            let background = &self.backgrounds[usize::from(map.is_some())];
            quads.push(MapQuad {
                model: Mat4::IDENTITY,
                first_vertex: BACKGROUND_QUAD * VERTICES_PER_QUAD,
                texture: background.1,
            });
        }
        let Some(map) = map else {
            return quads;
        };
        quads.push(MapQuad {
            model: Mat4::IDENTITY,
            first_vertex: PICTURE_QUAD * VERTICES_PER_QUAD,
            texture: self.picture(device, id, map),
        });
        let shown = map
            .decorations
            .iter()
            .filter(|decoration| !in_item_frame || decoration.shows_on_item_frame());
        for (index, decoration) in shown.enumerate() {
            // Each marker sits a little in front of the one before.
            let depth = -0.02 + 3.0 * (index as f32 * -0.001);
            let model = Mat4::from_translation(Vec3::new(
                f32::from(decoration.x) / 2.0 + 64.0,
                f32::from(decoration.y) / 2.0 + 64.0,
                depth,
            )) * Mat4::from_rotation_z(
                (f32::from(decoration.rot) * 360.0 / 16.0).to_radians(),
            ) * Mat4::from_scale(Vec3::new(4.0, 4.0, 3.0))
                * Mat4::from_translation(Vec3::new(-0.125, 0.125, 0.0));
            quads.push(MapQuad {
                model,
                first_vertex: (FIRST_DECORATION_QUAD + decoration.kind as u32) * VERTICES_PER_QUAD,
                texture: self.decorations.1,
            });
        }
        // A named marker's label is drawn by the caller, as world text.
        quads
    }

    /// The map's picture, uploaded again when it has changed.
    fn picture(
        &mut self,
        device: &rhi::backend::Device,
        id: i32,
        map: &SavedMap,
    ) -> rhi::backend::BindGroup {
        if let Some(current) = self.maps.get(&id)
            && current.revision == map.revision
        {
            return current.set;
        }
        let size = MAP_SIZE as u32;
        let texture = util::create_sampled_texture(device, "map", &map.rgba(), size, size);
        let set = util::create_texture_group(device, self.layout, texture.handle(), self.sampler);
        let fresh = MapTexture {
            revision: map.revision,
            texture,
            set,
        };
        if let Some(old) = self.maps.insert(id, fresh) {
            self.retired.push((MAX_FRAMES_IN_FLIGHT, old));
        }
        set
    }

    pub fn destroy(&mut self, device: &rhi::backend::Device) {
        let maps = self.maps.drain().map(|(_, map)| map);
        for mut map in maps.chain(self.retired.drain(..).map(|(_, map)| map)) {
            std::mem::take(&mut map.texture).destroy(device);
            map.set.destroy(device);
        }
        for (texture, set) in self
            .backgrounds
            .iter_mut()
            .chain(std::iter::once(&mut self.decorations))
        {
            std::mem::take(texture).destroy(device);
            set.destroy(device);
        }
        std::mem::take(&mut self.vertex_buffer).destroy(device);
        self.sampler.destroy(device);
        self.layout.destroy(device);
    }
}

/// The marker sprites side by side in one RGBA sheet, with its size and each
/// sprite's `[u0, v0, u1, v1]`. A sprite that didn't load leaves its cell
/// clear.
fn decoration_sheet(
    sprites: &[Option<(Vec<u8>, u32, u32)>],
) -> (Vec<u8>, (u32, u32), Vec<[f32; 4]>) {
    let side = sprites
        .iter()
        .flatten()
        .map(|&(_, w, h)| w.max(h))
        .max()
        .unwrap_or(8);
    let cell = side + 2 * GUTTER;
    let (width, height) = (cell * sprites.len().max(1) as u32, cell);
    let mut sheet = vec![0u8; (width * height * 4) as usize];
    let mut cells = Vec::with_capacity(sprites.len());
    for (index, sprite) in sprites.iter().enumerate() {
        let x0 = index as u32 * cell + GUTTER;
        let (w, h) = sprite.as_ref().map_or((side, side), |&(_, w, h)| (w, h));
        if let Some((pixels, ..)) = sprite {
            for y in 0..h {
                let from = (y * w * 4) as usize;
                let to = (((y + GUTTER) * width + x0) * 4) as usize;
                sheet[to..to + (w * 4) as usize]
                    .copy_from_slice(&pixels[from..from + (w * 4) as usize]);
            }
        }
        cells.push([
            x0 as f32 / width as f32,
            GUTTER as f32 / height as f32,
            (x0 + w) as f32 / width as f32,
            (GUTTER + h) as f32 / height as f32,
        ]);
    }
    (sheet, (width, height), cells)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each sprite lands in its own cell, a clear pixel in from the edges.
    #[test]
    fn the_sheet_keeps_sprites_apart() {
        let sprite = |value: u8| Some((vec![value; 2 * 2 * 4], 2, 2));
        let (sheet, (width, height), cells) = decoration_sheet(&[sprite(10), None, sprite(30)]);
        assert_eq!((width, height), (12, 4));
        let at = |x: u32, y: u32| sheet[((y * width + x) * 4) as usize];
        // Gutters, the first sprite, the missing one's clear cell, the third.
        assert_eq!([at(0, 1), at(1, 0), at(3, 1), at(1, 3)], [0; 4]);
        assert_eq!([at(1, 1), at(2, 2)], [10, 10]);
        assert_eq!([at(5, 1), at(6, 2)], [0, 0]);
        assert_eq!([at(9, 1), at(10, 2)], [30, 30]);
        assert_eq!(cells[0], [1.0 / 12.0, 0.25, 3.0 / 12.0, 0.75]);
        assert_eq!(cells[2], [9.0 / 12.0, 0.25, 11.0 / 12.0, 0.75]);
    }
}
