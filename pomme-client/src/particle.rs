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
    /// `TrailParticle`: closes on a point, arriving as its life ends.
    Homing(DVec3),
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
    /// `FireworkParticles.OverlayParticle` (`flash`): swells and shrinks
    /// over its four ticks.
    Flash,
}

/// After-tick behaviour beyond movement and the sprite.
#[derive(Clone, Copy, Debug, PartialEq)]
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
    DripHang(Drip),
    /// `DripParticle.FallAndLandParticle`: gone on landing, leaving its
    /// splash or landed drip.
    DripFall(Drip),
    /// `SquidInkParticle`: fades over its second half, and sinks in air.
    InkFade,
    /// `BubbleParticle`: gone once out of water.
    Bubble,
    /// `WakeParticle` (`fishing`): steps through the splash frames.
    Wake,
    /// `FallingLeavesParticle`: blown along `flow`, around in a `swirl` of
    /// that period, both scaled by `wind`; gone when it lands or stops.
    Leaf {
        flow: Option<[f32; 2]>,
        swirl: Option<f32>,
        wind: f32,
        /// How far round it has spun, last tick and this, in radians.
        roll: [f32; 2],
        /// Its spin, in radians a second, and how that grows a second.
        spin: f32,
        spin_up: f32,
    },
    /// `OverlayParticle`: dims each tick.
    Flash,
    /// `FireworkParticles.SparkParticle` from a rocket's burst: fading
    /// toward `fade`, leaving a spark behind every other tick of its first
    /// half with `trail`, and blinking through its last two thirds with
    /// `twinkle`.
    Spark {
        fade: Option<[f32; 3]>,
        trail: bool,
        twinkle: bool,
    },
    /// `WaterDropParticle`: gone on landing half the time, or once it sinks
    /// into a block's shape or a fluid.
    WaterDrop,
}

/// What a drip is made of (`DripParticle`'s providers).
#[derive(Clone, Copy, Debug, PartialEq)]
enum Drip {
    Water,
    Lava,
    ObsidianTear,
}

impl Drip {
    /// The colour of the drip once it falls; hanging lava cools toward it.
    fn color(self) -> [f32; 3] {
        match self {
            Self::Water => [0.2, 0.3, 1.0],
            Self::Lava => [1.0, 0.285_714_3, 0.083_333_336],
            Self::ObsidianTear => [0.51171875, 0.03125, 0.890625],
        }
    }

    /// `DripParticle.isGlowing`.
    fn light(self) -> Light {
        match self {
            Self::Water => Light::World,
            Self::Lava | Self::ObsidianTear => Light::BlockFull,
        }
    }
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
    /// What each axis keeps on top of `friction` (a `tick` override's own
    /// slowing, such as `SnowflakeParticle`'s).
    damp: DVec3,
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
            damp: DVec3::ONE,
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

    /// `WaterDropParticle` (`rain`): a small hop up, then falling at 0.06 a
    /// tick.
    fn water_drop(pos: DVec3) -> Self {
        let mut p = Self::thrown(pos, DVec3::ZERO);
        p.vel.x *= f64::from(0.3f32);
        p.vel.y = fastrand::f64() * f64::from(0.2f32) + f64::from(0.1f32);
        p.vel.z *= f64::from(0.3f32);
        // Its own tick subtracts `gravity` itself, not 0.04 times it.
        p.gravity = f64::from(0.06f32) / 0.04;
        p.lifetime = (8.0 / (fastrand::f64() * 0.8 + 0.2)) as i32;
        p.extra = Extra::WaterDrop;
        p
    }

    /// `WaterDropParticle.tick`'s ends: a landed drop goes half the time, and
    /// one below the top of its block's collision shape or fluid goes too.
    fn water_drop_gone(&self, chunks: &ChunkStore, block_state_table: BlockStateTable) -> bool {
        if self.on_ground && fastrand::bool() {
            return true;
        }
        let [bx, by, bz] = [self.pos.x, self.pos.y, self.pos.z].map(|c| c.floor() as i32);
        let data = block_state_table.block_state_data(chunks.get_block_state(bx, by, bz));
        let (fx, fz) = (self.pos.x - f64::from(bx), self.pos.z - f64::from(bz));
        // `VoxelShape.max(Y, x, z)`: the top of the boxes over that point.
        let shape_top = match crate::physics::block_shape::collision_shape(
            data,
            by,
            &CollisionContext::EMPTY,
        ) {
            None => 1.0,
            Some(boxes) => boxes
                .iter()
                .filter(|b| (b[0]..=b[3]).contains(&fx) && (b[2]..=b[5]).contains(&fz))
                .map(|b| b[4])
                .fold(0.0, f64::max),
        };
        let top = shape_top.max(f64::from(data.fluid().height()));
        top > 0.0 && self.pos.y < f64::from(by) + top
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

    /// `GustParticle` (`gust`, and `small_gust` at 0.15 of the size): a
    /// still, full-bright puff played through its frames.
    fn gust(pos: DVec3, small: bool) -> Self {
        Self {
            motion: Motion::Still,
            light: Light::Bright,
            animated: Some(if small {
                SpriteSet::SmallGust
            } else {
                SpriteSet::Gust
            }),
            lifetime: 12 + fastrand::i32(0..4),
            size: if small { 0.15 } else { 1.0 },
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
        let shade = fastrand::f32() * 0.5 + 0.35;
        Self::spell(pos, vel, [shade, 0.0, shade], 1.0)
    }

    /// `SpellParticle`, coloured by its provider: `witch` a random purple,
    /// `entity_effect` its option's colour and alpha.
    fn spell(pos: DVec3, vel: DVec3, color: [f32; 3], alpha: f32) -> Self {
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
        p.color = color;
        p.alpha = alpha;
        p.animated = Some(SpriteSet::Spell);
        p.translucent = true;
        p
    }

    /// `BubblePopParticle`: the spawn velocity, 0.008 gravity, no friction,
    /// four ticks through its frames.
    fn bubble_pop(pos: DVec3, vel: DVec3) -> Self {
        Self {
            vel,
            gravity: f64::from(0.008f32) / 0.04,
            friction: 1.0,
            lifetime: 4,
            animated: Some(SpriteSet::BubblePop),
            ..Self::at(pos)
        }
    }

    /// `BaseAshSmokeParticle` as `AshParticle` (grey, falling at 0.1,
    /// ignoring the spawn velocity) or `WhiteAshParticle` (pale lilac,
    /// drifting down at 0.0125 with its provider's random speed).
    fn ash(pos: DVec3, white: bool) -> Self {
        let mut p = Self::thrown(pos, DVec3::ZERO);
        p.motion = Motion::Free;
        p.friction = f64::from(0.96f32);
        p.speed_up_blocked = true;
        p.vel *= dvec3(0.1, -0.1, 0.1);
        if white {
            let r = || fastrand::f64();
            p.vel += dvec3(
                r() * -1.9 * r() * 0.1,
                r() * -0.5 * r() * 0.1 * 5.0,
                r() * -1.9 * r() * 0.1,
            );
            p.gravity = f64::from(0.0125f32);
            p.color = [0.729_411_8, 0.694_117_67, 0.760_784_3];
        } else {
            p.gravity = 0.1;
            p.color = [fastrand::f32() * 0.5; 3];
        }
        p.size *= 0.75;
        p.lifetime = ((20.0 / (fastrand::f32() * 0.8 + 0.2)) as i32).max(1);
        p.animated = Some(SpriteSet::Ash);
        p
    }

    /// `ExplodeParticle` (`poof`): the spawn velocity jittered, rising
    /// slowly, sized at random.
    fn poof(pos: DVec3, vel: DVec3) -> Self {
        let jitter = || (fastrand::f64() * 2.0 - 1.0) * 0.05;
        let shade = fastrand::f32() * 0.3 + 0.7;
        let r = fastrand::f32();
        Self {
            vel: vel + dvec3(jitter(), jitter(), jitter()),
            gravity: f64::from(-0.1f32),
            friction: f64::from(0.9f32),
            color: [shade; 3],
            size: 0.1 * (r * fastrand::f32() * 6.0 + 1.0),
            lifetime: (16.0 / (fastrand::f32() * 0.8 + 0.2)) as i32 + 2,
            animated: Some(SpriteSet::Generic),
            ..Self::at(pos)
        }
    }

    /// `SplashParticle`: a water drop at 0.04 gravity, thrown sideways when
    /// spawned moving only sideways.
    fn splash(pos: DVec3, vel: DVec3) -> Self {
        let mut p = Self::water_drop(pos);
        p.gravity = 1.0;
        if vel.y == 0.0 && (vel.x != 0.0 || vel.z != 0.0) {
            p.vel = dvec3(vel.x, 0.1, vel.z);
        }
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

    /// `DripParticle`: falls at 0.06 a tick (its own `tick`, not 0.04 times
    /// its gravity) and keeps 0.98 of its speed.
    fn drip(pos: DVec3, kind: Drip) -> Self {
        Self {
            light: kind.light(),
            color: kind.color(),
            gravity: f64::from(0.06f32) / 0.04,
            friction: f64::from(0.98f32),
            ..Self::at(pos)
        }
    }

    /// `DripParticle.DripHangParticle`: hangs for two seconds, barely
    /// moving, then falls.
    fn drip_hang(pos: DVec3, kind: Drip) -> Self {
        let mut p = Self::drip(pos, kind);
        p.gravity *= f64::from(0.02f32);
        p.friction *= f64::from(0.02f32);
        p.lifetime = 40;
        p.extra = Extra::DripHang(kind);
        p
    }

    /// `DripParticle.FallAndLandParticle`.
    fn drip_fall(pos: DVec3, kind: Drip) -> Self {
        let mut p = Self::drip(pos, kind);
        p.lifetime = (64.0 / (fastrand::f64() * 0.8 + 0.2)) as i32;
        p.extra = Extra::DripFall(kind);
        p
    }

    /// `DripParticle.DripLandParticle`.
    fn drip_land(pos: DVec3, kind: Drip) -> Self {
        let mut p = Self::drip(pos, kind);
        p.lifetime = (16.0 / (fastrand::f64() * 0.8 + 0.2)) as i32;
        p
    }

    /// `DripParticle.SporeBlossomFallProvider`: a slow green drip that
    /// lies where it lands.
    fn falling_spore_blossom(pos: DVec3) -> Self {
        let mut p = Self::drip(pos, Drip::Water);
        p.gravity = f64::from(0.005f32) / 0.04;
        p.lifetime = (64.0 / (fastrand::f32() * 0.8 + 0.1)) as i32;
        p.color = SPORE_BLOSSOM_GREEN;
        p
    }

    /// `SquidInkParticle`: a big full-bright puff in `color`.
    fn squid_ink(pos: DVec3, vel: DVec3, color: [f32; 3]) -> Self {
        Self {
            motion: Motion::Free,
            light: Light::Bright,
            extra: Extra::InkFade,
            animated: Some(SpriteSet::Generic),
            translucent: true,
            vel,
            friction: f64::from(0.92f32),
            size: 0.5,
            color,
            lifetime: (f64::from(0.5f32 * 12.0) / (fastrand::f64() * 0.8 + 0.2)) as i32,
            ..Self::at(pos)
        }
    }

    /// `FireworkParticles.SparkParticle` (`firework`): white glitter that
    /// falls a little.
    // TODO: a rocket's own burst (its colours, trails and twinkle).
    fn firework(pos: DVec3, vel: DVec3) -> Self {
        let mut p = Self::end_rod(pos, vel);
        p.extra = Extra::FadeOut(None);
        p.animated = Some(SpriteSet::Spark);
        p.gravity = f64::from(0.1f32);
        p.lifetime = 48 + fastrand::i32(0..12);
        p.alpha = 0.99;
        p
    }

    /// One spark of a rocket's burst
    /// (`FireworkParticles.Starter.createParticle`).
    fn burst_spark(pos: DVec3, vel: DVec3, explosion: &FireworkExplosion) -> Self {
        let pick = |colors: &[u32]| unpack_rgb(colors[fastrand::usize(..colors.len())]);
        let mut p = Self::firework(pos, vel);
        p.color = if explosion.colors.is_empty() {
            // `DyeColor.BLACK.getFireworkColor`.
            unpack_rgb(0x1E1B1B)
        } else {
            pick(&explosion.colors)
        };
        p.extra = Extra::Spark {
            fade: (!explosion.fade_colors.is_empty()).then(|| pick(&explosion.fade_colors)),
            trail: explosion.trail,
            twinkle: explosion.twinkle,
        };
        p
    }

    /// The spark a trailing one leaves behind: still, half way through its
    /// life already (`SparkParticle.tick`).
    fn trail_spark(&self) -> Option<Self> {
        let Extra::Spark {
            fade,
            trail: true,
            twinkle,
        } = self.extra
        else {
            return None;
        };
        if self.age >= self.lifetime / 2 || (self.age + self.lifetime) % 2 != 0 {
            return None;
        }
        let mut p = Self::firework(self.pos, DVec3::ZERO);
        p.color = self.color;
        p.age = p.lifetime / 2;
        p.extra = Extra::Spark {
            fade,
            trail: false,
            twinkle,
        };
        Some(p)
    }

    /// Whether it draws this tick: a twinkling spark blinks
    /// (`SparkParticle.extract`).
    fn shown(&self) -> bool {
        match self.extra {
            Extra::Spark { twinkle: true, .. } => {
                self.age < self.lifetime / 3 || (self.age + self.lifetime) / 3 % 2 == 0
            }
            _ => true,
        }
    }

    /// `SnowflakeParticle`.
    fn snowflake(pos: DVec3, vel: DVec3) -> Self {
        let jitter = || (fastrand::f64() * 2.0 - 1.0) * f64::from(0.05f32);
        Self {
            vel: vel + dvec3(jitter(), jitter(), jitter()),
            gravity: f64::from(0.225f32),
            friction: 1.0,
            damp: dvec3(f64::from(0.95f32), f64::from(0.9f32), f64::from(0.95f32)),
            size: 0.1 * (fastrand::f32() * fastrand::f32() + 1.0),
            lifetime: (16.0 / (f64::from(fastrand::f32()) * 0.8 + 0.2)) as i32 + 2,
            color: [0.923, 0.964, 0.999],
            animated: Some(SpriteSet::Generic),
            ..Self::at(pos)
        }
    }

    /// `SonicBoomParticle`: a `HugeExplosionParticle` through its own
    /// sixteen frames.
    fn sonic_boom(pos: DVec3) -> Self {
        Self {
            animated: Some(SpriteSet::SonicBoom),
            lifetime: 16,
            size: 1.5,
            ..Self::explosion(pos, 0.0)
        }
    }

    /// `DragonBreathParticle`: purple, drifting at `power` times its spawn
    /// speed, slowing sideways only (it never lands, having no collision).
    fn dragon_breath(pos: DVec3, vel: DVec3, power: f32) -> Self {
        let between = |a: f32, b: f32| a + fastrand::f32() * (b - a);
        let friction = f64::from(0.96f32);
        Self {
            motion: Motion::Free,
            size_curve: Size::GrowIn,
            animated: Some(SpriteSet::DragonBreath),
            speed_up_blocked: true,
            vel: vel * f64::from(power),
            friction,
            damp: dvec3(1.0, 1.0 / friction, 1.0),
            size: base_size() * 0.75,
            color: [
                between(0.717_647_1, 0.874_509_8),
                0.0,
                between(0.823_529_4, 0.976_470_6),
            ],
            lifetime: (20.0 / (f64::from(fastrand::f32()) * 0.8 + 0.2)) as i32,
            ..Self::at(pos)
        }
    }

    /// `GlowParticle`, before its provider colours and speeds it.
    fn glow(pos: DVec3, velocity_arg: DVec3) -> Self {
        let mut p = Self::thrown(pos, velocity_arg);
        p.motion = Motion::Free;
        p.light = Light::BlockRamp;
        p.friction = f64::from(0.96f32);
        p.speed_up_blocked = true;
        p.size *= 0.75;
        p.translucent = true;
        p
    }

    /// `SuspendedParticle`: a mote hanging in the air or water, an eighth
    /// of a block under where it was asked for. `velocity_arg` is thrown
    /// like any particle's; without one it starts still.
    fn suspended(pos: DVec3, velocity_arg: Option<DVec3>, color: [f32; 3]) -> Self {
        let pos = pos - dvec3(0.0, 0.125, 0.0);
        let mut p = match velocity_arg {
            Some(arg) => Self::thrown(pos, arg),
            None => Self::at(pos),
        };
        p.motion = Motion::Free;
        p.friction = 1.0;
        p.size *= fastrand::f32() * 0.6 + 0.2;
        p.lifetime = (16.0 / (fastrand::f64() * 0.8 + 0.2)) as i32;
        p.color = color;
        p
    }

    /// `FireworkParticles.OverlayParticle` (`flash`).
    fn flash(pos: DVec3, color: [f32; 3]) -> Self {
        Self {
            motion: Motion::Still,
            light: Light::Bright,
            size_curve: Size::Flash,
            extra: Extra::Flash,
            translucent: true,
            size: 7.1,
            color,
            alpha: 0.6,
            lifetime: 4,
            ..Self::at(pos)
        }
    }

    /// `SculkChargePopParticle`.
    fn sculk_charge_pop(pos: DVec3, vel: DVec3) -> Self {
        Self {
            motion: Motion::Free,
            light: Light::BlockFull,
            animated: Some(SpriteSet::SculkChargePop),
            translucent: true,
            vel,
            friction: f64::from(0.96f32),
            lifetime: fastrand::i32(0..4) + 6,
            ..Self::at(pos)
        }
    }

    /// `FallingLeavesParticle`: `fall` and `wind` are its providers' fall
    /// and side accelerations, `scale` its size, `start_velocity` how fast
    /// it starts down.
    fn leaves(
        pos: DVec3,
        (fall, wind, swirl, flow_away, scale, start_velocity): (f32, f32, bool, bool, f32, f32),
        color: [f32; 3],
    ) -> Self {
        let size = scale * if fastrand::bool() { 0.05 } else { 0.075 };
        let heading = (fastrand::f32() * 60.0).to_radians();
        let period = (1000.0 + fastrand::f32() * 3000.0).to_radians();
        Self {
            extra: Extra::Leaf {
                flow: flow_away.then(|| [heading.cos() * wind, heading.sin() * wind]),
                swirl: swirl.then_some(period),
                wind,
                roll: [0.0; 2],
                // Thirty degrees a second either way, quickening by five.
                spin: 30.0_f32.to_radians() * if fastrand::bool() { -1.0 } else { 1.0 },
                spin_up: 5.0_f32.to_radians() * if fastrand::bool() { -1.0 } else { 1.0 },
            },
            vel: dvec3(0.0, -f64::from(start_velocity), 0.0),
            // Its own tick subtracts `gravity` itself.
            gravity: f64::from(fall * 1.2 * 0.0025) / 0.04,
            friction: 1.0,
            size,
            color,
            lifetime: 300,
            ..Self::at(pos)
        }
    }

    /// `BubbleParticle`: rises through water, gone outside it.
    fn bubble(pos: DVec3, vel: DVec3) -> Self {
        let jitter = || (fastrand::f64() * 2.0 - 1.0) * f64::from(0.02f32);
        Self {
            extra: Extra::Bubble,
            vel: vel * f64::from(0.2f32) + dvec3(jitter(), jitter(), jitter()),
            // Its own tick adds 0.002 a tick.
            gravity: -0.002 / 0.04,
            friction: f64::from(0.85f32),
            size: base_size() * (fastrand::f32() * 0.6 + 0.2),
            lifetime: (8.0 / (fastrand::f64() * 0.8 + 0.2)) as i32,
            ..Self::at(pos)
        }
    }

    /// `TrailParticle`: flies to `target` over `duration` ticks.
    fn trail(pos: DVec3, target: DVec3, color: u32, duration: u32) -> Self {
        let shade = || 0.875 + fastrand::f32() * 0.25;
        Self {
            motion: Motion::Homing(target),
            light: Light::Bright,
            size: 0.26,
            color: unpack_rgb(color).map(|c| c * shade()),
            lifetime: duration.clamp(1, i32::MAX as u32) as i32,
            ..Self::at(pos)
        }
    }

    /// `NoteParticle`: `hue` (its `xd` argument) picks the colour.
    fn note(pos: DVec3, hue: f64) -> Self {
        let mut p = Self::thrown(pos, DVec3::ZERO);
        p.friction = f64::from(0.66f32);
        p.speed_up_blocked = true;
        p.vel *= f64::from(0.01f32);
        p.vel.y += 0.2;
        let channel = |offset: f32| {
            (((hue as f32 + offset) * std::f32::consts::TAU).sin() * 0.65 + 0.35).max(0.0)
        };
        p.color = [channel(0.0), channel(1.0 / 3.0), channel(2.0 / 3.0)];
        p.size *= 1.5;
        p.lifetime = 6;
        p.size_curve = Size::GrowIn;
        p
    }

    /// `WakeParticle` (`fishing`): the spawn velocity, slowing.
    fn wake(pos: DVec3, vel: DVec3) -> Self {
        Self {
            extra: Extra::Wake,
            vel,
            friction: f64::from(0.98f32),
            lifetime: (8.0 / (fastrand::f64() * 0.8 + 0.2)) as i32,
            ..Self::at(pos)
        }
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
                Size::Flash => ((self.age as f32 + partial - 1.0) * 0.25 * std::f32::consts::PI)
                    .sin()
                    .max(0.0),
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
            Motion::Homing(target) => {
                // `TrailParticle.tick`: the rest of the way, split evenly
                // over the ticks left.
                let left = self.lifetime - self.age;
                self.pos = if left > 0 {
                    self.pos.lerp(target, 1.0 / f64::from(left))
                } else {
                    target
                };
            }
            Motion::Physics | Motion::Free => {
                if let Extra::Leaf {
                    roll,
                    spin,
                    spin_up,
                    ..
                } = &mut self.extra
                {
                    *spin += *spin_up / 20.0;
                    *roll = [roll[1], roll[1] + *spin / 20.0];
                }
                if let Extra::Leaf {
                    flow, swirl, wind, ..
                } = self.extra
                {
                    let g = (self.age as f32 / 300.0).min(1.0);
                    let mut push = [0.0f32; 2];
                    if let Some(flow) = flow {
                        let strength = g.powf(1.25);
                        push = [flow[0] * strength, flow[1] * strength];
                    }
                    if let Some(period) = swirl {
                        push[0] += g * (g * period).cos() * wind;
                        push[1] += g * (g * period).sin() * wind;
                    }
                    self.vel.x += f64::from(push[0] * 0.0025);
                    self.vel.z += f64::from(push[1] * 0.0025);
                }
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
                self.vel *= self.damp;
                if self.on_ground {
                    self.vel.x *= 0.7;
                    self.vel.z *= 0.7;
                }
            }
        }
        match self.extra {
            Extra::FadeOut(fade) | Extra::Spark { fade, .. } => {
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
            Extra::InkFade => {
                if self.age > self.lifetime / 2 {
                    self.alpha = 1.0 - (self.age - self.lifetime / 2) as f32 / self.lifetime as f32;
                }
                let [x, y, z] = self.pos.floor().as_ivec3().to_array();
                let state = chunks.get_block_state(x, y, z);
                if block_state_table.block_state_data(state).is_air() {
                    self.vel.y -= f64::from(0.0074f32);
                }
            }
            // `CoolingDripHangParticle`: hanging lava cools from yellow.
            Extra::DripHang(Drip::Lava) => {
                let age = self.age as f32;
                self.color = [1.0, 16.0 / (age + 16.0), 4.0 / (age + 8.0)];
            }
            Extra::DripFall(_) => {
                if self.on_ground {
                    return false;
                }
            }
            Extra::Bubble => {
                let [x, y, z] = self.pos.floor().as_ivec3().to_array();
                let data = block_state_table.block_state_data(chunks.get_block_state(x, y, z));
                if data.fluid().kind != crate::world::block::FluidKind::Water {
                    return false;
                }
            }
            Extra::Wake => {
                // The frame of how long it has left, as vanilla counts it.
                let frames = sets.get(SpriteSet::Rain);
                if !frames.is_empty() {
                    let step = 60 - (self.lifetime - self.age + 1);
                    let frame = frames[step.rem_euclid(frames.len() as i32) as usize];
                    self.set_sprite(&frame);
                }
            }
            Extra::Leaf { .. } => {
                if self.on_ground || self.age > 1 && (self.vel.x == 0.0 || self.vel.z == 0.0) {
                    return false;
                }
            }
            Extra::Flash => self.alpha = 0.6 - (self.age as f32 - 1.0) * 0.25 * 0.5,
            Extra::None | Extra::DripHang(_) | Extra::WaterDrop => {}
        }
        if self.extra == Extra::WaterDrop && self.water_drop_gone(chunks, block_state_table) {
            return false;
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

/// The green of a spore blossom's spores, in the air and falling.
const SPORE_BLOSSOM_GREEN: [f32; 3] = [0.32, 0.5, 0.22];

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
    /// `nautilus`, drawn in to a conduit.
    Nautilus,
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
    /// `rain` and `splash`: one random `splash_*`.
    Rain,
    /// `entity_effect`: `effect_7` to `effect_0`.
    Effect,
    BubblePop,
    /// `ash`, `white_ash`: `generic_0` alone.
    Ash,
    Gust,
    SmallGust,
    /// `firework`: `spark_7` to `spark_0`.
    Spark,
    SculkSoul,
    SonicBoom,
    /// `dragon_breath`: `generic_5` to `generic_7`.
    DragonBreath,
    Damage,
    Angry,
    Flash,
    SculkChargePop,
    /// The leaves: one at random.
    Cherry,
    PaleOak,
    Leaf,
    Bubble,
    Note,
    DripLand,
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
    (SpriteSet::Nautilus, &["particle/nautilus"]),
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
    (
        SpriteSet::Rain,
        &[
            "particle/splash_0",
            "particle/splash_1",
            "particle/splash_2",
            "particle/splash_3",
        ],
    ),
    (
        SpriteSet::Effect,
        &[
            "particle/effect_7",
            "particle/effect_6",
            "particle/effect_5",
            "particle/effect_4",
            "particle/effect_3",
            "particle/effect_2",
            "particle/effect_1",
            "particle/effect_0",
        ],
    ),
    (
        SpriteSet::BubblePop,
        &[
            "particle/bubble_pop_0",
            "particle/bubble_pop_1",
            "particle/bubble_pop_2",
            "particle/bubble_pop_3",
            "particle/bubble_pop_4",
        ],
    ),
    (SpriteSet::Ash, &["particle/generic_0"]),
    (
        SpriteSet::Gust,
        &[
            "particle/gust_0",
            "particle/gust_1",
            "particle/gust_2",
            "particle/gust_3",
            "particle/gust_4",
            "particle/gust_5",
            "particle/gust_6",
            "particle/gust_7",
            "particle/gust_8",
            "particle/gust_9",
            "particle/gust_10",
            "particle/gust_11",
        ],
    ),
    (
        SpriteSet::SmallGust,
        &[
            "particle/small_gust_0",
            "particle/small_gust_1",
            "particle/small_gust_2",
            "particle/small_gust_3",
            "particle/small_gust_4",
            "particle/small_gust_5",
            "particle/small_gust_6",
        ],
    ),
    (
        SpriteSet::Spark,
        &[
            "particle/spark_7",
            "particle/spark_6",
            "particle/spark_5",
            "particle/spark_4",
            "particle/spark_3",
            "particle/spark_2",
            "particle/spark_1",
            "particle/spark_0",
        ],
    ),
    (
        SpriteSet::SculkSoul,
        &[
            "particle/sculk_soul_0",
            "particle/sculk_soul_1",
            "particle/sculk_soul_2",
            "particle/sculk_soul_3",
            "particle/sculk_soul_4",
            "particle/sculk_soul_5",
            "particle/sculk_soul_6",
            "particle/sculk_soul_7",
            "particle/sculk_soul_8",
            "particle/sculk_soul_9",
            "particle/sculk_soul_10",
        ],
    ),
    (
        SpriteSet::SonicBoom,
        &[
            "particle/sonic_boom_0",
            "particle/sonic_boom_1",
            "particle/sonic_boom_2",
            "particle/sonic_boom_3",
            "particle/sonic_boom_4",
            "particle/sonic_boom_5",
            "particle/sonic_boom_6",
            "particle/sonic_boom_7",
            "particle/sonic_boom_8",
            "particle/sonic_boom_9",
            "particle/sonic_boom_10",
            "particle/sonic_boom_11",
            "particle/sonic_boom_12",
            "particle/sonic_boom_13",
            "particle/sonic_boom_14",
            "particle/sonic_boom_15",
        ],
    ),
    (
        SpriteSet::DragonBreath,
        &[
            "particle/generic_5",
            "particle/generic_6",
            "particle/generic_7",
        ],
    ),
    (SpriteSet::Damage, &["particle/damage"]),
    (SpriteSet::Angry, &["particle/angry"]),
    (SpriteSet::Flash, &["particle/flash"]),
    (
        SpriteSet::SculkChargePop,
        &[
            "particle/sculk_charge_pop_0",
            "particle/sculk_charge_pop_1",
            "particle/sculk_charge_pop_2",
            "particle/sculk_charge_pop_3",
        ],
    ),
    (
        SpriteSet::Cherry,
        &[
            "particle/cherry_0",
            "particle/cherry_1",
            "particle/cherry_2",
            "particle/cherry_3",
            "particle/cherry_4",
            "particle/cherry_5",
            "particle/cherry_6",
            "particle/cherry_7",
            "particle/cherry_8",
            "particle/cherry_9",
            "particle/cherry_10",
            "particle/cherry_11",
        ],
    ),
    (
        SpriteSet::PaleOak,
        &[
            "particle/pale_oak_0",
            "particle/pale_oak_1",
            "particle/pale_oak_2",
            "particle/pale_oak_3",
            "particle/pale_oak_4",
            "particle/pale_oak_5",
            "particle/pale_oak_6",
            "particle/pale_oak_7",
            "particle/pale_oak_8",
            "particle/pale_oak_9",
            "particle/pale_oak_10",
            "particle/pale_oak_11",
        ],
    ),
    (
        SpriteSet::Leaf,
        &[
            "particle/leaf_0",
            "particle/leaf_1",
            "particle/leaf_2",
            "particle/leaf_3",
            "particle/leaf_4",
            "particle/leaf_5",
            "particle/leaf_6",
            "particle/leaf_7",
            "particle/leaf_8",
            "particle/leaf_9",
            "particle/leaf_10",
            "particle/leaf_11",
        ],
    ),
    (SpriteSet::Bubble, &["particle/bubble"]),
    (SpriteSet::Note, &["particle/note"]),
    (SpriteSet::DripLand, &["particle/drip_land"]),
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
    Nautilus,
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
    ExplosionEmitter,
    Gust,
    SmallGust,
    GustEmitterLarge,
    GustEmitterSmall,
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
    EntityEffect,
    BubblePop,
    Ash,
    WhiteAsh,
    Poof,
    Splash,
    SquidInk,
    SculkSoul,
    Firework,
    Snowflake,
    SonicBoom,
    DragonBreath,
    WaxOn,
    WaxOff,
    DamageIndicator,
    Composter,
    AngryVillager,
    SporeBlossomAir,
    FallingSporeBlossom,
    CrimsonSpore,
    WarpedSpore,
    Underwater,
    Glow,
    DrippingWater,
    FallingWater,
    DrippingLava,
    FallingLava,
    LandingLava,
    LandingObsidianTear,
    Flash,
    ItemSlime,
    ItemSnowball,
    SculkChargePop,
    CherryLeaves,
    PaleOakLeaves,
    TintedLeaves,
    Sneeze,
    Bubble,
    Trail,
    Note,
    Rain,
    Fishing,
    Effect,
    InstantEffect,
    Spit,
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
            "nautilus" => Self::Nautilus,
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
            "explosion_emitter" => Self::ExplosionEmitter,
            "gust" => Self::Gust,
            "small_gust" => Self::SmallGust,
            "gust_emitter_large" => Self::GustEmitterLarge,
            "gust_emitter_small" => Self::GustEmitterSmall,
            "happy_villager" => Self::HappyVillager,
            "witch" => Self::Witch,
            "entity_effect" => Self::EntityEffect,
            "bubble_pop" => Self::BubblePop,
            "ash" => Self::Ash,
            "white_ash" => Self::WhiteAsh,
            "poof" => Self::Poof,
            "splash" => Self::Splash,
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
            "squid_ink" => Self::SquidInk,
            "sculk_soul" => Self::SculkSoul,
            "firework" => Self::Firework,
            "snowflake" => Self::Snowflake,
            "sonic_boom" => Self::SonicBoom,
            "dragon_breath" => Self::DragonBreath,
            "wax_on" => Self::WaxOn,
            "wax_off" => Self::WaxOff,
            "damage_indicator" => Self::DamageIndicator,
            "composter" => Self::Composter,
            "angry_villager" => Self::AngryVillager,
            "spore_blossom_air" => Self::SporeBlossomAir,
            "falling_spore_blossom" => Self::FallingSporeBlossom,
            "crimson_spore" => Self::CrimsonSpore,
            "warped_spore" => Self::WarpedSpore,
            "underwater" => Self::Underwater,
            "glow" => Self::Glow,
            "dripping_water" => Self::DrippingWater,
            "falling_water" => Self::FallingWater,
            "dripping_lava" => Self::DrippingLava,
            "falling_lava" => Self::FallingLava,
            "landing_lava" => Self::LandingLava,
            "landing_obsidian_tear" => Self::LandingObsidianTear,
            "flash" => Self::Flash,
            "item_slime" => Self::ItemSlime,
            "item_snowball" => Self::ItemSnowball,
            "sculk_charge_pop" => Self::SculkChargePop,
            "cherry_leaves" => Self::CherryLeaves,
            "pale_oak_leaves" => Self::PaleOakLeaves,
            "tinted_leaves" => Self::TintedLeaves,
            "sneeze" => Self::Sneeze,
            "bubble" => Self::Bubble,
            "trail" => Self::Trail,
            "note" => Self::Note,
            "rain" => Self::Rain,
            "fishing" => Self::Fishing,
            "effect" => Self::Effect,
            "instant_effect" => Self::InstantEffect,
            "spit" => Self::Spit,
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

    /// What a drip particle's kind is made of.
    fn drip(self) -> Drip {
        match self {
            Self::DrippingWater | Self::FallingWater => Drip::Water,
            Self::DrippingLava | Self::FallingLava | Self::LandingLava => Drip::Lava,
            _ => Drip::ObsidianTear,
        }
    }

    /// Vanilla `ParticleType.getOverrideLimiter`: the types `ParticleTypes`
    /// registers to show from any distance.
    fn override_limiter(self) -> bool {
        matches!(
            self,
            Self::Explosion
                | Self::ExplosionEmitter
                | Self::Gust
                | Self::GustEmitterLarge
                | Self::GustEmitterSmall
                | Self::SweepAttack
                | Self::CampfireCosySmoke
                | Self::CampfireSignalSmoke
                | Self::SonicBoom
        )
    }
}

/// A particle that draws nothing and spawns others as it ages (vanilla's
/// `NoRenderParticle`s).
struct Seed {
    kind: SeedKind,
    pos: DVec3,
    age: i32,
}

#[derive(Clone, Copy)]
enum SeedKind {
    /// `HugeExplosionSeedParticle` (`explosion_emitter`): six explosions a
    /// tick within four blocks, for eight ticks, each smaller than the last.
    Explosion,
    /// `GustSeedParticle` (`gust_emitter_large` and `_small`): three gusts
    /// within `scale` blocks, then `rest` ticks before the next three,
    /// until it is `lifetime` old.
    Gust {
        scale: f64,
        lifetime: i32,
        rest: i32,
    },
}

/// `HugeExplosionSeedParticle`'s lifetime.
const EXPLOSION_SEED_TICKS: i32 = 8;

impl Seed {
    /// One tick: where it spawns its particles and what it hands each as
    /// its size argument, and whether it lives on.
    fn tick(&mut self) -> (Vec<DVec3>, f64, bool) {
        let pos = self.pos;
        let around = |scale: f64| {
            let offset = || (fastrand::f64() - fastrand::f64()) * scale;
            pos + dvec3(offset(), offset(), offset())
        };
        let age = self.age;
        self.age += 1;
        match self.kind {
            SeedKind::Explosion => (
                (0..6).map(|_| around(4.0)).collect(),
                f64::from(age as f32 / EXPLOSION_SEED_TICKS as f32),
                self.age != EXPLOSION_SEED_TICKS,
            ),
            SeedKind::Gust {
                scale,
                lifetime,
                rest,
            } => {
                let spawns = if age % (rest + 1) == 0 {
                    (0..3).map(|_| around(scale)).collect()
                } else {
                    Vec::new()
                };
                (spawns, 0.0, age != lifetime)
            }
        }
    }
}

/// A firework star's shape (`FireworkExplosion.Shape`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FireworkShape {
    SmallBall,
    LargeBall,
    Star,
    Creeper,
    Burst,
}

/// One star of a rocket (`FireworkExplosion`): its shape, the colours its
/// sparks take and fade to (RGB), and whether they trail and twinkle.
#[derive(Clone, Debug, PartialEq)]
pub struct FireworkExplosion {
    pub shape: FireworkShape,
    pub colors: Vec<u32>,
    pub fade_colors: Vec<u32>,
    pub trail: bool,
    pub twinkle: bool,
}

/// A rocket going off (`FireworkParticles.Starter`): a star every other
/// tick, each with its flash.
struct FireworkStarter {
    pos: DVec3,
    vel: DVec3,
    explosions: Vec<FireworkExplosion>,
    life: usize,
}

/// `FireworkParticles.Starter.STAR_PARTICLE_COORDS`: half a star's outline.
const STAR_OUTLINE: [[f64; 2]; 6] = [
    [0.0, 1.0],
    [0.3455, 0.309],
    [0.9511, 0.309],
    [0.379_591_836_734_693_9, -0.126_530_612_244_897_95],
    [0.612_244_897_959_183_7, -0.804_081_632_653_061_2],
    [0.0, -0.359_183_673_469_387_73],
];

/// `CREEPER_PARTICLE_COORDS`: half a creeper face's outline.
const CREEPER_OUTLINE: [[f64; 2]; 12] = [
    [0.0, 0.2],
    [0.2, 0.2],
    [0.2, 0.6],
    [0.6, 0.6],
    [0.6, 0.2],
    [0.2, 0.2],
    [0.2, 0.0],
    [0.4, 0.0],
    [0.4, -0.6],
    [0.2, -0.6],
    [0.2, -0.4],
    [0.0, -0.4],
];

impl FireworkStarter {
    /// The velocities of one star's sparks, by its shape.
    fn spark_velocities(&self, shape: FireworkShape) -> Vec<DVec3> {
        let mut out = Vec::new();
        let spread = || fastrand::f64() - fastrand::f64();
        // `createParticleBall`: the shell of a cube of sparks, thrown out
        // at about `speed`.
        let mut ball = |speed: f64, radius: i32| {
            for i in -radius..=radius {
                for j in -radius..=radius {
                    let mut k = -radius;
                    while k <= radius {
                        let v = dvec3(
                            f64::from(j) + spread() * 0.5,
                            f64::from(i) + spread() * 0.5,
                            f64::from(k) + spread() * 0.5,
                        );
                        let scale = v.length() / speed + next_gaussian() * 0.05;
                        out.push(v / scale);
                        if i != -radius && i != radius && j != -radius && j != radius {
                            k += radius * 2 - 1;
                        }
                        k += 1;
                    }
                }
            }
        };
        match shape {
            FireworkShape::SmallBall => ball(0.25, 2),
            FireworkShape::LargeBall => ball(0.5, 4),
            // `createParticleShape`: the outline and its mirror, drawn
            // three times turned about the vertical (a creeper's face
            // nearly flat).
            FireworkShape::Star | FireworkShape::Creeper => {
                let (outline, flat): (&[[f64; 2]], bool) = if shape == FireworkShape::Star {
                    (&STAR_OUTLINE, false)
                } else {
                    (&CREEPER_OUTLINE, true)
                };
                let speed = 0.5;
                out.push(dvec3(outline[0][0] * speed, outline[0][1] * speed, 0.0));
                let start = f64::from(fastrand::f32()) * std::f64::consts::PI;
                let step = if flat { 0.034 } else { 0.34 };
                for turn in 0..3 {
                    let angle = start + f64::from(turn) * std::f64::consts::PI * step;
                    for pair in outline.windows(2) {
                        for quarter in 1..=4 {
                            let t = f64::from(quarter) * 0.25;
                            let x = (pair[0][0] + (pair[1][0] - pair[0][0]) * t) * speed;
                            let y = (pair[0][1] + (pair[1][1] - pair[0][1]) * t) * speed;
                            let (x, z) = (x * angle.cos(), x * angle.sin());
                            out.push(dvec3(x, y, z));
                            out.push(dvec3(-x, y, -z));
                        }
                    }
                }
            }
            // `createParticleBurst`: seventy sparks along the rocket's way.
            FireworkShape::Burst => {
                let (dx, dz) = (next_gaussian() * 0.05, next_gaussian() * 0.05);
                for _ in 0..70 {
                    out.push(dvec3(
                        self.vel.x * 0.5 + next_gaussian() * 0.15 + dx,
                        self.vel.y * 0.5 + fastrand::f64() * 0.5,
                        self.vel.z * 0.5 + next_gaussian() * 0.15 + dz,
                    ));
                }
            }
        }
        out
    }
}

/// One of the particles an explosion throws from the blocks it blew
/// (`ExplosionParticleInfo`, in the packet's weighted list).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ExplosionParticle {
    pub kind: ServerParticleKind,
    pub options: ParticleOptions,
    /// How far along its line from the centre it starts.
    pub scaling: f32,
    pub speed: f32,
    pub weight: u32,
}

/// An explosion waiting for the next tick's debris (`ClientExplosionTracker`).
struct TrackedExplosion {
    center: DVec3,
    radius: f32,
    block_count: u32,
    particles: Vec<(ExplosionParticle, ParticleLook)>,
    camera_pos: DVec3,
}

/// `ClientExplosionTracker.MAX_PARTICLES_PER_TICK`.
const MAX_EXPLOSION_PARTICLES_PER_TICK: u32 = 512;

/// `WeightedRandom.getRandomItem`: the item `pick` lands in, `pick` being
/// below the weights' total.
fn weighted_pick<T>(items: &[T], weight: impl Fn(&T) -> u32, mut pick: u32) -> Option<&T> {
    items.iter().find(|item| {
        let weight = weight(item);
        if pick < weight {
            return true;
        }
        pick -= weight;
        false
    })
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
    /// `ColorParticleOption`: an ARGB colour.
    Color(u32),
    /// `PowerParticleOption`: what the spawn speed is multiplied by.
    Power(f32),
    /// `SpellParticleOption`: an ARGB colour and a power.
    Spell {
        color: u32,
        power: f32,
    },
    /// `TrailParticleOption`: where it flies to, its RGB colour and how
    /// many ticks it takes.
    Trail {
        target: DVec3,
        color: u32,
        duration: u32,
    },
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
    seeds: Vec<Seed>,
    /// Seeds spawned this tick, which start ticking the next.
    pending_seeds: Vec<Seed>,
    explosions: Vec<TrackedExplosion>,
    fireworks: Vec<FireworkStarter>,
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
            seeds: Vec::new(),
            pending_seeds: Vec::new(),
            explosions: Vec::new(),
            fireworks: Vec::new(),
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
    /// What `WeatherEffectRenderer.tickRainParticles` adds where rain lands:
    /// a `rain` splash, or `smoke` on lava, magma and lit campfires.
    pub fn add_rain_splash(&mut self, pos: DVec3, smoke: bool) {
        let particle = if smoke {
            let mut p = Particle::smoke(pos, DVec3::ZERO, 1.0);
            p.sprite_from_age(&self.sets);
            p
        } else {
            let mut p = Particle::water_drop(pos);
            let frames = self.sets.get(SpriteSet::Rain);
            if !frames.is_empty() {
                p.set_sprite(&frames[fastrand::usize(..frames.len())]);
            }
            p
        };
        self.push(particle);
    }

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
            // `FlyTowardsPositionParticle.NautilusProvider`: an enchanting
            // glyph's flight, in its own sprite.
            K::Nautilus => with(Particle::enchant(pos, vel), sets.pick(SpriteSet::Nautilus)),
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
            K::ExplosionEmitter | K::GustEmitterLarge | K::GustEmitterSmall => {
                let kind = match spawn.kind {
                    K::ExplosionEmitter => SeedKind::Explosion,
                    K::GustEmitterLarge => SeedKind::Gust {
                        scale: 3.0,
                        lifetime: 7,
                        rest: 0,
                    },
                    _ => SeedKind::Gust {
                        scale: 1.0,
                        lifetime: 3,
                        rest: 2,
                    },
                };
                self.pending_seeds.push(Seed { kind, pos, age: 0 });
                return;
            }
            K::Gust | K::SmallGust => {
                let mut p = Particle::gust(pos, spawn.kind == K::SmallGust);
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
            K::EntityEffect => {
                let argb = match spawn.options {
                    ParticleOptions::Color(argb) => argb,
                    _ => 0xFFFF_FFFF,
                };
                let channel = |shift: u32| ((argb >> shift) & 0xFF) as f32 / 255.0;
                let mut p =
                    Particle::spell(pos, vel, [channel(16), channel(8), channel(0)], channel(24));
                p.animated = Some(SpriteSet::Effect);
                p.sprite_from_age(sets);
                p
            }
            K::BubblePop => {
                let mut p = Particle::bubble_pop(pos, vel);
                p.sprite_from_age(sets);
                p
            }
            K::Ash | K::WhiteAsh => {
                let mut p = Particle::ash(pos, spawn.kind == K::WhiteAsh);
                p.sprite_from_age(sets);
                p
            }
            K::Poof => {
                let mut p = Particle::poof(pos, vel);
                p.sprite_from_age(sets);
                p
            }
            K::Splash => {
                let mut p = Particle::splash(pos, vel);
                let frames = sets.get(SpriteSet::Rain);
                if !frames.is_empty() {
                    p.set_sprite(&frames[fastrand::usize(..frames.len())]);
                }
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
            K::DrippingObsidianTear | K::DrippingWater | K::DrippingLava => with(
                Particle::drip_hang(pos, spawn.kind.drip()),
                sets.first(SpriteSet::DripHang),
            ),
            K::FallingObsidianTear | K::FallingWater | K::FallingLava => with(
                Particle::drip_fall(pos, spawn.kind.drip()),
                sets.first(SpriteSet::DripFall),
            ),
            K::LandingObsidianTear | K::LandingLava => with(
                Particle::drip_land(pos, spawn.kind.drip()),
                sets.first(SpriteSet::DripLand),
            ),
            K::FallingSporeBlossom => with(
                Particle::falling_spore_blossom(pos),
                sets.first(SpriteSet::DripFall),
            ),
            K::SquidInk => {
                let mut p = Particle::squid_ink(pos, vel, [0.0; 3]);
                p.sprite_from_age(sets);
                p
            }
            // `SoulParticle.EmissiveProvider`: a soul that glows.
            K::SculkSoul => {
                let mut p = Particle::soul(pos, vel);
                p.light = Light::BlockFull;
                p.animated = Some(SpriteSet::SculkSoul);
                p.sprite_from_age(sets);
                p
            }
            K::Firework => {
                let mut p = Particle::firework(pos, vel);
                p.sprite_from_age(sets);
                p
            }
            K::Snowflake => {
                let mut p = Particle::snowflake(pos, vel);
                p.sprite_from_age(sets);
                p
            }
            K::SonicBoom => {
                let mut p = Particle::sonic_boom(pos);
                p.sprite_from_age(sets);
                p
            }
            K::DragonBreath => {
                let power = match spawn.options {
                    ParticleOptions::Power(power) => power,
                    _ => 1.0,
                };
                let mut p = Particle::dragon_breath(pos, vel, power);
                p.sprite_from_age(sets);
                p
            }
            // `GlowParticle.WaxOnProvider` and `WaxOffProvider`.
            K::WaxOn | K::WaxOff => {
                let mut p = Particle::glow(pos, DVec3::ZERO);
                p.color = if spawn.kind == K::WaxOn {
                    [0.91, 0.55, 0.08]
                } else {
                    [1.0, 0.9, 1.0]
                };
                p.alpha = 0.8;
                p.vel = vel * dvec3(0.005, 0.01, 0.005);
                p.lifetime = fastrand::i32(0..30) + 10;
                with(p, sets.first(SpriteSet::Glow))
            }
            // `GlowParticle.GlowSquidProvider`.
            K::Glow => {
                let mut p = Particle::glow(
                    pos,
                    dvec3(0.5 - fastrand::f64(), vel.y, 0.5 - fastrand::f64()),
                );
                p.color = if fastrand::bool() {
                    [0.6, 1.0, 0.8]
                } else {
                    [0.08, 0.4, 0.4]
                };
                p.vel.y *= f64::from(0.2f32);
                if vel.x == 0.0 && vel.z == 0.0 {
                    p.vel.x *= f64::from(0.1f32);
                    p.vel.z *= f64::from(0.1f32);
                }
                p.lifetime = (8.0 / (fastrand::f64() * 0.8 + 0.2)) as i32;
                with(p, sets.first(SpriteSet::Glow))
            }
            // `CritParticle.DamageIndicatorProvider`.
            K::DamageIndicator => {
                let mut p = Particle::crit(pos, vel + DVec3::Y, false);
                p.lifetime = 20;
                with(p, sets.first(SpriteSet::Damage))
            }
            // `SuspendedTownParticle.ComposterFillProvider`.
            K::Composter => {
                let mut p = Particle::happy_villager(pos, vel);
                p.lifetime = 3 + fastrand::i32(0..5);
                with(p, sets.first(SpriteSet::Glint))
            }
            // `HeartParticle.AngryVillagerProvider`: half a block up.
            K::AngryVillager => with(
                Particle::heart(pos + dvec3(0.0, 0.5, 0.0)),
                sets.first(SpriteSet::Angry),
            ),
            // `SuspendedParticle`'s providers.
            K::SporeBlossomAir => {
                let down = dvec3(0.0, f64::from(-0.8f32), 0.0);
                let mut p = Particle::suspended(pos, Some(down), SPORE_BLOSSOM_GREEN);
                p.lifetime = fastrand::i32(500..=1000);
                p.gravity = f64::from(0.01f32);
                with(p, sets.first(SpriteSet::DripFall))
            }
            K::CrimsonSpore => {
                let drift = dvec3(
                    next_gaussian() * f64::from(1.0e-6f32),
                    next_gaussian() * f64::from(1.0e-4f32),
                    next_gaussian() * f64::from(1.0e-6f32),
                );
                with(
                    Particle::suspended(pos, Some(drift), [0.9, 0.4, 0.5]),
                    sets.first(SpriteSet::Ash),
                )
            }
            K::WarpedSpore => {
                let sink = f64::from(fastrand::f32()) * -1.9 * f64::from(fastrand::f32()) * 0.1;
                with(
                    Particle::suspended(pos, Some(dvec3(0.0, sink, 0.0)), [0.1, 0.1, 0.3]),
                    sets.first(SpriteSet::Ash),
                )
            }
            K::Underwater => with(
                Particle::suspended(pos, None, [0.4, 0.4, 0.7]),
                sets.first(SpriteSet::Ash),
            ),
            K::Flash => {
                let color = match spawn.options {
                    ParticleOptions::Color(argb) => unpack_rgb(argb),
                    _ => [1.0; 3],
                };
                with(Particle::flash(pos, color), sets.first(SpriteSet::Flash))
            }
            // `BreakingItemParticle.SlimeProvider` and `SnowballProvider`:
            // bits of the item, thrown without the spawn velocity.
            K::ItemSlime | K::ItemSnowball => {
                let item = if spawn.kind == K::ItemSlime {
                    "item/slime_ball"
                } else {
                    "item/snowball"
                };
                let Some(region) = self.item_region(item) else {
                    return;
                };
                Particle::terrain(pos, DVec3::ZERO, region, [1.0; 3])
            }
            K::SculkChargePop => {
                let mut p = Particle::sculk_charge_pop(pos, vel);
                p.sprite_from_age(sets);
                p
            }
            // `FallingLeavesParticle`'s providers.
            K::CherryLeaves => with(
                Particle::leaves(pos, (0.25, 2.0, false, true, 1.0, 0.0), [1.0; 3]),
                sets.pick(SpriteSet::Cherry),
            ),
            K::PaleOakLeaves => with(
                Particle::leaves(pos, (0.07, 10.0, true, false, 2.0, 0.021), [1.0; 3]),
                sets.pick(SpriteSet::PaleOak),
            ),
            K::TintedLeaves => {
                let color = match spawn.options {
                    ParticleOptions::Color(argb) => unpack_rgb(argb),
                    _ => [1.0; 3],
                };
                with(
                    Particle::leaves(pos, (0.07, 10.0, true, false, 2.0, 0.021), color),
                    sets.pick(SpriteSet::Leaf),
                )
            }
            // `PlayerCloudParticle.SneezeProvider`: its colour is set far
            // past white (200, 50, 120), so it draws white, and faint.
            K::Sneeze => {
                let mut p = Particle::cloud(pos, vel);
                p.color = [1.0; 3];
                p.alpha = 0.4;
                p.translucent = true;
                p.sprite_from_age(sets);
                p
            }
            K::Bubble => with(Particle::bubble(pos, vel), sets.first(SpriteSet::Bubble)),
            K::Trail => {
                let ParticleOptions::Trail {
                    target,
                    color,
                    duration,
                } = spawn.options
                else {
                    return;
                };
                with(
                    Particle::trail(pos, target, color, duration),
                    sets.first(SpriteSet::Ash),
                )
            }
            K::Note => with(Particle::note(pos, vel.x), sets.first(SpriteSet::Note)),
            K::Rain => with(Particle::water_drop(pos), sets.pick(SpriteSet::Rain)),
            K::Fishing => with(Particle::wake(pos, vel), sets.first(SpriteSet::Rain)),
            // `SpellParticle.Provider` and `InstantProvider`: the option's
            // colour, and its power on the speed.
            K::Effect | K::InstantEffect => {
                let (argb, power) = match spawn.options {
                    ParticleOptions::Spell { color, power } => (color, f64::from(power)),
                    _ => (0xFFFF_FFFF, 1.0),
                };
                let mut p = Particle::spell(pos, vel, unpack_rgb(argb), 1.0);
                p.vel.x *= power;
                p.vel.y = (p.vel.y - 0.1) * power + 0.1;
                p.vel.z *= power;
                p.animated = Some(if spawn.kind == K::Effect {
                    SpriteSet::Effect
                } else {
                    SpriteSet::Spell
                });
                p.sprite_from_age(sets);
                p
            }
            // `SpitParticle`: a poof that falls.
            K::Spit => {
                let mut p = Particle::poof(pos, vel);
                p.gravity = 0.5;
                p.sprite_from_age(sets);
                p
            }
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

    /// A rocket goes off at `pos`, moving at `vel`
    /// (`ClientLevel.createFireworks`): its stars burst one every other
    /// tick from the next tick on.
    pub fn start_fireworks(&mut self, pos: DVec3, vel: DVec3, explosions: Vec<FireworkExplosion>) {
        if !explosions.is_empty() {
            self.fireworks.push(FireworkStarter {
                pos,
                vel,
                explosions,
                life: 0,
            });
        }
    }

    /// `FireworkParticles.Starter.tick`.
    fn tick_fireworks(&mut self) {
        let mut starters = std::mem::take(&mut self.fireworks);
        starters.retain_mut(|starter| {
            if starter.life % 2 == 0 && starter.life / 2 < starter.explosions.len() {
                let explosion = &starter.explosions[starter.life / 2];
                for vel in starter.spark_velocities(explosion.shape) {
                    let mut p = Particle::burst_spark(starter.pos, vel, explosion);
                    p.sprite_from_age(&self.sets);
                    self.push(p);
                }
                let color = explosion
                    .colors
                    .first()
                    .map_or([0.0; 3], |c| unpack_rgb(*c));
                let mut flash = Particle::flash(starter.pos, color);
                if let Some(sprite) = self.sets.first(SpriteSet::Flash) {
                    flash.set_sprite(&sprite);
                }
                self.push(flash);
            }
            starter.life += 1;
            starter.life < starter.explosions.len() * 2
        });
        self.fireworks = starters;
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
        self.seeds.clear();
        self.pending_seeds.clear();
        self.fireworks.clear();
        self.explosions.clear();
    }

    /// Vanilla `ClientExplosionTracker.track`: an explosion that blew
    /// `block_count` blocks within `radius` of `center`, to throw
    /// `particles` from them on the next tick.
    pub fn track_explosion(
        &mut self,
        center: DVec3,
        radius: f32,
        block_count: i32,
        particles: Vec<(ExplosionParticle, ParticleLook)>,
        camera_pos: DVec3,
    ) {
        if !particles.is_empty() {
            self.explosions.push(TrackedExplosion {
                center,
                radius,
                block_count: block_count.max(0) as u32,
                particles,
                camera_pos,
            });
        }
    }

    /// Vanilla `ClientExplosionTracker.tick`: a particle for each block
    /// blown since the last tick, 512 at most, shared among the explosions
    /// by how many blocks each blew.
    fn tick_explosions(&mut self, chunks: &ChunkStore, block_state_table: BlockStateTable) {
        let explosions = std::mem::take(&mut self.explosions);
        let total = explosions
            .iter()
            .fold(0u32, |sum, e| sum.saturating_add(e.block_count));
        for _ in 0..total.min(MAX_EXPLOSION_PARTICLES_PER_TICK) {
            let pick = fastrand::u32(0..total);
            let Some(explosion) = weighted_pick(&explosions, |e| e.block_count, pick) else {
                continue;
            };
            // `addParticle`: a point in the blast's ball, thrown outward
            // faster the nearer the centre it starts.
            let axis = || f64::from(fastrand::f32() * 2.0 - 1.0);
            let direction = dvec3(axis(), axis(), axis()).normalize_or_zero();
            let distance = fastrand::f32().cbrt() * explosion.radius;
            let offset = direction * f64::from(distance);
            let at = BlockPos::containing(explosion.center + offset);
            let state = chunks.get_block_state(at.x, at.y, at.z);
            if !block_state_table.block_state_data(state).is_air() {
                continue;
            }
            let speed =
                0.5 / (distance / explosion.radius + 0.1) * fastrand::f32() * fastrand::f32() + 0.3;
            let weights = explosion
                .particles
                .iter()
                .fold(0u32, |sum, (p, _)| sum.saturating_add(p.weight));
            if weights == 0 {
                continue;
            }
            let pick = fastrand::u32(0..weights);
            let Some(&(info, look)) = weighted_pick(&explosion.particles, |(p, _)| p.weight, pick)
            else {
                continue;
            };
            self.add_server_particle(
                Spawn {
                    kind: info.kind,
                    options: info.options,
                    look,
                    override_limiter: false,
                },
                explosion.center + offset * f64::from(info.scaling),
                direction * f64::from(speed * info.speed),
                explosion.camera_pos,
            );
        }
    }

    /// Ticks the seeds, which spawn their particles for the next tick.
    fn tick_seeds(&mut self) {
        let mut seeds = std::mem::take(&mut self.seeds);
        seeds.retain_mut(|seed| {
            let (spawns, size_arg, alive) = seed.tick();
            for pos in spawns {
                let mut p = match seed.kind {
                    SeedKind::Explosion => Particle::explosion(pos, size_arg),
                    SeedKind::Gust { .. } => Particle::gust(pos, false),
                };
                p.sprite_from_age(&self.sets);
                self.push(p);
            }
            alive
        });
        seeds.append(&mut self.pending_seeds);
        self.seeds = seeds;
    }

    pub fn tick(&mut self, chunks: &ChunkStore, block_state_table: BlockStateTable) {
        self.tick_explosions(chunks, block_state_table);
        self.tick_seeds();
        self.tick_fireworks();
        let sets = &self.sets;
        let pending = &mut self.pending;
        self.particles.retain_mut(|p| {
            let alive = p.tick(chunks, block_state_table, sets);
            // What a drip leaves behind: a hanging one the drop that
            // falls (`DripHangParticle.preMoveUpdate`), a fallen one its
            // splash or the drip lying where it landed
            // (`FallAndLandParticle.postMoveUpdate`).
            let next = match p.extra {
                _ if alive => None,
                Extra::DripHang(kind) => {
                    Some((Particle::drip_fall(p.pos, kind), SpriteSet::DripFall))
                }
                Extra::DripFall(Drip::Water) if p.on_ground => {
                    Some((Particle::splash(p.pos, DVec3::ZERO), SpriteSet::Rain))
                }
                Extra::DripFall(kind) if p.on_ground => {
                    Some((Particle::drip_land(p.pos, kind), SpriteSet::DripLand))
                }
                _ => None,
            };
            if let Some((mut next, set)) = next {
                if let Some(sprite) = sets.pick(set) {
                    next.set_sprite(&sprite);
                }
                pending.push(next);
            }
            if alive && let Some(mut spark) = p.trail_spark() {
                spark.sprite_from_age(sets);
                pending.push(spark);
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
            .filter(|p| p.shown())
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
                    roll: match p.extra {
                        Extra::Leaf { roll, .. } => roll[0] + (roll[1] - roll[0]) * partial_tick,
                        _ => 0.0,
                    },
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

    /// `SpriteSets::load` indexes the sets by `SpriteSet` discriminant.
    #[test]
    fn sprite_sets_are_listed_in_enum_order() {
        for (i, (set, _)) in SPRITE_SETS.iter().enumerate() {
            assert_eq!(*set as usize, i, "{set:?}");
        }
    }

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
        assert_eq!(ServerParticleKind::from_id(id("elder_guardian")), None);
        for (name, kind) in [
            ("explosion_emitter", ServerParticleKind::ExplosionEmitter),
            ("gust", ServerParticleKind::Gust),
            ("small_gust", ServerParticleKind::SmallGust),
            ("gust_emitter_large", ServerParticleKind::GustEmitterLarge),
            ("gust_emitter_small", ServerParticleKind::GustEmitterSmall),
        ] {
            assert_eq!(ServerParticleKind::from_id(id(name)), Some(kind));
        }
        assert_eq!(ServerParticleKind::from_id(9999), None);
    }

    fn new_store() -> ParticleStore {
        let map = || Arc::new(Colormap::fallback());
        ParticleStore::new(AtlasUVMap::whole_texture(), map(), map(), map())
    }

    fn spawn(store: &mut ParticleStore, kind: ServerParticleKind) {
        store.add_particles_from_packet(
            kind,
            ParticleOptions::None,
            ParticleLook::None,
            false,
            dvec3(0.5, 64.0, 0.5),
            DVec3::X,
            1.0,
            0,
            // Far off: these kinds show from any distance.
            dvec3(500.0, 64.0, 0.5),
        );
    }

    /// The emitters are seeds: the large gust throws three gusts a tick for
    /// eight ticks, the small one three now and three more three ticks on,
    /// and an explosion six a tick for eight, each smaller.
    #[test]
    fn emitters_spawn_their_particles_over_their_lives() {
        use ServerParticleKind as K;
        let chunks = ChunkStore::new(8);
        let table = BlockStateTable::native();
        for (kind, per_tick) in [
            (K::GustEmitterLarge, vec![3, 3, 3, 3, 3, 3, 3, 3, 0]),
            (K::GustEmitterSmall, vec![3, 0, 0, 3, 0]),
            (K::ExplosionEmitter, vec![6, 6, 6, 6, 6, 6, 6, 6, 0]),
        ] {
            let mut store = new_store();
            spawn(&mut store, kind);
            // The seed itself starts the tick after it spawns.
            store.tick(&chunks, table);
            assert!(store.particles.is_empty(), "{kind:?}");
            let mut spawned = Vec::new();
            let mut total = 0;
            for _ in 0..per_tick.len() {
                store.tick(&chunks, table);
                // Count what this tick added: every particle here outlives
                // the test's few ticks or is counted before it dies.
                let born = store.particles.iter().filter(|p| p.age == 0).count();
                spawned.push(born);
                total += born;
            }
            assert_eq!(spawned, per_tick, "{kind:?}");
            assert!(store.seeds.is_empty(), "{kind:?}");
            assert_eq!(total, per_tick.iter().sum::<usize>());
        }

        let mut store = new_store();
        spawn(&mut store, K::Gust);
        spawn(&mut store, K::SmallGust);
        store.tick(&chunks, table);
        // A small gust is the one kind here that only shows near the camera.
        assert_eq!(store.particles.len(), 1);
        store.add_particles_from_packet(
            K::SmallGust,
            ParticleOptions::None,
            ParticleLook::None,
            false,
            dvec3(0.5, 64.0, 0.5),
            DVec3::ZERO,
            0.0,
            0,
            dvec3(8.0, 64.0, 0.5),
        );
        store.tick(&chunks, table);
        let sizes: Vec<f32> = store.particles.iter().map(|p| p.quad_size(0.0)).collect();
        assert_eq!(sizes, [1.0, 0.15]);
        assert!(
            store
                .particles
                .iter()
                .all(|p| (12..16).contains(&p.lifetime))
        );

        // An explosion's later bursts are smaller: `1 - age / 8` of 2 down
        // to half.
        let mut seed = Seed {
            kind: SeedKind::Explosion,
            pos: DVec3::ZERO,
            age: 4,
        };
        let (spawns, size_arg, alive) = seed.tick();
        assert_eq!((spawns.len(), size_arg, alive), (6, 0.5, true));
        assert_eq!(Particle::explosion(DVec3::ZERO, size_arg).size, 1.5);
        assert!(spawns.iter().all(|p| p.abs().max_element() <= 4.0));
    }

    /// An explosion's debris: one particle for each block it blew, 512 a
    /// tick at most, thrown outward from inside its radius, and only once.
    #[test]
    fn explosions_throw_debris_from_their_blocks() {
        let chunks = ChunkStore::new(8);
        let table = BlockStateTable::native();
        let debris = |kind, scaling, weight| {
            (
                ExplosionParticle {
                    kind,
                    options: ParticleOptions::None,
                    scaling,
                    speed: 1.0,
                    weight,
                },
                ParticleLook::None,
            )
        };
        let center = dvec3(0.5, 64.0, 0.5);
        let mut store = new_store();
        store.track_explosion(
            center,
            4.0,
            40,
            vec![debris(ServerParticleKind::Poof, 0.5, 1)],
            center,
        );
        // No block particles: nothing to track (a wind charge's burst).
        store.track_explosion(center, 4.0, 1000, Vec::new(), center);
        store.tick(&chunks, table);
        assert_eq!(store.particles.len(), 40);
        assert!(
            store
                .particles
                .iter()
                .all(|p| p.pos.distance(center) <= 2.0 + 1e-6)
        );
        store.tick(&chunks, table);
        assert_eq!(store.particles.len(), 40, "tracked for one tick only");

        let mut store = store_with(center, 100_000);
        store.tick(&chunks, table);
        assert_eq!(store.particles.len(), 512);

        fn store_with(center: DVec3, blocks: i32) -> ParticleStore {
            let mut store = new_store();
            store.track_explosion(
                center,
                4.0,
                blocks,
                vec![(
                    ExplosionParticle {
                        kind: ServerParticleKind::Smoke,
                        options: ParticleOptions::None,
                        scaling: 1.0,
                        speed: 1.0,
                        weight: 3,
                    },
                    ParticleLook::None,
                )],
                center,
            );
            store
        }

        assert_eq!(weighted_pick(&[2u32, 3, 5], |w| *w, 0), Some(&2));
        assert_eq!(weighted_pick(&[2u32, 3, 5], |w| *w, 4), Some(&3));
        assert_eq!(weighted_pick(&[2u32, 3, 5], |w| *w, 9), Some(&5));
        assert_eq!(weighted_pick(&[2u32, 3, 5], |w| *w, 10), None);
    }

    /// A rocket's stars go off one every other tick: a small ball's 98
    /// sparks and its flash, then a burst's 70, in the stars' own colours;
    /// trailing sparks leave more behind, and twinkling ones blink.
    #[test]
    fn a_rocket_bursts_star_by_star() {
        let chunks = ChunkStore::new(8);
        let table = BlockStateTable::native();
        let star = |shape, trail, twinkle| FireworkExplosion {
            shape,
            colors: vec![0xFF0000],
            fade_colors: vec![0x0000FF],
            trail,
            twinkle,
        };
        let mut store = new_store();
        let at = dvec3(0.5, 80.0, 0.5);
        store.start_fireworks(
            at,
            dvec3(0.0, 1.0, 0.0),
            vec![
                star(FireworkShape::SmallBall, false, false),
                star(FireworkShape::Burst, false, true),
            ],
        );
        store.start_fireworks(at, DVec3::ZERO, Vec::new());
        assert_eq!(store.fireworks.len(), 1, "an empty rocket is nothing");
        store.tick(&chunks, table);
        // A 5 x 5 x 5 cube's shell is 98 sparks, and the flash.
        assert_eq!(store.particles.len(), 99);
        assert!(
            store.particles[..98]
                .iter()
                .all(|p| p.color == [1.0, 0.0, 0.0])
        );
        assert!(
            store.particles[..98]
                .iter()
                .all(|p| (p.vel.length() - 0.25).abs() < 0.2)
        );
        store.tick(&chunks, table);
        assert_eq!(store.particles.len(), 99);
        store.tick(&chunks, table);
        assert_eq!(store.particles.len(), 99 + 71);
        // The burst goes the rocket's way.
        assert!(store.particles[99..169].iter().all(|p| p.vel.y >= 0.5));

        // The starter outlives its last star by a tick, as vanilla's does.
        assert_eq!(store.fireworks.len(), 1);
        store.tick(&chunks, table);
        assert!(store.fireworks.is_empty());

        // A twinkling spark blinks once past a third of its life.
        let twinkler = store.particles[99..169]
            .iter()
            .find(|p| matches!(p.extra, Extra::Spark { twinkle: true, .. }))
            .unwrap();
        assert!(twinkler.shown());
        let mut late =
            Particle::burst_spark(at, DVec3::ZERO, &star(FireworkShape::Burst, false, true));
        late.lifetime = 48;
        late.age = 17;
        assert!(!late.shown());
        late.age = 24;
        assert!(late.shown());

        // A trailing spark leaves one behind on the ticks its age and life
        // sum even, for its first half.
        let mut trailing =
            Particle::burst_spark(at, DVec3::ZERO, &star(FireworkShape::Star, true, false));
        trailing.lifetime = 48;
        trailing.age = 4;
        let left = trailing.trail_spark().unwrap();
        assert_eq!(left.color, trailing.color);
        assert_eq!(left.age, left.lifetime / 2);
        assert!(left.trail_spark().is_none());
        trailing.age = 5;
        assert!(trailing.trail_spark().is_none());
        trailing.age = 30;
        assert!(trailing.trail_spark().is_none());

        // A star's outline: its tip, then 5 edges x 4 points x 2 sides x 3.
        let starter = FireworkStarter {
            pos: at,
            vel: DVec3::ZERO,
            explosions: Vec::new(),
            life: 0,
        };
        assert_eq!(starter.spark_velocities(FireworkShape::Star).len(), 121);
        assert_eq!(starter.spark_velocities(FireworkShape::Creeper).len(), 265);
        assert_eq!(
            starter.spark_velocities(FireworkShape::LargeBall).len(),
            386
        );
    }

    /// Spawns `kind` with `options` beside the camera, with the packet's
    /// `count` of zero (its spread times speed as the velocity).
    fn spawn_near(
        store: &mut ParticleStore,
        kind: ServerParticleKind,
        options: ParticleOptions,
        vel: DVec3,
    ) {
        let pos = dvec3(0.5, 64.0, 0.5);
        let look = ParticleLook::None;
        store.add_particles_from_packet(kind, options, look, false, pos, vel, 1.0, 0, pos);
    }

    /// Every kind a server can name by a bare type id draws something,
    /// but for the bits of an item the test atlas has no sprite for.
    #[test]
    fn every_plain_kind_spawns_a_particle() {
        use ServerParticleKind as K;
        let table = pomme_protocol::RegistryTable::native();
        let mut known = 0;
        for id in 0.. {
            let Some(name) = table.name_of(pomme_protocol::ClientRegistry::ParticleType, id) else {
                break;
            };
            let Some(kind) = K::from_name(name) else {
                continue;
            };
            known += 1;
            let options = match kind {
                K::Dust => ParticleOptions::Dust {
                    color: 0xFF0000,
                    scale: 1.0,
                },
                K::DustColorTransition => ParticleOptions::DustTransition {
                    from: 0,
                    to: 0xFFFFFF,
                    scale: 1.0,
                },
                K::Trail => ParticleOptions::Trail {
                    target: dvec3(4.5, 64.0, 0.5),
                    color: 0x00FF00,
                    duration: 8,
                },
                // These draw a terrain or item sprite, resolved by the caller.
                K::Block | K::BlockCrumble | K::FallingDust | K::Item => continue,
                K::ItemSlime | K::ItemSnowball => continue,
                // Seeds, which draw nothing themselves.
                K::ExplosionEmitter | K::GustEmitterLarge | K::GustEmitterSmall => continue,
                _ => ParticleOptions::None,
            };
            let mut store = new_store();
            spawn_near(&mut store, kind, options, DVec3::ZERO);
            assert_eq!(store.pending.len(), 1, "{name}");
            assert!(store.pending[0].size > 0.0, "{name}");
        }
        assert!(known >= 80, "{known} kinds");
    }

    /// A hanging drip turns into the falling one, lava cooling as it hangs;
    /// a trail arrives at its target as it ends; a flash swells and dims;
    /// snow slows on every axis; a bubble pops outside water.
    #[test]
    fn drips_trails_flashes_snow_and_bubbles_behave() {
        use ServerParticleKind as K;
        let chunks = ChunkStore::new(8);
        let table = BlockStateTable::native();

        let mut store = new_store();
        spawn_near(
            &mut store,
            K::DrippingLava,
            ParticleOptions::None,
            DVec3::ZERO,
        );
        store.tick(&chunks, table);
        assert_eq!(store.particles[0].extra, Extra::DripHang(Drip::Lava));
        let start = store.particles[0].pos;
        for _ in 0..20 {
            store.tick(&chunks, table);
        }
        let hanging = &store.particles[0];
        assert_eq!(hanging.color, [1.0, 16.0 / 36.0, 4.0 / 28.0]);
        // 0.0012 a tick, nearly all of it shed again.
        assert!(
            start.y - hanging.pos.y < 0.03,
            "a hanging drip barely moves"
        );
        for _ in 0..22 {
            store.tick(&chunks, table);
        }
        assert_eq!(store.particles.len(), 1);
        let falling = &store.particles[0];
        assert_eq!(falling.extra, Extra::DripFall(Drip::Lava));
        assert_eq!(falling.color, Drip::Lava.color());
        // It falls 0.06 a tick faster, then sheds 2% of its speed.
        let (y, speed) = (falling.pos.y, falling.vel.y);
        store.tick(&chunks, table);
        let fell = &store.particles[0];
        let gravity = f64::from(0.06f32);
        assert!((y - fell.pos.y - (gravity - speed)).abs() < 1e-9);
        assert!((fell.vel.y - (speed - gravity) * f64::from(0.98f32)).abs() < 1e-9);

        let target = dvec3(4.5, 66.0, 0.5);
        let mut store = new_store();
        spawn_near(
            &mut store,
            K::Trail,
            ParticleOptions::Trail {
                target,
                color: 0x00FF00,
                duration: 8,
            },
            DVec3::ZERO,
        );
        for tick in 0..9 {
            store.tick(&chunks, table);
            if tick == 4 {
                // Four ticks in it has gone four sevenths of the way: each
                // tick covers an equal share of what the ticks left span.
                let p = &store.particles[0];
                let along = 4.0 / 7.0;
                assert!((p.pos.x - (0.5 + 4.0 * along)).abs() < 1e-9);
                assert!((p.pos.y - (64.0 + 2.0 * along)).abs() < 1e-9);
            }
        }
        assert!(store.particles[0].pos.distance(target) < 1e-9);
        store.tick(&chunks, table);
        assert!(store.particles.is_empty());

        let mut flash = Particle::flash(DVec3::ZERO, [1.0; 3]);
        assert_eq!(flash.quad_size(0.0), 0.0);
        let sets = SpriteSets::load(&AtlasUVMap::whole_texture());
        let mut sizes = Vec::new();
        while flash.tick(&chunks, table, &sets) {
            sizes.push((flash.quad_size(0.0), flash.alpha));
        }
        assert_eq!(sizes.len(), 4);
        assert!((sizes[2].0 - 7.1).abs() < 1e-5, "widest at its third tick");
        assert!(
            sizes.windows(2).all(|w| w[1].1 < w[0].1),
            "dimmer each tick"
        );
        assert!(sizes[3].1 > 0.0);

        let mut snow = Particle::snowflake(DVec3::ZERO, dvec3(1.0, 0.0, 1.0));
        let before = snow.vel;
        snow.tick(&chunks, table, &sets);
        assert!((snow.vel.x - before.x * f64::from(0.95f32)).abs() < 1e-9);
        let fallen = (before.y - 0.04 * f64::from(0.225f32)) * f64::from(0.9f32);
        assert!((snow.vel.y - fallen).abs() < 1e-9);

        // Dragon breath keeps its height's speed and slows sideways.
        let mut breath = Particle::dragon_breath(DVec3::ZERO, dvec3(0.1, 0.05, 0.0), 2.0);
        assert_eq!(breath.vel, dvec3(0.2, 0.1, 0.0));
        breath.tick(&chunks, table, &sets);
        assert!((breath.vel.y - 0.1).abs() < 1e-9);
        assert!((breath.vel.x - 0.2 * f64::from(0.96f32)).abs() < 1e-9);

        let mut store = new_store();
        spawn_near(&mut store, K::Bubble, ParticleOptions::None, DVec3::ZERO);
        store.tick(&chunks, table);
        store.tick(&chunks, table);
        assert!(store.particles.is_empty(), "no water here");
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
