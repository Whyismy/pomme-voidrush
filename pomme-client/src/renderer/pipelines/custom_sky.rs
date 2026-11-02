//! OptiFine custom skies, as resource packs ship them
//! (`assets/minecraft/optifine/sky/world<N>/sky<i>.properties`): each layer is
//! a cube textured from one 3 x 2 image, faded in and out by the time of day
//! and the weather, optionally limited to some days and turned with the sun,
//! and blended over the sky colour (alpha, add or replace) before the sun,
//! moon and stars draw.

use glam::{Mat4, Vec3};

/// Ticks in a day; times of day count from 06:00 as tick 0, as `/time` does.
const DAY: i32 = 24_000;

/// How a layer goes onto the sky below it (`Blender`). Packs' other modes
/// draw as `Alpha`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Blend {
    Alpha,
    Add,
    Replace,
}

/// A layer's fade: in from `start_in` to `end_in`, out from `start_out` to
/// `end_out`, in ticks of the day.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fade {
    pub start_in: i32,
    pub end_in: i32,
    pub start_out: i32,
    pub end_out: i32,
}

/// One `sky<i>.properties`.
#[derive(Clone, Debug, PartialEq)]
pub struct LayerProps {
    /// The texture's asset key (`minecraft/optifine/sky/world0/day.png`).
    pub source: String,
    pub blend: Blend,
    pub rotate: bool,
    pub speed: f32,
    pub axis: Vec3,
    /// `None`: always shown.
    pub fade: Option<Fade>,
    /// Shown in clear weather, rain and thunder (`weather`, default clear).
    pub weather: [bool; 3],
    /// `days` within a `daysLoop`-day cycle; `None`: every day.
    pub days: Option<(Vec<u32>, u32)>,
}

/// `HH:MM` as a tick of the day.
fn time(value: &str) -> Option<i32> {
    let (h, m) = value.trim().split_once(':')?;
    let (h, m): (i32, i32) = (h.parse().ok()?, m.parse().ok()?);
    if !(0..24).contains(&h) || !(0..60).contains(&m) {
        return None;
    }
    Some((h * 1000 + m * 1000 / 60 - 6000).rem_euclid(DAY))
}

/// `days`: numbers and ranges (`1 3 5-7`).
fn days(value: &str) -> Vec<u32> {
    let mut out = Vec::new();
    for part in value.split_whitespace() {
        match part.split_once('-') {
            Some((a, b)) => {
                if let (Ok(a), Ok(b)) = (a.parse::<u32>(), b.parse::<u32>()) {
                    out.extend(a.min(b)..=a.max(b));
                }
            }
            None => out.extend(part.parse::<u32>().ok()),
        }
    }
    out
}

impl LayerProps {
    /// Parses a layer file found at `dir` (its folder's asset key, as
    /// `minecraft/optifine/sky/world0`). `None` without a usable `source`.
    pub fn parse(text: &str, dir: &str) -> Option<Self> {
        let mut props = std::collections::HashMap::new();
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('#') || line.starts_with('!') {
                continue;
            }
            if let Some((k, v)) = line.split_once('=') {
                props.insert(k.trim(), v.trim());
            }
        }
        let source = props.get("source")?;
        let source = if let Some(rest) = source.strip_prefix("./") {
            format!("{dir}/{rest}")
        } else if let Some((namespace, path)) = source.split_once(':') {
            format!("{namespace}/{path}")
        } else if source.contains('/') {
            format!("minecraft/{source}")
        } else {
            format!("{dir}/{source}")
        };
        let blend = match props.get("blend").copied().unwrap_or("add") {
            "alpha" => Blend::Alpha,
            "replace" => Blend::Replace,
            "add" => Blend::Add,
            _ => Blend::Alpha,
        };
        let rotate = props.get("rotate").is_none_or(|v| *v != "false");
        let speed = props
            .get("speed")
            .and_then(|v| v.parse().ok())
            .unwrap_or(1.0);
        let axis = props
            .get("axis")
            .and_then(|v| {
                let n: Vec<f32> = v
                    .split_whitespace()
                    .filter_map(|x| x.parse().ok())
                    .collect();
                (n.len() == 3).then(|| Vec3::new(n[0], n[1], n[2]))
            })
            .filter(|a| a.length_squared() > 0.0)
            .unwrap_or(Vec3::Z);
        let fade = (|| {
            let start_in = time(props.get("startFadeIn")?)?;
            let end_in = time(props.get("endFadeIn")?)?;
            let end_out = time(props.get("endFadeOut")?)?;
            let start_out = match props.get("startFadeOut").and_then(|v| time(v)) {
                Some(t) => t,
                // The fade out lasts as long as the fade in.
                None => (end_out - (end_in - start_in).rem_euclid(DAY)).rem_euclid(DAY),
            };
            Some(Fade {
                start_in,
                end_in,
                start_out,
                end_out,
            })
        })();
        let weather = match props.get("weather") {
            Some(v) => {
                let has = |w| v.split_whitespace().any(|x| x == w);
                [has("clear"), has("rain"), has("thunder")]
            }
            None => [true, false, false],
        };
        let days = props.get("days").map(|v| {
            let looped = props
                .get("daysLoop")
                .and_then(|l| l.parse().ok())
                .filter(|l| *l > 0)
                .unwrap_or(8);
            (days(v), looped)
        });
        Some(Self {
            source,
            blend,
            rotate,
            speed,
            axis: axis.normalize(),
            fade,
            weather,
            days,
        })
    }

    /// How strongly the layer shows: its fade at this time of day, times
    /// the share of the sky's weather it is shown in, 0 on a day it skips.
    pub fn brightness(&self, day_time: u64, rain: f32, thunder: f32) -> f32 {
        if let Some((days, looped)) = &self.days {
            let day = (day_time / DAY as u64 % u64::from(*looped)) as u32;
            if !days.contains(&day) {
                return 0.0;
            }
        }
        let t = (day_time % DAY as u64) as i32;
        let fade = match self.fade {
            None => 1.0,
            Some(f) => {
                let span = |a: i32, b: i32| (b - a).rem_euclid(DAY).max(1) as f32;
                let since = |a: i32| (t - a).rem_euclid(DAY) as f32;
                if since(f.start_in) <= span(f.start_in, f.end_in) {
                    since(f.start_in) / span(f.start_in, f.end_in)
                } else if since(f.end_in) <= span(f.end_in, f.start_out) {
                    1.0
                } else if since(f.start_out) <= span(f.start_out, f.end_out) {
                    1.0 - since(f.start_out) / span(f.start_out, f.end_out)
                } else {
                    0.0
                }
            }
        };
        let [clear, raining, thundering] = self.weather;
        let mut weather = 0.0;
        if clear {
            weather += 1.0 - rain;
        }
        if raining {
            weather += rain - thunder;
        }
        if thundering {
            weather += thunder;
        }
        (fade * weather.clamp(0.0, 1.0)).clamp(0.0, 1.0)
    }

    /// The layer's turn on the sky: about its axis by the sun's angle times
    /// its speed, when it rotates.
    pub fn pose(&self, sun_angle_degrees: f32) -> Mat4 {
        if !self.rotate {
            return Mat4::IDENTITY;
        }
        Mat4::from_axis_angle(self.axis, (sun_angle_degrees * self.speed).to_radians())
    }
}

/// How far inside its cell a face's texture coordinates stay, as a share
/// of the cell.
const EDGE_INSET: f32 = 0.002;

/// The cube a layer draws on, its 3 x 2 image spread over the faces as packs
/// lay it out: the top row holds the bottom (cell 0), the top (1) and a side
/// (2); the bottom row the other sides (3, 4, 5), and the sides run 3, 4, 5, 2
/// round the horizon, each upright. Matched against Void Rush's sky, whose
/// edges then meet without a seam: north 3, east 4, south 5, west 2, the top
/// and bottom turned to meet them.
pub fn build_cube(verts: &mut Vec<super::sky::SkyVertex>) {
    use std::f32::consts::{FRAC_PI_2, PI};
    // The inside faces, wound as vanilla's end sky's.
    let faces = [
        glam::Mat3::IDENTITY,
        glam::Mat3::from_rotation_x(FRAC_PI_2),
        glam::Mat3::from_rotation_x(-FRAC_PI_2),
        glam::Mat3::from_rotation_x(PI),
        glam::Mat3::from_rotation_z(FRAC_PI_2),
        glam::Mat3::from_rotation_z(-FRAC_PI_2),
    ];
    let s = 100.0;
    let corners = [[-s, -s, -s], [-s, -s, s], [s, -s, s], [s, -s, -s]];
    for face in faces {
        let points = corners.map(|c| face * Vec3::from_array(c));
        let centre = points.iter().copied().sum::<Vec3>() / 4.0;
        for i in [0, 1, 2, 0, 2, 3] {
            let p = points[i];
            // 0..1 across the cube along each axis, and down the sides.
            let (x, y, z) = (
                (p.x + s) / (2.0 * s),
                (s - p.y) / (2.0 * s),
                (p.z + s) / (2.0 * s),
            );
            // Measured by matching the faces' shared edges in Void Rush's
            // sky textures: the bottom's u runs north to south.
            let (cell, u, v) = if centre.y < -s * 0.5 {
                (0, z, 1.0 - x)
            } else if centre.y > s * 0.5 {
                (1, z, x)
            } else if centre.z < -s * 0.5 {
                (3, x, y)
            } else if centre.x > s * 0.5 {
                (4, z, y)
            } else if centre.z > s * 0.5 {
                (5, 1.0 - x, y)
            } else {
                (2, 1.0 - z, y)
            };
            // Kept a hair inside the cell, so filtering never reaches into
            // the next one and draws a line along the cube's edges (half a
            // texel of a 256-pixel cell).
            let (u, v) = (
                EDGE_INSET + u * (1.0 - 2.0 * EDGE_INSET),
                EDGE_INSET + v * (1.0 - 2.0 * EDGE_INSET),
            );
            let (u0, v0) = ((cell % 3) as f32 / 3.0, (cell / 3) as f32 / 2.0);
            verts.push(super::sky::SkyVertex::new(
                p.to_array(),
                [u0 + u / 3.0, v0 + v / 2.0],
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY_LAYER: &str = "source=./overworld_day.png\nblend=alpha\nrotate=true\naxis=0.0 1.0 0.0\nspeed=0.05\nstartFadeIn=05:00\nendFadeIn=06:30\nendFadeOut=19:00\nweather=clear\n";

    #[test]
    fn parses_void_rushs_day_layer() {
        let l = LayerProps::parse(DAY_LAYER, "minecraft/optifine/sky/world0").unwrap();
        assert_eq!(l.source, "minecraft/optifine/sky/world0/overworld_day.png");
        assert_eq!(l.blend, Blend::Alpha);
        assert!(l.rotate);
        assert_eq!(l.speed, 0.05);
        assert_eq!(l.axis, Vec3::Y);
        // 05:00 is tick 23000, 06:30 is 500, 19:00 is 13000; the fade out
        // lasts as long as the fade in (1500 ticks).
        assert_eq!(
            l.fade,
            Some(Fade {
                start_in: 23000,
                end_in: 500,
                start_out: 11500,
                end_out: 13000
            })
        );
        assert_eq!(l.weather, [true, false, false]);
    }

    #[test]
    fn fades_in_holds_and_fades_out_across_midnight() {
        let l = LayerProps::parse(DAY_LAYER, "d").unwrap();
        assert_eq!(l.brightness(23000, 0.0, 0.0), 0.0);
        assert!((l.brightness(23750, 0.0, 0.0) - 0.5).abs() < 1e-3);
        assert_eq!(l.brightness(6000, 0.0, 0.0), 1.0);
        assert!((l.brightness(12250, 0.0, 0.0) - 0.5).abs() < 1e-3);
        assert_eq!(l.brightness(18000, 0.0, 0.0), 0.0);
        // A clear-weather layer fades with the rain.
        assert!((l.brightness(6000, 0.75, 0.0) - 0.25).abs() < 1e-3);
    }

    #[test]
    fn days_and_defaults() {
        let aurora =
            "source=./overworld_aurora.png\nblend=add\nrotate=false\ndays=1 3 6\ndaysLoop=8\n";
        let l = LayerProps::parse(aurora, "d").unwrap();
        assert_eq!(l.blend, Blend::Add);
        assert!(!l.rotate);
        assert_eq!(l.days, Some((vec![1, 3, 6], 8)));
        assert_eq!(l.brightness(24_000 + 100, 0.0, 0.0), 1.0);
        assert_eq!(l.brightness(2 * 24_000 + 100, 0.0, 0.0), 0.0);
        assert_eq!(l.brightness(9 * 24_000 + 100, 0.0, 0.0), 1.0);
        assert_eq!(l.pose(90.0), Mat4::IDENTITY);
        let end = "source=./end.png\nblend=replace\nrotate=false\nweather=clear rain thunder\n";
        let e = LayerProps::parse(end, "minecraft/optifine/sky/world1").unwrap();
        assert_eq!(e.blend, Blend::Replace);
        assert_eq!(e.brightness(15000, 1.0, 1.0), 1.0);
    }

    #[test]
    fn the_cube_has_six_faces_over_the_whole_image() {
        let mut verts = Vec::new();
        build_cube(&mut verts);
        assert_eq!(verts.len(), 36);
    }
}
