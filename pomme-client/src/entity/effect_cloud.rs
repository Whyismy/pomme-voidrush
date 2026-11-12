//! Area effect clouds (vanilla `AreaEffectCloud`): nothing but the particles
//! they scatter over their disc each tick, a lingering potion's or whatever
//! a server sets.

use std::collections::HashMap;

use glam::DVec3;

use crate::particle::{ParticleOptions, ServerParticleKind};

/// `AreaEffectCloud.DEFAULT_RADIUS`.
const DEFAULT_RADIUS: f32 = 3.0;
/// The particle of a cloud that names none: a white `entity_effect`.
const DEFAULT_PARTICLE: (ServerParticleKind, ParticleOptions) = (
    ServerParticleKind::EntityEffect,
    ParticleOptions::Color(0xFFFF_FFFF),
);

pub struct EffectCloud {
    pub position: DVec3,
    radius: f32,
    /// `DATA_WAITING`: not yet spread out, a sparse puff where it landed.
    waiting: bool,
    /// `None` for a particle this client doesn't draw.
    particle: Option<(ServerParticleKind, ParticleOptions)>,
}

/// One particle a cloud throws this tick.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CloudParticle {
    pub kind: ServerParticleKind,
    pub options: ParticleOptions,
    pub position: DVec3,
    pub velocity: DVec3,
}

#[derive(Default)]
pub struct EffectCloudStore {
    pub clouds: HashMap<i32, EffectCloud>,
}

impl EffectCloudStore {
    pub fn spawn(&mut self, id: i32, position: DVec3) {
        self.clouds.insert(
            id,
            EffectCloud {
                position,
                radius: DEFAULT_RADIUS,
                waiting: false,
                particle: Some(DEFAULT_PARTICLE),
            },
        );
    }

    pub fn move_delta(&mut self, id: i32, delta: DVec3) {
        if let Some(cloud) = self.clouds.get_mut(&id) {
            cloud.position += delta;
        }
    }

    pub fn teleport(&mut self, id: i32, position: DVec3) {
        if let Some(cloud) = self.clouds.get_mut(&id) {
            cloud.position = position;
        }
    }

    /// `DATA_RADIUS` (index 8) and `DATA_WAITING`: 9 since 1.20.5
    /// (protocol 766) dropped the colour synced ahead of it, 10 before.
    pub fn apply_entity_data(&mut self, id: i32, index: u8, value: super::MetaValue) {
        let Some(cloud) = self.clouds.get_mut(&id) else {
            return;
        };
        let waiting_index = if crate::version::session_protocol() >= 766 {
            9
        } else {
            10
        };
        match value {
            super::MetaValue::Float(radius) if index == 8 => cloud.radius = radius,
            super::MetaValue::Bool(waiting) if index == waiting_index => cloud.waiting = waiting,
            _ => {}
        }
    }

    /// `DATA_PARTICLE`; `None` for one this client doesn't draw.
    pub fn set_particle(
        &mut self,
        id: i32,
        particle: Option<(ServerParticleKind, ParticleOptions)>,
    ) {
        if let Some(cloud) = self.clouds.get_mut(&id) {
            cloud.particle = particle;
        }
    }

    /// `AreaEffectCloud.clientTick`: a particle for every block of the
    /// disc's area, scattered over it and drifting; a waiting cloud two,
    /// close in and still, on half its ticks.
    pub fn tick(&self) -> Vec<CloudParticle> {
        let mut out = Vec::new();
        for cloud in self.clouds.values() {
            let Some((kind, options)) = cloud.particle else {
                continue;
            };
            if cloud.waiting && fastrand::bool() {
                continue;
            }
            let (count, reach) = if cloud.waiting {
                (2, 0.2)
            } else {
                let area = std::f32::consts::PI * cloud.radius * cloud.radius;
                (area.ceil().max(0.0) as u32, cloud.radius)
            };
            for _ in 0..count {
                let angle = fastrand::f32() * std::f32::consts::TAU;
                let out_by = fastrand::f32().sqrt() * reach;
                let position = cloud.position
                    + DVec3::new(
                        f64::from(angle.cos() * out_by),
                        0.0,
                        f64::from(angle.sin() * out_by),
                    );
                let effect = kind == ServerParticleKind::EntityEffect;
                let (kind, options) = if effect && cloud.waiting && fastrand::bool() {
                    DEFAULT_PARTICLE
                } else {
                    (kind, options)
                };
                let velocity = if effect || cloud.waiting {
                    DVec3::ZERO
                } else {
                    let drift = || (0.5 - fastrand::f64()) * 0.15;
                    DVec3::new(drift(), f64::from(0.01f32), drift())
                };
                out.push(CloudParticle {
                    kind,
                    options,
                    position,
                    velocity,
                });
            }
        }
        out
    }

    pub fn remove(&mut self, ids: &[i32]) {
        for id in ids {
            self.clouds.remove(id);
        }
    }

    pub fn clear(&mut self) {
        self.clouds.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::MetaValue;

    /// A cloud throws a particle for each block of its disc, inside it;
    /// one of another kind drifts up, a potion's hangs still; a cloud whose
    /// particle the client can't draw throws none.
    #[test]
    fn a_cloud_scatters_its_particle_over_its_disc() {
        let mut store = EffectCloudStore::default();
        let at = DVec3::new(10.0, 64.0, -5.0);
        store.spawn(1, at);
        let thrown = store.tick();
        // The default radius of three: pi * 9, rounded up.
        assert_eq!(thrown.len(), 29);
        assert!(thrown.iter().all(|p| p.position.y == 64.0));
        assert!(thrown.iter().all(|p| p.position.distance(at) <= 3.0 + 1e-4));
        assert!(thrown.iter().all(|p| p.velocity == DVec3::ZERO));
        assert_eq!(thrown[0].options, ParticleOptions::Color(0xFFFF_FFFF));

        store.apply_entity_data(1, 8, MetaValue::Float(1.0));
        store.set_particle(1, Some((ServerParticleKind::Flame, ParticleOptions::None)));
        let thrown = store.tick();
        assert_eq!(thrown.len(), 4);
        assert!(thrown.iter().all(|p| p.kind == ServerParticleKind::Flame));
        assert!(thrown.iter().all(|p| p.velocity.y > 0.0));

        store.move_delta(1, DVec3::new(0.0, 2.0, 0.0));
        assert_eq!(store.tick()[0].position.y, 66.0);
        store.set_particle(1, None);
        assert!(store.tick().is_empty());
        store.remove(&[1]);
        assert!(store.clouds.is_empty());
    }

    /// A waiting cloud puffs two particles close in, on about half its
    /// ticks.
    #[test]
    fn a_waiting_cloud_only_puffs() {
        let mut store = EffectCloudStore::default();
        let at = DVec3::new(0.0, 64.0, 0.0);
        store.spawn(1, at);
        store.apply_entity_data(1, 9, MetaValue::Bool(true));
        let mut puffs = 0;
        for _ in 0..200 {
            let thrown = store.tick();
            assert!(thrown.is_empty() || thrown.len() == 2);
            assert!(thrown.iter().all(|p| p.position.distance(at) <= 0.2 + 1e-4));
            puffs += usize::from(!thrown.is_empty());
        }
        assert!((50..150).contains(&puffs), "{puffs}");
    }
}
