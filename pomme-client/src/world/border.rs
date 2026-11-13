//! The world border (vanilla `WorldBorder`): a square about a centre, whose
//! size the server sets at once or over a number of ticks. Its walls stop
//! the player, show as a forcefield when near, and tint the vignette as
//! they close in.
// TODO: a size's change is timed in ticks, as 26.1 sends it; older servers
// sent milliseconds, which a translated session should convert.

use glam::{DVec3, dvec3};

use crate::physics::aabb::Aabb;

/// Further than any world reaches, for the walls' far sides.
const FAR: f64 = 1.0e8;
/// How far above and below the camera the drawn walls reach.
const WALL_REACH: f64 = 512.0;

/// One of the border's packets.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BorderUpdate {
    Init {
        x: f64,
        z: f64,
        old_size: f64,
        new_size: f64,
        lerp_time: u64,
        absolute_max: i32,
        warning_blocks: i32,
        warning_time: i32,
    },
    Center {
        x: f64,
        z: f64,
    },
    LerpSize {
        old_size: f64,
        new_size: f64,
        lerp_time: u64,
    },
    Size {
        size: f64,
    },
    WarningTime {
        time: i32,
    },
    WarningBlocks {
        blocks: i32,
    },
}

/// A size on its way to another (`WorldBorder.MovingBorderExtent`).
#[derive(Clone, Copy, Debug)]
struct Lerp {
    from: f64,
    to: f64,
    duration: f64,
    /// Ticks left.
    left: i64,
}

/// The square the border bounds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds {
    pub min_x: f64,
    pub min_z: f64,
    pub max_x: f64,
    pub max_z: f64,
}

impl Bounds {
    /// `WorldBorder.getDistanceToBorder`: how far inside the nearest wall
    /// a point is; negative outside.
    pub fn distance(&self, x: f64, z: f64) -> f64 {
        (x - self.min_x)
            .min(self.max_x - x)
            .min(z - self.min_z)
            .min(self.max_z - z)
    }
}

#[derive(Clone, Debug)]
pub struct WorldBorder {
    center: (f64, f64),
    absolute_max: f64,
    warning_blocks: i32,
    warning_time: i32,
    size: f64,
    /// The size a tick ago, for the frames between.
    prev_size: f64,
    lerp: Option<Lerp>,
}

impl Default for WorldBorder {
    /// `WorldBorder`'s own defaults: as wide as a world can be.
    fn default() -> Self {
        Self {
            center: (0.0, 0.0),
            absolute_max: 29_999_984.0,
            warning_blocks: 5,
            warning_time: 15,
            size: 5.999_996_8e7,
            prev_size: 5.999_996_8e7,
            lerp: None,
        }
    }
}

impl WorldBorder {
    /// `ClientPacketListener`'s border handlers.
    pub fn apply(&mut self, update: BorderUpdate) {
        match update {
            BorderUpdate::Init {
                x,
                z,
                old_size,
                new_size,
                lerp_time,
                absolute_max,
                warning_blocks,
                warning_time,
            } => {
                self.center = (x, z);
                if lerp_time > 0 {
                    self.lerp_between(old_size, new_size, lerp_time);
                } else {
                    self.set_size(new_size);
                }
                self.absolute_max = f64::from(absolute_max);
                self.warning_blocks = warning_blocks;
                self.warning_time = warning_time;
            }
            BorderUpdate::Center { x, z } => self.center = (x, z),
            BorderUpdate::LerpSize {
                old_size,
                new_size,
                lerp_time,
            } => self.lerp_between(old_size, new_size, lerp_time),
            BorderUpdate::Size { size } => self.set_size(size),
            BorderUpdate::WarningTime { time } => self.warning_time = time,
            BorderUpdate::WarningBlocks { blocks } => self.warning_blocks = blocks,
        }
    }

    fn set_size(&mut self, size: f64) {
        self.size = size;
        self.prev_size = size;
        self.lerp = None;
    }

    /// `WorldBorder.lerpSizeBetween`.
    fn lerp_between(&mut self, from: f64, to: f64, ticks: u64) {
        if from == to || ticks == 0 {
            self.set_size(to);
            return;
        }
        self.size = from;
        self.prev_size = from;
        self.lerp = Some(Lerp {
            from,
            to,
            duration: ticks as f64,
            left: ticks.min(i64::MAX as u64) as i64,
        });
    }

    /// `MovingBorderExtent.update`: a tick of a changing size.
    pub fn tick(&mut self) {
        self.prev_size = self.size;
        let Some(lerp) = &mut self.lerp else {
            return;
        };
        lerp.left -= 1;
        let along = (lerp.duration - lerp.left as f64) / lerp.duration;
        self.size = if along < 1.0 {
            lerp.from + (lerp.to - lerp.from) * along
        } else {
            lerp.to
        };
        if lerp.left <= 0 {
            self.lerp = None;
        }
    }

    /// The square at `partial_tick` between the last tick's size and this
    /// one's, held within the absolute maximum.
    pub fn bounds(&self, partial_tick: f32) -> Bounds {
        let half = (self.prev_size + (self.size - self.prev_size) * f64::from(partial_tick)) / 2.0;
        let clamp = |v: f64| v.clamp(-self.absolute_max, self.absolute_max);
        Bounds {
            min_x: clamp(self.center.0 - half),
            min_z: clamp(self.center.1 - half),
            max_x: clamp(self.center.0 + half),
            max_z: clamp(self.center.1 + half),
        }
    }

    /// `BorderStatus.getColor`: red shrinking, green growing, else blue.
    fn color(&self) -> [f32; 3] {
        let rgb = match self.lerp {
            Some(lerp) if lerp.to < lerp.from => 0xFF3030u32,
            Some(_) => 0x40FF80,
            None => 0x20A0FF,
        };
        [16, 8, 0].map(|shift| ((rgb >> shift) & 0xFF) as f32 / 255.0)
    }

    /// `Gui.extractVignette`'s border term: 0 away from the border, rising
    /// to 1 at it, from the warning distance or as far as it will close in
    /// the warning time.
    pub fn warning(&self, x: f64, z: f64) -> f32 {
        let distance = self.bounds(1.0).distance(x, z);
        let closing = self.lerp.map_or(0.0, |lerp| {
            ((lerp.from - lerp.to).abs() / lerp.duration * f64::from(self.warning_time))
                .min((lerp.to - self.size).abs())
        });
        let reach = f64::from(self.warning_blocks).max(closing);
        if distance < reach {
            (1.0 - distance / reach).clamp(0.0, 1.0) as f32
        } else {
            0.0
        }
    }

    /// `Entity.collectColliders`: the border's walls as boxes, for a box
    /// `entity` whose move sweeps `region`, when it is inside and close
    /// (`isInsideCloseToBorder`). Everything outside the square, widened to
    /// whole blocks, is solid.
    pub fn add_walls(&self, entity: &Aabb, region: &Aabb, boxes: &mut Vec<Aabb>) {
        let bounds = self.bounds(1.0);
        let size = region.max - region.min;
        let margin = size.x.abs().max(size.z.abs()).max(1.0);
        let x = (entity.min.x + entity.max.x) / 2.0;
        let z = (entity.min.z + entity.max.z) / 2.0;
        let within = x >= bounds.min_x - margin
            && x < bounds.max_x + margin
            && z >= bounds.min_z - margin
            && z < bounds.max_z + margin;
        if !(bounds.distance(x, z) < margin * 2.0 && within) {
            return;
        }
        let (min_x, max_x) = (bounds.min_x.floor(), bounds.max_x.ceil());
        let (min_z, max_z) = (bounds.min_z.floor(), bounds.max_z.ceil());
        boxes.extend([
            Aabb::new(dvec3(-FAR, -FAR, -FAR), dvec3(min_x, FAR, FAR)),
            Aabb::new(dvec3(max_x, -FAR, -FAR), dvec3(FAR, FAR, FAR)),
            Aabb::new(dvec3(-FAR, -FAR, -FAR), dvec3(FAR, FAR, min_z)),
            Aabb::new(dvec3(-FAR, -FAR, max_z), dvec3(FAR, FAR, FAR)),
        ]);
    }

    /// `WorldBorderRenderer`: the walls within `reach` blocks of the
    /// camera, as triangles of (position less `anchor`, texture
    /// coordinate), and their colour, fading in over the last of the
    /// distance. The texture repeats every two blocks and scrolls once in
    /// three seconds (`millis`).
    #[allow(clippy::type_complexity)]
    pub fn mesh(
        &self,
        camera: DVec3,
        anchor: DVec3,
        reach: f64,
        partial_tick: f32,
        millis: u64,
    ) -> Option<(Vec<([f32; 3], [f32; 2])>, [f32; 4])> {
        let b = self.bounds(partial_tick);
        let (x, z) = (camera.x, camera.z);
        let near_east = x > b.max_x - reach;
        let near_west = x < b.min_x + reach;
        let near_south = z > b.max_z - reach;
        let near_north = z < b.min_z + reach;
        if !(near_east || near_west || near_south || near_north) {
            return None;
        }
        let alpha = (1.0 - b.distance(x, z) / reach).powi(4).clamp(0.0, 1.0) as f32;
        if alpha <= 0.0 {
            return None;
        }
        let scroll = (millis % 3000) as f32 / 3000.0;
        let top = scroll - (camera.y * 0.5).rem_euclid(1.0) as f32;
        let bottom = top + WALL_REACH as f32;
        let (low, high) = (camera.y - WALL_REACH, camera.y + WALL_REACH);

        let mut triangles = Vec::with_capacity(24);
        // A wall from `from` to `to` along its length, at `at` across;
        // `along_x` for the north and south walls. `sign` is which way the
        // texture runs along it.
        let mut wall = |along_x: bool, at: f64, from: f64, to: f64, sign: f32| {
            if to <= from {
                return;
            }
            let start = scroll + sign * ((from.floor() as i64 & 1) as f32 * 0.5);
            let end = start + sign * ((to - from) as f32 * 0.5);
            let corner = |along: f64, y: f64, u: f32, v: f32| {
                let world = if along_x {
                    dvec3(along, y, at)
                } else {
                    dvec3(at, y, along)
                };
                ((world - anchor).as_vec3().to_array(), [u, v])
            };
            let quad = [
                corner(from, low, start, bottom),
                corner(to, low, end, bottom),
                corner(to, high, end, top),
                corner(from, high, start, top),
            ];
            triangles.extend([0, 1, 2, 0, 2, 3].map(|index| quad[index]));
        };
        let from_z = (z - reach).floor().max(b.min_z);
        let to_z = (z + reach).ceil().min(b.max_z);
        if near_east {
            wall(false, b.max_x, from_z, to_z, -1.0);
        }
        if near_west {
            wall(false, b.min_x, from_z, to_z, 1.0);
        }
        let from_x = (x - reach).floor().max(b.min_x);
        let to_x = (x + reach).ceil().min(b.max_x);
        if near_south {
            wall(true, b.max_z, from_x, to_x, 1.0);
        }
        if near_north {
            wall(true, b.min_z, from_x, to_x, -1.0);
        }
        let [red, green, blue] = self.color();
        Some((triangles, [red, green, blue, alpha]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn border(size: f64) -> WorldBorder {
        let mut border = WorldBorder::default();
        border.apply(BorderUpdate::Center { x: 100.0, z: 0.0 });
        border.apply(BorderUpdate::Size { size });
        border
    }

    /// A border set over time closes a step a tick, the frames between
    /// lying between, and then stays.
    #[test]
    fn a_border_closes_over_its_ticks() {
        let mut border = border(20.0);
        border.apply(BorderUpdate::LerpSize {
            old_size: 20.0,
            new_size: 10.0,
            lerp_time: 10,
        });
        assert_eq!(border.bounds(1.0).max_x, 110.0);
        border.tick();
        assert_eq!(border.bounds(1.0).max_x, 109.5);
        assert_eq!(border.bounds(0.5).max_x, 109.75);
        assert_eq!(border.color(), [1.0, 48.0 / 255.0, 48.0 / 255.0]);
        for _ in 0..12 {
            border.tick();
        }
        let bounds = border.bounds(1.0);
        assert_eq!((bounds.min_x, bounds.max_x), (95.0, 105.0));
        assert_eq!((bounds.min_z, bounds.max_z), (-5.0, 5.0));
        assert_eq!(border.color(), [32.0 / 255.0, 160.0 / 255.0, 1.0]);
    }

    /// The walls stop a box inside and near them, are not there for one far
    /// off, and the vignette warns within the warning distance.
    #[test]
    fn walls_stand_about_one_close_inside() {
        let border = border(20.0);
        let player = |x: f64| Aabb::new(dvec3(x - 0.3, 64.0, -0.3), dvec3(x + 0.3, 65.8, 0.3));
        let mut boxes = Vec::new();
        border.add_walls(&player(100.0), &player(100.0), &mut boxes);
        assert!(boxes.is_empty());
        border.add_walls(&player(109.0), &player(109.0), &mut boxes);
        assert_eq!(boxes.len(), 4);
        // The east wall begins at the border.
        assert_eq!(boxes[1].min.x, 110.0);
        assert!(boxes.iter().all(|wall| !wall.intersects(&player(109.0))));

        assert_eq!(border.warning(100.0, 0.0), 0.0);
        assert_eq!(border.warning(107.5, 0.0), 0.5);
        assert_eq!(border.warning(111.0, 0.0), 1.0);
    }

    /// Only walls within reach are drawn, each two triangles, fading in as
    /// the camera nears.
    #[test]
    fn near_walls_are_drawn() {
        let border = border(2000.0);
        let anchor = dvec3(1000.0, 0.0, 0.0);
        assert!(
            border
                .mesh(dvec3(100.0, 70.0, 0.0), anchor, 128.0, 1.0, 0)
                .is_none()
        );

        let camera = dvec3(1090.0, 70.0, 0.0);
        let (triangles, color) = border.mesh(camera, anchor, 128.0, 1.0, 0).unwrap();
        assert_eq!(triangles.len(), 6);
        // The east wall, 100 blocks from the anchor, 128 each way along.
        assert!(triangles.iter().all(|(at, _)| at[0] == 100.0));
        let along: Vec<f32> = triangles.iter().map(|(at, _)| at[2]).collect();
        assert_eq!(along.iter().copied().fold(f32::MAX, f32::min), -128.0);
        assert_eq!(along.iter().copied().fold(f32::MIN, f32::max), 128.0);
        // Ten blocks off of 128: (1 - 10/128)^4.
        assert!((color[3] - 0.7222).abs() < 1e-3, "{color:?}");
        assert_eq!(&color[..3], &[32.0 / 255.0, 160.0 / 255.0, 1.0]);

        // In a corner, two walls.
        let corner = dvec3(1090.0, 70.0, 990.0);
        assert_eq!(
            border.mesh(corner, anchor, 128.0, 1.0, 0).unwrap().0.len(),
            12
        );
    }
}
