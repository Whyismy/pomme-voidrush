//! Models baked straight from vanilla's own model-layer geometry
//! ([`super::exported_data`]): each part's `PartPose` and its baked polygons,
//! as `LayerDefinitions` builds them, for the mobs pomme has no hand-ported
//! model of. Animation still comes from the renderer's per-kind hooks, which
//! find parts by their vanilla names.

use glam::Vec3;

use super::{BakedEntityModel, EntityPart, EntityVertex, pack_normal};
use crate::renderer::chunk::mesher::pack_uv;

/// One part of an exported layer: its name, its parent (an earlier index),
/// its pose (x, y, z in model pixels, then x/y/z rotation in radians, then
/// x/y/z scale) and its quads.
pub(crate) struct ExportedPart {
    pub name: &'static str,
    pub parent: Option<u8>,
    pub pose: [f32; 9],
    pub quads: &'static [ExportedQuad],
}

/// A baked `ModelPart.Polygon`: four corners of x, y, z (model pixels, y
/// down) and u, v (fractions of the texture sheet), and vanilla's normal.
pub(crate) struct ExportedQuad {
    pub normal: [f32; 3],
    pub corners: [[f32; 5]; 4],
}

/// Bakes an exported layer in pomme's entity convention: positions in blocks
/// with y up, and each quad wound like [`super::generate_cube_vertices`]'s
/// (its first two edges cross to point into the model).
pub(crate) fn bake_exported(parts: &[ExportedPart]) -> BakedEntityModel {
    bake_exported_hiding(parts, &[])
}

/// [`bake_exported`] with the parts named in `hidden` left without
/// geometry (kept as parts, so poses still find them by name).
pub(crate) fn bake_exported_hiding(parts: &[ExportedPart], hidden: &[&str]) -> BakedEntityModel {
    let mut entity_parts = Vec::with_capacity(parts.len());
    let mut vertices = Vec::new();
    let mut part_ranges = Vec::with_capacity(parts.len());
    for part in parts {
        let [x, y, z, x_rot, y_rot, z_rot, ..] = part.pose;
        entity_parts.push(EntityPart {
            name: part.name.into(),
            offset: Vec3::new(x, y, z),
            default_rotation: Vec3::new(x_rot, y_rot, z_rot),
            cubes: Vec::new(),
            parent: part.parent.map(usize::from),
        });
        let start = vertices.len() as u32;
        let quads = if hidden.contains(&part.name) {
            &[][..]
        } else {
            part.quads
        };
        for quad in quads {
            let normal = Vec3::new(quad.normal[0], -quad.normal[1], quad.normal[2]);
            let mut corners = quad
                .corners
                .map(|[x, y, z, u, v]| (Vec3::new(x, -y, z) / 16.0, [u, v]));
            let (a, b, c) = (corners[0].0, corners[1].0, corners[2].0);
            if (b - a).cross(c - a).dot(normal) > 0.0 {
                corners.reverse();
            }
            for i in [0, 1, 2, 0, 2, 3] {
                let (position, uv) = corners[i];
                vertices.push(EntityVertex {
                    position: position.to_array(),
                    tex_coords: pack_uv(uv[0], uv[1]),
                    normal: pack_normal(normal),
                });
            }
        }
        part_ranges.push((start, vertices.len() as u32 - start));
    }
    let mut model = BakedEntityModel::new(entity_parts, vertices, part_ranges);
    // `PartPose` scale (uniform in every exported layer): the small armor
    // stand's parts are posed at half and three-quarter size.
    model.part_scales = parts.iter().map(|part| part.pose[6]).collect();
    model
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exported zombie bakes to exactly the triangles of pomme's
    /// hand-ported one: same corners, UVs, normals and winding.
    #[test]
    fn the_exported_zombie_matches_the_hand_ported_one() {
        let key = |v: &EntityVertex| {
            (
                v.position.map(|p| (p * 4096.0).round() as i32),
                v.tex_coords,
                v.normal,
            )
        };
        let triangles = |model: &BakedEntityModel| {
            let mut tris: Vec<_> = model
                .vertices
                .chunks(3)
                .map(|t| [key(&t[0]), key(&t[1]), key(&t[2])])
                .map(|mut t| {
                    // The same triangle may start at another corner.
                    let first = (0..3).min_by_key(|&i| t[i]).unwrap();
                    t.rotate_left(first);
                    t
                })
                .collect();
            tris.sort();
            tris
        };
        let exported = bake_exported(super::super::exported_data::ZOMBIE);
        let ported = super::super::bake_zombie_model();
        assert_eq!(triangles(&exported), triangles(&ported));
    }
}
