//! A beacon beam's geometry (vanilla `BeaconRenderer.renderBeaconBeam`): a
//! square column whose texture repeats two and a half times a block, baked
//! in whole lengths so a beam of any height is a few pieces.

use glam::Vec3;

use super::{BakedEntityModel, EntityPart, EntityVertex, pack_normal};
use crate::renderer::chunk::mesher::pack_uv;

/// The lengths a beam is pieced together from, longest first, in blocks.
pub(crate) const BEAM_LENGTHS: [u32; 7] = [64, 32, 16, 8, 4, 2, 1];

/// The beam itself is 0.2 from its middle to its corners, set diagonally;
/// the glow round it 0.25 to its sides, set square.
const BEAM_RADIUS: f32 = 0.2;
const GLOW_RADIUS: f32 = 0.25;
/// How much of a block one repeat of the texture covers.
const REPEAT: f32 = 0.4;

/// A column `length` blocks tall standing on the origin, in a non-living
/// renderer's frame (drawn as built, in blocks): the beam, or its glow.
/// Its texture starts a repeat at the bottom, so pieces of even length
/// stack seamlessly.
pub(crate) fn bake_beam(length: u32, glow: bool) -> BakedEntityModel {
    let corners: [(f32, f32); 4] = if glow {
        let r = GLOW_RADIUS;
        [(-r, -r), (r, -r), (r, r), (-r, r)]
    } else {
        let r = BEAM_RADIUS;
        [(0.0, r), (r, 0.0), (0.0, -r), (-r, 0.0)]
    };
    let mut vertices = Vec::new();
    let top = length as f32;
    for side in 0..4 {
        let (x0, z0) = corners[side];
        let (x1, z1) = corners[(side + 1) % 4];
        let outward = Vec3::new(x0 + x1, 0.0, z0 + z1).normalize();
        let mut y = 0.0;
        while y < top - 1.0e-4 {
            let end = (y + REPEAT).min(top);
            // The texture runs down from the top of each repeat.
            let v_top = 1.0 - (end - y) / REPEAT;
            let quad = [
                (x0, y, z0, 0.0, 1.0),
                (x1, y, z1, 1.0, 1.0),
                (x1, end, z1, 1.0, v_top),
                (x0, end, z0, 0.0, v_top),
            ];
            // Pomme's entity convention flips y, as `bake_exported` does.
            let normal = Vec3::new(outward.x, -outward.y, outward.z);
            let mut quad = quad.map(|(x, y, z, u, v)| (Vec3::new(x, -y, z), [u, v]));
            let (a, b, c) = (quad[0].0, quad[1].0, quad[2].0);
            if (b - a).cross(c - a).dot(normal) > 0.0 {
                quad.reverse();
            }
            for i in [0, 1, 2, 0, 2, 3] {
                let (position, uv) = quad[i];
                vertices.push(EntityVertex {
                    position: position.to_array(),
                    tex_coords: pack_uv(uv[0], uv[1]),
                    normal: pack_normal(normal),
                });
            }
            y = end;
        }
    }
    let count = vertices.len() as u32;
    let part = EntityPart {
        name: "beam".into(),
        offset: Vec3::ZERO,
        default_rotation: Vec3::ZERO,
        cubes: Vec::new(),
        parent: None,
    };
    BakedEntityModel::new(vec![part], vertices, vec![(0, count)])
}

/// The pieces a stretch `height` blocks tall is drawn as: each piece's
/// offset from the stretch's foot and its index into [`BEAM_LENGTHS`].
pub(crate) fn beam_pieces(height: u32) -> Vec<(u32, usize)> {
    let mut pieces = Vec::new();
    let mut at = 0;
    for (index, length) in BEAM_LENGTHS.iter().enumerate() {
        while height - at >= *length {
            pieces.push((at, index));
            at += length;
        }
    }
    pieces
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two blocks are five repeats a side; one block two and a half, the
    /// last cut short; any height is made of the baked lengths.
    #[test]
    fn a_beam_is_pieced_from_whole_repeats() {
        assert_eq!(bake_beam(2, false).vertices.len(), 4 * 5 * 6);
        let one = bake_beam(1, true);
        assert_eq!(one.vertices.len(), 4 * 3 * 6);
        // The cut repeat stops half way up the texture.
        assert!(
            one.vertices
                .iter()
                .any(|v| v.tex_coords == pack_uv(0.0, 0.5))
        );
        assert!(one.vertices.iter().all(|v| v.position[1] >= -1.0 - 1e-6));
        // The glow is half a block across.
        assert!(one.vertices.iter().all(|v| v.position[0].abs() == 0.25));

        assert_eq!(beam_pieces(71), [(0, 0), (64, 4), (68, 5), (70, 6)]);
        assert_eq!(beam_pieces(0), []);
        let total: u32 = beam_pieces(300).iter().map(|(_, i)| BEAM_LENGTHS[*i]).sum();
        assert_eq!(total, 300);
    }
}
