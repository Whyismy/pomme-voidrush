//! A humanoid's arms on a crossbow (vanilla
//! `AnimationUtils.animateCrossbowHold` and `animateCrossbowCharge`): levelled
//! along the look when it is loaded, drawn back across the body while it is
//! being loaded.

use std::f32::consts::FRAC_PI_2;

use glam::Vec3;

use super::{BakedEntityModel, PartAnim, find_part};

/// `CrossbowItem.DEFAULT_CHARGE_DURATION`, in ticks.
// TODO: Quick Charge shortens it.
pub const CHARGE_TICKS: f32 = 25.0;

/// What a humanoid's arms do with its crossbow (`ArmPose.CROSSBOW_*`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum CrossbowPose {
    #[default]
    None,
    /// Loaded and held at the ready.
    Hold,
    /// Being loaded, this far through (0 to 1).
    Charge(f32),
}

/// Poses the arms over the kind's own animation. The crossbow is in the
/// right hand; `head_x` and `head_y` are the head's pitch and yaw from the
/// body, in radians. Illagers' crossed arms give way to the two free ones.
// TODO: left-handed holders.
pub fn pose_arms(
    model: &BakedEntityModel,
    anim: &mut PartAnim,
    pose: CrossbowPose,
    head_x: f32,
    head_y: f32,
) {
    let (main, off) = match pose {
        CrossbowPose::None => return,
        CrossbowPose::Hold => (
            Vec3::new(-FRAC_PI_2 + head_x + 0.1, -0.3 + head_y, 0.0),
            Vec3::new(-1.5 + head_x, 0.6 + head_y, 0.0),
        ),
        CrossbowPose::Charge(progress) => {
            let progress = progress.clamp(0.0, 1.0);
            let main = Vec3::new(-0.970_796_35, -0.8, 0.0);
            (
                main,
                Vec3::new(
                    main.x + (-FRAC_PI_2 - main.x) * progress,
                    0.4 + (0.85 - 0.4) * progress,
                    0.0,
                ),
            )
        }
    };
    for (name, rotation) in [("right_arm", main), ("left_arm", off)] {
        if let Some(i) = find_part(&model.parts, name) {
            *anim.rotation_mut(model, i) = rotation;
            // An illager's free arms hide while its arms are crossed.
            anim.scale.retain(|&(part, _)| part != i);
        }
    }
    if let Some(i) = find_part(&model.parts, "arms") {
        anim.scale.retain(|&(part, _)| part != i);
        anim.scale.push((i, Vec3::ZERO));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::entity_model::{bake_exported, exported_data};

    /// A loaded crossbow is levelled along the look; loading it draws the
    /// off arm across; an illager's crossed arms make way.
    #[test]
    fn arms_take_the_crossbow() {
        let model = bake_exported(exported_data::PILLAGER);
        let part = |name: &str| find_part(&model.parts, name).unwrap();
        let rot = |anim: &PartAnim, name: &str| {
            let i = part(name);
            anim.rotation.iter().find(|r| r.0 == i).unwrap().1
        };
        let mut anim = PartAnim::default();
        // At rest an illager's free arms are hidden.
        anim.scale.push((part("right_arm"), Vec3::ZERO));
        anim.scale.push((part("left_arm"), Vec3::ZERO));
        pose_arms(&model, &mut anim, CrossbowPose::Hold, 0.2, -0.1);
        assert_eq!(
            rot(&anim, "right_arm"),
            Vec3::new(-FRAC_PI_2 + 0.3, -0.4, 0.0)
        );
        assert_eq!(rot(&anim, "left_arm"), Vec3::new(-1.3, 0.5, 0.0));
        assert_eq!(anim.scale, [(part("arms"), Vec3::ZERO)]);

        let mut anim = PartAnim::default();
        pose_arms(&model, &mut anim, CrossbowPose::Charge(0.0), 0.0, 0.0);
        assert_eq!(rot(&anim, "left_arm"), Vec3::new(-0.970_796_35, 0.4, 0.0));
        pose_arms(&model, &mut anim, CrossbowPose::Charge(2.0), 0.0, 0.0);
        assert_eq!(rot(&anim, "left_arm"), Vec3::new(-FRAC_PI_2, 0.85, 0.0));
        assert_eq!(rot(&anim, "right_arm"), Vec3::new(-0.970_796_35, -0.8, 0.0));

        let mut anim = PartAnim::default();
        pose_arms(&model, &mut anim, CrossbowPose::None, 0.0, 0.0);
        assert!(anim.rotation.is_empty() && anim.scale.is_empty());
    }
}
