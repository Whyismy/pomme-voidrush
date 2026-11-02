//! Vanilla `setupAnim` for mobs drawn from vanilla's own model layers
//! ([`super::exported`]): what each kind does beyond the generic head look
//! and limb swing ([`super::compute_generic_anim`]), part by part.

use std::f32::consts::{FRAC_PI_6, PI};

use azalea_registry::builtin::EntityKind;
use glam::Vec3;

use super::{BakedEntityModel, PartAnim, find_part};

/// What the kinds' animations read from the render state.
pub struct MobAnimInput {
    pub age_in_ticks: f32,
    pub walk_pos: f32,
    pub walk_speed: f32,
    /// Head yaw relative to the body, degrees.
    pub local_head_y_rot_deg: f32,
    pub aggressive: bool,
    pub attack_time: f32,
}

/// Applies `kind`'s own `setupAnim` over the generic pose in `anim`.
pub fn apply(
    kind: EntityKind,
    model: &BakedEntityModel,
    anim: &mut PartAnim,
    input: &MobAnimInput,
) {
    let mut parts = Parts { model, anim };
    match kind {
        EntityKind::Blaze => blaze(&mut parts, input),
        EntityKind::Ghast => ghast(&mut parts, input),
        EntityKind::Phantom => phantom(&mut parts, input),
        EntityKind::Vex => vex(&mut parts, input),
        EntityKind::Piglin | EntityKind::PiglinBrute => piglin(&mut parts, input),
        EntityKind::ZombifiedPiglin => {
            piglin(&mut parts, input);
            zombie_arms(&mut parts, input);
        }
        EntityKind::Hoglin | EntityKind::Zoglin => hoglin(&mut parts, input),
        EntityKind::Ravager => ravager(&mut parts, input),
        _ => {}
    }
}

/// A model's parts by vanilla name, posed into `anim`.
struct Parts<'a> {
    model: &'a BakedEntityModel,
    anim: &'a mut PartAnim,
}

impl Parts<'_> {
    /// The part's rotation (radians, x y z), from its rest pose if the
    /// generic pass left it alone.
    fn rot(&mut self, name: &str) -> Option<&mut Vec3> {
        let i = find_part(&self.model.parts, name)?;
        Some(self.anim.rotation_mut(self.model, i))
    }

    /// Moves the part's pivot to `pos` (model pixels, vanilla's `x y z`).
    fn set_pos(&mut self, name: &str, pos: Vec3) {
        let Some(i) = find_part(&self.model.parts, name) else {
            return;
        };
        let delta = pos - self.model.parts[i].offset;
        self.anim.translation.retain(|&(j, _)| j != i);
        self.anim.translation.push((i, delta));
    }
}

/// `BlazeModel.setupAnim`: three rings of four rods circling at different
/// rates and heights, each bobbing.
fn blaze(p: &mut Parts, input: &MobAnimInput) {
    let age = input.age_in_ticks;
    let rings: [(std::ops::Range<usize>, f32, f32, f32, f32, f32); 3] = [
        (0..4, age * PI * -0.1, 9.0, -2.0, 2.0, 0.25),
        (4..8, PI / 4.0 + age * PI * 0.03, 7.0, 2.0, 2.0, 0.25),
        (8..12, 0.471_238_94 + age * PI * -0.05, 5.0, 11.0, 1.5, 0.5),
    ];
    for (rods, start, radius, height, spread, rate) in rings {
        let mut angle = start;
        for i in rods {
            let bob = ((i as f32 * spread + age) * rate).cos();
            p.set_pos(
                &format!("part{i}"),
                Vec3::new(angle.cos() * radius, height + bob, angle.sin() * radius),
            );
            angle += 1.0;
        }
    }
}

/// `GhastModel.setupAnim`: each tentacle sways on its own phase.
fn ghast(p: &mut Parts, input: &MobAnimInput) {
    for i in 0..9 {
        if let Some(rot) = p.rot(&format!("tentacle{i}")) {
            rot.x = 0.2 * (input.age_in_ticks * 0.3 + i as f32).sin() + 0.4;
        }
    }
}

/// `PhantomModel.setupAnim`: the wings flap and the tail beats with them;
/// the head stays put (the renderer pitches the whole body).
// TODO: the per-entity flap offset (`getUniqueFlapTickOffset`, id * 3).
fn phantom(p: &mut Parts, input: &MobAnimInput) {
    let flap = (input.age_in_ticks * 7.448_451).to_radians();
    let wing = (flap.cos() * 16.0).to_radians();
    let tail = -(5.0 + (flap * 2.0).cos() * 5.0).to_radians();
    for (name, z) in [
        ("left_wing_base", wing),
        ("left_wing_tip", wing),
        ("right_wing_base", -wing),
        ("right_wing_tip", -wing),
    ] {
        if let Some(rot) = p.rot(name) {
            rot.z = z;
        }
    }
    for name in ["tail_base", "tail_tip"] {
        if let Some(rot) = p.rot(name) {
            rot.x = tail;
        }
    }
    if let Some(i) = find_part(&p.model.parts, "head") {
        p.anim.rotation.retain(|&(j, _)| j != i);
    }
}

/// `VexModel.setupAnim`: arms held out and swaying, wings fluttering, the
/// body leaning forward.
// TODO: the charging pose (`Vex.isCharging`, not tracked yet).
fn vex(p: &mut Parts, input: &MobAnimInput) {
    let age = input.age_in_ticks;
    let sway = (age * 5.5).to_radians().cos() * 0.1;
    for (name, z) in [
        ("right_arm", PI / 5.0 + sway),
        ("left_arm", -(PI / 5.0 + sway)),
    ] {
        let rest = find_part(&p.model.parts, name).map(|i| p.model.parts[i].default_rotation);
        if let (Some(rest), Some(rot)) = (rest, p.rot(name)) {
            *rot = Vec3::new(rest.x, rest.y, z);
        }
    }
    if let Some(rot) = p.rot("body") {
        rot.x = 0.157_079_64;
    }
    let wing_y = 1.099_557_4 + (age * 45.836_624).to_radians().cos() * 16.2_f32.to_radians();
    if let Some(rot) = p.rot("left_wing") {
        *rot = Vec3::new(0.471_238_88, wing_y, -0.471_238_88);
    }
    if let Some(rot) = p.rot("right_wing") {
        *rot = Vec3::new(0.471_238_88, -wing_y, 0.471_238_88);
    }
}

/// `PiglinModel.setupAnim` over `HumanoidModel`'s: the ears wobble with
/// time and walking, and the arms bob idly (`AnimationUtils.bobArms`).
fn piglin(p: &mut Parts, input: &MobAnimInput) {
    let g = input.age_in_ticks * 0.1 + input.walk_pos * 0.5;
    let h = 0.08 + input.walk_speed * 0.4;
    if let Some(rot) = p.rot("left_ear") {
        rot.z = -FRAC_PI_6 - (g * 1.2).cos() * h;
    }
    if let Some(rot) = p.rot("right_ear") {
        rot.z = FRAC_PI_6 + g.cos() * h;
    }
    for (name, side) in [("right_arm", 1.0), ("left_arm", -1.0)] {
        if let Some(rot) = p.rot(name) {
            rot.z += side * ((input.age_in_ticks * 0.09).cos() * 0.05 + 0.05);
            rot.x += side * (input.age_in_ticks * 0.067).sin() * 0.05;
        }
    }
}

/// `AnimationUtils.animateZombieArms` (zombified piglins): arms held out,
/// higher when aggressive, with the attack swing.
fn zombie_arms(p: &mut Parts, input: &MobAnimInput) {
    let drop = if input.aggressive {
        -PI / 1.5
    } else {
        -PI / 2.25
    };
    let attack_y = (input.attack_time * PI).sin();
    let attack_x = ((1.0 - (1.0 - input.attack_time) * (1.0 - input.attack_time)) * PI).sin();
    let swing = attack_y * 1.2 - attack_x * 0.4;
    for (name, side) in [("right_arm", 1.0), ("left_arm", -1.0)] {
        if let Some(rot) = p.rot(name) {
            let bob_z = side * ((input.age_in_ticks * 0.09).cos() * 0.05 + 0.05);
            let bob_x = side * (input.age_in_ticks * 0.067).sin() * 0.05;
            *rot = Vec3::new(drop + swing + bob_x, side * (-0.1 + attack_y * 0.6), bob_z);
        }
    }
}

/// `HoglinModel.setupAnim`: the head hangs low and only turns, the ears
/// flap with the gait, and the legs swing at their own rate.
// TODO: the head-butt (`attackAnimationRemainingTicks`).
fn hoglin(p: &mut Parts, input: &MobAnimInput) {
    let (pos, speed) = (input.walk_pos, input.walk_speed);
    if let Some(rot) = p.rot("right_ear") {
        rot.z = -0.698_131_7 - speed * pos.sin();
    }
    if let Some(rot) = p.rot("left_ear") {
        rot.z = 0.698_131_7 + speed * pos.sin();
    }
    if let Some(rot) = p.rot("head") {
        *rot = Vec3::new(0.872_664_63, input.local_head_y_rot_deg.to_radians(), 0.0);
    }
    let front_right = pos.cos() * 1.2 * speed;
    let front_left = (pos + PI).cos() * 1.2 * speed;
    for (name, x) in [
        ("right_front_leg", front_right),
        ("left_front_leg", front_left),
        ("right_hind_leg", front_left),
        ("left_hind_leg", front_right),
    ] {
        if let Some(rot) = p.rot(name) {
            rot.x = x;
        }
    }
}

/// `RavagerModel.setupAnim`'s gait: a short, heavy leg swing.
// TODO: the stun shake, roar and attack jaw.
fn ravager(p: &mut Parts, input: &MobAnimInput) {
    let h = 0.4 * input.walk_speed;
    let swing = |phase: f32| (input.walk_pos * 0.6662 + phase).cos() * h;
    for (name, x) in [
        ("right_hind_leg", swing(0.0)),
        ("left_hind_leg", swing(PI)),
        ("right_front_leg", swing(PI)),
        ("left_front_leg", swing(0.0)),
    ] {
        if let Some(rot) = p.rot(name) {
            rot.x = x;
        }
    }
}

/// `EndCrystalModel.setupAnim`: the outer glass bobs (`getY`), and the
/// glass and core spin 3 degrees a tick, tilted 60 degrees about the
/// (1, 0, 1) diagonal.
pub fn end_crystal(model: &BakedEntityModel, age_in_ticks: f32) -> PartAnim {
    let mut anim = PartAnim::default();
    let spin = glam::Quat::from_rotation_y((age_in_ticks * 3.0).to_radians());
    let tilt = glam::Quat::from_axis_angle(
        Vec3::new(1.0, 0.0, 1.0).normalize(),
        std::f32::consts::FRAC_PI_3,
    );
    let euler = |q: glam::Quat| {
        let (z, y, x) = q.to_euler(glam::EulerRot::ZYX);
        Vec3::new(x, y, z)
    };
    // `EndCrystalRenderer.getY`, in model pixels.
    let f = (age_in_ticks * 0.2).sin() / 2.0 + 0.5;
    let bob = ((f * f + f) * 0.4 - 1.4) * 16.0;
    if let Some(i) = find_part(&model.parts, "outer_glass") {
        anim.rotation.push((i, euler(spin * tilt)));
        // Pomme's crystal frame stands the base on the feet, which puts
        // the glass's 24-pixel pivot a block (8 pixels at twice the size)
        // higher than vanilla draws it.
        anim.translation
            .push((i, Vec3::new(0.0, bob / 2.0 - 8.0, 0.0)));
    }
    for name in ["inner_glass", "cube"] {
        if let Some(i) = find_part(&model.parts, name) {
            anim.rotation.push((i, euler(tilt * spin)));
        }
    }
    anim
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::entity_model::{bake_exported, exported_data};

    fn input(age: f32) -> MobAnimInput {
        MobAnimInput {
            age_in_ticks: age,
            walk_pos: 0.0,
            walk_speed: 0.0,
            local_head_y_rot_deg: 0.0,
            aggressive: false,
            attack_time: 0.0,
        }
    }

    /// The blaze's rods circle: the top ring's first rod is 9 pixels out on
    /// +x at tick 0 and has moved a quarter turn by tick 5.
    #[test]
    fn blaze_rods_orbit() {
        let model = bake_exported(exported_data::BLAZE);
        let rod = find_part(&model.parts, "part0").unwrap();
        let at = |age| {
            let mut anim = PartAnim::default();
            apply(EntityKind::Blaze, &model, &mut anim, &input(age));
            let (_, delta) = anim.translation.iter().find(|&&(i, _)| i == rod).unwrap();
            model.parts[rod].offset + *delta
        };
        let start = at(0.0);
        assert!(
            (start.x - 9.0).abs() < 1e-4 && start.z.abs() < 1e-4,
            "{start}"
        );
        let later = at(5.0);
        assert!(
            later.x.abs() < 1e-3 && (later.z + 9.0).abs() < 1e-3,
            "{later}"
        );
    }

    /// The vex's wings mirror each other.
    #[test]
    fn vex_wings_mirror() {
        let model = bake_exported(exported_data::VEX);
        let mut anim = PartAnim::default();
        apply(EntityKind::Vex, &model, &mut anim, &input(7.0));
        let rot = |name| {
            let i = find_part(&model.parts, name).unwrap();
            anim.rotation.iter().find(|&&(j, _)| j == i).unwrap().1
        };
        let (left, right) = (rot("left_wing"), rot("right_wing"));
        assert_eq!(left.y, -right.y);
        assert_eq!(left.z, -right.z);
    }
}
