use std::path::Path;

use glam::{Mat3, Mat4, Vec3};

use crate::renderer::camera::CameraUniform;
use crate::renderer::chunk::atlas::TextureAtlas;
use crate::renderer::chunk::block_lighter::LightLevels;
use crate::renderer::pipelines::hand;
use crate::renderer::pipelines::item_display::{DisplayResolver, DisplayTransform};
use crate::renderer::pipelines::item_entity::{self, ItemEntityPipeline, ItemPipelineShared};
use crate::renderer::pipelines::lightmap::Lightmap;
use crate::renderer::pipelines::map::MapQuad;
use crate::renderer::rhi;
use crate::world::block::model::CardinalLightType;

pub struct HeldItemInfo {
    /// The item, whose model JSON gives the display transforms.
    pub name: String,
    pub item_tints: Vec<u32>,
    pub has_3d_model: bool,
    pub charged_crossbow: bool,
    /// The item's swing is a stab (a spear's `swing_animation`).
    pub stabs: bool,
    /// The baked model's own first-person transforms, right hand then left
    /// (packs and the stack's model included); `None` falls back to the
    /// item definition's.
    pub first_person: [Option<DisplayTransform>; 2],
    /// The mesh for each arm, right then left: the model's variant for that
    /// view where its parts are posed apart there.
    pub arm_models: [String; 2],
}

/// One first-person hand holding an item (vanilla `submitArmWithItem` for a
/// non-empty stack).
pub struct HeldHand<'a> {
    pub item: &'a HeldItemInfo,
    pub main_hand: bool,
    pub right_arm: bool,
    pub swing_progress: f32,
    pub use_anim: Option<UseAnim>,
}

/// First-person use-animation state (eat/drink), the inputs to vanilla
/// `ItemInHandRenderer.applyEatTransform`.
#[derive(Clone, Copy)]
pub struct UseAnim {
    /// Vanilla `useItemRemaining - partialTick + 1`.
    pub curr_usage_time: f32,
    pub duration: f32,
}

/// A map held in first person: its quads, and where the map's own space
/// (128 units a side) sits in view.
pub struct HeldMap<'a> {
    pub quads: &'a [MapQuad],
    pub pose: Mat4,
}

/// The poses of a map held in both hands (vanilla
/// `ItemInHandRenderer.renderTwoHandedMap`): the map's, then the right and
/// left hands'. `x_rot` is the view's pitch in degrees and `attack` the
/// swing's progress.
pub fn two_handed_map_poses(x_rot: f32, attack: f32) -> (Mat4, [Mat4; 2]) {
    let pi = std::f32::consts::PI;
    let sqrt_attack = attack.sqrt();
    let y_swing = -0.2 * (attack * pi).sin();
    let z_swing = -0.4 * (sqrt_attack * pi).sin();
    let tilt = map_tilt(x_rot);
    let base = Mat4::from_translation(Vec3::new(0.0, -y_swing / 2.0, z_swing))
        * Mat4::from_translation(Vec3::new(0.0, 0.04 + tilt * -0.5, -0.72))
        * Mat4::from_rotation_x((tilt * -85.0).to_radians());
    let hands = [1.0, -1.0].map(|invert| {
        base * Mat4::from_rotation_y(90.0_f32.to_radians()) * hand::map_hand_matrix(invert)
    });
    let map = base
        * Mat4::from_rotation_x(((sqrt_attack * pi).sin() * 20.0).to_radians())
        * Mat4::from_scale(Vec3::splat(2.0))
        * map_matrix();
    (map, hands)
}

/// The poses of a map held in one hand (vanilla `renderOneHandedMap`): the
/// map's, then its arm's. `invert` is [`hand::arm_invert`].
pub fn one_handed_map_poses(invert: f32, attack: f32) -> (Mat4, Mat4) {
    let pi = std::f32::consts::PI;
    let sqrt_attack = attack.sqrt();
    let x_swing = (sqrt_attack * pi).sin();
    let base = Mat4::from_translation(Vec3::new(invert * 0.125, -0.125, 0.0));
    let arm = base
        * Mat4::from_rotation_z((invert * 10.0_f32).to_radians())
        * hand::arm_matrix(attack, invert);
    let map = base
        * Mat4::from_translation(Vec3::new(invert * 0.51, -0.08, -0.75))
        * Mat4::from_translation(Vec3::new(
            invert * -0.5 * x_swing,
            0.4 * (sqrt_attack * pi * 2.0).sin() - 0.3 * x_swing,
            -0.3 * (attack * pi).sin(),
        ))
        * Mat4::from_rotation_x((x_swing * -45.0).to_radians())
        * Mat4::from_rotation_y((invert * x_swing * -30.0).to_radians())
        * map_matrix();
    (map, arm)
}

/// Vanilla `ItemInHandRenderer.calculateMapTilt`: the map lies flat while
/// the view is level and comes up to face it as the view turns down.
fn map_tilt(x_rot: f32) -> f32 {
    let tilt = (1.0 - x_rot / 45.0 + 0.1).clamp(0.0, 1.0);
    -(tilt * std::f32::consts::PI).cos() * 0.5 + 0.5
}

/// Vanilla `ItemInHandRenderer.renderMap`: the map's 128-unit space, y down,
/// turned to face the view and sized to the hand.
fn map_matrix() -> Mat4 {
    Mat4::from_rotation_y(std::f32::consts::PI)
        * Mat4::from_rotation_z(std::f32::consts::PI)
        * Mat4::from_scale(Vec3::splat(0.38))
        * Mat4::from_translation(Vec3::new(-0.5, -0.5, 0.0))
        * Mat4::from_scale(Vec3::splat(0.0078125))
}

/// Vanilla `ItemInHandRenderer.HandRenderSelection`: which hands first
/// person draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HandRenderSelection {
    pub main: bool,
    pub off: bool,
}

impl HandRenderSelection {
    pub const BOTH: Self = Self {
        main: true,
        off: true,
    };
    pub const MAIN_ONLY: Self = Self {
        main: true,
        off: false,
    };
    pub const OFF_ONLY: Self = Self {
        main: false,
        off: true,
    };
    pub const NONE: Self = Self {
        main: false,
        off: false,
    };
}

pub struct HeldItemPipeline {
    pipeline: rhi::backend::RenderPipeline,
    shared: ItemPipelineShared,
    right_display: DisplayResolver,
    left_display: DisplayResolver,
}

impl HeldItemPipeline {
    pub fn new(
        device: &rhi::backend::Device,
        targets: rhi::backend::PipelineTargets,
        atlas: &TextureAtlas,
        jar_assets_dir: &Path,
        lightmap: &Lightmap,
    ) -> Self {
        let shared = ItemPipelineShared::new(device, atlas, lightmap, "held_item");
        let pipeline = item_entity::create_pipeline(device, targets, shared.pipeline_layout);
        Self {
            pipeline,
            shared,
            right_display: DisplayResolver::new(jar_assets_dir, &["firstperson_righthand"]),
            left_display: DisplayResolver::new(
                jar_assets_dir,
                &["firstperson_lefthand", "firstperson_righthand"],
            ),
        }
    }

    pub fn rebind_atlas(&mut self, device: &rhi::backend::Device, atlas: &TextureAtlas) {
        self.shared.rebind_atlas(device, atlas);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update_and_draw(
        &mut self,
        device: &rhi::backend::Device,
        pass: &mut rhi::backend::RenderPassEncoder,
        frame: usize,
        aspect: f32,
        hud_fov: f32,
        hands: &[HeldHand<'_>],
        maps: &[HeldMap<'_>],
        map_vertices: rhi::backend::BufferHandle,
        meshes: &ItemEntityPipeline,
        bob: Mat4,
        hand_pose: Mat4,
        light: LightLevels,
        cardinal_light: CardinalLightType,
        fire_overlay: bool,
    ) {
        let uniform = CameraUniform::with_view_proj(hand::projection(aspect, hud_fov) * bob)
            .with_cardinal_light(cardinal_light);
        self.shared.update_camera(frame, &uniform);

        let tint_count = hands.iter().map(|hand| hand.item.item_tints.len()).sum();
        let map_quads: usize = maps.iter().map(|map| map.quads.len()).sum();
        self.shared
            .begin_frame(device, frame, hands.len() + 1 + map_quads, tint_count);
        self.shared.bind(pass, frame, self.pipeline);
        for hand in hands {
            let item = hand.item;
            let mesh = &item.arm_models[usize::from(!hand.right_arm)];
            let Some((buffer, vertex_count)) = meshes.mesh_handle(mesh) else {
                continue;
            };
            let default = default_first_person(item.has_3d_model, hand.right_arm);
            let display = if hand.right_arm {
                item.first_person[0]
                    .unwrap_or_else(|| self.right_display.resolve(&item.name, default))
            } else {
                item.first_person[1]
                    .unwrap_or_else(|| self.left_display.resolve(&item.name, default))
                    .mirrored()
            };
            let invert = hand::arm_invert(hand.right_arm);
            let mut arm = match hand.use_anim {
                Some(anim) => eat_item_matrix(anim, invert),
                None if item.stabs => {
                    item_arm_matrix(invert) * stab_matrix(hand.swing_progress, invert)
                }
                None => first_person_item_matrix(hand.swing_progress, invert),
            };
            // Vanilla `submitArmWithItem`'s crossbow branch: a charged one
            // held still in the main hand turns toward the crosshair.
            if item.charged_crossbow && hand.main_hand && hand.swing_progress < 0.001 {
                arm *= Mat4::from_translation(Vec3::new(invert * -0.641864, 0.0, 0.0))
                    * Mat4::from_rotation_y((invert * 10.0).to_radians());
            }
            let model = arm * display.to_matrix();
            let instance = self.shared.push_item(
                frame,
                &model,
                Mat3::from_mat4(hand_pose),
                light.lane(),
                &item.item_tints,
                0,
            );
            pass.set_vertex_buffer(0, buffer);
            pass.draw(vertex_count, 1, 0, instance);
        }
        // A map's paper, picture and markers, each from a texture of its
        // own in place of the atlas, lit as the hand is.
        if map_quads > 0 {
            pass.set_vertex_buffer(0, map_vertices);
            for map in maps {
                for quad in map.quads {
                    let model = map.pose * quad.model;
                    // Undoes the instance's own normal matrix, so the quad's
                    // normal reaches the shader as built, whatever way the
                    // map is turned: vanilla's text render type isn't shaded.
                    let unturned = Mat3::from_mat4(model).transpose();
                    let instance =
                        self.shared
                            .push_item(frame, &model, unturned, light.lane(), &[], 0);
                    pass.set_bind_group(1, quad.texture);
                    pass.draw(6, 1, quad.first_vertex, instance);
                }
            }
            pass.set_bind_group(1, self.shared.atlas_set());
        }
        // Vanilla draws the burning overlay after the hands, in a pose
        // without the view bobbing, at full brightness.
        if fire_overlay
            && let Some((buffer, vertex_count)) = meshes.mesh_handle(item_entity::FIRE_OVERLAY_MESH)
        {
            let instance = self.shared.push_item(
                frame,
                &bob.inverse(),
                Mat3::IDENTITY,
                LightLevels::new(15, 15).lane(),
                &[],
                0,
            );
            pass.set_vertex_buffer(0, buffer);
            pass.draw(vertex_count, 1, 0, instance);
        }
    }

    pub fn recreate_pipeline(
        &mut self,
        device: &rhi::backend::Device,
        targets: rhi::backend::PipelineTargets,
    ) {
        self.pipeline.destroy(device);
        self.pipeline = item_entity::create_pipeline(device, targets, self.shared.pipeline_layout);
    }

    pub fn destroy(&mut self, device: &rhi::backend::Device) {
        self.pipeline.destroy(device);
        self.shared.destroy(device);
    }
}

/// Vanilla `SpearAnimations.firstPersonAttack`: the spear drawn back, thrust
/// out past the arm and eased home.
fn stab_matrix(attack: f32, invert: f32) -> Mat4 {
    let progress = |start: f32, end: f32| ((attack - start) / (end - start)).clamp(0.0, 1.0);
    let in_out_sine = |x: f32| -((std::f32::consts::PI * x).cos() - 1.0) / 2.0;
    let out_back = |x: f32| 1.0 + 2.70158 * (x - 1.0).powi(3) + 1.70158 * (x - 1.0).powi(2);
    let in_out_expo = |x: f32| {
        if x <= 0.0 {
            0.0
        } else if x >= 1.0 {
            1.0
        } else if x < 0.5 {
            2f32.powf(20.0 * x - 10.0) / 2.0
        } else {
            (2.0 - 2f32.powf(-20.0 * x + 10.0)) / 2.0
        }
    };
    let starting = in_out_sine(progress(0.0, 0.05));
    let middle = out_back(progress(0.05, 0.2));
    let ending = in_out_expo(progress(0.4, 1.0));
    Mat4::from_translation(Vec3::new(
        invert * 0.1 * (starting - middle),
        -0.075 * (starting - ending),
        0.65 * (starting - middle),
    )) * Mat4::from_rotation_x((-70.0 * (starting - ending)).to_radians())
        * Mat4::from_translation(Vec3::new(0.0, 0.0, -0.25 * (ending - middle)))
}

/// Vanilla `ItemInHandRenderer.applyItemArmTransform` (inverseArmHeight 0):
/// `invert` is [`hand::arm_invert`].
fn item_arm_matrix(invert: f32) -> Mat4 {
    Mat4::from_translation(Vec3::new(invert * 0.56, -0.52, -0.72))
}

// Vanilla ItemInHandRenderer: applyItemArmTransform + swingArm.
fn first_person_item_matrix(swing_progress: f32, invert: f32) -> Mat4 {
    let a = swing_progress;
    let sq = a.sqrt();
    let pi = std::f32::consts::PI;

    item_arm_matrix(invert)
        * Mat4::from_translation(Vec3::new(
            invert * -0.4 * (sq * pi).sin(),
            0.2 * (sq * pi * 2.0).sin(),
            -0.2 * (a * pi).sin(),
        ))
        * Mat4::from_rotation_y((invert * (45.0 + (a * a * pi).sin() * -20.0)).to_radians())
        * Mat4::from_rotation_z((invert * (sq * pi).sin() * -20.0).to_radians())
        * Mat4::from_rotation_x(((sq * pi).sin() * -80.0).to_radians())
        * Mat4::from_rotation_y((invert * -45.0).to_radians())
}

// Vanilla ItemInHandRenderer: applyEatTransform then applyItemArmTransform
// (EAT/DRINK skip the usual pre-transform).
fn eat_item_matrix(anim: UseAnim, invert: f32) -> Mat4 {
    let scaled = anim.curr_usage_time / anim.duration;
    // The chew bob runs after the first 20% of the eat, oscillating every 4
    // ticks; the jiggle shoves the item into the mouth over the last bite.
    let bob = if scaled < 0.8 {
        ((anim.curr_usage_time / 4.0 * std::f32::consts::PI).cos() * 0.1).abs()
    } else {
        0.0
    };
    let jiggle = 1.0 - (scaled as f64).powf(27.0) as f32;

    Mat4::from_translation(Vec3::new(invert * jiggle * 0.6, bob + jiggle * -0.5, 0.0))
        * Mat4::from_rotation_y((invert * jiggle * 90.0).to_radians())
        * Mat4::from_rotation_x((jiggle * 10.0).to_radians())
        * Mat4::from_rotation_z((invert * jiggle * 30.0).to_radians())
        * item_arm_matrix(invert)
}

/// The fallback first-person display: `block/block.json`'s for the hand, or
/// `item/generated.json`'s right-hand one, which its left hand reuses.
fn default_first_person(has_3d_model: bool, right_arm: bool) -> DisplayTransform {
    if has_3d_model {
        DisplayTransform {
            rotation: Vec3::new(0.0, if right_arm { 45.0 } else { 225.0 }, 0.0),
            translation: Vec3::ZERO,
            scale: Vec3::splat(0.40),
        }
    } else {
        DisplayTransform {
            rotation: Vec3::new(0.0, -90.0, 25.0),
            translation: Vec3::new(1.13, 3.2, 1.13) / 16.0,
            scale: Vec3::splat(0.68),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `calculateMapTilt`: flat while the view is level, upright once it
    /// looks down past 49.5 degrees.
    #[test]
    fn a_map_tilts_up_as_the_view_turns_down() {
        assert_eq!(map_tilt(0.0), 1.0);
        assert_eq!(map_tilt(-30.0), 1.0);
        assert!((map_tilt(27.0) - 0.5).abs() < 1e-6);
        assert!(map_tilt(49.5).abs() < 1e-6);
        assert_eq!(map_tilt(90.0), 0.0);
    }

    /// Looking straight down, a map in both hands faces the view squarely:
    /// its first pixel up and to the left, its picture wound to face the
    /// eye and in front of the paper.
    #[test]
    fn a_two_handed_map_faces_the_view() {
        let (map, _) = two_handed_map_poses(90.0, 0.0);
        let at = |x: f32, y: f32, z: f32| map.transform_point3(Vec3::new(x, y, z));
        let near = |a: Vec3, b: Vec3| (a - b).length() < 1e-5;
        assert!(near(at(64.0, 64.0, 0.0), Vec3::new(0.0, 0.04, -0.72)));
        assert!(near(at(0.0, 0.0, 0.0), Vec3::new(-0.38, 0.42, -0.72)));
        assert!(near(at(128.0, 128.0, 0.0), Vec3::new(0.38, -0.34, -0.72)));
        // The picture's corners in the order its quad is built: they turn
        // anticlockwise seen from the eye, the side the pipeline keeps.
        let corners = [(0.0, 128.0), (128.0, 128.0), (128.0, 0.0)].map(|(x, y)| at(x, y, -0.01));
        let facing = (corners[1] - corners[0]).cross(corners[2] - corners[1]);
        assert!(facing.z > 0.0);
        assert!(corners[0].z > at(0.0, 128.0, 0.0).z);
    }

    /// A map in one hand sits to that hand's side, mirrored for the other.
    #[test]
    fn a_one_handed_map_sits_by_its_hand() {
        let centre = |invert: f32| {
            one_handed_map_poses(invert, 0.0)
                .0
                .transform_point3(Vec3::new(64.0, 64.0, 0.0))
        };
        let (right, left) = (centre(1.0), centre(-1.0));
        assert!((right - Vec3::new(0.635, -0.205, -0.75)).length() < 1e-5);
        assert!((left - Vec3::new(-0.635, -0.205, -0.75)).length() < 1e-5);
    }

    /// `m` reflected across the X axis, as the left hand mirrors the right.
    fn mirror(m: Mat4) -> Mat4 {
        let flip = Mat4::from_scale(Vec3::new(-1.0, 1.0, 1.0));
        flip * m * flip
    }

    #[test]
    fn the_left_arm_mirrors_the_right() {
        for swing in [0.0, 0.3, 0.7, 1.0] {
            assert!(
                first_person_item_matrix(swing, -1.0)
                    .abs_diff_eq(mirror(first_person_item_matrix(swing, 1.0)), 1e-5),
                "swing {swing}"
            );
        }
        let anim = UseAnim {
            curr_usage_time: 12.0,
            duration: 32.0,
        };
        assert!(eat_item_matrix(anim, -1.0).abs_diff_eq(mirror(eat_item_matrix(anim, 1.0)), 1e-5));
        let display = default_first_person(false, true);
        assert!(
            display
                .mirrored()
                .to_matrix()
                .abs_diff_eq(mirror(display.to_matrix()), 1e-5)
        );
    }
}
