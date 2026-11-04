//! Vanilla `SpearAnimations`' third-person half: the arm a humanoid holds a
//! spear with (`ArmPose.SPEAR`), raised and swaying through a charge, and the
//! spear itself turned out level for the charge and thrust for the jab.

use std::f32::consts::{FRAC_PI_2, PI};

use azalea_registry::builtin::{EntityKind, ItemKind};
use glam::{Mat4, Vec3};

use super::{BakedEntityModel, PartAnim, find_part};

/// A `kinetic_weapon` component's timings (vanilla `KineticWeapon`), as its
/// use animation reads them.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct KineticTimings {
    pub delay_ticks: i32,
    pub dismount_ticks: i32,
    pub knockback_ticks: i32,
    pub damage_ticks: i32,
    pub forward_movement: f32,
}

impl From<&azalea_inventory::components::KineticWeapon> for KineticTimings {
    fn from(weapon: &azalea_inventory::components::KineticWeapon) -> Self {
        let ticks = |condition: &Option<azalea_inventory::components::KineticWeaponCondition>| {
            condition.as_ref().map_or(0, |c| c.max_duration_ticks)
        };
        Self {
            delay_ticks: weapon.delay_ticks,
            dismount_ticks: ticks(&weapon.dismount_conditions),
            knockback_ticks: ticks(&weapon.knockback_conditions),
            damage_ticks: ticks(&weapon.damage_conditions),
            forward_movement: weapon.forward_movement,
        }
    }
}

/// How a humanoid holds a spear this frame (vanilla `HumanoidRenderState`'s
/// arm poses and `ticksUsingItem`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SpearPose {
    /// The arms in `ArmPose.SPEAR`: right, then left.
    pub arms: [bool; 2],
    /// The arms a zombie-armed kind lets hang, since a stabbing weapon skips
    /// `AnimationUtils.animateZombieArms`: right, then left.
    pub hanging: [bool; 2],
    /// While a hand uses a kinetic weapon (a mob's charge, a player's held
    /// spear): its arm (`true` for the right), `ticksUsingItem` and the
    /// weapon's timings.
    pub using: Option<(bool, f32, KineticTimings)>,
}

/// Vanilla `ItemTags.SPEARS`.
pub fn is_spear(kind: ItemKind) -> bool {
    matches!(
        kind,
        ItemKind::WoodenSpear
            | ItemKind::StoneSpear
            | ItemKind::CopperSpear
            | ItemKind::IronSpear
            | ItemKind::GoldenSpear
            | ItemKind::DiamondSpear
            | ItemKind::NetheriteSpear
    )
}

/// Which arms `kind` poses `ArmPose.SPEAR`, given what each hand holds
/// (`(is a spear, its swing stabs)`, main hand then off hand) and whether
/// it's mid-swing: vanilla `HumanoidMobRenderer.getArmPose` (a spear, or a
/// stabbing item mid-swing), `AbstractZombieRenderer`'s (either arm, when the
/// other hand's item stabs) and `AvatarRenderer`'s. A right-handed main arm.
/// Then the arms [`SpearPose::hanging`] lets hang.
pub fn spear_arms(
    kind: EntityKind,
    aggressive: bool,
    hands: [Option<(bool, bool)>; 2],
    swinging: bool,
) -> ([bool; 2], [bool; 2]) {
    let own =
        |hand: Option<(bool, bool)>| hand.is_some_and(|(spear, stab)| spear || stab && swinging);
    let stabs = |hand: Option<(bool, bool)>| hand.is_some_and(|(_, stab)| stab);
    let arms = match kind {
        EntityKind::Zombie | EntityKind::Husk | EntityKind::Drowned => [
            stabs(hands[1]) || own(hands[0]),
            stabs(hands[0]) || own(hands[1]),
        ],
        EntityKind::ZombieVillager | EntityKind::ZombifiedPiglin => {
            let arms = [own(hands[0]), own(hands[1])];
            // The (main-hand) attack arm's weapon stabs: the zombie arms
            // aren't held out, so the others hang.
            let hanging = if stabs(hands[0]) {
                arms.map(|spear| !spear)
            } else {
                [false; 2]
            };
            return (arms, hanging);
        }
        // `SkeletonModel.setupAnim` holds an aggressive skeleton's arms out
        // over any pose; piglins wave theirs over it when aggressive.
        EntityKind::Skeleton
        | EntityKind::Stray
        | EntityKind::Bogged
        | EntityKind::WitherSkeleton
        | EntityKind::Piglin
        | EntityKind::PiglinBrute
            if !aggressive =>
        {
            [own(hands[0]), own(hands[1])]
        }
        EntityKind::Player | EntityKind::Mannequin => [own(hands[0]), own(hands[1])],
        _ => [false; 2],
    };
    (arms, [false; 2])
}

fn progress(time: f32, start: f32, end: f32) -> f32 {
    if end == start {
        return if time >= end { 1.0 } else { 0.0 };
    }
    ((time - start) / (end - start)).clamp(0.0, 1.0)
}

fn in_quad(x: f32) -> f32 {
    x * x
}

fn in_out_sine(x: f32) -> f32 {
    -((PI * x).cos() - 1.0) / 2.0
}

fn out_back(x: f32) -> f32 {
    1.0 + 2.70158 * (x - 1.0).powi(3) + 1.70158 * (x - 1.0).powi(2)
}

fn out_cubic(x: f32) -> f32 {
    1.0 - (1.0 - x).powi(3)
}

fn in_out_expo(x: f32) -> f32 {
    if x <= 0.0 {
        0.0
    } else if x >= 1.0 {
        1.0
    } else if x < 0.5 {
        2f32.powf(20.0 * x - 10.0) / 2.0
    } else {
        (2.0 - 2f32.powf(-20.0 * x + 10.0)) / 2.0
    }
}

fn in_out_elastic(x: f32) -> f32 {
    if x == 0.0 || x == 1.0 {
        return x;
    }
    let sin = ((20.0 * x - 11.125) * PI * 4.0 / 9.0).sin();
    if x < 0.5 {
        -(2f32.powf(20.0 * x - 10.0) * sin) / 2.0
    } else {
        2f32.powf(-20.0 * x + 10.0) * sin / 2.0 + 1.0
    }
}

fn out_circ(x: f32) -> f32 {
    (1.0 - (x - 1.0) * (x - 1.0)).max(0.0).sqrt()
}

fn in_circ(x: f32) -> f32 {
    1.0 - (1.0 - x * x).max(0.0).sqrt()
}

/// Vanilla `SpearAnimations.UseParams`: where a charge is, `time` ticks into
/// it: raised over the weapon's delay, swaying toward the end of its
/// dismount window, lowered toward the end of its knockback one and raised
/// back as its damage one closes.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct UseParams {
    raise: f32,
    raise_start: f32,
    raise_middle: f32,
    raise_end: f32,
    sway: f32,
    lower: f32,
    raise_back: f32,
    sway_intensity: f32,
    sway_slow: f32,
    sway_fast: f32,
}

impl UseParams {
    fn new(weapon: &KineticTimings, time: f32) -> Self {
        let finish_raising = weapon.delay_ticks as f32;
        let finish_swaying = (weapon.dismount_ticks + weapon.delay_ticks) as f32;
        let finish_lowering = (weapon.knockback_ticks + weapon.delay_ticks) as f32;
        let finish_raising_back = (weapon.damage_ticks + weapon.delay_ticks) as f32;
        let raise = progress(time, 0.0, finish_raising);
        let sway = progress(time, finish_swaying - 20.0, finish_swaying);
        let raise_back = progress(time, finish_raising_back - 5.0, finish_raising_back);
        let sway_intensity = 2.0 * out_circ(sway) - 2.0 * in_circ(raise_back);
        Self {
            raise,
            raise_start: progress(raise, 0.0, 0.5),
            raise_middle: progress(raise, 0.5, 0.8),
            raise_end: progress(raise, 0.8, 1.0),
            sway,
            lower: out_cubic(in_out_elastic(progress(
                time - 20.0,
                finish_lowering - 40.0,
                finish_lowering,
            ))),
            raise_back,
            sway_intensity,
            sway_slow: (time * 19.0).to_radians().sin() * sway_intensity,
            sway_fast: (time * 30.0).to_radians().sin() * sway_intensity,
        }
    }
}

/// Vanilla `SpearAnimations.thirdPersonHandUse`: the arm (`right` or left)
/// pointing the spear where the head looks, then raised and swaying through
/// a charge (`charge`: its ticks and the weapon's timings) in that arm.
fn hand_use_rotation(
    head_x: f32,
    head_y: f32,
    right: bool,
    charge: Option<(f32, &KineticTimings)>,
) -> Vec3 {
    let invert = if right { 1.0 } else { -1.0 };
    let y = (-0.1 * invert + head_y)
        .to_degrees()
        .clamp(-60.0, 60.0)
        .to_radians();
    let x = (-FRAC_PI_2 + head_x + 0.8)
        .to_degrees()
        .clamp(-120.0, 30.0)
        .to_radians();
    let mut rot = Vec3::new(x, y, 0.0);
    if let Some((ticks, weapon)) = charge.filter(|(ticks, _)| *ticks > 0.0) {
        let p = UseParams::new(weapon, ticks);
        rot.y += -invert * p.sway_fast * p.sway_intensity.to_radians();
        rot.z += -invert * p.sway_slow * p.sway_intensity.to_radians() * 0.5;
        rot.x += (-40.0 * p.raise_start
            + 30.0 * p.raise_middle
            + -20.0 * p.raise_end
            + 20.0 * p.lower
            + 10.0 * p.raise_back
            + 0.6 * p.sway_slow * p.sway_intensity)
            .to_radians();
    }
    rot
}

/// Vanilla `SpearAnimations.thirdPersonAttackHand`: the attacking arm drawn
/// back, thrust and eased home over the swing (`attack_time`), radians of
/// `xRot`.
pub fn attack_hand_pitch(attack_time: f32) -> f32 {
    let prepare = in_out_sine(progress(attack_time, 0.0, 0.05));
    let attack = in_quad(progress(attack_time, 0.05, 0.2));
    let retract = in_out_expo(progress(attack_time, 0.4, 1.0));
    (90.0 * prepare - 120.0 * attack + 30.0 * retract).to_radians()
}

/// What [`pose_arms`] reads from the render state.
pub struct ArmInput {
    pub head_x_rot_deg: f32,
    pub local_head_y_rot_deg: f32,
    pub attack_time: f32,
    pub attack_stabs: bool,
    pub is_crouching: bool,
    pub age_in_ticks: f32,
    pub walk_pos: f32,
    pub walk_speed: f32,
}

/// `HumanoidModel.setupAnim` for the arms `pose` holds a spear with, over
/// whatever the kind's own animation gave them: `poseRightArm` /
/// `poseLeftArm`'s SPEAR case, the stab (a right-handed attack arm), the
/// crouch and the idle bob.
// TODO: the lowered arm while swimming or fall flying, and the stab's
// shoulder twist for the kinds that don't already draw it.
pub fn pose_arms(
    model: &BakedEntityModel,
    anim: &mut PartAnim,
    pose: &SpearPose,
    input: &ArmInput,
) {
    let head_x = input.head_x_rot_deg.to_radians();
    let head_y = input.local_head_y_rot_deg.to_radians();
    for (arm, right) in [("right_arm", true), ("left_arm", false)] {
        let side = if right { 1.0 } else { -1.0 };
        let Some(i) = find_part(&model.parts, arm) else {
            continue;
        };
        // `AnimationUtils.bobModelPart`.
        let bob = Vec3::new(
            side * (input.age_in_ticks * 0.067).sin() * 0.05,
            0.0,
            side * ((input.age_in_ticks * 0.09).cos() * 0.05 + 0.05),
        );
        if pose.hanging[usize::from(!right)] {
            // The walk's swing (`ArmPose.EMPTY`).
            let phase = if right { PI } else { 0.0 };
            let swing = (input.walk_pos * 0.6662 + phase).cos() * input.walk_speed;
            *anim.rotation_mut(model, i) = Vec3::new(swing, 0.0, 0.0) + bob;
            continue;
        }
        if !pose.arms[usize::from(!right)] {
            continue;
        }
        let charge = pose
            .using
            .as_ref()
            .filter(|(use_right, _, _)| *use_right == right)
            .map(|(_, ticks, weapon)| (*ticks, weapon));
        let mut rot = hand_use_rotation(head_x, head_y, right, charge);
        if right && input.attack_stabs && input.attack_time > 0.0 {
            rot.x += attack_hand_pitch(input.attack_time);
        }
        if input.is_crouching {
            rot.x += 0.4;
        }
        *anim.rotation_mut(model, i) = rot + bob;
    }
}

/// `Axis.XN` / `Axis.YP` rotations about a pivot (`PoseStack.rotateAround`).
fn rotate_around(rotation: Mat4, pivot: Vec3) -> Mat4 {
    Mat4::from_translation(pivot) * rotation * Mat4::from_translation(-pivot)
}

/// Vanilla `SpearAnimations.thirdPersonAttackItem`: the spear pitched forward
/// and pushed along its shaft through the stab (`attack_time`), in the hand's
/// item frame. `forward_movement` is the main hand's weapon's.
pub fn attack_item_matrix(attack_time: f32, forward_movement: f32) -> Mat4 {
    if attack_time <= 0.0 {
        return Mat4::IDENTITY;
    }
    let attack = in_quad(progress(attack_time, 0.05, 0.2));
    let retract = in_out_expo(progress(attack_time, 0.4, 1.0));
    rotate_around(
        Mat4::from_rotation_x(-(70.0 * (attack - retract)).to_radians()),
        Vec3::new(0.0, -0.125, 0.125),
    ) * Mat4::from_translation(Vec3::new(0.0, forward_movement * (attack - retract), 0.0))
}

/// Vanilla `SpearAnimations.thirdPersonUseItem`: the spear turned out level
/// and pushed forward through a charge `time_held` ticks in, in the `right`
/// (or left) hand's item frame.
// TODO: the jolt when a charge lands (`ticksSinceKineticHitFeedback`).
pub fn use_item_matrix(
    weapon: &KineticTimings,
    time_held: f32,
    attack_time: f32,
    right: bool,
) -> Mat4 {
    if time_held == 0.0 {
        return Mat4::IDENTITY;
    }
    let attack = in_quad(progress(attack_time, 0.05, 0.2));
    let retract = in_out_expo(progress(attack_time, 0.4, 1.0));
    let p = UseParams::new(weapon, time_held);
    let invert = if right { 1.0 } else { -1.0 };
    let raise = 1.0 - out_back(1.0 - p.raise);
    Mat4::from_translation(Vec3::new(
        0.0,
        0.0,
        -weapon.forward_movement * (raise - p.raise_back),
    )) * rotate_around(
        Mat4::from_rotation_x(
            -(70.0 * (p.raise - p.raise_back) - 40.0 * (attack - retract)).to_radians(),
        ),
        Vec3::new(0.0, -0.03125, 0.125),
    ) * rotate_around(
        Mat4::from_rotation_y(
            (invert * 90.0 * (p.raise - p.sway + 3.0 * retract + attack)).to_radians(),
        ),
        Vec3::new(0.0, 0.0, 0.125),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The iron spear's component (vanilla `Items.IRON_SPEAR`'s `spear(...)`
    /// arguments, seconds times 20).
    const IRON: KineticTimings = KineticTimings {
        delay_ticks: 12,
        dismount_ticks: 50,
        knockback_ticks: 135,
        damage_ticks: 225,
        forward_movement: 0.38,
    };

    #[test]
    fn a_held_spear_points_where_the_head_looks() {
        let rot = hand_use_rotation(0.0, 0.0, true, None);
        assert!((rot.x - (-FRAC_PI_2 + 0.8)).abs() < 1e-5);
        assert!((rot.y + 0.1).abs() < 1e-5);
        // Looking far up clamps at 120 degrees.
        let up = hand_use_rotation(-PI / 2.0, 0.0, true, None);
        assert!((up.x + 120f32.to_radians()).abs() < 1e-4);
    }

    #[test]
    fn a_charge_raises_the_arm_and_levels_the_spear() {
        let idle = hand_use_rotation(0.0, 0.0, true, None);
        let charging = hand_use_rotation(0.0, 0.0, true, Some((IRON.delay_ticks as f32, &IRON)));
        // Raised: -40 + 30 - 20 degrees of pitch once the delay is up.
        assert!((charging.x - idle.x + 30f32.to_radians()).abs() < 1e-4);
        // The spear swings 70 degrees forward, out of its upright hold.
        let m = use_item_matrix(&IRON, IRON.delay_ticks as f32, 0.0, true);
        assert_ne!(m, Mat4::IDENTITY);
        assert_eq!(use_item_matrix(&IRON, 0.0, 0.0, true), Mat4::IDENTITY);
    }

    #[test]
    fn zombies_hold_a_spear_in_both_arms() {
        let spear = Some((true, true));
        assert_eq!(
            spear_arms(EntityKind::Zombie, true, [spear, None], false),
            ([true, true], [false, false])
        );
        assert_eq!(
            spear_arms(EntityKind::Piglin, false, [spear, None], false).0,
            [true, false]
        );
        // A zombified piglin's other arm hangs rather than reaching out.
        assert_eq!(
            spear_arms(EntityKind::ZombifiedPiglin, true, [spear, None], false),
            ([true, false], [false, true])
        );
        // A sword whacks: no spear pose.
        let sword = Some((false, false));
        assert_eq!(
            spear_arms(EntityKind::Zombie, false, [sword, None], true),
            ([false, false], [false, false])
        );
        assert_eq!(
            spear_arms(EntityKind::Skeleton, true, [spear, None], false).0,
            [false, false]
        );
    }
}
