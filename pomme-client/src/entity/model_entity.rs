//! Entities drawn as a model of their own that are neither mobs nor items:
//! shulker bullets, wither skulls, llama spit, evoker fangs, lead knots,
//! minecarts and boats. The projectiles fly on the client between the
//! server's position updates, as vanilla's do, and leave their trails; the
//! vehicles glide to where the server last put them.
// TODO: driving a boat or a minecart (their client-side physics and the
// paddle and vehicle-move packets), their hurt wobble, a boat's bubble
// column rocking, and a custom minecart display block.

use std::collections::HashMap;

use azalea_registry::builtin::EntityKind;
use glam::DVec3;

use crate::entity::components::Position;

/// `AbstractBoat.lerpSteps` and a minecart's: ticks a vehicle takes to
/// reach where the server put it.
const VEHICLE_LERP_STEPS: u8 = 3;
/// How far a paddle's stroke moves on a tick (`AbstractBoat.tick`).
const PADDLE_STEP: f32 = std::f32::consts::FRAC_PI_8;

/// `EvokerFangs.lifeTicks` when the bite starts.
const FANGS_LIFE_TICKS: i32 = 22;
/// `AbstractHurtingProjectile.accelerationPower`'s default, which the spawn
/// packet doesn't carry.
const SKULL_ACCELERATION: f64 = 0.1;

pub struct ModelEntity {
    pub kind: EntityKind,
    pub position: Position,
    pub prev_position: Position,
    /// Where the server last put it, which its relative moves build on.
    server_position: Position,
    pub velocity: DVec3,
    pub y_rot: f32,
    pub x_rot: f32,
    pub prev_y_rot: f32,
    pub prev_x_rot: f32,
    /// Ticks since it spawned (vanilla `tickCount`).
    pub age: u32,
    /// `WitherSkull.DATA_DANGEROUS`: a blue skull.
    pub dangerous: bool,
    /// Evoker fangs: ticks of the bite left, once entity event 4 starts it.
    fangs_life: Option<i32>,
    /// Vehicles: the facing the server last sent, and the ticks left of
    /// the glide there.
    server_y_rot: f32,
    server_x_rot: f32,
    lerp_steps: u8,
    /// A boat's paddles, left then right: whether each is rowing
    /// (`AbstractBoat.DATA_ID_PADDLE_*`) and how far through its stroke.
    paddling: [bool; 2],
    paddle: [f32; 2],
}

impl ModelEntity {
    /// `EvokerFangs.getAnimationProgress`: 0 until the bite starts, 1 once
    /// it is done.
    pub fn bite_progress(&self, partial_tick: f32) -> f32 {
        match self.fangs_life {
            None => 0.0,
            Some(life) if life - 2 <= 0 => 1.0,
            Some(life) => 1.0 - ((life - 2) as f32 - partial_tick) / 20.0,
        }
    }

    /// `AbstractBoat.getRowingTime` for the left and right paddles: 0 for
    /// one at rest.
    pub fn rowing_time(&self, partial_tick: f32) -> [f32; 2] {
        std::array::from_fn(|side| {
            if self.paddling[side] {
                self.paddle[side] - (1.0 - partial_tick) * PADDLE_STEP
            } else {
                0.0
            }
        })
    }
}

/// A boat's wood (its texture's name), whether it carries a chest and
/// whether it is a bamboo raft.
pub fn boat(kind: EntityKind) -> Option<(&'static str, bool, bool)> {
    Some(match kind {
        EntityKind::AcaciaBoat => ("acacia", false, false),
        EntityKind::AcaciaChestBoat => ("acacia", true, false),
        EntityKind::BirchBoat => ("birch", false, false),
        EntityKind::BirchChestBoat => ("birch", true, false),
        EntityKind::CherryBoat => ("cherry", false, false),
        EntityKind::CherryChestBoat => ("cherry", true, false),
        EntityKind::DarkOakBoat => ("dark_oak", false, false),
        EntityKind::DarkOakChestBoat => ("dark_oak", true, false),
        EntityKind::JungleBoat => ("jungle", false, false),
        EntityKind::JungleChestBoat => ("jungle", true, false),
        EntityKind::MangroveBoat => ("mangrove", false, false),
        EntityKind::MangroveChestBoat => ("mangrove", true, false),
        EntityKind::OakBoat => ("oak", false, false),
        EntityKind::OakChestBoat => ("oak", true, false),
        EntityKind::PaleOakBoat => ("pale_oak", false, false),
        EntityKind::PaleOakChestBoat => ("pale_oak", true, false),
        EntityKind::SpruceBoat => ("spruce", false, false),
        EntityKind::SpruceChestBoat => ("spruce", true, false),
        EntityKind::BambooRaft => ("bamboo", false, true),
        EntityKind::BambooChestRaft => ("bamboo", true, true),
        _ => return None,
    })
}

/// A minecart's kind: the block it carries with the properties it shows
/// (`getDefaultDisplayBlockState`) and how high that sits
/// (`getDefaultDisplayOffset`), or neither for a plain cart.
pub fn minecart(kind: EntityKind) -> Option<Option<MinecartContents>> {
    let contents = |block, props, offset| {
        Some(Some(MinecartContents {
            block,
            props,
            offset,
        }))
    };
    match kind {
        EntityKind::Minecart => Some(None),
        EntityKind::ChestMinecart => contents("chest", &[("facing", "north")], 8),
        EntityKind::FurnaceMinecart => contents("furnace", &[("facing", "north")], 6),
        EntityKind::TntMinecart => contents("tnt", &[], 6),
        EntityKind::HopperMinecart => contents("hopper", &[], 1),
        EntityKind::SpawnerMinecart => contents("spawner", &[], 6),
        EntityKind::CommandBlockMinecart => contents("command_block", &[], 6),
        _ => None,
    }
}

/// What a minecart carries.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MinecartContents {
    pub block: &'static str,
    pub props: &'static [(&'static str, &'static str)],
    /// `getDisplayOffset`, in sixteenths of a block.
    pub offset: i32,
}

fn is_vehicle(kind: EntityKind) -> bool {
    boat(kind).is_some() || minecart(kind).is_some()
}

/// `getPassengerAttachmentPoint`'s height for a vehicle: a third of a
/// boat's box, eight ninths of a raft's, and a minecart's 0.1875.
fn passenger_attachment(kind: EntityKind) -> f64 {
    match boat(kind) {
        Some((.., true)) => 0.5625 * 0.888_888_9,
        Some(_) => 0.5625 / 3.0,
        None => 0.1875,
    }
}

/// The kinds [`ModelEntityStore`] tracks.
pub fn is_model_entity(kind: EntityKind) -> bool {
    matches!(
        kind,
        EntityKind::ShulkerBullet
            | EntityKind::WitherSkull
            | EntityKind::LlamaSpit
            | EntityKind::EvokerFangs
            | EntityKind::LeashKnot
    ) || is_vehicle(kind)
}

impl ModelEntity {
    /// The server moved it: a vehicle glides there, anything else is there.
    fn moved_by_server(&mut self) {
        if is_vehicle(self.kind) {
            self.lerp_steps = VEHICLE_LERP_STEPS;
        } else {
            self.position = self.server_position;
        }
    }
}

/// The two ends of a rail of `shape` from its block's middle, in half
/// blocks (vanilla `AbstractMinecart.exits`): a slope's lower end is a
/// block down.
pub fn rail_exits(shape: &str) -> Option<Exits> {
    const NORTH: [i32; 3] = [0, 0, -1];
    const SOUTH: [i32; 3] = [0, 0, 1];
    const WEST: [i32; 3] = [-1, 0, 0];
    const EAST: [i32; 3] = [1, 0, 0];
    let below = |[x, _, z]: [i32; 3]| [x, -1, z];
    Some(match shape {
        "north_south" => (NORTH, SOUTH),
        "east_west" => (WEST, EAST),
        "ascending_east" => (below(WEST), EAST),
        "ascending_west" => (WEST, below(EAST)),
        "ascending_north" => (NORTH, below(SOUTH)),
        "ascending_south" => (below(NORTH), SOUTH),
        "south_east" => (SOUTH, EAST),
        "south_west" => (SOUTH, WEST),
        "north_west" => (NORTH, WEST),
        "north_east" => (NORTH, EAST),
        _ => return None,
    })
}

/// A rail's two ends from its block's middle, in half blocks.
pub type Exits = ([i32; 3], [i32; 3]);

/// A rail's shape at a block, for [`pos_on_rail`]: its two ends, if a rail
/// is there.
pub type Rails<'a> = &'a dyn Fn([i32; 3]) -> Option<Exits>;

/// The rail block a cart at `at` rides: its own block, or the one below.
fn rail_under(rails: Rails, at: DVec3) -> Option<([i32; 3], Exits)> {
    let [x, y, z] = at.floor().as_ivec3().to_array();
    let block = if rails([x, y - 1, z]).is_some() {
        [x, y - 1, z]
    } else {
        [x, y, z]
    };
    Some((block, rails(block)?))
}

/// `OldMinecartBehavior.getPos`: the point of the rail under `at` nearest
/// it, or `None` off the rails.
pub fn pos_on_rail(rails: Rails, at: DVec3) -> Option<DVec3> {
    let ([i, j, k], (a, b)) = rail_under(rails, at)?;
    let end = |e: [i32; 3]| {
        DVec3::new(
            f64::from(i) + 0.5 + f64::from(e[0]) * 0.5,
            f64::from(j) + 0.0625 + f64::from(e[1]) * 0.5,
            f64::from(k) + 0.5 + f64::from(e[2]) * 0.5,
        )
    };
    let (from, to) = (end(a), end(b));
    let (dx, dy, dz) = (to.x - from.x, (to.y - from.y) * 2.0, to.z - from.z);
    let along = if dx == 0.0 {
        at.z - f64::from(k)
    } else if dz == 0.0 {
        at.x - f64::from(i)
    } else {
        ((at.x - from.x) * dx + (at.z - from.z) * dz) * 2.0
    };
    let mut on = DVec3::new(
        from.x + dx * along,
        from.y + dy * along,
        from.z + dz * along,
    );
    if dy < 0.0 {
        on.y += 1.0;
    } else if dy > 0.0 {
        on.y += 0.5;
    }
    Some(on)
}

/// `OldMinecartBehavior.getPosOffs`: the point on the rails `offset` along
/// them from `at`, onto the next block's rail when it leaves this one.
pub fn pos_on_rail_offset(rails: Rails, at: DVec3, offset: f64) -> Option<DVec3> {
    let ([i, j, k], (a, b)) = rail_under(rails, at)?;
    let slope = a[1] != 0 || b[1] != 0;
    let mut y = f64::from(if slope { j + 1 } else { j });
    let (dx, dz) = (f64::from(b[0] - a[0]), f64::from(b[2] - a[2]));
    let length = (dx * dx + dz * dz).sqrt();
    let x = at.x + dx / length * offset;
    let z = at.z + dz / length * offset;
    let (bx, bz) = (x.floor() as i32 - i, z.floor() as i32 - k);
    if a[1] != 0 && bx == a[0] && bz == a[2] {
        y += f64::from(a[1]);
    } else if b[1] != 0 && bx == b[0] && bz == b[2] {
        y += f64::from(b[1]);
    }
    pos_on_rail(rails, DVec3::new(x, y, z))
}

/// Where an entity put at `position` is: a lead knot is sent as the block
/// it is tied in, and sits in the middle of it, three eighths up
/// (`LeashFenceKnotEntity.recalculateBoundingBox`).
fn settled(kind: EntityKind, position: Position) -> Position {
    if kind == EntityKind::LeashKnot {
        (position.floor() + DVec3::new(0.5, 0.375, 0.5)).into()
    } else {
        position
    }
}

/// What a tick asks the particle engine for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Trail {
    /// A shulker bullet's end rod sparkle.
    EndRod(DVec3),
    /// A wither skull's smoke.
    Smoke(DVec3),
    /// One of the crits a pair of fangs throws as it snaps shut.
    Crit(DVec3, DVec3),
}

#[derive(Default)]
pub struct ModelEntityStore {
    pub entities: HashMap<i32, ModelEntity>,
}

impl ModelEntityStore {
    pub fn spawn(
        &mut self,
        id: i32,
        kind: EntityKind,
        position: Position,
        velocity: DVec3,
        y_rot: f32,
        x_rot: f32,
    ) {
        let position = settled(kind, position);
        self.entities.insert(
            id,
            ModelEntity {
                kind,
                position,
                prev_position: position,
                server_position: position,
                velocity,
                y_rot,
                x_rot,
                prev_y_rot: y_rot,
                prev_x_rot: x_rot,
                age: 0,
                dangerous: false,
                fangs_life: None,
                server_y_rot: y_rot,
                server_x_rot: x_rot,
                lerp_steps: 0,
                paddling: [false; 2],
                paddle: [0.0; 2],
            },
        );
    }

    pub fn move_delta(&mut self, id: i32, delta: DVec3) {
        if let Some(entity) = self.entities.get_mut(&id) {
            entity.server_position = (*entity.server_position + delta).into();
            entity.moved_by_server();
        }
    }

    pub fn teleport(&mut self, id: i32, position: Position) {
        if let Some(entity) = self.entities.get_mut(&id) {
            entity.server_position = settled(entity.kind, position);
            entity.moved_by_server();
        }
    }

    pub fn rotate(&mut self, id: i32, y_rot: f32, x_rot: f32) {
        if let Some(entity) = self.entities.get_mut(&id) {
            entity.server_y_rot = y_rot;
            entity.server_x_rot = x_rot;
            if is_vehicle(entity.kind) {
                entity.lerp_steps = VEHICLE_LERP_STEPS;
            } else {
                entity.y_rot = y_rot;
                entity.x_rot = x_rot;
            }
        }
    }

    /// Where a passenger of vehicle `id` rides
    /// (`Entity.getPassengerRidingPosition`).
    // TODO: the seats' offsets along a boat (one rider sits forward of the
    // middle, two fore and aft).
    pub fn seat(&self, id: i32) -> Option<DVec3> {
        let vehicle = self.entities.get(&id)?;
        is_vehicle(vehicle.kind)
            .then(|| *vehicle.position + DVec3::new(0.0, passenger_attachment(vehicle.kind), 0.0))
    }

    pub fn set_motion(&mut self, id: i32, velocity: DVec3) {
        if let Some(entity) = self.entities.get_mut(&id) {
            entity.velocity = velocity;
        }
    }

    /// `WitherSkull.DATA_DANGEROUS` (index 8), and a boat's paddles: 11 and
    /// 12 since 1.21.2 (protocol 768) gave each wood its own entity type,
    /// one later while the wood was synced ahead of them.
    pub fn apply_entity_data(&mut self, id: i32, index: u8, value: super::MetaValue) {
        let Some(entity) = self.entities.get_mut(&id) else {
            return;
        };
        let super::MetaValue::Bool(flag) = value else {
            return;
        };
        if entity.kind == EntityKind::WitherSkull && index == 8 {
            entity.dangerous = flag;
        }
        if boat(entity.kind).is_some() {
            let first = if crate::version::session_protocol() >= 768 {
                11
            } else {
                12
            };
            if let Some(side) = index.checked_sub(first).filter(|side| *side < 2) {
                entity.paddling[usize::from(side)] = flag;
            }
        }
    }

    /// Entity event 4 on a pair of evoker fangs
    /// (`EvokerFangs.handleEntityEvent`): the bite starts. Where they are,
    /// for the snap's sound.
    pub fn start_bite(&mut self, id: i32) -> Option<Position> {
        let fangs = self.entities.get_mut(&id)?;
        if fangs.kind != EntityKind::EvokerFangs {
            return None;
        }
        fangs.fangs_life = Some(FANGS_LIFE_TICKS);
        Some(fangs.position)
    }

    /// Each kind's client `tick`; the particles they leave this tick.
    pub fn tick(&mut self) -> Vec<Trail> {
        let mut trails = Vec::new();
        for entity in self.entities.values_mut() {
            entity.prev_position = entity.position;
            entity.prev_y_rot = entity.y_rot;
            entity.prev_x_rot = entity.x_rot;
            entity.age = entity.age.saturating_add(1);
            match entity.kind {
                // `ShulkerBullet.tick`: straight on, sparkling behind.
                EntityKind::ShulkerBullet => {
                    entity.position = (*entity.position + entity.velocity).into();
                    rotate_towards_movement(entity, 0.5);
                    trails.push(Trail::EndRod(
                        *entity.position - entity.velocity + DVec3::new(0.0, 0.15, 0.0),
                    ));
                }
                // `AbstractHurtingProjectile.tick`: it speeds up along its
                // flight and sheds some of that speed, then moves.
                EntityKind::WitherSkull => {
                    let inertia = if entity.dangerous { 0.73f32 } else { 0.95f32 };
                    let v = entity.velocity;
                    entity.velocity =
                        (v + v.normalize_or_zero() * SKULL_ACCELERATION) * f64::from(inertia);
                    entity.position = (*entity.position + entity.velocity).into();
                    rotate_towards_movement(entity, 0.2);
                    trails.push(Trail::Smoke(*entity.position + DVec3::new(0.0, 0.5, 0.0)));
                }
                // `LlamaSpit.tick`: an arc, turning to face it.
                EntityKind::LlamaSpit => {
                    let v = entity.velocity;
                    let flat = (v.x * v.x + v.z * v.z).sqrt();
                    let target_y = v.x.atan2(v.z).to_degrees() as f32;
                    let target_x = v.y.atan2(flat).to_degrees() as f32;
                    entity.y_rot = super::lerp_angle(entity.y_rot, target_y, 0.2);
                    entity.x_rot = super::lerp_angle(entity.x_rot, target_x, 0.2);
                    entity.position = (*entity.position + v).into();
                    entity.velocity = v * f64::from(0.99f32);
                    entity.velocity.y -= 0.06;
                }
                // `EvokerFangs.tick`: twelve crits as the jaws meet.
                EntityKind::EvokerFangs => {
                    if let Some(life) = &mut entity.fangs_life {
                        *life -= 1;
                        if *life == 14 {
                            let spread = || fastrand::f64() * 2.0 - 1.0;
                            for _ in 0..12 {
                                let at = *entity.position
                                    + DVec3::new(
                                        spread() * 0.25,
                                        0.05 + fastrand::f64(),
                                        spread() * 0.25,
                                    );
                                let speed = DVec3::new(
                                    spread() * 0.3,
                                    0.3 + fastrand::f64() * 0.3,
                                    spread() * 0.3,
                                );
                                trails.push(Trail::Crit(at, speed));
                            }
                        }
                    }
                }
                // `AbstractBoat.tick` and a minecart's: a few ticks' glide to
                // where the server put it, a rowing paddle's stroke moving on.
                kind if is_vehicle(kind) => {
                    if entity.lerp_steps > 0 {
                        let steps = f32::from(entity.lerp_steps);
                        entity.position = (*entity.position
                            + (*entity.server_position - *entity.position) / f64::from(steps))
                        .into();
                        entity.y_rot +=
                            super::wrap_degrees(entity.server_y_rot - entity.y_rot) / steps;
                        entity.x_rot += (entity.server_x_rot - entity.x_rot) / steps;
                        entity.lerp_steps -= 1;
                    }
                    for side in 0..2 {
                        entity.paddle[side] = if entity.paddling[side] {
                            entity.paddle[side] + PADDLE_STEP
                        } else {
                            0.0
                        };
                    }
                }
                _ => {}
            }
        }
        trails
    }

    pub fn remove(&mut self, ids: &[i32]) {
        for id in ids {
            self.entities.remove(id);
        }
    }

    pub fn clear(&mut self) {
        self.entities.clear();
    }
}

/// `ProjectileUtil.rotateTowardsMovement`: `share` of the way from where it
/// faced to where it flies.
fn rotate_towards_movement(entity: &mut ModelEntity, share: f32) {
    let v = entity.velocity;
    if v.length_squared() == 0.0 {
        return;
    }
    let flat = (v.x * v.x + v.z * v.z).sqrt();
    let target_y = v.z.atan2(v.x).to_degrees() as f32 + 90.0;
    let target_x = flat.atan2(v.y).to_degrees() as f32 - 90.0;
    entity.y_rot = super::lerp_angle(entity.prev_y_rot, target_y, share);
    entity.x_rot = super::lerp_angle(entity.prev_x_rot, target_x, share);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A boat glides to where the server moves it over three ticks, turns
    /// the short way, rows the paddle it is told to, and seats its rider a
    /// third of the way up its box.
    #[test]
    fn vehicles_glide_and_boats_row() {
        let mut store = store_with(EntityKind::OakBoat, DVec3::ZERO);
        store.move_delta(1, DVec3::new(3.0, 0.0, 0.0));
        store.rotate(1, 270.0, 0.0);
        store.apply_entity_data(1, 11, crate::entity::MetaValue::Bool(true));
        store.tick();
        let boat = &store.entities[&1];
        assert_eq!(boat.position.x, 1.0);
        assert_eq!(boat.y_rot, -30.0);
        assert_eq!(boat.rowing_time(1.0), [PADDLE_STEP, 0.0]);
        assert_eq!(boat.rowing_time(0.0), [0.0, 0.0]);
        store.tick();
        store.tick();
        let boat = &store.entities[&1];
        assert!((boat.position.x - 3.0).abs() < 1e-9 && (boat.y_rot + 90.0).abs() < 1e-4);
        assert_eq!(
            store.seat(1),
            Some(DVec3::new(boat.position.x, 64.1875, 0.0))
        );
        store.apply_entity_data(1, 11, crate::entity::MetaValue::Bool(false));
        store.tick();
        assert_eq!(store.entities[&1].rowing_time(0.5), [0.0, 0.0]);

        assert_eq!(
            boat_of(EntityKind::DarkOakChestBoat),
            ("dark_oak", true, false)
        );
        assert_eq!(boat_of(EntityKind::BambooRaft), ("bamboo", false, true));
        assert_eq!(minecart(EntityKind::Minecart), Some(None));
        assert_eq!(
            minecart(EntityKind::HopperMinecart)
                .unwrap()
                .unwrap()
                .offset,
            1
        );
        assert!(minecart(EntityKind::OakBoat).is_none());

        fn boat_of(kind: EntityKind) -> (&'static str, bool, bool) {
            super::boat(kind).unwrap()
        }
    }

    /// A cart on flat rails sits a sixteenth above the block's floor on
    /// the rail's line, on a slope climbs with it, and off the rails has
    /// no place.
    #[test]
    fn carts_find_their_place_on_the_rails() {
        let rails = |[x, y, z]: [i32; 3]| match (x, y, z) {
            (0, 64, 0) => rail_exits("north_south"),
            (0, 64, 1) => rail_exits("ascending_south"),
            (5, 64, 5) => rail_exits("south_east"),
            _ => None,
        };
        let on = pos_on_rail(&rails, DVec3::new(0.3, 64.2, 0.25)).unwrap();
        assert_eq!(on, DVec3::new(0.5, 64.0625, 0.25));
        // Half way up the slope to the south, half a block higher.
        let slope = pos_on_rail(&rails, DVec3::new(0.5, 64.5, 1.5)).unwrap();
        assert!((slope - DVec3::new(0.5, 64.5625, 1.5)).length() < 1e-9);
        // A curve cuts the corner: its middle is a quarter in from both.
        let curve = pos_on_rail(&rails, DVec3::new(5.75, 64.0, 5.75)).unwrap();
        assert!((curve - DVec3::new(5.75, 64.0625, 5.75)).length() < 1e-9);
        assert!(pos_on_rail(&rails, DVec3::new(9.0, 64.0, 9.0)).is_none());

        // Ahead of a cart at the flat rail's south end is the slope.
        let ahead = pos_on_rail_offset(&rails, DVec3::new(0.5, 64.0, 0.9), 0.3).unwrap();
        assert!(ahead.z > 1.0 && ahead.y > 64.0625);
        let behind = pos_on_rail_offset(&rails, DVec3::new(0.5, 64.0, 0.9), -0.3).unwrap();
        assert!((behind - DVec3::new(0.5, 64.0625, 0.6)).length() < 1e-9);
    }

    fn store_with(kind: EntityKind, velocity: DVec3) -> ModelEntityStore {
        let mut store = ModelEntityStore::default();
        store.spawn(1, kind, Position::new(0.0, 64.0, 0.0), velocity, 0.0, 0.0);
        store
    }

    #[test]
    fn bullets_fly_straight_and_sparkle_behind() {
        let mut store = store_with(EntityKind::ShulkerBullet, DVec3::new(0.15, 0.0, 0.0));
        let trails = store.tick();
        let bullet = &store.entities[&1];
        assert_eq!(*bullet.position, DVec3::new(0.15, 64.0, 0.0));
        assert_eq!(trails, [Trail::EndRod(DVec3::new(0.0, 64.15, 0.0))]);
        // Flying +x is facing yaw 90 in a projectile's terms; it turns
        // half the way there each tick.
        assert_eq!(bullet.y_rot, 45.0);
        assert!(is_model_entity(bullet.kind) && !is_model_entity(EntityKind::Arrow));
    }

    #[test]
    fn skulls_speed_up_and_a_blue_one_drags() {
        let mut store = store_with(EntityKind::WitherSkull, DVec3::new(1.0, 0.0, 0.0));
        store.tick();
        let speed = store.entities[&1].velocity.x;
        assert!((speed - 1.1 * f64::from(0.95f32)).abs() < 1e-9);
        assert_eq!(store.entities[&1].position.x, speed);

        let mut store = store_with(EntityKind::WitherSkull, DVec3::new(1.0, 0.0, 0.0));
        store.apply_entity_data(1, 8, crate::entity::MetaValue::Bool(true));
        let trails = store.tick();
        assert!((store.entities[&1].velocity.x - 1.1 * f64::from(0.73f32)).abs() < 1e-9);
        assert!(matches!(trails[..], [Trail::Smoke(at)] if at.y == 64.5));
    }

    #[test]
    fn spit_arcs() {
        let mut store = store_with(EntityKind::LlamaSpit, DVec3::new(0.5, 0.0, 0.0));
        store.tick();
        let spit = &store.entities[&1];
        assert_eq!(*spit.position, DVec3::new(0.5, 64.0, 0.0));
        assert!((spit.velocity.x - 0.5 * f64::from(0.99f32)).abs() < 1e-12);
        assert_eq!(spit.velocity.y, -0.06);
    }

    /// The bite runs 20 ticks from the event, throwing its crits 8 ticks in,
    /// and holds shut after.
    #[test]
    fn fangs_bite_when_told_to() {
        let mut store = store_with(EntityKind::EvokerFangs, DVec3::ZERO);
        store.tick();
        assert_eq!(store.entities[&1].bite_progress(0.5), 0.0);
        assert!(store.start_bite(1).is_some());
        assert_eq!(store.entities[&1].bite_progress(0.0), 0.0);
        let mut crits = Vec::new();
        for _ in 0..10 {
            crits.push(store.tick().len());
        }
        assert_eq!(crits, [0, 0, 0, 0, 0, 0, 0, 12, 0, 0]);
        assert!((store.entities[&1].bite_progress(0.0) - 0.5).abs() < 1e-6);
        for _ in 0..30 {
            store.tick();
        }
        assert_eq!(store.entities[&1].bite_progress(0.0), 1.0);

        // Fangs aren't sat on.
        assert!(store.seat(1).is_none());
        // Only fangs bite.
        let mut store = store_with(EntityKind::LeashKnot, DVec3::ZERO);
        assert!(store.start_bite(1).is_none());
        // A knot sits in the middle of its block, three eighths up.
        assert_eq!(*store.entities[&1].position, DVec3::new(0.5, 64.375, 0.5));
    }
}
