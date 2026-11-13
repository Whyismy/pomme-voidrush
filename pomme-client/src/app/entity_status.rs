//! What the client shows for an entity's status events
//! ([`EntityStatus`]): particles about the entity, a sound, and for the
//! local player's totem the item on screen.

use azalea_inventory::ItemStack;
use azalea_inventory::components::EquipmentSlot;
use azalea_registry::builtin::{EntityKind, ItemKind};
use glam::{DVec3, dvec3};

use crate::app::phases::in_game::GameState;
use crate::audio::{AudioEngine, CATEGORY_NEUTRAL, CATEGORY_PLAYERS, SoundRef};
use crate::entity::components::Position;
use crate::entity::dimensions::EntityDimensions;
use crate::entity::status::{EntityStatus, ItemActivation, TrackingEmitter};
use crate::particle::{ParticleLook, ParticleOptions, ServerParticleKind, next_gaussian};
use crate::player::inventory::item_resource_name;

/// Where an entity is and how big.
#[derive(Clone, Copy)]
struct Body {
    pos: DVec3,
    /// Where it was a tick ago.
    prev: DVec3,
    width: f64,
    height: f64,
    eye_y: f64,
    x_rot: f32,
    y_rot: f32,
    kind: EntityKind,
}

impl Body {
    /// `Entity.getRandomX` and `getRandomZ`: a point up to `scale` widths
    /// either side of its middle.
    fn random_x(&self, scale: f64) -> f64 {
        self.pos.x + self.width * (2.0 * fastrand::f64() - 1.0) * scale
    }

    fn random_z(&self, scale: f64) -> f64 {
        self.pos.z + self.width * (2.0 * fastrand::f64() - 1.0) * scale
    }

    /// `Entity.getRandomY`: a height up its box.
    fn random_y(&self) -> f64 {
        self.pos.y + self.height * fastrand::f64()
    }

    fn sound_category(&self) -> u8 {
        if self.kind == EntityKind::Player {
            CATEGORY_PLAYERS
        } else {
            CATEGORY_NEUTRAL
        }
    }
}

fn body(game: &GameState, id: i32) -> Option<Body> {
    if id == game.player.entity_id {
        let pos: DVec3 = game.player.position.into();
        return Some(Body {
            pos,
            prev: game.player.prev_position.into(),
            width: 0.6,
            height: 1.8,
            eye_y: pos.y + f64::from(game.player.eye_height),
            x_rot: game.player.look_dir.x_rot_deg(),
            y_rot: game.player.look_dir.y_rot_deg(),
            kind: EntityKind::Player,
        });
    }
    let entity = game.entity_store.living.get(&id)?;
    let dims = crate::entity::living_entity_dimensions(entity);
    let pos: DVec3 = entity.position.into();
    Some(Body {
        pos,
        prev: entity.prev_position.into(),
        width: f64::from(dims.width),
        height: f64::from(dims.height),
        eye_y: pos.y + f64::from(EntityDimensions::eye_height(entity.entity_type)),
        x_rot: entity.look_dir.x_rot_deg(),
        y_rot: entity.look_dir.y_rot_deg(),
        kind: entity.entity_type,
    })
}

/// One particle of `kind` at `at`, moving at `velocity`.
fn spawn(
    game: &mut GameState,
    camera: DVec3,
    kind: ServerParticleKind,
    options: ParticleOptions,
    at: DVec3,
    velocity: DVec3,
) {
    game.particle_store.add_particles_from_packet(
        kind,
        options,
        ParticleLook::None,
        false,
        at,
        velocity,
        1.0,
        0,
        camera,
    );
}

/// A slow random drift, as these effects all start with.
fn drift() -> DVec3 {
    drift_of(0.02)
}

fn drift_of(speed: f64) -> DVec3 {
    dvec3(next_gaussian(), next_gaussian(), next_gaussian()) * speed
}

/// `LivingEntity.makePoofParticles`: twenty puffs through its box, each
/// set back along its own drift.
fn poof(game: &mut GameState, camera: DVec3, body: Body) {
    for _ in 0..20 {
        let velocity = drift();
        let at = dvec3(body.random_x(1.0), body.random_y(), body.random_z(1.0)) - velocity * 10.0;
        spawn(
            game,
            camera,
            ServerParticleKind::Poof,
            ParticleOptions::None,
            at,
            velocity,
        );
    }
}

/// `count` of `kind` about the entity, `raise` above a height up its box
/// (`Animal`'s hearts, `spawnTamingParticles`, `addParticlesAroundSelf`).
fn around(
    game: &mut GameState,
    camera: DVec3,
    body: Body,
    kind: ServerParticleKind,
    count: u32,
    raise: f64,
) {
    for _ in 0..count {
        let at = dvec3(
            body.random_x(1.0),
            body.random_y() + raise,
            body.random_z(1.0),
        );
        spawn(game, camera, kind, ParticleOptions::None, at, drift());
    }
}

/// The local player's stack in an equipment slot.
fn local_equipment(game: &GameState, slot: EquipmentSlot, selected_slot: u8) -> ItemStack {
    let inventory = &game.player.inventory;
    match slot {
        EquipmentSlot::Mainhand => inventory
            .held_stack(selected_slot)
            .cloned()
            .map_or(ItemStack::Empty, ItemStack::Present),
        EquipmentSlot::Offhand => inventory.offhand().clone(),
        EquipmentSlot::Head => inventory.slot(5).clone(),
        EquipmentSlot::Chest => inventory.slot(6).clone(),
        EquipmentSlot::Legs => inventory.slot(7).clone(),
        EquipmentSlot::Feet => inventory.slot(8).clone(),
        _ => ItemStack::Empty,
    }
}

/// An item's `break_sound`, by the items whose own differs from the rest.
fn break_sound(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::Shield => "item.shield.break",
        ItemKind::WolfArmor => "item.wolf_armor.break",
        _ => "entity.item.break",
    }
}

/// Answers `status` for entity `id`. `selected_slot` is the local player's
/// hotbar slot, for what it holds.
pub(super) fn apply(
    audio: &AudioEngine,
    game: &mut GameState,
    camera: DVec3,
    id: i32,
    status: EntityStatus,
    selected_slot: u8,
) {
    let Some(body) = body(game, id) else {
        return;
    };
    let local = id == game.player.entity_id;
    let animal = crate::entity::is_animal(body.kind);
    let villager = body.kind == EntityKind::Villager;
    match status {
        EntityStatus::DeathPoof => poof(game, camera, body),
        EntityStatus::SpawnPoof if !local => poof(game, camera, body),
        EntityStatus::LoveHearts | EntityStatus::TameHearts if animal => {
            around(game, camera, body, ServerParticleKind::Heart, 7, 0.5);
        }
        EntityStatus::TameSmoke if animal => {
            around(game, camera, body, ServerParticleKind::Smoke, 7, 0.5);
        }
        EntityStatus::VillagerHearts if villager => {
            around(game, camera, body, ServerParticleKind::Heart, 5, 1.0);
        }
        EntityStatus::VillagerAngry if villager => {
            around(
                game,
                camera,
                body,
                ServerParticleKind::AngryVillager,
                5,
                1.0,
            );
        }
        EntityStatus::VillagerHappy if villager => {
            around(
                game,
                camera,
                body,
                ServerParticleKind::HappyVillager,
                5,
                1.0,
            );
        }
        EntityStatus::VillagerSplash if villager => {
            around(game, camera, body, ServerParticleKind::Splash, 5, 1.0);
        }
        // `Dolphin.addParticlesAroundSelf`: seven, at half the usual drift.
        EntityStatus::DolphinHappy if body.kind == EntityKind::Dolphin => {
            for _ in 0..7 {
                let at = dvec3(
                    body.random_x(1.0),
                    body.random_y() + 0.2,
                    body.random_z(1.0),
                );
                spawn(
                    game,
                    camera,
                    ServerParticleKind::HappyVillager,
                    ParticleOptions::None,
                    at,
                    drift_of(0.01),
                );
            }
        }
        // `Witch.handleEntityEvent` 15: ten to forty-four, still, half a
        // block over its head.
        EntityStatus::WitchMagic if body.kind == EntityKind::Witch => {
            for _ in 0..fastrand::u32(10..45) {
                let at =
                    body.pos + dvec3(0.0, body.height + 0.5, 0.0) + drift_of(f64::from(0.13f32));
                spawn(
                    game,
                    camera,
                    ServerParticleKind::Witch,
                    ParticleOptions::None,
                    at,
                    DVec3::ZERO,
                );
            }
        }
        EntityStatus::CureSound if body.kind == EntityKind::ZombieVillager => {
            audio.play_world_sound(
                &SoundRef::event("entity.zombie_villager.cure"),
                body.sound_category(),
                Position::new(body.pos.x, body.eye_y, body.pos.z),
                1.0 + fastrand::f32(),
                fastrand::f32() * 0.7 + 0.3,
                fastrand::u64(..),
            );
        }
        EntityStatus::ArmorStandHit if body.kind == EntityKind::ArmorStand => {
            audio.play_world_sound(
                &SoundRef::event("entity.armor_stand.hit"),
                body.sound_category(),
                Position::new(body.pos.x, body.pos.y, body.pos.z),
                0.3,
                1.0,
                fastrand::u64(..),
            );
        }
        // `ClientPacketListener.handleEntityEvent` 35: particles from the
        // entity for thirty ticks, the totem's sound, and for the local
        // player the totem itself on screen.
        EntityStatus::Totem => {
            game.tracking_emitters.push(TrackingEmitter {
                entity: id,
                life: 0,
            });
            audio.play_world_sound(
                &SoundRef::event("item.totem.use"),
                body.sound_category(),
                Position::new(body.pos.x, body.pos.y, body.pos.z),
                1.0,
                1.0,
                fastrand::u64(..),
            );
            if local {
                // `findTotem`: the one in a hand, else a plain one.
                let held = [EquipmentSlot::Mainhand, EquipmentSlot::Offhand]
                    .into_iter()
                    .find_map(|slot| match local_equipment(game, slot, selected_slot) {
                        ItemStack::Present(stack) if stack.kind == ItemKind::TotemOfUndying => {
                            Some(stack)
                        }
                        _ => None,
                    });
                game.item_activation = Some(ItemActivation::new(held.unwrap_or_else(|| {
                    azalea_inventory::ItemStackData::new(ItemKind::TotemOfUndying, 1)
                })));
            }
        }
        // `LivingEntity.handleEntityEvent` 46: 128 portal particles strung
        // from where it was to where it is.
        EntityStatus::Teleport => {
            for step in 0..128 {
                let along = f64::from(step) / 127.0;
                let velocity = dvec3(
                    f64::from((fastrand::f32() - 0.5) * 0.2),
                    f64::from((fastrand::f32() - 0.5) * 0.2),
                    f64::from((fastrand::f32() - 0.5) * 0.2),
                );
                let at = body.prev.lerp(body.pos, along)
                    + dvec3(
                        (fastrand::f64() - 0.5) * body.width * 2.0,
                        fastrand::f64() * body.height,
                        (fastrand::f64() - 0.5) * body.width * 2.0,
                    );
                spawn(
                    game,
                    camera,
                    ServerParticleKind::Portal,
                    ParticleOptions::None,
                    at,
                    velocity,
                );
            }
        }
        // `LivingEntity.breakItem`: its break sound and five crumbs of it.
        EntityStatus::ItemBreak(slot) => {
            let stack = if local {
                local_equipment(game, slot, selected_slot)
            } else {
                game.entity_store
                    .living
                    .get(&id)
                    .and_then(|entity| entity.equipment(slot))
                    .cloned()
                    .unwrap_or(ItemStack::Empty)
            };
            let ItemStack::Present(stack) = stack else {
                return;
            };
            audio.play_world_sound(
                &SoundRef::event(break_sound(stack.kind)),
                body.sound_category(),
                Position::new(body.pos.x, body.pos.y, body.pos.z),
                0.8,
                0.8 + fastrand::f32() * 0.4,
                fastrand::u64(..),
            );
            game.particle_store.add_item_use_particles(
                5,
                &format!("item/{}", item_resource_name(stack.kind)),
                dvec3(body.pos.x, body.eye_y, body.pos.z),
                body.x_rot,
                body.y_rot,
            );
        }
        // An event for another kind of entity than this one.
        _ => {}
    }
}

/// `Snowball.handleEntityEvent` and `ThrownEgg`'s, event 3: eight crumbs
/// where it broke, a snowball's still and an egg's scattering.
pub(super) fn projectile_break(game: &mut GameState, camera: DVec3, id: i32) {
    let Some(thrown) = game.thrown_items.items.get(&id) else {
        return;
    };
    let at: DVec3 = thrown.position.into();
    let egg = match thrown.kind {
        EntityKind::Snowball => false,
        EntityKind::Egg => true,
        _ => return,
    };
    // What it carries, where the server named an item; else its own kind.
    let (kind, options) = match (&thrown.stack, egg) {
        (Some(stack), _) => (
            ServerParticleKind::Item,
            ParticleOptions::Item(azalea_registry::Registry::to_u32(&stack.kind)),
        ),
        (None, true) => (
            ServerParticleKind::Item,
            ParticleOptions::Item(azalea_registry::Registry::to_u32(&ItemKind::Egg)),
        ),
        (None, false) => (ServerParticleKind::ItemSnowball, ParticleOptions::None),
    };
    for _ in 0..8 {
        let velocity = if egg {
            dvec3(
                (f64::from(fastrand::f32()) - 0.5) * 0.08,
                (f64::from(fastrand::f32()) - 0.5) * 0.08,
                (f64::from(fastrand::f32()) - 0.5) * 0.08,
            )
        } else {
            DVec3::ZERO
        };
        spawn(game, camera, kind, options, at, velocity);
    }
}

/// A tick of what the events left running: `TrackingEmitter.tick` (sixteen
/// tries a tick at a point in the unit ball, thrown out from the entity's
/// middle) and the totem on screen.
pub(super) fn tick(game: &mut GameState, camera: DVec3) {
    let mut emitters = std::mem::take(&mut game.tracking_emitters);
    emitters.retain_mut(|emitter| {
        let Some(body) = body(game, emitter.entity) else {
            return false;
        };
        for _ in 0..16 {
            let out = dvec3(
                f64::from(fastrand::f32() * 2.0 - 1.0),
                f64::from(fastrand::f32() * 2.0 - 1.0),
                f64::from(fastrand::f32() * 2.0 - 1.0),
            );
            if out.length_squared() > 1.0 {
                continue;
            }
            let at = dvec3(
                body.pos.x + body.width * out.x / 4.0,
                body.pos.y + body.height * (0.5 + out.y / 4.0),
                body.pos.z + body.width * out.z / 4.0,
            );
            spawn(
                game,
                camera,
                ServerParticleKind::TotemOfUndying,
                ParticleOptions::None,
                at,
                out + dvec3(0.0, 0.2, 0.0),
            );
        }
        emitter.life += 1;
        emitter.life < TrackingEmitter::LIFETIME
    });
    game.tracking_emitters = emitters;

    if let Some(activation) = &mut game.item_activation {
        activation.ticks -= 1;
        if activation.ticks <= 0 {
            game.item_activation = None;
        }
    }
}
