//! A painting's board (vanilla `PaintingRenderer.renderPainting`), baked a
//! block at a time as vanilla draws it: the picture on the front, and the
//! back texture on the back and, a pixel of it, round the edges.

use glam::Vec3;

use super::{BakedEntityModel, EntityPart, EntityVertex, pack_normal};
use crate::renderer::chunk::mesher::pack_uv;

/// Half the board's thickness.
const HALF_DEPTH: f32 = 0.03125;

/// Which faces a baked board has.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Board {
    /// The front, mapped across the whole texture (a picture).
    Picture,
    /// The front, each block showing the whole texture (the back texture
    /// standing in for a picture the client has none of).
    Blank,
    /// The back and the four edges.
    Back,
}

/// A board `width` by `height` blocks, centred on the origin with its front
/// facing -z, in a non-living renderer's frame (the model drawn as built,
/// in blocks). Each block of it is a part of its own, column by column
/// and from the bottom up, so each can be drawn in its own light.
pub(crate) fn bake_painting(width: u32, height: u32, board: Board) -> BakedEntityModel {
    let (w, h) = (width as f32, height as f32);
    let (left, bottom) = (-w / 2.0, -h / 2.0);
    let mut vertices = Vec::new();
    let mut parts = Vec::new();
    let mut ranges = Vec::new();
    let quad = |vertices: &mut Vec<EntityVertex>,
                corners: [(f32, f32, f32, f32, f32); 4],
                normal: Vec3| {
        // Pomme's entity convention flips y, as `bake_exported` does.
        let normal = Vec3::new(normal.x, -normal.y, normal.z);
        let mut corners = corners.map(|(x, y, z, u, v)| (Vec3::new(x, -y, z), [u, v]));
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
    };
    // One texel of the 16-pixel back texture.
    let texel = 1.0 / 16.0;
    for column in 0..width {
        for row in 0..height {
            let start = vertices.len() as u32;
            let mut quad = |corners, normal| quad(&mut vertices, corners, normal);
            let (x0, x1) = (left + column as f32, left + column as f32 + 1.0);
            let (y0, y1) = (bottom + row as f32, bottom + row as f32 + 1.0);
            match board {
                Board::Picture | Board::Blank => {
                    // The picture runs right to left across +x, top down.
                    let (u0, u1, v0, v1) = if board == Board::Picture {
                        (
                            (w - column as f32) / w,
                            (w - column as f32 - 1.0) / w,
                            (h - row as f32) / h,
                            (h - row as f32 - 1.0) / h,
                        )
                    } else {
                        (1.0, 0.0, 1.0, 0.0)
                    };
                    quad(
                        [
                            (x1, y0, -HALF_DEPTH, u1, v0),
                            (x0, y0, -HALF_DEPTH, u0, v0),
                            (x0, y1, -HALF_DEPTH, u0, v1),
                            (x1, y1, -HALF_DEPTH, u1, v1),
                        ],
                        Vec3::NEG_Z,
                    );
                }
                Board::Back => {
                    quad(
                        [
                            (x1, y1, HALF_DEPTH, 1.0, 0.0),
                            (x0, y1, HALF_DEPTH, 0.0, 0.0),
                            (x0, y0, HALF_DEPTH, 0.0, 1.0),
                            (x1, y0, HALF_DEPTH, 1.0, 1.0),
                        ],
                        Vec3::Z,
                    );
                    // Top and bottom: a texel-high strip of the back.
                    for (y, normal) in [(y1, Vec3::Y), (y0, Vec3::NEG_Y)] {
                        quad(
                            [
                                (x1, y, -HALF_DEPTH, 0.0, 0.0),
                                (x0, y, -HALF_DEPTH, 1.0, 0.0),
                                (x0, y, HALF_DEPTH, 1.0, texel),
                                (x1, y, HALF_DEPTH, 0.0, texel),
                            ],
                            normal,
                        );
                    }
                    // The sides: a texel-wide strip.
                    for (x, normal) in [(x1, Vec3::X), (x0, Vec3::NEG_X)] {
                        quad(
                            [
                                (x, y1, HALF_DEPTH, texel, 0.0),
                                (x, y0, HALF_DEPTH, texel, 1.0),
                                (x, y0, -HALF_DEPTH, 0.0, 1.0),
                                (x, y1, -HALF_DEPTH, 0.0, 0.0),
                            ],
                            normal,
                        );
                    }
                }
            }
            parts.push(EntityPart {
                name: format!("block_{column}_{row}"),
                offset: Vec3::ZERO,
                default_rotation: Vec3::ZERO,
                cubes: Vec::new(),
                parent: None,
            });
            ranges.push((start, vertices.len() as u32 - start));
        }
    }
    BakedEntityModel::new(parts, vertices, ranges)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A picture is one quad a block; a back adds the four edges to each
    /// (vanilla draws the inner ones too).
    #[test]
    fn a_board_is_baked_a_block_at_a_time() {
        assert_eq!(bake_painting(4, 2, Board::Picture).vertices.len(), 8 * 6);
        assert_eq!(bake_painting(4, 2, Board::Blank).vertices.len(), 8 * 6);
        assert_eq!(bake_painting(4, 2, Board::Back).vertices.len(), 8 * 5 * 6);
        // Each block is a part of its own, column by column.
        let back = bake_painting(4, 2, Board::Back);
        assert_eq!(back.parts.len(), 8);
        assert_eq!(back.parts[3].name, "block_1_1");
        assert_eq!(back.part_ranges[3], (3 * 30, 30));
        // The picture's top left corner (u 0, v 0) is at the top of +x.
        let picture = bake_painting(2, 1, Board::Picture);
        let corner = picture
            .vertices
            .iter()
            .find(|v| v.tex_coords == pack_uv(0.0, 0.0))
            .unwrap();
        assert_eq!(corner.position, [1.0, -0.5, -HALF_DEPTH]);
    }
}
