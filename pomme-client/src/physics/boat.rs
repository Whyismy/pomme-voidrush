//! A boat rowed by the player in it, as vanilla's is: the boat is
//! `isLocalInstanceAuthoritative` for the player at its oars, so the server
//! stops moving it and waits for the client's `move_vehicle`.
//! `AbstractBoat.tick`'s `getStatus`, `floatBoat` and `controlBoat`, then
//! `Entity.move`.
// TODO: a bubble column's pull on the boat is the server's to give.

use glam::DVec3;

use crate::physics::aabb::Aabb;
use crate::physics::block_shape::CollisionContext;
use crate::physics::collision::{
    CollisionWorld, collect_block_aabbs, collide_along_axes, no_collision, state_boxes,
};
use crate::position::BlockPos;
use crate::world::block::FluidKind;

/// Every boat's and raft's box (`EntityType.*_BOAT`'s `sized`).
pub const WIDTH: f64 = 1.375;
pub const HEIGHT: f64 = 0.5625;
/// `AbstractBoat.getDefaultGravity`.
const GRAVITY: f64 = 0.04;

/// `AbstractBoat.Status`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Status {
    InWater,
    UnderWater,
    UnderFlowingWater,
    OnLand,
    InAir,
}

/// The rider's keys (`AbstractBoat.setInput`).
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Keys {
    pub left: bool,
    pub right: bool,
    pub up: bool,
    pub down: bool,
}

/// What the boat's own tick moves.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Boat {
    pub position: DVec3,
    pub velocity: DVec3,
    pub y_rot: f32,
}

/// The state a boat keeps between ticks while this client rows it.
#[derive(Default)]
pub struct Drive {
    /// The boat this is the state of.
    pub vehicle: Option<i32>,
    status: Option<Status>,
    water_level: f64,
    land_friction: f32,
    /// The fall speed the last move began with (`lastYd`).
    last_yd: f64,
    /// Degrees the boat turns a tick (`deltaRotation`).
    pub delta_rotation: f32,
    /// The keys `LocalPlayer.rideTick` set after the boat's last tick.
    keys: Keys,
    pub on_ground: bool,
}

impl Drive {
    pub fn of(vehicle: i32) -> Self {
        Self {
            vehicle: Some(vehicle),
            ..Self::default()
        }
    }
}

fn bounding_box(position: DVec3) -> Aabb {
    Aabb::from_center(position, WIDTH * 0.5, HEIGHT * 0.5)
}

/// `FluidState.getHeight` of the water at a block: a full block under more
/// water, else its own level. `None` where there is no water.
fn water_height(world: CollisionWorld<'_>, x: i32, y: i32, z: i32) -> Option<(f32, bool)> {
    let fluid_at = |y| {
        world
            .block_state_table
            .block_state_data(world.chunks.get_block_state(x, y, z))
            .fluid()
    };
    let fluid = fluid_at(y);
    if fluid.kind != FluidKind::Water {
        return None;
    }
    let height = if fluid_at(y + 1).kind == FluidKind::Water {
        1.0
    } else {
        fluid.height()
    };
    Some((height, fluid.amount == 8 && !fluid.falling))
}

/// The cells a box spans along one axis, as vanilla's `floor(min)..ceil(max)`.
fn cells(min: f64, max: f64) -> std::ops::Range<i32> {
    min.floor() as i32..max.ceil() as i32
}

/// `AbstractBoat.isUnderwater`: water over the top of the boat.
fn under_water(world: CollisionWorld<'_>, aabb: &Aabb) -> Option<Status> {
    let top = aabb.max.y + 0.001;
    let mut under = false;
    for x in cells(aabb.min.x, aabb.max.x) {
        for y in cells(aabb.max.y, top) {
            for z in cells(aabb.min.z, aabb.max.z) {
                let Some((height, source)) = water_height(world, x, y, z) else {
                    continue;
                };
                if top < f64::from(y as f32 + height) {
                    if !source {
                        return Some(Status::UnderFlowingWater);
                    }
                    under = true;
                }
            }
        }
    }
    under.then_some(Status::UnderWater)
}

/// `AbstractBoat.checkInWater`: whether the boat's floor is in water, and
/// the highest surface it touches.
fn check_in_water(world: CollisionWorld<'_>, aabb: &Aabb) -> (bool, f64) {
    let mut in_water = false;
    let mut level = f64::MIN;
    for x in cells(aabb.min.x, aabb.max.x) {
        for y in cells(aabb.min.y, aabb.min.y + 0.001) {
            for z in cells(aabb.min.z, aabb.max.z) {
                if let Some((height, _)) = water_height(world, x, y, z) {
                    let surface = f64::from(y as f32 + height);
                    level = level.max(surface);
                    in_water |= aabb.min.y < surface;
                }
            }
        }
    }
    (in_water, level)
}

/// `AbstractBoat.getGroundFriction`: the mean slipperiness of the blocks
/// the boat's floor rests on, or zero when it rests on none.
fn ground_friction(world: CollisionWorld<'_>, aabb: &Aabb) -> f32 {
    let floor = Aabb::new(
        DVec3::new(aabb.min.x, aabb.min.y - 0.001, aabb.min.z),
        DVec3::new(aabb.max.x, aabb.min.y, aabb.max.z),
    );
    let (x0, x1) = (
        floor.min.x.floor() as i32 - 1,
        floor.max.x.ceil() as i32 + 1,
    );
    let (y0, y1) = (
        floor.min.y.floor() as i32 - 1,
        floor.max.y.ceil() as i32 + 1,
    );
    let (z0, z1) = (
        floor.min.z.floor() as i32 - 1,
        floor.max.z.ceil() as i32 + 1,
    );
    let ctx = CollisionContext::entity(aabb.min.y, false, false);
    let mut boxes = Vec::new();
    let mut friction = 0.0_f32;
    let mut count = 0_u32;
    for x in x0..x1 {
        for z in z0..z1 {
            // The corners of the ring round the box are left out, and its
            // sides only count at the box's own heights.
            let edges = u8::from(x == x0 || x == x1 - 1) + u8::from(z == z0 || z == z1 - 1);
            if edges == 2 {
                continue;
            }
            for y in y0..y1 {
                if edges > 0 && (y == y0 || y == y1 - 1) {
                    continue;
                }
                let state = world.chunks.get_block_state(x, y, z);
                let data = world.block_state_table.block_state_data(state);
                if data.id() == "lily_pad" {
                    continue;
                }
                boxes.clear();
                state_boxes(
                    world.block_state_table,
                    state,
                    BlockPos::new(x, y, z),
                    &ctx,
                    DVec3::ZERO,
                    &mut boxes,
                );
                if boxes.iter().any(|b| b.intersects(&floor)) {
                    friction += data.movement_friction();
                    count += 1;
                }
            }
        }
    }
    if count == 0 {
        0.0
    } else {
        friction / count as f32
    }
}

/// `Entity.updateFluidHeightAndDoFluidPushing` for the water a boat sits
/// in: what its current adds to the boat's speed this tick.
fn current_push(world: CollisionWorld<'_>, aabb: &Aabb, velocity: DVec3) -> DVec3 {
    let aabb = aabb.deflate(0.001);
    let mut depth = 0.0_f64;
    let mut flow = DVec3::ZERO;
    let mut count = 0_u32;
    for x in cells(aabb.min.x, aabb.max.x) {
        for y in cells(aabb.min.y, aabb.max.y) {
            for z in cells(aabb.min.z, aabb.max.z) {
                let Some((height, _)) = water_height(world, x, y, z) else {
                    continue;
                };
                let surface = f64::from(y as f32 + height);
                if surface < aabb.min.y {
                    continue;
                }
                depth = depth.max(surface - aabb.min.y);
                let fluid = world
                    .block_state_table
                    .block_state_data(world.chunks.get_block_state(x, y, z))
                    .fluid();
                let mut here = crate::player::fluid_flow_at(
                    |x, y, z| world.chunks.get_block_state(x, y, z),
                    world.block_state_table,
                    x,
                    y,
                    z,
                    fluid,
                );
                // Shallow water pushes by its depth so far.
                if depth < 0.4 {
                    here *= depth;
                }
                flow += here;
                count += 1;
            }
        }
    }
    // `Vec3.normalize`, which gives up on anything too short.
    let unit = |v: DVec3| {
        let length = v.length();
        if length < f64::from(1.0e-5_f32) {
            DVec3::ZERO
        } else {
            v / length
        }
    };
    if count == 0 || flow.length() <= 0.0 {
        return DVec3::ZERO;
    }
    let mut push = unit(flow / f64::from(count)) * 0.014;
    // A boat at rest is always nudged at least this much.
    const LEAST: f64 = 0.004_500_000_000_000_000_5;
    if velocity.x.abs() < 0.003 && velocity.z.abs() < 0.003 && push.length() < LEAST {
        push = unit(push) * LEAST;
    }
    push
}

impl Drive {
    /// `AbstractBoat.getStatus`.
    fn status(&mut self, world: CollisionWorld<'_>, aabb: &Aabb) -> Status {
        if let Some(status) = under_water(world, aabb) {
            self.water_level = aabb.max.y;
            return status;
        }
        let (in_water, level) = check_in_water(world, aabb);
        self.water_level = level;
        if in_water {
            return Status::InWater;
        }
        let friction = ground_friction(world, aabb);
        if friction > 0.0 {
            self.land_friction = friction;
            Status::OnLand
        } else {
            Status::InAir
        }
    }

    /// `AbstractBoat.getWaterLevelAbove`: the surface of the water the boat
    /// fell into.
    fn water_level_above(&self, world: CollisionWorld<'_>, aabb: &Aabb) -> f32 {
        let top = (aabb.max.y - self.last_yd).ceil() as i32;
        'layers: for y in aabb.max.y.floor() as i32..top {
            let mut height = 0.0_f32;
            for x in cells(aabb.min.x, aabb.max.x) {
                for z in cells(aabb.min.z, aabb.max.z) {
                    if let Some((h, _)) = water_height(world, x, y, z) {
                        height = height.max(h);
                    }
                    if height >= 1.0 {
                        continue 'layers;
                    }
                }
            }
            if height < 1.0 {
                return y as f32 + height;
            }
        }
        (top + 1) as f32
    }

    /// `AbstractBoat.floatBoat`: gravity, the water's lift and the drag of
    /// whatever the boat is on.
    fn float(
        &mut self,
        boat: &mut Boat,
        old: Option<Status>,
        status: Status,
        world: CollisionWorld<'_>,
    ) {
        let mut fall = -GRAVITY;
        let mut lift = 0.0_f64;
        if old == Some(Status::InAir) && status != Status::InAir && status != Status::OnLand {
            // It landed in water: straight to floating on the surface.
            self.water_level = boat.position.y + HEIGHT;
            let aabb = bounding_box(boat.position);
            let y = f64::from(self.water_level_above(world, &aabb) - HEIGHT as f32) + 0.101;
            let there = aabb.offset(DVec3::new(0.0, y - boat.position.y, 0.0));
            if no_collision(world, &there, &CollisionContext::entity(y, false, false)) {
                boat.position.y = y;
                boat.velocity.y = 0.0;
                self.last_yd = 0.0;
            }
            self.status = Some(Status::InWater);
            return;
        }
        let drag: f32 = match status {
            Status::InWater => {
                lift = (self.water_level - boat.position.y) / HEIGHT;
                0.9
            }
            Status::UnderFlowingWater => {
                fall = -7.0e-4;
                0.9
            }
            Status::UnderWater => {
                lift = f64::from(0.01_f32);
                0.45
            }
            Status::InAir => 0.9,
            Status::OnLand => self.land_friction,
        };
        boat.velocity.x *= f64::from(drag);
        boat.velocity.z *= f64::from(drag);
        boat.velocity.y += fall;
        self.delta_rotation *= drag;
        if lift > 0.0 {
            boat.velocity.y = (boat.velocity.y + lift * (GRAVITY / 0.65)) * 0.75;
        }
    }

    /// `AbstractBoat.controlBoat`: the oars turn and push the boat. The
    /// paddles that row for it, left then right.
    fn control(&mut self, boat: &mut Boat) -> [bool; 2] {
        let Keys {
            left,
            right,
            up,
            down,
        } = self.keys;
        let mut push = 0.0_f32;
        if left {
            self.delta_rotation -= 1.0;
        }
        if right {
            self.delta_rotation += 1.0;
        }
        if right != left && !up && !down {
            push += 0.005;
        }
        boat.y_rot += self.delta_rotation;
        if up {
            push += 0.04;
        }
        if down {
            push -= 0.005;
        }
        let yaw = boat.y_rot.to_radians();
        boat.velocity.x += f64::from((-yaw).sin() * push);
        boat.velocity.z += f64::from(yaw.cos() * push);
        [right && !left || up, left && !right || up]
    }
}

/// One tick of the boat under its rower: it floats, answers the keys set
/// after its last tick, and moves, stopped by blocks and by `obstacles`,
/// the boxes of what a boat can't pass through
/// (`AbstractBoat.canVehicleCollide`: other boats and whatever can be
/// pushed, its own riders aside). `keys` are the rider's now, for the
/// next. The paddles rowing, left then right.
pub fn tick(
    boat: &mut Boat,
    drive: &mut Drive,
    keys: Keys,
    world: CollisionWorld<'_>,
    obstacles: &[Aabb],
) -> [bool; 2] {
    let old = drive.status;
    let status = drive.status(world, &bounding_box(boat.position));
    drive.status = Some(status);
    // `Entity.baseTick`: the current it floats in carries it.
    boat.velocity += current_push(world, &bounding_box(boat.position), boat.velocity);
    drive.float(boat, old, status, world);
    let paddles = drive.control(boat);

    // `Entity.move`: what the blocks let through; a blocked axis stops.
    let ctx = CollisionContext::entity(boat.position.y, false, false);
    let aabb = bounding_box(boat.position);
    let reach = aabb.expand(boat.velocity);
    let mut colliders = collect_block_aabbs(world, &reach, &ctx);
    colliders.extend(obstacles.iter().filter(|o| o.intersects(&reach)));
    // A boat doesn't step up, so `Entity.collide` is the one pass.
    let (delta, _) = collide_along_axes(&colliders, aabb, boat.velocity.into());
    let on_ground = boat.velocity.y < 0.0 && delta.y != boat.velocity.y;
    drive.last_yd = boat.velocity.y;
    if (delta.x - boat.velocity.x).abs() > 1.0e-7 {
        boat.velocity.x = 0.0;
    }
    if (delta.z - boat.velocity.z).abs() > 1.0e-7 {
        boat.velocity.z = 0.0;
    }
    if (delta.y - boat.velocity.y).abs() > 1.0e-7 {
        boat.velocity.y = 0.0;
    }
    boat.position += delta;
    drive.on_ground = on_ground;
    drive.keys = keys;
    paddles
}

/// `AbstractBoat.clampRotation`: how far a rider facing `rider_y_rot` must
/// turn to stay within 105 degrees of the boat's bow.
pub fn rider_turn(boat_y_rot: f32, rider_y_rot: f32) -> f32 {
    let off = crate::entity::wrap_degrees(rider_y_rot - boat_y_rot);
    off.clamp(-105.0, 105.0) - off
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rowed(drive: &mut Drive, boat: &mut Boat, keys: Keys) -> [bool; 2] {
        drive.keys = keys;
        drive.control(boat)
    }

    /// Forward rows both paddles and pushes 0.04 along the bow; a turn
    /// alone rows the outer paddle, gathers a degree a tick and creeps
    /// forward; backing rows neither.
    #[test]
    fn the_oars_push_and_turn() {
        let mut drive = Drive::of(1);
        let mut boat = Boat {
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            y_rot: 0.0,
        };
        let forward = Keys {
            up: true,
            ..Keys::default()
        };
        assert_eq!(rowed(&mut drive, &mut boat, forward), [true, true]);
        assert!((boat.velocity.z - 0.04).abs() < 1.0e-6 && boat.velocity.x.abs() < 1.0e-9);

        boat.velocity = DVec3::ZERO;
        let right = Keys {
            right: true,
            ..Keys::default()
        };
        assert_eq!(rowed(&mut drive, &mut boat, right), [true, false]);
        assert_eq!(rowed(&mut drive, &mut boat, right), [true, false]);
        assert_eq!((drive.delta_rotation, boat.y_rot), (2.0, 3.0));
        assert!(boat.velocity.length() > 0.009 && boat.velocity.length() < 0.011);

        let back = Keys {
            down: true,
            ..Keys::default()
        };
        assert_eq!(rowed(&mut Drive::of(1), &mut boat, back), [false, false]);
    }

    /// A boat low in the water is lifted, one on land or in the air only
    /// falls, and each drags its speed and turning by its own share.
    #[test]
    fn water_lifts_and_everything_drags() {
        let world_free = |status, drive: &mut Drive, boat: &mut Boat| {
            // `float` reads the world only on the tick a boat lands in water.
            let chunks = crate::world::chunk::ChunkStore::new(2);
            let world =
                CollisionWorld::new(&chunks, crate::world::block::BlockStateTable::native());
            drive.float(boat, Some(status), status, world);
        };
        let moving = Boat {
            position: DVec3::new(0.0, 63.5, 0.0),
            velocity: DVec3::new(1.0, 0.0, 0.0),
            y_rot: 0.0,
        };

        let mut drive = Drive::of(1);
        drive.water_level = 64.0;
        drive.delta_rotation = 10.0;
        let mut boat = moving;
        world_free(Status::InWater, &mut drive, &mut boat);
        let lift = (64.0 - 63.5) / HEIGHT * (GRAVITY / 0.65);
        assert!((boat.velocity.y - (lift - GRAVITY) * 0.75).abs() < 1.0e-9);
        assert!((boat.velocity.x - f64::from(0.9_f32)).abs() < 1.0e-9);
        assert_eq!(drive.delta_rotation, 9.0);

        let mut drive = Drive::of(1);
        drive.land_friction = 0.6;
        let mut boat = moving;
        world_free(Status::OnLand, &mut drive, &mut boat);
        assert_eq!(boat.velocity.y, -GRAVITY);
        assert!((boat.velocity.x - f64::from(0.6_f32)).abs() < 1.0e-9);

        let mut boat = moving;
        world_free(Status::UnderWater, &mut Drive::of(1), &mut boat);
        assert!((boat.velocity.x - f64::from(0.45_f32)).abs() < 1.0e-9);
        assert!(boat.velocity.y > -GRAVITY);
    }

    /// A boat rowed at another's box stops against it; one it is already
    /// in doesn't hold it.
    #[test]
    fn another_boat_is_in_the_way() {
        let chunks = crate::world::chunk::ChunkStore::new(2);
        let world = || CollisionWorld::new(&chunks, crate::world::block::BlockStateTable::native());
        let rowing = Boat {
            position: DVec3::new(0.0, 100.0, 0.0),
            velocity: DVec3::new(0.0, 0.0, 0.5),
            y_rot: 0.0,
        };
        // In the air, so only its own drag and fall change its speed.
        let free = {
            let mut boat = rowing;
            tick(&mut boat, &mut Drive::of(1), Keys::default(), world(), &[]);
            boat
        };
        assert!(free.position.z > 0.4);

        let ahead = bounding_box(DVec3::new(0.0, 100.0, WIDTH + 0.1));
        let mut boat = rowing;
        tick(
            &mut boat,
            &mut Drive::of(1),
            Keys::default(),
            world(),
            &[ahead],
        );
        assert!((boat.position.z - 0.1).abs() < 1.0e-6 && boat.velocity.z == 0.0);

        let around = bounding_box(DVec3::new(0.0, 100.0, 0.2));
        let mut boat = rowing;
        tick(
            &mut boat,
            &mut Drive::of(1),
            Keys::default(),
            world(),
            &[around],
        );
        assert_eq!(boat.position.z, free.position.z);
    }

    /// A rider may look 105 degrees either side of the bow and no further.
    #[test]
    fn the_rider_turns_with_the_boat() {
        assert_eq!(rider_turn(0.0, 90.0), 0.0);
        assert_eq!(rider_turn(0.0, 120.0), -15.0);
        assert_eq!(rider_turn(350.0, -130.0), 15.0);
    }
}
