//! Particles: a port of vanilla `TerrainParticle`, `BreakingItemParticle`,
//! `EndRodParticle`, `Particle`, `ClientLevel.addDestroyBlockEffect`, and the
//! `ClientboundLevelParticles` spawn path.

use std::collections::HashMap;
use std::sync::Arc;

use azalea_block::BlockState;
use glam::{DVec3, dvec3};

use crate::physics::aabb::Aabb;
use crate::physics::block_shape::{self, CollisionContext, LocalBox};
use crate::physics::collision::resolve_collision;
use crate::position::{BlockPos, ChunkPos};
use crate::renderer::ParticleQuad;
use crate::renderer::chunk::atlas::{AtlasRegion, AtlasUVMap};
use crate::renderer::chunk::block_lighter::{self, LightLevels, WorldLight};
use crate::renderer::chunk::mesher::{
    BiomeClimate, Colormap, blend_color, dry_foliage_color, foliage_color, grass_color,
    tint_sample_y,
};
use crate::renderer::pipelines::particle::MAX_PARTICLE_QUADS as MAX_PARTICLES;
use crate::world::block::BlockStateTable;
use crate::world::block::render_assets::{BlockRenderView, Tint};
use crate::world::chunk::ChunkStore;

/// Vanilla `ParticleGroup.RESERVOIR_START` — above this, new particles are
/// probabilistically dropped.
const RESERVOIR_START: usize = 12288;
/// Vanilla `Particle.MAXIMUM_COLLISION_VELOCITY_SQUARED` (100²).
const MAX_COLLISION_VELOCITY_SQ: f64 = 10000.0;
/// Terrain particles use the default 0.2-wide, 0.2-tall bounding box.
const HALF_WIDTH: f64 = 0.1;

/// How a particle moves each tick.
#[derive(Clone, Copy)]
enum Motion {
    /// `Particle.move`: block collision.
    Physics,
    /// No collision (`hasPhysics = false`, or a `move` override).
    Free,
    /// `PortalParticle`: drawn in along a curve from `start + vel` to `start`.
    Portal(DVec3),
    /// `ReversePortalParticle`: thrown out, speeding up with age.
    ReversePortal,
    /// `FlyTowardsPositionParticle` (enchant glyphs): from `start + vel` to
    /// `start`, dipping as it lands.
    FlyTowards(DVec3),
    /// Stays where it spawned (explosions, sweeps).
    Still,
}

/// How a particle's light is read (`getLightCoords`).
#[derive(Clone, Copy, PartialEq)]
enum Light {
    /// `Particle.getLightCoords`: the light where it is.
    World,
    /// Full bright (`SimpleAnimatedParticle`, explosions, sweeps).
    Bright,
    /// `PortalParticle` / enchant: sky light raised by `age⁴` toward full.
    SkyRamp,
    /// `FlameParticle` / `LavaParticle`: block light raised with age.
    BlockRamp,
    /// `GlowParticle`, obsidian tears: block light full.
    BlockFull,
}

/// `getQuadSize(partialTick)` over the base `quadSize`.
#[derive(Clone, Copy)]
enum Size {
    Fixed,
    /// Dust, smoke, hearts: grows in over the first 1/32 of its life.
    GrowIn,
    /// `PortalParticle`: `1 - (1 - f)²`.
    Portal,
    /// `ReversePortalParticle`: shrinks over 1.5 lifetimes.
    ReversePortal,
    /// `FlameParticle`: `1 - f²/2`.
    Flame,
    /// `LavaParticle`: `1 - f²`.
    Lava,
}

/// After-tick behaviour beyond movement and the sprite.
#[derive(Clone, Copy, PartialEq)]
enum Extra {
    None,
    /// `SimpleAnimatedParticle`: past half life, fade out (and toward the
    /// fade colour, when it has one).
    FadeOut(Option<[f32; 3]>),
    /// `CritParticle`: green and blue drain each tick.
    CritDrain,
    /// `CampfireSmokeParticle`: drift, fade over its last 60 ticks.
    CampfireSmoke,
    /// `DripParticle.DripHangParticle`: becomes the falling drip on expiry.
    DripHang,
}

pub struct Particle {
    motion: Motion,
    light: Light,
    size_curve: Size,
    extra: Extra,
    /// Animated from age through this set (`setSpriteFromAge`), else fixed.
    animated: Option<SpriteSet>,
    translucent: bool,
    /// `Particle.speedUpWhenYMotionIsBlocked`.
    speed_up_blocked: bool,
    /// Bounding-box bottom-center, like vanilla `Particle.setPos`.
    pos: DVec3,
    prev_pos: DVec3,
    vel: DVec3,
    age: i32,
    lifetime: i32,
    on_ground: bool,
    stopped_by_collision: bool,
    /// Vanilla `Particle.gravity` and `friction`.
    gravity: f64,
    friction: f64,
    /// Vanilla `quadSize`; the billboard spans twice this.
    size: f32,
    u0: f32,
    u1: f32,
    v0: f32,
    v1: f32,
    color: [f32; 3],
    /// `DustColorTransitionParticle`: the colour it ends at.
    color_to: Option<[f32; 3]>,
    alpha: f32,
    /// The light where it was at its last tick: read once a tick rather than
    /// every frame, which for thousands of particles was most of their cost.
    /// `None` until its first tick.
    levels: Option<LightLevels>,
}

/// `SingleQuadParticle`'s base `quadSize`.
fn base_size() -> f32 {
    0.1 * (fastrand::f32() * 0.5 + 0.5) * 2.0
}

impl Particle {
    /// The plain 3-arg `Particle(level, x, y, z)`: still, opaque, lit.
    fn at(pos: DVec3) -> Self {
        Self {
            motion: Motion::Physics,
            light: Light::World,
            size_curve: Size::Fixed,
            extra: Extra::None,
            animated: None,
            translucent: false,
            speed_up_blocked: false,
            pos,
            prev_pos: pos,
            vel: DVec3::ZERO,
            age: 0,
            lifetime: (4.0 / (fastrand::f32() * 0.9 + 0.1)) as i32,
            on_ground: false,
            stopped_by_collision: false,
            gravity: 0.0,
            friction: 0.98,
            size: base_size(),
            u0: 0.0,
            u1: 0.0,
            v0: 0.0,
            v1: 0.0,
            color: [1.0; 3],
            color_to: None,
            alpha: 1.0,
            levels: None,
        }
    }

    /// The 6-arg `Particle(level, x, y, z, xa, ya, za)`: the given velocity
    /// jittered and renormalised to a small random speed, plus a hop.
    fn thrown(pos: DVec3, velocity_arg: DVec3) -> Self {
        let jitter = || (fastrand::f64() * 2.0 - 1.0) * 0.4;
        let dir = velocity_arg + DVec3::new(jitter(), jitter(), jitter());
        let speed = (fastrand::f64() + fastrand::f64() + 1.0) * 0.15;
        let mut vel = dir / dir.length() * speed * 0.4;
        vel.y += 0.1;
        Self {
            vel,
            ..Self::at(pos)
        }
    }

    /// Vanilla `TerrainParticle` constructor chain: velocity jitter +
    /// normalization from `Particle(level, x, y, z, xa, ya, za)`, then the
    /// terrain quad size and random quarter sub-tile.
    fn terrain(pos: DVec3, velocity_arg: DVec3, region: AtlasRegion, color: [f32; 3]) -> Self {
        // SingleQuadParticle base size, halved by TerrainParticle.
        let size = base_size() / 2.0;

        // Random quarter sub-tile of the block sprite. The u0/u1 flip is
        // vanilla (`getU0` samples uo+1, `getU1` samples uo).
        let sprite_u = |f: f32| region.u_min + f * (region.u_max - region.u_min);
        let sprite_v = |f: f32| region.v_min + f * (region.v_max - region.v_min);
        let uo = fastrand::f32() * 3.0;
        let vo = fastrand::f32() * 3.0;

        Self {
            gravity: 1.0,
            size,
            u0: sprite_u((uo + 1.0) / 4.0),
            u1: sprite_u(uo / 4.0),
            v0: sprite_v(vo / 4.0),
            v1: sprite_v((vo + 1.0) / 4.0),
            color,
            ..Self::thrown(pos, velocity_arg)
        }
    }

    /// Vanilla `EndRodParticle`: velocity taken verbatim from the spawn call
    /// (the 3-arg `Particle` constructor adds no jitter), tiny gravity, no
    /// collision, warm fade color.
    fn end_rod(pos: DVec3, vel: DVec3) -> Self {
        Self {
            motion: Motion::Free,
            light: Light::Bright,
            extra: Extra::FadeOut(Some(END_ROD_FADE)),
            animated: Some(SpriteSet::Glitter),
            translucent: true,
            vel,
            lifetime: 60 + fastrand::i32(0..12),
            gravity: f64::from(0.0125f32),
            friction: f64::from(0.91f32),
            // SingleQuadParticle base quadSize, then EndRodParticle *= 0.75.
            size: base_size() * 0.75,
            ..Self::at(pos)
        }
    }

    /// `DustParticleBase`: a tenth of the thrown velocity, friction 0.96,
    /// scaled by the options, lifetime scaled too.
    fn dust(pos: DVec3, vel: DVec3, color: u32, to: Option<u32>, scale: f32) -> Self {
        let mut p = Self::thrown(pos, vel);
        p.vel *= f64::from(0.1f32);
        p.friction = f64::from(0.96f32);
        p.speed_up_blocked = true;
        p.size *= 0.75 * scale;
        let lifetime = (8.0 / (fastrand::f64() * 0.8 + 0.2)) as i32;
        p.lifetime = ((lifetime as f32 * scale) as i32).max(1);
        p.size_curve = Size::GrowIn;
        p.animated = Some(SpriteSet::Generic);
        // `randomizeColor`: one shade for the particle, a little noise each.
        let shade = fastrand::f32() * 0.4 + 0.6;
        p.color = randomize_color(color, shade);
        p.color_to = to.map(|to| randomize_color(to, shade));
        p
    }

    /// `PortalParticle`: drawn in from `pos + vel` to `pos` over its life.
    fn portal(pos: DVec3, vel: DVec3, reverse: bool) -> Self {
        let shade = fastrand::f32() * 0.6 + 0.4;
        let mut p = Self {
            motion: Motion::Portal(pos),
            light: Light::SkyRamp,
            size_curve: Size::Portal,
            vel,
            size: 0.1 * (fastrand::f32() * 0.2 + 0.5),
            color: [shade * 0.9, shade * 0.3, shade],
            lifetime: (fastrand::f32() * 10.0) as i32 + 40,
            ..Self::at(pos)
        };
        if reverse {
            p.motion = Motion::ReversePortal;
            p.size_curve = Size::ReversePortal;
            p.size *= 1.5;
            p.lifetime = (fastrand::f32() * 2.0) as i32 + 60;
        }
        p
    }

    /// `FlyTowardsPositionParticle` (`enchant`): from `pos + vel` in to `pos`.
    fn enchant(pos: DVec3, vel: DVec3) -> Self {
        let shade = fastrand::f32() * 0.6 + 0.4;
        Self {
            motion: Motion::FlyTowards(pos),
            light: Light::SkyRamp,
            pos: pos + vel,
            prev_pos: pos + vel,
            vel,
            size: 0.1 * (fastrand::f32() * 0.5 + 0.2),
            color: [shade * 0.9, shade * 0.9, shade],
            lifetime: (fastrand::f32() * 10.0) as i32 + 30,
            ..Self::at(pos)
        }
    }

    /// `CritParticle` (`crit`, and `enchanted_hit` tinted).
    fn crit(pos: DVec3, vel: DVec3, enchanted: bool) -> Self {
        let mut p = Self::thrown(pos, DVec3::ZERO);
        p.motion = Motion::Free;
        p.friction = f64::from(0.7f32);
        p.gravity = 0.5;
        p.vel = p.vel * f64::from(0.1f32) + vel * 0.4;
        let shade = fastrand::f32() * 0.3 + 0.6;
        p.color = if enchanted {
            [shade * 0.3, shade * 0.8, shade]
        } else {
            [shade; 3]
        };
        p.size *= 0.75;
        p.lifetime = ((6.0 / (fastrand::f32() * 0.8 + 0.6)) as i32).max(1);
        p.extra = Extra::CritDrain;
        p
    }

    /// `RisingParticle` (`FlameParticle`): a little of its own drift plus the
    /// spawn velocity, nudged about.
    fn flame(pos: DVec3, vel: DVec3, size_scale: f32) -> Self {
        let mut p = Self::thrown(pos, vel);
        p.motion = Motion::Free;
        p.friction = f64::from(0.96f32);
        p.vel = p.vel * f64::from(0.01f32) + vel;
        let nudge = || (fastrand::f64() - fastrand::f64()) * 0.05;
        p.pos += dvec3(nudge(), nudge(), nudge());
        p.prev_pos = p.pos;
        p.lifetime = (8.0 / (fastrand::f32() * 0.8 + 0.2)) as i32 + 4;
        p.light = Light::BlockRamp;
        p.size_curve = Size::Flame;
        p.size *= size_scale;
        p
    }

    /// `BaseAshSmokeParticle` (`SmokeParticle`, large smoke at 2.5).
    fn smoke(pos: DVec3, vel: DVec3, size_scale: f32) -> Self {
        let mut p = Self::thrown(pos, DVec3::ZERO);
        p.friction = f64::from(0.96f32);
        p.gravity = f64::from(-0.1f32);
        p.speed_up_blocked = true;
        p.vel = p.vel * 0.1 + vel;
        let grey = fastrand::f32() * 0.3;
        p.color = [grey; 3];
        p.size *= 0.75 * size_scale;
        p.lifetime = (((8.0 / (fastrand::f32() * 0.8 + 0.2)) * size_scale) as i32).max(1);
        p.size_curve = Size::GrowIn;
        p.animated = Some(SpriteSet::Generic);
        p
    }

    /// `PlayerCloudParticle` (`cloud`).
    fn cloud(pos: DVec3, vel: DVec3) -> Self {
        let mut p = Self::thrown(pos, DVec3::ZERO);
        p.motion = Motion::Free;
        p.friction = f64::from(0.96f32);
        p.vel = p.vel * 0.1 + vel;
        p.color = [1.0 - fastrand::f32() * 0.3; 3];
        p.size *= 1.875;
        p.lifetime = (((8.0 / (fastrand::f32() * 0.8 + 0.3)) * 2.5) as i32).max(1);
        p.size_curve = Size::GrowIn;
        p.animated = Some(SpriteSet::Generic);
        p
    }

    /// `HugeExplosionParticle` (`explosion`), `x_speed` as its size.
    fn explosion(pos: DVec3, size_arg: f64) -> Self {
        Self {
            motion: Motion::Still,
            light: Light::Bright,
            animated: Some(SpriteSet::Explosion),
            lifetime: 6 + fastrand::i32(0..4),
            color: [fastrand::f32() * 0.6 + 0.4; 3],
            size: 2.0 * (1.0 - size_arg as f32 * 0.5),
            ..Self::at(pos)
        }
    }

    /// `AttackSweepParticle`.
    fn sweep(pos: DVec3, size_arg: f64) -> Self {
        Self {
            motion: Motion::Still,
            light: Light::Bright,
            animated: Some(SpriteSet::Sweep),
            lifetime: 4,
            color: [fastrand::f32() * 0.6 + 0.4; 3],
            size: 1.0 - size_arg as f32 * 0.5,
            ..Self::at(pos)
        }
    }

    /// `SuspendedTownParticle` (`happy_villager`).
    fn happy_villager(pos: DVec3, vel: DVec3) -> Self {
        let mut p = Self::thrown(pos, vel);
        p.motion = Motion::Free;
        p.friction = 0.99;
        p.size *= fastrand::f32() * 0.6 + 0.5;
        p.vel *= f64::from(0.02f32);
        p.lifetime = (20.0 / (fastrand::f32() * 0.8 + 0.2)) as i32;
        p
    }

    /// `SpellParticle` (`witch`): purple, rising.
    fn witch(pos: DVec3, vel: DVec3) -> Self {
        let mut p = Self::thrown(
            pos,
            dvec3(0.5 - fastrand::f64(), vel.y, 0.5 - fastrand::f64()),
        );
        p.motion = Motion::Free;
        p.friction = f64::from(0.96f32);
        p.gravity = f64::from(-0.1f32);
        p.speed_up_blocked = true;
        p.vel.y *= f64::from(0.2f32);
        if vel.x == 0.0 && vel.z == 0.0 {
            p.vel.x *= f64::from(0.1f32);
            p.vel.z *= f64::from(0.1f32);
        }
        p.size *= 0.75;
        p.lifetime = (8.0 / (fastrand::f32() * 0.8 + 0.2)) as i32;
        let shade = fastrand::f32() * 0.5 + 0.35;
        p.color = [shade, 0.0, shade];
        p.animated = Some(SpriteSet::Spell);
        p.translucent = true;
        p
    }

    /// `SoulParticle`: a flame-like riser through the soul frames.
    fn soul(pos: DVec3, vel: DVec3) -> Self {
        let mut p = Self::flame(pos, vel, 1.5);
        p.light = Light::World;
        p.size_curve = Size::Fixed;
        p.animated = Some(SpriteSet::Soul);
        p
    }

    /// `GlowParticle` (`electric_spark`).
    fn spark(pos: DVec3, vel: DVec3) -> Self {
        let mut p = Self::thrown(pos, vel);
        p.motion = Motion::Free;
        p.friction = f64::from(0.96f32);
        p.speed_up_blocked = true;
        p.vel = vel * 0.25;
        p.color = [1.0, 0.9, 1.0];
        p.lifetime = fastrand::i32(0..2) + 2;
        p.light = Light::BlockFull;
        p.translucent = true;
        p
    }

    /// `LavaParticle`: popped up, falls with collision.
    fn lava(pos: DVec3) -> Self {
        let mut p = Self::thrown(pos, DVec3::ZERO);
        p.gravity = 0.75;
        p.friction = 0.999;
        p.vel.x *= f64::from(0.8f32);
        p.vel.z *= f64::from(0.8f32);
        p.vel.y = fastrand::f64() * 0.4 + 0.05;
        p.size *= fastrand::f32() * 2.0 + 0.2;
        p.lifetime = (16.0 / (fastrand::f32() * 0.8 + 0.2)) as i32;
        p.light = Light::BlockRamp;
        p.size_curve = Size::Lava;
        p
    }

    /// `TotemParticle` (a `SimpleAnimatedParticle`): green and gold glitter.
    fn totem(pos: DVec3, vel: DVec3) -> Self {
        let mut p = Self::end_rod(pos, vel);
        p.friction = f64::from(0.6f32);
        p.gravity = 1.25;
        p.extra = Extra::FadeOut(None);
        let r = fastrand::f32();
        p.color = if fastrand::u8(0..4) == 0 {
            [0.6 + r * 0.2, 0.6 + r * 0.3, r * 0.2]
        } else {
            [0.1 + r * 0.2, 0.4 + r * 0.3, r * 0.2]
        };
        p
    }

    /// `HeartParticle`: floats up, no collision.
    fn heart(pos: DVec3) -> Self {
        let mut p = Self::thrown(pos, DVec3::ZERO);
        p.motion = Motion::Free;
        p.speed_up_blocked = true;
        p.friction = f64::from(0.86f32);
        p.vel *= f64::from(0.01f32);
        p.vel.y += 0.1;
        p.size *= 1.5;
        p.lifetime = 16;
        p.size_curve = Size::GrowIn;
        p
    }

    /// `CampfireSmokeParticle` (cosy, or the taller signal smoke).
    fn campfire_smoke(pos: DVec3, vel: DVec3, signal: bool) -> Self {
        Self {
            motion: Motion::Free,
            extra: Extra::CampfireSmoke,
            translucent: true,
            size: base_size() * 3.0,
            vel: dvec3(vel.x, vel.y + fastrand::f64() / 500.0, vel.z),
            lifetime: fastrand::i32(0..50) + if signal { 280 } else { 80 },
            gravity: 3.0e-6,
            friction: 1.0,
            ..Self::at(pos)
        }
    }

    /// `DripParticle` for obsidian tears: hangs, then falls.
    fn obsidian_tear(pos: DVec3, hanging: bool) -> Self {
        let mut p = Self {
            light: Light::BlockFull,
            size: base_size(),
            color: [0.51171875, 0.03125, 0.890625],
            gravity: 0.06,
            friction: 1.0,
            lifetime: (64.0 / (fastrand::f32() * 0.8 + 0.2)) as i32,
            ..Self::at(pos)
        };
        if hanging {
            p.gravity *= 0.02;
            p.lifetime = 40;
            p.extra = Extra::DripHang;
        }
        p
    }

    /// The lane `light_lane_color` reads, at `partial` into the tick.
    fn light_lane(&self, world: &WorldLight, partial: f32) -> f32 {
        let levels = match self.light {
            Light::Bright => return LightLevels::FULL_BRIGHT.lane(),
            _ => self.levels.unwrap_or_else(|| self.world_light(world)),
        };
        let (block, sky) = (
            f32::from(levels.block()) * 16.0,
            f32::from(levels.sky()) * 16.0,
        );
        let f = ((self.age as f32 + partial) / self.lifetime as f32).clamp(0.0, 1.0);
        let (block, sky) = match self.light {
            Light::SkyRamp => (block, (sky + f.powi(4) * 240.0).min(240.0)),
            Light::BlockRamp => ((block + f * 240.0).min(240.0), sky),
            Light::BlockFull => (240.0, sky),
            _ => (block, sky),
        };
        block + sky * 256.0
    }

    /// Vanilla `Particle.getLightCoords`: sky light alone outside loaded
    /// chunks.
    fn world_light(&self, world: &WorldLight) -> LightLevels {
        let at = self.pos.floor().as_ivec3();
        let loaded = world
            .chunks
            .get_chunk(&ChunkPos::new(at.x.div_euclid(16), at.z.div_euclid(16)))
            .is_some();
        if loaded {
            block_lighter::light_coords(world, at.to_array())
        } else {
            LightLevels::new(15, 0)
        }
    }

    /// The light levels a particle draws with, at the start of the tick.
    #[cfg(test)]
    fn light(&self, world: &WorldLight) -> LightLevels {
        match self.light {
            Light::Bright => LightLevels::FULL_BRIGHT,
            _ => self.world_light(world),
        }
    }

    /// `getQuadSize(partialTick)`.
    fn quad_size(&self, partial: f32) -> f32 {
        let f = (self.age as f32 + partial) / self.lifetime as f32;
        self.size
            * match self.size_curve {
                Size::Fixed => 1.0,
                Size::GrowIn => (f * 32.0).clamp(0.0, 1.0),
                Size::Portal => 1.0 - (1.0 - f) * (1.0 - f),
                Size::ReversePortal => {
                    1.0 - (self.age as f32 + partial) / (self.lifetime as f32 * 1.5)
                }
                Size::Flame => 1.0 - f * f * 0.5,
                Size::Lava => 1.0 - f * f,
            }
    }

    /// The colour at `partial` (`DustColorTransitionParticle.lerpColors`).
    fn color_at(&self, partial: f32) -> [f32; 3] {
        match self.color_to {
            Some(to) => {
                let f = (self.age as f32 + partial) / (self.lifetime as f32 + 1.0);
                std::array::from_fn(|i| self.color[i] + (to[i] - self.color[i]) * f)
            }
            None => self.color,
        }
    }

    /// Vanilla `SingleQuadParticle.setSprite`.
    fn set_sprite(&mut self, frame: &AtlasRegion) {
        self.u0 = frame.u_min;
        self.u1 = frame.u_max;
        self.v0 = frame.v_min;
        self.v1 = frame.v_max;
    }

    /// `setSpriteFromAge`: frame `age * (n - 1) / lifetime` of the set.
    fn sprite_from_age(&mut self, sets: &SpriteSets) {
        if let Some(set) = self.animated {
            let frames = sets.get(set);
            if !frames.is_empty() {
                let last = frames.len() as i32 - 1;
                let i = (self.age * last / self.lifetime.max(1)).clamp(0, last) as usize;
                self.set_sprite(&frames[i]);
            }
        }
    }

    /// Vanilla `BreakingItemParticle`: the base-constructor velocity (zero
    /// argument, jitter only) scaled to 10%, plus the spawn velocity. Shares
    /// the halved quad size and quarter sub-tile sampling with terrain.
    fn breaking_item(pos: DVec3, velocity: DVec3, region: AtlasRegion) -> Self {
        let mut p = Self::terrain(pos, DVec3::ZERO, region, [1.0; 3]);
        p.vel = p.vel * 0.1 + velocity;
        p
    }

    /// Vanilla `Particle.tick` and its overrides. Returns false when the
    /// particle expires; a hanging drip hands back the drop that replaces it.
    fn tick(
        &mut self,
        chunks: &ChunkStore,
        block_state_table: BlockStateTable,
        sets: &SpriteSets,
    ) -> bool {
        self.prev_pos = self.pos;
        if self.age >= self.lifetime {
            return false;
        }
        self.age += 1;
        let f = self.age as f64 / self.lifetime as f64;
        match self.motion {
            Motion::Portal(start) => {
                let g = 1.0 - (-f + f * f * 2.0);
                self.pos = start + self.vel * g + dvec3(0.0, 1.0 - f, 0.0);
            }
            Motion::ReversePortal => self.pos += self.vel * f,
            Motion::FlyTowards(start) => {
                // FlyTowardsPositionParticle.tick: in along (1 - f), dipping by f⁴.
                let g = f.powi(4);
                self.pos = start + self.vel * (1.0 - f) - dvec3(0.0, g * 1.2, 0.0);
            }
            Motion::Still => {}
            Motion::Physics | Motion::Free => {
                if self.extra == Extra::CampfireSmoke {
                    let drift =
                        || fastrand::f64() / 5000.0 * if fastrand::bool() { 1.0 } else { -1.0 };
                    self.vel.x += drift();
                    self.vel.z += drift();
                }
                self.vel.y -= 0.04 * self.gravity;
                let before_y = self.pos.y;
                match self.motion {
                    Motion::Physics => self.move_with_collision(chunks, block_state_table),
                    _ => self.pos += self.vel,
                }
                if self.speed_up_blocked && self.pos.y == before_y {
                    self.vel.x *= 1.1;
                    self.vel.z *= 1.1;
                }
                self.vel *= self.friction;
                if self.on_ground {
                    self.vel.x *= 0.7;
                    self.vel.z *= 0.7;
                }
            }
        }
        match self.extra {
            Extra::FadeOut(fade) => {
                if self.age > self.lifetime / 2 {
                    self.alpha = 1.0 - (self.age - self.lifetime / 2) as f32 / self.lifetime as f32;
                    if let Some(fade) = fade {
                        for (c, f) in self.color.iter_mut().zip(fade) {
                            *c += (f - *c) * 0.2;
                        }
                    }
                }
            }
            Extra::CritDrain => {
                self.color[1] *= 0.96;
                self.color[2] *= 0.9;
            }
            Extra::CampfireSmoke => {
                if self.age >= self.lifetime - 60 && self.alpha > 0.01 {
                    self.alpha -= 0.015;
                }
            }
            Extra::None | Extra::DripHang => {}
        }
        self.sprite_from_age(sets);
        if self.light != Light::Bright {
            let world = WorldLight {
                chunks,
                table: block_state_table,
            };
            self.levels = Some(self.world_light(&world));
        }
        true
    }

    /// Vanilla `Particle.move`.
    fn move_with_collision(&mut self, chunks: &ChunkStore, block_state_table: BlockStateTable) {
        if self.stopped_by_collision {
            return;
        }
        let orig = self.vel;
        let mut delta = orig;
        // Most particles drift through open air (dust clouds by the
        // thousand): when every block the moving box can touch is air, the
        // collision sweep can't change anything, so it is skipped.
        if delta != DVec3::ZERO
            && delta.length_squared() < MAX_COLLISION_VELOCITY_SQ
            && !only_air_around(chunks, block_state_table, self.pos, delta)
        {
            let aabb = Aabb::from_center(self.pos, HALF_WIDTH, HALF_WIDTH);
            (delta, _) = resolve_collision(
                crate::physics::collision::CollisionWorld::new(chunks, block_state_table),
                aabb,
                orig.into(),
                0.0,
                false,
                &CollisionContext::position(self.pos.y),
            );
        }
        self.pos += delta;
        if orig.y.abs() >= 1e-5 && delta.y.abs() < 1e-5 {
            self.stopped_by_collision = true;
        }
        self.on_ground = orig.y != delta.y && orig.y < 0.0;
        if orig.x != delta.x {
            self.vel.x = 0.0;
        }
        if orig.z != delta.z {
            self.vel.z = 0.0;
        }
    }
}

/// `SimpleAnimatedParticle.setFadeColor(0xF2DEC9)` in `EndRodParticle`.
const END_ROD_FADE: [f32; 3] = [242.0 / 255.0, 222.0 / 255.0, 201.0 / 255.0];

/// The sprite sets of `assets/minecraft/particles/*.json` server particles
/// draw from, in each file's frame order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SpriteSet {
    /// `dust`, `smoke`, `cloud`...: `generic_7` to `generic_0`.
    Generic,
    /// `portal`, `reverse_portal`: picked at random from `generic_0..7`.
    Portal,
    /// `end_rod`, `totem_of_undying`: `glitter_7` to `glitter_0`.
    Glitter,
    /// `enchant`: one random standard galactic glyph.
    Glyph,
    CriticalHit,
    EnchantedHit,
    Flame,
    SoulFireFlame,
    Explosion,
    Glint,
    /// `witch`: `spell_7` to `spell_0`.
    Spell,
    Soul,
    Glow,
    Lava,
    Sweep,
    Heart,
    /// `campfire_*_smoke`: one random `big_smoke_*`.
    BigSmoke,
    DripHang,
    DripFall,
}

/// The sprites of each set, as atlas texture keys.
const SPRITE_SETS: &[(SpriteSet, &[&str])] = &[
    (
        SpriteSet::Generic,
        &[
            "particle/generic_7",
            "particle/generic_6",
            "particle/generic_5",
            "particle/generic_4",
            "particle/generic_3",
            "particle/generic_2",
            "particle/generic_1",
            "particle/generic_0",
        ],
    ),
    (
        SpriteSet::Portal,
        &[
            "particle/generic_0",
            "particle/generic_1",
            "particle/generic_2",
            "particle/generic_3",
            "particle/generic_4",
            "particle/generic_5",
            "particle/generic_6",
            "particle/generic_7",
        ],
    ),
    (SpriteSet::Glitter, &END_ROD_SPRITES),
    (
        SpriteSet::Glyph,
        &[
            "particle/sga_a",
            "particle/sga_b",
            "particle/sga_c",
            "particle/sga_d",
            "particle/sga_e",
            "particle/sga_f",
            "particle/sga_g",
            "particle/sga_h",
            "particle/sga_i",
            "particle/sga_j",
            "particle/sga_k",
            "particle/sga_l",
            "particle/sga_m",
            "particle/sga_n",
            "particle/sga_o",
            "particle/sga_p",
            "particle/sga_q",
            "particle/sga_r",
            "particle/sga_s",
            "particle/sga_t",
            "particle/sga_u",
            "particle/sga_v",
            "particle/sga_w",
            "particle/sga_x",
            "particle/sga_y",
            "particle/sga_z",
        ],
    ),
    (SpriteSet::CriticalHit, &["particle/critical_hit"]),
    (SpriteSet::EnchantedHit, &["particle/enchanted_hit"]),
    (SpriteSet::Flame, &["particle/flame"]),
    (SpriteSet::SoulFireFlame, &["particle/soul_fire_flame"]),
    (
        SpriteSet::Explosion,
        &[
            "particle/explosion_0",
            "particle/explosion_1",
            "particle/explosion_2",
            "particle/explosion_3",
            "particle/explosion_4",
            "particle/explosion_5",
            "particle/explosion_6",
            "particle/explosion_7",
            "particle/explosion_8",
            "particle/explosion_9",
            "particle/explosion_10",
            "particle/explosion_11",
            "particle/explosion_12",
            "particle/explosion_13",
            "particle/explosion_14",
            "particle/explosion_15",
        ],
    ),
    (SpriteSet::Glint, &["particle/glint"]),
    (
        SpriteSet::Spell,
        &[
            "particle/spell_7",
            "particle/spell_6",
            "particle/spell_5",
            "particle/spell_4",
            "particle/spell_3",
            "particle/spell_2",
            "particle/spell_1",
            "particle/spell_0",
        ],
    ),
    (
        SpriteSet::Soul,
        &[
            "particle/soul_0",
            "particle/soul_1",
            "particle/soul_2",
            "particle/soul_3",
            "particle/soul_4",
            "particle/soul_5",
            "particle/soul_6",
            "particle/soul_7",
            "particle/soul_8",
            "particle/soul_9",
            "particle/soul_10",
        ],
    ),
    (SpriteSet::Glow, &["particle/glow"]),
    (SpriteSet::Lava, &["particle/lava"]),
    (
        SpriteSet::Sweep,
        &[
            "particle/sweep_0",
            "particle/sweep_1",
            "particle/sweep_2",
            "particle/sweep_3",
            "particle/sweep_4",
            "particle/sweep_5",
            "particle/sweep_6",
            "particle/sweep_7",
        ],
    ),
    (SpriteSet::Heart, &["particle/heart"]),
    (
        SpriteSet::BigSmoke,
        &[
            "particle/big_smoke_0",
            "particle/big_smoke_1",
            "particle/big_smoke_2",
            "particle/big_smoke_3",
            "particle/big_smoke_4",
            "particle/big_smoke_5",
            "particle/big_smoke_6",
            "particle/big_smoke_7",
            "particle/big_smoke_8",
            "particle/big_smoke_9",
            "particle/big_smoke_10",
            "particle/big_smoke_11",
        ],
    ),
    (SpriteSet::DripHang, &["particle/drip_hang"]),
    (SpriteSet::DripFall, &["particle/drip_fall"]),
];

/// Frame order from `assets/minecraft/particles/end_rod.json`: frame 0 is
/// `glitter_7` and the animation walks toward `glitter_0`.
pub const END_ROD_SPRITES: [&str; 8] = [
    "particle/glitter_7",
    "particle/glitter_6",
    "particle/glitter_5",
    "particle/glitter_4",
    "particle/glitter_3",
    "particle/glitter_2",
    "particle/glitter_1",
    "particle/glitter_0",
];

/// Every particle sprite the block atlas packs.
pub fn particle_sprites<'a>() -> impl Iterator<Item = &'a str> {
    SPRITE_SETS
        .iter()
        .flat_map(|(_, names)| names.iter().map(|name| -> &'a str { name }))
}

/// The atlas regions of every sprite set.
struct SpriteSets(Vec<Vec<AtlasRegion>>);

impl SpriteSets {
    fn load(uv_map: &AtlasUVMap) -> Self {
        let mut sets = Vec::new();
        for (i, (set, names)) in SPRITE_SETS.iter().enumerate() {
            debug_assert_eq!(*set as usize, i, "SPRITE_SETS in SpriteSet order");
            sets.push(
                names
                    .iter()
                    .filter(|n| uv_map.has_region(n))
                    .map(|n| uv_map.get_region(n))
                    .collect(),
            );
        }
        Self(sets)
    }

    fn get(&self, set: SpriteSet) -> &[AtlasRegion] {
        &self.0[set as usize]
    }

    /// `SpriteSet.get(random)`.
    fn pick(&self, set: SpriteSet) -> Option<AtlasRegion> {
        let frames = self.get(set);
        (!frames.is_empty()).then(|| frames[fastrand::usize(0..frames.len())])
    }

    fn first(&self, set: SpriteSet) -> Option<AtlasRegion> {
        self.get(set).first().copied()
    }
}

/// Server-sent particle types pomme implements. `from_id` returning `None`
/// drops the packet in the network handler, before the event channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServerParticleKind {
    EndRod,
    Dust,
    DustColorTransition,
    Portal,
    ReversePortal,
    Enchant,
    Block,
    BlockCrumble,
    FallingDust,
    Crit,
    EnchantedHit,
    Flame,
    SoulFireFlame,
    SmallFlame,
    Smoke,
    LargeSmoke,
    Cloud,
    Explosion,
    HappyVillager,
    Witch,
    Soul,
    ElectricSpark,
    Lava,
    SweepAttack,
    TotemOfUndying,
    Heart,
    Item,
    CampfireCosySmoke,
    CampfireSignalSmoke,
    DrippingObsidianTear,
    FallingObsidianTear,
}

impl ServerParticleKind {
    /// The kind of a particle type, by its registry name.
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name.strip_prefix("minecraft:").unwrap_or(name) {
            "end_rod" => Self::EndRod,
            "dust" => Self::Dust,
            "dust_color_transition" => Self::DustColorTransition,
            "portal" => Self::Portal,
            "reverse_portal" => Self::ReversePortal,
            "enchant" => Self::Enchant,
            "block" => Self::Block,
            "block_crumble" => Self::BlockCrumble,
            "falling_dust" => Self::FallingDust,
            "crit" => Self::Crit,
            "enchanted_hit" => Self::EnchantedHit,
            "flame" => Self::Flame,
            "soul_fire_flame" => Self::SoulFireFlame,
            "small_flame" => Self::SmallFlame,
            "smoke" => Self::Smoke,
            "large_smoke" => Self::LargeSmoke,
            "cloud" => Self::Cloud,
            "explosion" => Self::Explosion,
            "happy_villager" => Self::HappyVillager,
            "witch" => Self::Witch,
            "soul" => Self::Soul,
            "electric_spark" => Self::ElectricSpark,
            "lava" => Self::Lava,
            "sweep_attack" => Self::SweepAttack,
            "totem_of_undying" => Self::TotemOfUndying,
            "heart" => Self::Heart,
            "item" => Self::Item,
            "campfire_cosy_smoke" => Self::CampfireCosySmoke,
            "campfire_signal_smoke" => Self::CampfireSignalSmoke,
            "dripping_obsidian_tear" => Self::DrippingObsidianTear,
            "falling_obsidian_tear" => Self::FallingObsidianTear,
            _ => return None,
        })
    }

    /// Maps a native particle registry id (`ParticleTypes` registration
    /// order; ids shift between versions, the handler remaps older ones
    /// first). Pomme owns this mapping because azalea's particle wire enum
    /// is out of sync with the registry.
    pub fn from_id(id: u32) -> Option<Self> {
        static BY_ID: std::sync::OnceLock<Vec<Option<ServerParticleKind>>> =
            std::sync::OnceLock::new();
        BY_ID
            .get_or_init(|| {
                let table = pomme_protocol::RegistryTable::native();
                (0..)
                    .map_while(|i| {
                        table
                            .name_of(pomme_protocol::ClientRegistry::ParticleType, i)
                            .map(Self::from_name)
                    })
                    .collect()
            })
            .get(id as usize)
            .copied()
            .flatten()
    }

    /// Vanilla `ParticleType.getOverrideLimiter`.
    fn override_limiter(self) -> bool {
        false
    }
}

/// A particle's options after its type id (`ParticleOptions` codecs).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ParticleOptions {
    None,
    /// `DustParticleOptions`: an RGB colour and a scale.
    Dust {
        color: u32,
        scale: f32,
    },
    /// `DustColorTransitionOptions`.
    DustTransition {
        from: u32,
        to: u32,
        scale: f32,
    },
    /// `BlockParticleOption`: a block state, in the session's state ids.
    Block(u32),
    /// `ItemParticleOption`: a native item id.
    Item(u32),
}

/// How a block or item particle looks, resolved by the caller (which has
/// the block registry and the world).
#[derive(Clone, Copy)]
pub enum ParticleLook {
    None,
    /// A terrain sprite and its tint.
    Terrain(AtlasRegion, [f32; 3]),
}

pub struct ParticleStore {
    particles: Vec<Particle>,
    /// Spawned this tick; drained after live particles tick, so a particle's
    /// first physics tick is the tick after it spawns (vanilla
    /// `ParticleEngine.particlesToAdd`).
    pending: Vec<Particle>,
    uv_map: AtlasUVMap,
    sets: SpriteSets,
    grass_colormap: Arc<Colormap>,
    foliage_colormap: Arc<Colormap>,
    dry_foliage_colormap: Arc<Colormap>,
}

impl ParticleStore {
    pub fn new(
        uv_map: AtlasUVMap,
        grass_colormap: Arc<Colormap>,
        foliage_colormap: Arc<Colormap>,
        dry_foliage_colormap: Arc<Colormap>,
    ) -> Self {
        let sets = SpriteSets::load(&uv_map);
        Self {
            particles: Vec::new(),
            pending: Vec::new(),
            uv_map,
            sets,
            grass_colormap,
            foliage_colormap,
            dry_foliage_colormap,
        }
    }

    /// Vanilla `ClientLevel.addDestroyBlockEffect`: a grid of terrain
    /// particles across each box of the block's shape (4x4x4 for a full
    /// cube).
    pub fn add_destroy_block_effect(
        &mut self,
        pos: BlockPos,
        state: BlockState,
        registry: BlockRenderView<'_>,
        chunks: &ChunkStore,
        biome_climate: &HashMap<u32, BiomeClimate>,
    ) {
        let Some((region, color)) =
            self.terrain_appearance(pos, state, registry, chunks, biome_climate)
        else {
            return;
        };
        let block_state_data = registry.block_state_table().block_state_data(state);
        // Vanilla `ParticleEngine.destroy` scatters over the outline shape's
        // boxes, so a block with an empty outline emits nothing.
        let boxes: &[LocalBox] = block_shape::outline_shape(block_state_data);
        let shape_origin = block_state_data.outline_shape_position(pos.x, pos.y, pos.z);

        for b in boxes {
            let width_x = (b[3] - b[0]).min(1.0);
            let width_y = (b[4] - b[1]).min(1.0);
            let width_z = (b[5] - b[2]).min(1.0);
            let count_x = ((width_x / 0.25).ceil() as i32).max(2);
            let count_y = ((width_y / 0.25).ceil() as i32).max(2);
            let count_z = ((width_z / 0.25).ceil() as i32).max(2);
            for xx in 0..count_x {
                for yy in 0..count_y {
                    for zz in 0..count_z {
                        let rel_x = (xx as f64 + 0.5) / count_x as f64;
                        let rel_y = (yy as f64 + 0.5) / count_y as f64;
                        let rel_z = (zz as f64 + 0.5) / count_z as f64;
                        let spawn = DVec3::new(
                            shape_origin.x + rel_x * width_x + b[0],
                            shape_origin.y + rel_y * width_y + b[1],
                            shape_origin.z + rel_z * width_z + b[2],
                        );
                        self.push(Particle::terrain(
                            spawn,
                            DVec3::new(rel_x - 0.5, rel_y - 0.5, rel_z - 0.5),
                            region,
                            color,
                        ));
                    }
                }
            }
        }
    }

    /// Vanilla `ClientLevel.addParticle` of a `minecraft:block` particle
    /// (`TerrainParticle.Provider`) with zero speed, tinted at the block
    /// containing `pos`.
    pub fn add_block_particle(
        &mut self,
        pos: DVec3,
        state: BlockState,
        registry: BlockRenderView<'_>,
        chunks: &ChunkStore,
        biome_climate: &HashMap<u32, BiomeClimate>,
        camera_pos: DVec3,
    ) {
        if beyond_particle_range(camera_pos, pos)
            || registry.block_state_table().block_state_data(state).id() == "moving_piston"
        {
            return;
        }
        if let Some((region, color)) = self.terrain_appearance(
            BlockPos::containing(pos),
            state,
            registry,
            chunks,
            biome_climate,
        ) {
            self.push(Particle::terrain(pos, DVec3::ZERO, region, color));
        }
    }

    /// `TerrainParticle`'s sprite and color for `state` at `pos`; `None` for
    /// blocks that spawn no terrain particles.
    pub(crate) fn terrain_appearance(
        &self,
        pos: BlockPos,
        state: BlockState,
        registry: BlockRenderView<'_>,
        chunks: &ChunkStore,
        biome_climate: &HashMap<u32, BiomeClimate>,
    ) -> Option<(AtlasRegion, [f32; 3])> {
        let block_state_data = registry.block_state_table().block_state_data(state);
        if block_state_data.is_air() {
            return None;
        }
        let block_id = block_state_data.id();
        // The only vanilla `noTerrainParticles` blocks.
        if matches!(block_id, "barrier" | "structure_void") {
            return None;
        }

        // TerrainParticle base grey, multiplied by the block's biome tint.
        // grass_block is exempt (vanilla `colorAsTerrainParticle` returns
        // white for it — its particle texture is dirt).
        let mut color = [0.6f32; 3];
        let faces = registry.get_textures(state);
        if let Some(faces) = faces
            && faces.tint != Tint::None
            && block_id != "grass_block"
        {
            let tint = match faces.tint {
                Tint::Redstone => block_state_data.redstone_wire_rgb(),
                Tint::Stem => block_state_data.stem_rgb(),
                Tint::Constant(color) => crate::renderer::chunk::mesher::int_to_rgb(color as i32),
                _ => self.blend_tint(
                    faces.tint,
                    state,
                    pos,
                    chunks,
                    biome_climate,
                    registry.block_state_table(),
                ),
            };
            for (c, t) in color.iter_mut().zip(tint) {
                *c *= t;
            }
        }

        let region = match faces {
            Some(faces) => self
                .uv_map
                .get_region(faces.particle.as_deref().unwrap_or(&faces.top)),
            // No face textures at all: the missing-texture region.
            None => self.uv_map.get_region(""),
        };
        Some((region, color))
    }

    /// `texture`'s atlas region, if the atlas has it.
    pub(crate) fn item_region(&self, texture: &str) -> Option<AtlasRegion> {
        self.uv_map
            .has_region(texture)
            .then(|| self.uv_map.get_region(texture))
    }

    /// Vanilla `LivingEntity.spawnItemParticles`: item crumbs thrown from the
    /// mouth along the look direction while eating.
    pub fn add_item_use_particles(
        &mut self,
        count: u32,
        texture: &str,
        eye_pos: DVec3,
        x_rot_deg: f32,
        y_rot_deg: f32,
    ) {
        let Some(region) = self.item_region(texture) else {
            return;
        };
        let x_rot = -(x_rot_deg as f64).to_radians();
        let y_rot = -(y_rot_deg as f64).to_radians();
        for _ in 0..count {
            let p = dvec3(
                (fastrand::f64() - 0.5) * 0.3,
                -fastrand::f64() * 0.6 - 0.3,
                0.6,
            );
            let p = rot_y(rot_x(p, x_rot), y_rot) + eye_pos;
            self.push(Particle::breaking_item(
                p,
                crumb_velocity(x_rot, y_rot),
                region,
            ));
        }
    }

    /// Vanilla `Fox.handleEntityEvent(45)`: eight crumbs of the food in its
    /// mouth, half its look vector ahead of its feet.
    pub fn add_fox_eating_particles(
        &mut self,
        texture: &str,
        pos: DVec3,
        look: crate::entity::components::LookDirection,
        camera_pos: DVec3,
    ) {
        let Some(region) = self.item_region(texture) else {
            return;
        };
        let x_rot = -f64::from(look.x_rot_rad());
        let y_rot = -f64::from(look.y_rot_rad());
        let ahead = look.as_vec().as_dvec3() / 2.0;
        let p = dvec3(pos.x + ahead.x, pos.y, pos.z + ahead.z);
        if beyond_particle_range(camera_pos, p) {
            return;
        }
        for _ in 0..8 {
            self.push(Particle::breaking_item(
                p,
                crumb_velocity(x_rot, y_rot),
                region,
            ));
        }
    }

    /// Vanilla `Panda.addEatingParticles`: six crumbs of the held item in
    /// front of the head. `ClientLevel.addParticle` culls each past 32 blocks.
    #[allow(clippy::too_many_arguments)]
    pub fn add_panda_eating_particles(
        &mut self,
        texture: &str,
        pos: DVec3,
        eye_y: f64,
        x_rot_deg: f32,
        y_rot_deg: f32,
        body_y_rot_deg: f32,
        camera_pos: DVec3,
    ) {
        let Some(region) = self.item_region(texture) else {
            return;
        };
        let rand = || f64::from(fastrand::f32());
        let x_rot = f64::from(-x_rot_deg.to_radians());
        let y_rot = f64::from(-y_rot_deg.to_radians());
        let body_rot = f64::from(-body_y_rot_deg.to_radians());
        for _ in 0..6 {
            let v = dvec3(
                (rand() - 0.5) * 0.1,
                rand() * 0.1 + 0.1,
                (rand() - 0.5) * 0.1,
            );
            let v = rot_y(rot_x(v, x_rot), y_rot);
            let p = dvec3(
                (rand() - 0.5) * 0.8,
                -rand() * 0.6 - 0.3,
                1.0 + (rand() - 0.5) * 0.4,
            );
            let p = rot_y(p, body_rot) + dvec3(pos.x, eye_y + 1.0, pos.z);
            if beyond_particle_range(camera_pos, p) {
                continue;
            }
            self.push(Particle::breaking_item(
                p,
                dvec3(v.x, v.y + 0.05, v.z),
                region,
            ));
        }
    }

    /// Vanilla `ClientPacketListener.handleParticleEvent`: count 0 is a
    /// single directional particle (velocity = dist * max_speed), otherwise a
    /// gaussian scatter of `count` particles around the position.
    #[allow(clippy::too_many_arguments)]
    pub fn add_particles_from_packet(
        &mut self,
        kind: ServerParticleKind,
        options: ParticleOptions,
        look: ParticleLook,
        override_limiter: bool,
        pos: DVec3,
        dist: DVec3,
        max_speed: f64,
        count: u32,
        camera_pos: DVec3,
    ) {
        let spawn = Spawn {
            kind,
            options,
            look,
            override_limiter,
        };
        if count == 0 {
            self.add_server_particle(spawn, pos, dist * max_speed, camera_pos);
            return;
        }
        for _ in 0..count {
            let scatter = dvec3(
                next_gaussian() * dist.x,
                next_gaussian() * dist.y,
                next_gaussian() * dist.z,
            );
            let vel = dvec3(next_gaussian(), next_gaussian(), next_gaussian()) * max_speed;
            self.add_server_particle(spawn, pos + scatter, vel, camera_pos);
        }
    }

    /// Vanilla `ClientLevel.doAddParticle`, then the type's `Provider`.
    /// Pomme reports `ParticleStatus::All` in client information, so the
    /// MINIMAL/DECREASED branches are unreachable and only the 32-block
    /// camera cull applies.
    fn add_server_particle(&mut self, spawn: Spawn, pos: DVec3, vel: DVec3, camera_pos: DVec3) {
        use ServerParticleKind as K;
        if !(spawn.override_limiter || spawn.kind.override_limiter())
            && beyond_particle_range(camera_pos, pos)
        {
            return;
        }
        let sets = &self.sets;
        let with = |mut p: Particle, sprite: Option<AtlasRegion>| {
            if let Some(sprite) = sprite {
                p.set_sprite(&sprite);
            }
            p
        };
        let particle = match spawn.kind {
            K::EndRod => {
                let mut p = Particle::end_rod(pos, vel);
                p.sprite_from_age(sets);
                p
            }
            K::Dust | K::DustColorTransition => {
                let (color, to, scale) = match spawn.options {
                    ParticleOptions::Dust { color, scale } => (color, None, scale),
                    ParticleOptions::DustTransition { from, to, scale } => (from, Some(to), scale),
                    _ => return,
                };
                let mut p = Particle::dust(pos, vel, color, to, scale.clamp(0.01, 4.0));
                p.sprite_from_age(sets);
                p
            }
            K::Portal => with(
                Particle::portal(pos, vel, false),
                sets.pick(SpriteSet::Portal),
            ),
            K::ReversePortal => with(
                Particle::portal(pos, vel, true),
                sets.pick(SpriteSet::Portal),
            ),
            K::Enchant => with(Particle::enchant(pos, vel), sets.pick(SpriteSet::Glyph)),
            K::Block | K::BlockCrumble | K::FallingDust => {
                let ParticleLook::Terrain(region, color) = spawn.look else {
                    return;
                };
                let mut p = Particle::terrain(pos, vel, region, color);
                match spawn.kind {
                    // `FallingDustParticle`: drifts down slowly, no hop.
                    K::FallingDust => {
                        p.vel = DVec3::ZERO;
                        p.gravity = 0.075;
                        p.lifetime = (32.0 / (fastrand::f32() * 0.8 + 0.2)) as i32;
                    }
                    // `TerrainParticle.CrumblingProvider`: settles in place.
                    K::BlockCrumble => {
                        p.vel = DVec3::ZERO;
                        p.gravity = 0.0;
                        p.lifetime = fastrand::i32(0..4) + 1;
                    }
                    _ => {}
                }
                p
            }
            K::Item => {
                let ParticleLook::Terrain(region, _) = spawn.look else {
                    return;
                };
                Particle::breaking_item(pos, vel, region)
            }
            K::Crit => with(
                Particle::crit(pos, vel, false),
                sets.first(SpriteSet::CriticalHit),
            ),
            K::EnchantedHit => with(
                Particle::crit(pos, vel, true),
                sets.first(SpriteSet::EnchantedHit),
            ),
            K::Flame => with(Particle::flame(pos, vel, 1.0), sets.first(SpriteSet::Flame)),
            K::SmallFlame => with(Particle::flame(pos, vel, 0.5), sets.first(SpriteSet::Flame)),
            K::SoulFireFlame => with(
                Particle::flame(pos, vel, 1.0),
                sets.first(SpriteSet::SoulFireFlame),
            ),
            K::Smoke | K::LargeSmoke => {
                let scale = if spawn.kind == K::LargeSmoke {
                    2.5
                } else {
                    1.0
                };
                let mut p = Particle::smoke(pos, vel, scale);
                p.sprite_from_age(sets);
                p
            }
            K::Cloud => {
                let mut p = Particle::cloud(pos, vel);
                p.sprite_from_age(sets);
                p
            }
            K::Explosion => {
                let mut p = Particle::explosion(pos, vel.x);
                p.sprite_from_age(sets);
                p
            }
            K::SweepAttack => {
                let mut p = Particle::sweep(pos, vel.x);
                p.sprite_from_age(sets);
                p
            }
            K::HappyVillager => with(
                Particle::happy_villager(pos, vel),
                sets.first(SpriteSet::Glint),
            ),
            K::Witch => {
                let mut p = Particle::witch(pos, vel);
                p.sprite_from_age(sets);
                p
            }
            K::Soul => {
                let mut p = Particle::soul(pos, vel);
                p.sprite_from_age(sets);
                p
            }
            K::ElectricSpark => with(Particle::spark(pos, vel), sets.first(SpriteSet::Glow)),
            K::Lava => with(Particle::lava(pos), sets.first(SpriteSet::Lava)),
            K::TotemOfUndying => {
                let mut p = Particle::totem(pos, vel);
                p.sprite_from_age(sets);
                p
            }
            K::Heart => with(Particle::heart(pos), sets.first(SpriteSet::Heart)),
            K::CampfireCosySmoke | K::CampfireSignalSmoke => with(
                Particle::campfire_smoke(pos, vel, spawn.kind == K::CampfireSignalSmoke),
                sets.pick(SpriteSet::BigSmoke),
            ),
            K::DrippingObsidianTear => with(
                Particle::obsidian_tear(pos, true),
                sets.first(SpriteSet::DripHang),
            ),
            K::FallingObsidianTear => with(
                Particle::obsidian_tear(pos, false),
                sets.first(SpriteSet::DripFall),
            ),
        };
        self.push(particle);
    }

    /// The block's biome tint using vanilla's default blend radius.
    fn blend_tint(
        &self,
        tint: Tint,
        state: BlockState,
        pos: BlockPos,
        chunks: &ChunkStore,
        biome_climate: &HashMap<u32, BiomeClimate>,
        block_state_table: BlockStateTable,
    ) -> [f32; 3] {
        let sample_y = tint_sample_y(tint, state, pos.y, block_state_table);
        blend_color(pos.x, pos.z, |x, z| {
            let climate = biome_climate
                .get(&chunks.biome_id(x, sample_y, z))
                .copied()
                .unwrap_or_default();
            match tint {
                Tint::Grass | Tint::DoubleGrass => {
                    grass_color(&climate, &self.grass_colormap, x, z)
                }
                Tint::Foliage => foliage_color(&climate, &self.foliage_colormap),
                Tint::DryFoliage => dry_foliage_color(&climate, &self.dry_foliage_colormap),
                Tint::Water => climate.water_color,
                // State/fixed colors are resolved by the caller.
                Tint::None | Tint::Constant(_) | Tint::Redstone | Tint::Stem => [1.0; 3],
            }
        })
    }

    /// Vanilla `ParticleGroup` caps: hard limit plus probabilistic rejection
    /// once the reservoir fills.
    fn push(&mut self, particle: Particle) {
        let count = self.particles.len() + self.pending.len();
        if count >= MAX_PARTICLES {
            return;
        }
        if count >= RESERVOIR_START {
            let free = (MAX_PARTICLES - count) as f32 / 4096.0;
            if fastrand::f32() >= free * free {
                return;
            }
        }
        self.pending.push(particle);
    }

    pub fn clear(&mut self) {
        self.particles.clear();
        self.pending.clear();
    }

    pub fn tick(&mut self, chunks: &ChunkStore, block_state_table: BlockStateTable) {
        let sets = &self.sets;
        let pending = &mut self.pending;
        self.particles.retain_mut(|p| {
            let alive = p.tick(chunks, block_state_table, sets);
            // `DripHangParticle.postMoveUpdate` on expiry: the drop falls.
            if !alive && p.extra == Extra::DripHang {
                let mut drop = Particle::obsidian_tear(p.pos, false);
                if let Some(sprite) = sets.first(SpriteSet::DripFall) {
                    drop.set_sprite(&sprite);
                }
                pending.push(drop);
            }
            alive && p.alpha > 0.0
        });
        self.particles.append(&mut self.pending);
    }

    /// Quad positions are anchor-relative, subtracted in f64 (see
    /// `Camera::anchor`).
    pub fn extract(
        &self,
        partial_tick: f32,
        anchor: DVec3,
        world: &WorldLight,
    ) -> Vec<ParticleQuad> {
        self.particles
            .iter()
            .map(|p| {
                let pos = (p.prev_pos.lerp(p.pos, partial_tick as f64) - anchor).as_vec3();
                let channel = |c: f32| (c.clamp(0.0, 1.0) * 255.0).round() as u8;
                let color = p.color_at(partial_tick);
                ParticleQuad {
                    pos: pos.into(),
                    size: p.quad_size(partial_tick),
                    u0: p.u0,
                    u1: p.u1,
                    v0: p.v0,
                    v1: p.v1,
                    color: u32::from_le_bytes([
                        channel(color[0]),
                        channel(color[1]),
                        channel(color[2]),
                        channel(p.alpha),
                    ]),
                    light: p.light_lane(world, partial_tick),
                    translucent: p.translucent,
                }
            })
            .collect()
    }
}

/// What one packet spawns: the kind, its options and how it looks.
#[derive(Clone, Copy)]
struct Spawn {
    kind: ServerParticleKind,
    options: ParticleOptions,
    look: ParticleLook,
    override_limiter: bool,
}

/// `DustParticleBase.randomizeColor` of each channel of `rgb`, after the
/// particle's `shade`.
fn randomize_color(rgb: u32, shade: f32) -> [f32; 3] {
    unpack_rgb(rgb).map(|c| (fastrand::f32() * 0.2 + 0.8) * c * shade)
}

/// `0xRRGGBB` as channels in 0..1.
fn unpack_rgb(rgb: u32) -> [f32; 3] {
    [rgb >> 16, rgb >> 8, rgb].map(|c| (c & 0xFF) as f32 / 255.0)
}

/// Whether every block a particle's box can reach moving by `delta` this
/// tick is air: the collision sweep then changes nothing.
fn only_air_around(
    chunks: &ChunkStore,
    block_state_table: BlockStateTable,
    pos: DVec3,
    delta: DVec3,
) -> bool {
    let to = pos + delta;
    let lo = pos.min(to) - dvec3(HALF_WIDTH, 0.0, HALF_WIDTH);
    let hi = pos.max(to) + dvec3(HALF_WIDTH, 2.0 * HALF_WIDTH, HALF_WIDTH);
    let (lo, hi) = (lo.floor().as_ivec3(), hi.floor().as_ivec3());
    if (hi - lo).cmpgt(glam::IVec3::splat(1)).any() {
        return false;
    }
    for x in lo.x..=hi.x {
        for y in lo.y..=hi.y {
            for z in lo.z..=hi.z {
                let state = chunks.get_block_state(x, y, z);
                if !block_state_table.block_state_data(state).is_air() {
                    return false;
                }
            }
        }
    }
    true
}

/// `LevelRenderer.addParticleInternal`'s 32-block camera cull.
fn beyond_particle_range(camera_pos: DVec3, pos: DVec3) -> bool {
    camera_pos.distance_squared(pos) > 1024.0
}

/// `java.util.Random.nextGaussian` (Marsaglia polar method), minus the
/// second-sample cache.
pub(crate) fn next_gaussian() -> f64 {
    loop {
        let v1 = 2.0 * fastrand::f64() - 1.0;
        let v2 = 2.0 * fastrand::f64() - 1.0;
        let s = v1 * v1 + v2 * v2;
        if s < 1.0 && s != 0.0 {
            return v1 * (-2.0 * s.ln() / s).sqrt();
        }
    }
}

/// The velocity vanilla throws item crumbs with: a small random push up and
/// sideways, turned by the eater's `x_rot` and `y_rot` (radians, negated).
fn crumb_velocity(x_rot: f64, y_rot: f64) -> DVec3 {
    let d = dvec3(
        (fastrand::f64() - 0.5) * 0.1,
        fastrand::f64() * 0.1 + 0.1,
        0.0,
    );
    rot_y(rot_x(d, x_rot), y_rot) + dvec3(0.0, 0.05, 0.0)
}

/// Vanilla `Vec3.xRot`: rotation about the X axis.
fn rot_x(v: DVec3, angle: f64) -> DVec3 {
    let (sin, cos) = angle.sin_cos();
    dvec3(v.x, v.y * cos - v.z * sin, v.y * sin + v.z * cos)
}

/// Vanilla `Vec3.yRot`: rotation about the Y axis.
fn rot_y(v: DVec3, angle: f64) -> DVec3 {
    let (sin, cos) = angle.sin_cos();
    dvec3(v.x * cos + v.z * sin, v.y, -v.x * sin + v.z * cos)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn particles_take_vanilla_light_coords() {
        let chunks = ChunkStore::new(8);
        let world = WorldLight {
            chunks: &chunks,
            table: BlockStateTable::native(),
        };
        let region = AtlasRegion::UNIT;
        let terrain = Particle::terrain(DVec3::new(0.5, 64.0, 0.5), DVec3::ZERO, region, [1.0; 3]);
        // No chunk loaded: `Particle.getLightCoords`' sky-only fallback.
        assert_eq!(terrain.light(&world), LightLevels::new(15, 0));
        let end_rod = Particle::end_rod(DVec3::ZERO, DVec3::ZERO);
        assert_eq!(end_rod.light(&world), LightLevels::FULL_BRIGHT);
    }

    #[test]
    fn particle_kinds_come_from_native_registry_names() {
        let table = pomme_protocol::RegistryTable::native();
        let id = |name: &str| {
            table
                .id_of(pomme_protocol::ClientRegistry::ParticleType, name)
                .unwrap()
        };
        assert_eq!(
            ServerParticleKind::from_id(id("end_rod")),
            Some(ServerParticleKind::EndRod)
        );
        assert_eq!(
            ServerParticleKind::from_id(id("dust_color_transition")),
            Some(ServerParticleKind::DustColorTransition)
        );
        assert_eq!(ServerParticleKind::from_id(id("angry_villager")), None);
        assert_eq!(ServerParticleKind::from_id(9999), None);
    }

    #[test]
    fn dust_takes_its_colours_and_grows_in() {
        let p = Particle::dust(DVec3::ZERO, DVec3::ZERO, 0xFF0000, Some(0x0000FF), 1.0);
        // randomizeColor: 0.6..1 shade times 0.8..1 noise.
        assert!(p.color[0] > 0.45 && p.color[1] == 0.0 && p.color[2] == 0.0);
        let to = p.color_to.unwrap();
        assert!(to[2] > 0.45 && to[0] == 0.0);
        assert_eq!(p.quad_size(0.0), 0.0);
        let end = p.color_at(p.lifetime as f32 + 1.0);
        assert!((end[2] - to[2]).abs() < 1e-5);
    }
}
