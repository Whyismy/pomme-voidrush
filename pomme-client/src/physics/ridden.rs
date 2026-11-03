//! A ridden horse driven by its rider's client, as vanilla's is: the horse is
//! `isLocalInstanceAuthoritative` for the player in its saddle, so the server
//! stops moving it and waits for the client's `move_vehicle`.
//! `LivingEntity.travelRidden` with `AbstractHorse`'s ridden input, rotation
//! and jump, over `travelInAir`'s ground and air movement.
// TODO: swimming horses (`travelInFluid`), climbing, and the camel's dash.

use glam::DVec3;

use crate::attribute::AttributeKind;
use crate::entity::LivingEntity;
use crate::entity::components::LookDirection;
use crate::physics::aabb::Aabb;
use crate::physics::block_shape::CollisionContext;
use crate::physics::collision::{CollisionWorld, resolve_collision};

/// Vanilla `AbstractHorse`'s step height (`createBaseHorseAttributes`).
const HORSE_STEP_HEIGHT: f64 = 1.0;
const GRAVITY: f64 = 0.08;

/// Vanilla `AbstractHorse.getPlayerJumpPendingScale`: the jump bar's charge
/// (0 to 100) as the jump's strength.
pub fn jump_pending_scale(amount: i32) -> f32 {
    if amount >= 90 {
        1.0
    } else {
        0.4 + 0.4 * amount.max(0) as f32 / 90.0
    }
}

/// One tick of the horse under its rider: `forward` and `strafe` are the
/// rider's `zza` and `xxa`, `jump_pending` the charged jump the rider
/// released (`playerJumpPendingScale`), spent when the horse leaves the
/// ground.
pub fn tick(
    horse: &mut LivingEntity,
    rider_look: LookDirection,
    forward: f32,
    strafe: f32,
    jump_pending: &mut f32,
    world: CollisionWorld<'_>,
) {
    horse.prev_position = horse.position;
    horse.prev_look_dir = horse.look_dir;
    horse.prev_body_y_rot_deg = horse.body_y_rot_deg;
    horse.prev_head_y_rot_deg = horse.head_y_rot_deg;

    // `AbstractHorse.tickRidden`: the horse faces where its rider looks.
    let y_rot = rider_look.y_rot_deg();
    horse.look_dir = LookDirection::new(y_rot, rider_look.x_rot_deg() * 0.5);
    horse.body_y_rot_deg = y_rot;
    horse.head_y_rot_deg = y_rot;

    // `getRiddenInput`: half speed sideways, a quarter backwards.
    let strafe = strafe * 0.5;
    let forward = if forward <= 0.0 {
        forward * 0.25
    } else {
        forward
    };

    if horse.on_ground && *jump_pending > 0.0 {
        // `executeRidersJump`.
        let power = horse
            .attributes
            .value(AttributeKind::JumpStrength)
            .unwrap_or(0.7)
            * f64::from(*jump_pending);
        horse.velocity.y = power;
        if forward > 0.0 {
            let (sin, cos) = y_rot.to_radians().sin_cos();
            horse.velocity.x += f64::from(-0.4 * sin * *jump_pending);
            horse.velocity.z += f64::from(0.4 * cos * *jump_pending);
        }
        *jump_pending = 0.0;
    }

    // `travelInAir`: friction from the block under the horse.
    let speed = horse
        .attributes
        .value(AttributeKind::MovementSpeed)
        .unwrap_or(0.225) as f32;
    let block_friction = if horse.on_ground {
        let below = DVec3::new(
            horse.position.x,
            horse.position.y - 0.500_001,
            horse.position.z,
        )
        .floor()
        .as_ivec3();
        let state = world.chunks.get_block_state(below.x, below.y, below.z);
        world
            .block_state_table
            .block_state_data(state)
            .movement_friction()
    } else {
        1.0
    };
    let friction = block_friction * 0.91;
    // `getFrictionInfluencedSpeed`; a ridden horse's air speed is a tenth
    // of its speed (`getFlyingSpeed` with a player controlling).
    let accel = if horse.on_ground {
        speed * (0.216_000_02 / (block_friction * block_friction * block_friction))
    } else {
        speed * 0.1
    };
    horse.velocity += move_relative(forward, strafe, accel, y_rot);

    let dims = crate::entity::living_entity_dimensions(horse);
    let aabb = Aabb::from_center(
        *horse.position,
        f64::from(dims.width) * 0.5,
        f64::from(dims.height) * 0.5,
    );
    let ctx = CollisionContext::entity(horse.position.y, false, false);
    let (delta, on_ground) = resolve_collision(
        world,
        aabb,
        horse.velocity.into(),
        HORSE_STEP_HEIGHT,
        horse.on_ground,
        &ctx,
    );
    // `Entity.move`: a blocked axis stops.
    if (delta.x - horse.velocity.x).abs() > 1.0e-7 {
        horse.velocity.x = 0.0;
    }
    if (delta.z - horse.velocity.z).abs() > 1.0e-7 {
        horse.velocity.z = 0.0;
    }
    if (delta.y - horse.velocity.y).abs() > 1.0e-7 {
        horse.velocity.y = 0.0;
    }
    horse.position = (*horse.position + delta).into();
    horse.on_ground = on_ground;

    horse.velocity.y -= GRAVITY;
    horse.velocity.x *= f64::from(friction);
    horse.velocity.z *= f64::from(friction);
    horse.velocity.y *= 0.98;
    // `aiStep` drops what's too small to see.
    if horse.velocity.x.abs() < 0.003 {
        horse.velocity.x = 0.0;
    }
    if horse.velocity.y.abs() < 0.003 {
        horse.velocity.y = 0.0;
    }
    if horse.velocity.z.abs() < 0.003 {
        horse.velocity.z = 0.0;
    }
    // Nothing else moves it this tick.
    horse.interp_steps = 0;
}

/// Vanilla `Entity.getInputVector`: the input, capped to length 1, scaled by
/// `speed` and turned to face `y_rot`.
fn move_relative(forward: f32, strafe: f32, speed: f32, y_rot: f32) -> DVec3 {
    let input = DVec3::new(f64::from(strafe), 0.0, f64::from(forward));
    let len_sq = input.length_squared();
    if len_sq < 1.0e-7 {
        return DVec3::ZERO;
    }
    let input = if len_sq > 1.0 {
        input.normalize()
    } else {
        input
    } * f64::from(speed);
    let (sin, cos) = (
        y_rot.to_radians().sin() as f64,
        y_rot.to_radians().cos() as f64,
    );
    DVec3::new(
        input.x * cos - input.z * sin,
        input.y,
        input.z * cos + input.x * sin,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_jump_bar_scales_the_jump() {
        assert_eq!(jump_pending_scale(100), 1.0);
        assert_eq!(jump_pending_scale(90), 1.0);
        assert!((jump_pending_scale(45) - 0.6).abs() < 1.0e-6);
        assert!((jump_pending_scale(-5) - 0.4).abs() < 1.0e-6);
    }

    #[test]
    fn a_ridden_horse_goes_where_its_rider_looks() {
        use azalea_registry::builtin::EntityKind;

        use crate::entity::components::Position;
        use crate::world::block::BlockStateTable;
        use crate::world::chunk::ChunkStore;

        let chunks = ChunkStore::new(2);
        let mut horse = LivingEntity::new(
            EntityKind::Horse,
            Position::new(0.5, 80.0, 0.5),
            LookDirection::new(0.0, 0.0),
            uuid::Uuid::nil(),
        );
        let mut jump = 0.0;
        for _ in 0..5 {
            tick(
                &mut horse,
                LookDirection::new(90.0, 20.0),
                1.0,
                0.0,
                &mut jump,
                CollisionWorld::new(&chunks, BlockStateTable::native()),
            );
        }
        // Facing west, in the air: it drifts -x and falls.
        assert!(horse.position.x < 0.5 && horse.position.y < 80.0);
        assert_eq!(horse.body_y_rot_deg, 90.0);
        assert_eq!(horse.look_dir.x_rot_deg(), 10.0);
    }

    #[test]
    fn forward_input_moves_where_the_horse_faces() {
        // Facing south (yaw 0) goes +z; facing west (90) goes -x.
        let south = move_relative(1.0, 0.0, 1.0, 0.0);
        assert!(south.z > 0.99 && south.x.abs() < 1.0e-6);
        let west = move_relative(1.0, 0.0, 1.0, 90.0);
        assert!(west.x < -0.99 && west.z.abs() < 1.0e-6);
    }
}
