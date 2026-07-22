//! The generation pipeline. Phase 2: elevation is shaped by tectonics —
//! spherical-Voronoi plates with Euler-pole motion, boundaries classified by
//! relative velocity, and mountain belts that exist where plates collide
//! rather than wherever ridged noise felt like putting them. Later stages
//! (climate, hydrology, biomes, civilization) slot in as further functions of
//! (seed, position).

use world_core::geo::lat_lon_to_unit;
use world_core::hash::{hash3, splitmix64};
use world_core::noise::{fbm, ridged};

/// Bump whenever generated output changes — cached tiles are keyed on this,
/// so stale caches invalidate themselves.
pub const GEN_VERSION: u32 = 4;

// Stage tags: each pipeline stage draws from its own seed stream.
const STAGE_CONTINENTS: u64 = 0xC0_4713;
const STAGE_DETAIL: u64 = 0xDE_7A11;
const STAGE_RIDGE: u64 = 0x0F_11F7;
const STAGE_PLATES: u64 = 0x91_A7E5;
const STAGE_WARP: u64 = 0x3A_D077;

const NUM_PLATES: usize = 14;
/// Half-width of tectonic boundary belts, in radians of arc (~700 km).
const BELT_WIDTH: f64 = 0.11;
/// Amplitude of the domain warp applied before plate lookup, so boundaries
/// wander like sutures instead of tracing clean Voronoi arcs.
const WARP_AMP: f64 = 0.075;

#[derive(Clone, Copy)]
pub struct Plate {
    /// Seed point of the plate's Voronoi cell, on the unit sphere.
    pub seat: [f64; 3],
    /// Euler rotation vector: axis × angular speed. Velocity at p is ω × p.
    pub omega: [f64; 3],
}

/// Tectonic situation at a point.
pub struct Tectonics {
    pub plate: usize,
    /// Voronoi margin (d2 − d1) in radians: 0 on a boundary, grows inward.
    pub edge: f64,
    /// Normalized relative motion at the nearest boundary: >0 collision,
    /// <0 rift, ~0 transform.
    pub convergence: f64,
    /// Gaussian belt factor: 1 on the boundary → 0 in the plate interior.
    pub belt: f64,
}

pub struct Planet {
    pub seed: u64,
    plates: Vec<Plate>,
}

impl Planet {
    pub fn new(seed: u64) -> Self {
        let s = splitmix64(seed ^ STAGE_PLATES);
        let plates = (0..NUM_PLATES)
            .map(|k| {
                let k = k as i64;
                let seat = unit_from_hashes(hash3(s, k, 0, 0), hash3(s, k, 1, 0));
                let axis = unit_from_hashes(hash3(s, k, 2, 0), hash3(s, k, 3, 0));
                let speed = 0.35 + 0.65 * unit_f64(hash3(s, k, 4, 0));
                Plate {
                    seat,
                    omega: [axis[0] * speed, axis[1] * speed, axis[2] * speed],
                }
            })
            .collect();
        Self { seed, plates }
    }

    /// Tectonics at a lat/lon (radians), including the boundary-wander warp.
    pub fn tectonics_at(&self, lat: f64, lon: f64) -> Tectonics {
        self.tectonics(self.warp(lat_lon_to_unit(lat, lon)))
    }

    fn tectonics(&self, p: [f64; 3]) -> Tectonics {
        // Nearest two plate seats; with ~14 plates brute force is cheapest.
        let (mut i1, mut dot1) = (0usize, -2.0f64);
        let (mut i2, mut dot2) = (0usize, -2.0f64);
        for (i, pl) in self.plates.iter().enumerate() {
            let d = dot(p, pl.seat);
            if d > dot1 {
                (i2, dot2) = (i1, dot1);
                (i1, dot1) = (i, d);
            } else if d > dot2 {
                (i2, dot2) = (i, d);
            }
        }
        let d1 = dot1.clamp(-1.0, 1.0).acos();
        let d2 = dot2.clamp(-1.0, 1.0).acos();
        let edge = d2 - d1;
        let belt = (-(edge / BELT_WIDTH) * (edge / BELT_WIDTH)).exp();

        // Relative velocity of plate 1 with respect to plate 2, projected on
        // the tangent direction toward plate 2: positive means collision.
        let (p1, p2) = (self.plates[i1], self.plates[i2]);
        let dv = sub(cross(p1.omega, p), cross(p2.omega, p));
        let mut t = sub(p2.seat, p1.seat);
        let along = dot(t, p);
        t = sub(t, [p[0] * along, p[1] * along, p[2] * along]);
        let len = dot(t, t).sqrt();
        let convergence = if len > 1e-9 {
            (dot(dv, t) / (len * 1.6)).clamp(-1.0, 1.0)
        } else {
            0.0
        };

        Tectonics {
            plate: i1,
            edge,
            convergence,
            belt,
        }
    }

    /// Domain warp for plate lookup: boundaries meander at continental scale.
    fn warp(&self, p: [f64; 3]) -> [f64; 3] {
        let s = splitmix64(self.seed ^ STAGE_WARP);
        let mut q = [0.0f64; 3];
        for (i, qi) in q.iter_mut().enumerate() {
            let sw = splitmix64(s ^ (i as u64 + 1));
            *qi = p[i] + WARP_AMP * fbm(sw, [p[0] * 2.0, p[1] * 2.0, p[2] * 2.0], 4, 2.0, 0.5);
        }
        let inv = 1.0 / dot(q, q).sqrt();
        [q[0] * inv, q[1] * inv, q[2] * inv]
    }

    /// Normalized elevation at a point: negative is below sea level, positive
    /// above, roughly [-1, 1]. `detail_octaves` scales synthesis depth to the
    /// zoom level being rendered so detail keeps arriving as you descend.
    pub fn elevation(&self, lat: f64, lon: f64, detail_octaves: u32) -> f64 {
        let p = lat_lon_to_unit(lat, lon);
        let tect = self.tectonics(self.warp(p));

        // Where the landmasses are. Low frequency, few octaves. (Continents
        // ride on plates but are not plates — Earth gets this right too.)
        let c = fbm(
            splitmix64(self.seed ^ STAGE_CONTINENTS),
            [p[0] * 1.4, p[1] * 1.4, p[2] * 1.4],
            4,
            2.0,
            0.55,
        );
        // Bias toward ocean: Earth-like ~35-40% land.
        let base = c * 1.05 - 0.18;
        let land_mask = smoothstep(-0.02, 0.18, base);

        // Uplift from the boundary: collisions raise belts (orogeny on land,
        // island arcs at sea); rifts sink them, more weakly.
        let uplift = tect.belt
            * if tect.convergence >= 0.0 {
                tect.convergence
            } else {
                0.35 * tect.convergence
            };

        // Ridged relief carries the uplift; without uplift it stays subdued.
        let mountain = ridged(
            splitmix64(self.seed ^ STAGE_RIDGE),
            [p[0] * 2.3, p[1] * 2.3, p[2] * 2.3],
            5,
            2.0,
            0.5,
        );

        // Fine terrain detail, deepening with zoom. Starts near continental
        // scale so coastlines stay fractal instead of going smooth at mid-zoom.
        let detail = fbm(
            splitmix64(self.seed ^ STAGE_DETAIL),
            [p[0] * 3.0, p[1] * 3.0, p[2] * 3.0],
            detail_octaves,
            2.0,
            0.55,
        );

        base
            + 0.42 * mountain * mountain * uplift.max(0.0).powf(0.7)
            + 0.16 * uplift
            + detail * (0.16 + 0.16 * land_mask)
    }
}

#[inline]
fn smoothstep(e0: f64, e1: f64, x: f64) -> f64 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[inline]
fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

#[inline]
fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

#[inline]
fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

#[inline]
fn unit_f64(h: u64) -> f64 {
    (h >> 11) as f64 / (1u64 << 53) as f64
}

/// Uniform point on the unit sphere from two hashes.
fn unit_from_hashes(h1: u64, h2: u64) -> [f64; 3] {
    let z = 2.0 * unit_f64(h1) - 1.0;
    let phi = std::f64::consts::TAU * unit_f64(h2);
    let r = (1.0 - z * z).max(0.0).sqrt();
    [r * phi.cos(), r * phi.sin(), z]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn planet_has_land_and_ocean() {
        let planet = Planet::new(42);
        let (mut land, mut ocean) = (0, 0);
        for i in 0..40 {
            for j in 0..80 {
                let lat = (i as f64 / 40.0 - 0.5) * 2.8; // avoid exact poles
                let lon = (j as f64 / 80.0 - 0.5) * std::f64::consts::TAU;
                let e = planet.elevation(lat, lon, 8);
                assert!(e.is_finite() && e.abs() < 3.0, "wild elevation {e}");
                if e > 0.0 {
                    land += 1;
                } else {
                    ocean += 1;
                }
            }
        }
        let frac = land as f64 / (land + ocean) as f64;
        assert!(
            (0.15..0.60).contains(&frac),
            "land fraction {frac} outside plausible range"
        );
    }

    #[test]
    fn tectonics_is_sane() {
        let planet = Planet::new(42);
        for i in 0..500 {
            let lat = (i as f64 / 500.0 - 0.5) * 3.0;
            let lon = (i as f64 * 0.618).rem_euclid(1.0) * std::f64::consts::TAU
                - std::f64::consts::PI;
            let t = planet.tectonics_at(lat, lon);
            assert!(t.plate < NUM_PLATES);
            assert!(t.edge >= 0.0, "voronoi margin must be non-negative");
            assert!((-1.0..=1.0).contains(&t.convergence));
            assert!((0.0..=1.0).contains(&t.belt));
        }
    }
}
