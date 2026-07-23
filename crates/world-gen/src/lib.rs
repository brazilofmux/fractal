//! The generation pipeline. Through Phase 4: elevation is shaped by
//! tectonics (spherical-Voronoi plates, mountain belts where plates
//! collide), climate follows insolation, winds and rain shadows, and
//! hydrology solves a global drainage graph whose rivers carve their
//! valleys back into the terrain — the first stage where coarse output
//! feeds forward as a hard constraint on fine synthesis. Later stages
//! (civilization, lore) slot in as further functions of (seed, position).

pub mod citymap;
pub mod civilization;
pub mod economy;
pub mod geography;
pub mod history;
pub mod hydrology;
pub mod interior;
pub mod peerage;
pub mod people;
pub mod timeline;

use std::sync::OnceLock;

use world_core::geo::lat_lon_to_unit;
use world_core::hash::{hash3, splitmix64};
use world_core::noise::{fbm, ridged};

pub use citymap::CityPlan;
pub use civilization::{river_name, Civilization, Road, Settlement, SettlementKind};
pub use economy::{Economy, Good, Lane};
pub use geography::{Geography, NaturalFeature, NaturalKind};
pub use history::{History, Plague, RealmHistory, Ruler, War, PRESENT_YEAR};
pub use hydrology::{Hydrology, RiverEdge};
pub use interior::{interior, Interior};
pub use peerage::{Holding, House, Peerage};
pub use people::{household, household_lines, people_of, person_at};
pub use timeline::{founded_in, population_in, realm_in};

/// Bump whenever generated output changes — cached tiles are keyed on this,
/// so stale caches invalidate themselves.
pub const GEN_VERSION: u32 = 18;

/// Version of the lore-facing context, kept apart from GEN_VERSION so a
/// tile-only change does not orphan the written canon. Bump this when the
/// context assembly or any generator fact feeding it changes meaning.
/// (16: honest sea lanes redrew the trade map, and with it every "buys X
/// from Y" the chronicler had been told. 17: settlements came ashore —
/// positions, and every distance and bearing derived from them, moved.
/// 18: the manor roll — ledgers rebuilt from the land up.)
pub const LORE_VERSION: u32 = 18;

// Stage tags: each pipeline stage draws from its own seed stream.
const STAGE_CONTINENTS: u64 = 0xC0_4713;
const STAGE_DETAIL: u64 = 0xDE_7A11;
const STAGE_RIDGE: u64 = 0x0F_11F7;
const STAGE_PLATES: u64 = 0x91_A7E5;
const STAGE_WARP: u64 = 0x3A_D077;
const STAGE_T_WOBBLE: u64 = 0x7E_3F01;
const STAGE_P_WOBBLE: u64 = 0x9B_1D22;

/// °C lost per unit of normalized elevation. Gentler than the physical
/// 6.9 °C/km × 9 km because our terrain carries a lot of mass at mid
/// elevations — the honest value froze half the world.
pub const LAPSE_C: f64 = 40.0;

const NUM_PLATES: usize = 14;
/// Half-width of tectonic boundary belts, in radians of arc (~700 km).
const BELT_WIDTH: f64 = 0.11;
/// Amplitude of the domain warp applied before plate lookup, so boundaries
/// wander like sutures instead of tracing clean Voronoi arcs.
const WARP_AMP: f64 = 0.075;
/// Radians of (d3 − d2) over which convergence blends between the 2nd and
/// 3rd nearest plates, keeping the uplift field continuous where they swap.
const CONV_BLEND: f64 = 0.08;

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
    hydro: OnceLock<Hydrology>,
    civ: OnceLock<Civilization>,
    hist: OnceLock<History>,
    peers: OnceLock<Peerage>,
    geo: OnceLock<Geography>,
    econ: OnceLock<Economy>,
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
        Self {
            seed,
            plates,
            hydro: OnceLock::new(),
            civ: OnceLock::new(),
            hist: OnceLock::new(),
            peers: OnceLock::new(),
            geo: OnceLock::new(),
            econ: OnceLock::new(),
        }
    }

    /// The global drainage solution, built once per planet on first use.
    /// (The build samples raw elevation and climate only, so there is no
    /// recursion through the carved `elevation`.)
    pub fn hydrology(&self) -> &Hydrology {
        self.hydro.get_or_init(|| Hydrology::build(self))
    }

    /// Settlements, roads and names, built once per planet on first use
    /// (pulls in hydrology if it hasn't been solved yet).
    pub fn civilization(&self) -> &Civilization {
        self.civ.get_or_init(|| Civilization::build(self))
    }

    /// Five hundred years of deterministic annals, built once per planet.
    pub fn history(&self) -> &History {
        self.hist.get_or_init(|| History::build(self))
    }

    /// Great houses, manors and the tenure web, built once per planet.
    pub fn peerage(&self) -> &Peerage {
        self.peers.get_or_init(|| Peerage::build(self))
    }

    /// The named natural world, built once per planet.
    pub fn geography(&self) -> &Geography {
        self.geo.get_or_init(|| Geography::build(self))
    }

    /// Production, trade flows, wealth and ledgers, built once per planet.
    pub fn economy(&self) -> &Economy {
        self.econ.get_or_init(|| Economy::build(self))
    }

    /// Tectonics at a lat/lon (radians), including the boundary-wander warp.
    pub fn tectonics_at(&self, lat: f64, lon: f64) -> Tectonics {
        self.tectonics(self.warp(lat_lon_to_unit(lat, lon)))
    }

    fn tectonics(&self, p: [f64; 3]) -> Tectonics {
        // Nearest three plate seats; with ~14 plates brute force is cheapest.
        let (mut i1, mut dot1) = (0usize, -2.0f64);
        let (mut i2, mut dot2) = (0usize, -2.0f64);
        let (mut i3, mut dot3) = (0usize, -2.0f64);
        for (i, pl) in self.plates.iter().enumerate() {
            let d = dot(p, pl.seat);
            if d > dot1 {
                (i3, dot3) = (i2, dot2);
                (i2, dot2) = (i1, dot1);
                (i1, dot1) = (i, d);
            } else if d > dot2 {
                (i3, dot3) = (i2, dot2);
                (i2, dot2) = (i, d);
            } else if d > dot3 {
                (i3, dot3) = (i, d);
            }
        }
        let d1 = dot1.clamp(-1.0, 1.0).acos();
        let d2 = dot2.clamp(-1.0, 1.0).acos();
        let d3 = dot3.clamp(-1.0, 1.0).acos();
        let edge = d2 - d1;
        let belt = (-(edge / BELT_WIDTH) * (edge / BELT_WIDTH)).exp();

        // Convergence toward the nearest boundary — but where the 2nd and
        // 3rd plates are nearly equidistant, blend their contributions.
        // Using only the 2nd would step discontinuously along the internal
        // bisector where they swap rank, drawing thousand-km cliff lines
        // through plate interiors (found as a dead-straight trench crossing
        // an ocean; it had been hiding in the terrain since Phase 2).
        let u = ((d3 - d2) / CONV_BLEND).clamp(0.0, 1.0);
        let w = 0.5 + 0.5 * smoothstep(0.0, 1.0, u);
        let convergence =
            w * self.rel_convergence(p, i1, i2) + (1.0 - w) * self.rel_convergence(p, i1, i3);

        Tectonics {
            plate: i1,
            edge,
            convergence,
            belt,
        }
    }

    /// Relative velocity of plate `a` with respect to plate `b`, projected
    /// on the tangent direction toward `b`'s seat: positive means collision.
    fn rel_convergence(&self, p: [f64; 3], a: usize, b: usize) -> f64 {
        let (pa, pb) = (self.plates[a], self.plates[b]);
        let dv = sub(cross(pa.omega, p), cross(pb.omega, p));
        let mut t = sub(pb.seat, pa.seat);
        let along = dot(t, p);
        t = sub(t, [p[0] * along, p[1] * along, p[2] * along]);
        let len = dot(t, t).sqrt();
        if len > 1e-9 {
            (dot(dv, t) / (len * 1.6)).clamp(-1.0, 1.0)
        } else {
            0.0
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
    /// This is the carved elevation — near a river the raw synthesis is
    /// pulled down toward the river's water surface, so valleys exist at
    /// every zoom exactly where the drainage graph says they do.
    pub fn elevation(&self, lat: f64, lon: f64, detail_octaves: u32) -> f64 {
        let e = self.elevation_raw(lat, lon, detail_octaves);
        self.hydrology().carve(lat_lon_to_unit(lat, lon), e)
    }

    /// Elevation as synthesized, before hydrological carving. Everything the
    /// drainage solver itself consumes must come from here.
    pub fn elevation_raw(&self, lat: f64, lon: f64, detail_octaves: u32) -> f64 {
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

pub struct Climate {
    /// Temperature at sea level, before altitude lapse. °C.
    pub sea_level_temp_c: f64,
    /// Temperature at ground elevation. °C.
    pub temp_c: f64,
    /// Annual precipitation, normalized 0 (hyperarid) .. 1 (rainforest-wet).
    pub precip: f64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Biome {
    Ocean,
    IceCap,
    Tundra,
    ColdSteppe,
    Boreal,
    Desert,
    Grassland,
    TemperateForest,
    TemperateRainforest,
    Savanna,
    TropicalForest,
    TropicalRainforest,
}

/// Whittaker-style classification from temperature and precipitation.
/// Callers decide Ocean (elevation < 0) themselves.
pub fn classify_biome(temp_c: f64, precip: f64) -> Biome {
    if temp_c < -13.0 {
        Biome::IceCap
    } else if temp_c < -4.0 {
        Biome::Tundra
    } else if temp_c < 5.0 {
        if precip < 0.22 {
            Biome::ColdSteppe
        } else {
            Biome::Boreal
        }
    } else if temp_c < 20.0 {
        match precip {
            p if p < 0.15 => Biome::Desert,
            p if p < 0.40 => Biome::Grassland,
            p if p < 0.75 => Biome::TemperateForest,
            _ => Biome::TemperateRainforest,
        }
    } else {
        match precip {
            p if p < 0.15 => Biome::Desert,
            p if p < 0.45 => Biome::Savanna,
            p if p < 0.70 => Biome::TropicalForest,
            _ => Biome::TropicalRainforest,
        }
    }
}

impl Planet {
    /// Macro-scale elevation: enough octaves for climate and hydrology to see
    /// mountains, cheap enough to sample repeatedly (e.g. upwind). Raw — the
    /// drainage solver feeds on this, so it must not depend on the solution.
    pub fn bulk_elevation(&self, lat: f64, lon: f64) -> f64 {
        self.elevation_raw(lat, lon, 4)
    }

    /// Water surface at/near this point — lake fill level or a passing
    /// river's surface — if terrain below it should flood.
    pub fn water_level(&self, lat: f64, lon: f64) -> Option<f64> {
        self.hydrology().water_level(lat_lon_to_unit(lat, lon))
    }

    /// Sea-level temperature: insolation bands plus a low-frequency wobble so
    /// climate zones waver instead of tracing perfect parallels.
    pub fn sea_level_temperature(&self, lat: f64, lon: f64) -> f64 {
        let p = lat_lon_to_unit(lat, lon);
        let s = lat.sin();
        let band = 29.0 - 44.0 * s * s - 14.0 * s.powi(8);
        band + 4.0
            * fbm(
                splitmix64(self.seed ^ STAGE_T_WOBBLE),
                [p[0] * 2.5, p[1] * 2.5, p[2] * 2.5],
                3,
                2.0,
                0.5,
            )
    }

    /// Precipitation 0..1: zonal bands (wet ITCZ, dry subtropical highs, wet
    /// storm tracks, dry poles), drier deep inside continents, and orographic
    /// effects from sampling terrain upwind along the prevailing wind —
    /// windward slopes wring out moisture, lee sides sit in rain shadow.
    fn precipitation_at(&self, lat: f64, lon: f64, e_here: f64) -> f64 {
        let p = lat_lon_to_unit(lat, lon);

        // Let the climate bands wander in latitude, like real jet streams do —
        // this is what keeps them from tracing ruler-straight parallels.
        let sp = splitmix64(self.seed ^ STAGE_P_WOBBLE);
        let drift = 6.0 * fbm(sp, [p[0] * 2.2, p[1] * 2.2, p[2] * 2.2], 3, 2.0, 0.5);
        let deg_signed = lat.to_degrees() + drift;
        let deg = deg_signed.abs();

        let mut precip = 0.15
            + 0.85 * (-(deg_signed / 13.0).powi(2)).exp()
            + 0.55 * (-((deg - 50.0) / 15.0).powi(2)).exp()
            - 0.28 * (-((deg - 25.0) / 12.0).powi(2)).exp();

        precip *= 1.0
            + 0.30
                * fbm(
                    splitmix64(sp ^ 0x51DE),
                    [p[0] * 3.0, p[1] * 3.0, p[2] * 3.0],
                    3,
                    2.0,
                    0.5,
                );

        // Continentality: the deeper into a landmass, the drier.
        let c = fbm(
            splitmix64(self.seed ^ STAGE_CONTINENTS),
            [p[0] * 1.4, p[1] * 1.4, p[2] * 1.4],
            4,
            2.0,
            0.55,
        );
        precip *= 1.0 - 0.45 * smoothstep(0.15, 0.60, c * 1.05 - 0.18);

        // Prevailing wind: trade easterlies in the tropics and polar cells,
        // westerlies between 30° and 60°. Sample the terrain the air crossed.
        let westerly = (30.0..60.0).contains(&deg);
        let dir = if westerly { -1.0 } else { 1.0 };
        let dlon = dir * (0.045 / lat.cos().abs().max(0.35)).min(0.15);
        let e_up1 = self.bulk_elevation(lat, lon + dlon).max(0.0);
        let e_up2 = self.bulk_elevation(lat, lon + 2.0 * dlon).max(0.0);
        let here = e_here.max(0.0);

        let barrier = e_up1.max(e_up2);
        let shadow =
            smoothstep(0.18, 0.50, barrier) * smoothstep(0.0, 0.15, barrier - here);
        precip *= 1.0 - 0.65 * shadow;
        precip *= 1.0 + 0.8 * smoothstep(0.04, 0.25, here - e_up1);

        precip.clamp(0.0, 1.0)
    }

    pub fn climate(&self, lat: f64, lon: f64) -> Climate {
        let e = self.bulk_elevation(lat, lon);
        let t_sea = self.sea_level_temperature(lat, lon);
        Climate {
            sea_level_temp_c: t_sea,
            temp_c: t_sea - LAPSE_C * e.max(0.0),
            precip: self.precipitation_at(lat, lon, e),
        }
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

    /// One planet per test binary — hydrology builds once, tests share it.
    fn planet() -> &'static Planet {
        static P: OnceLock<Planet> = OnceLock::new();
        P.get_or_init(|| Planet::new(42))
    }

    #[test]
    fn planet_has_land_and_ocean() {
        let planet = planet();
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
    fn climate_is_sane() {
        let planet = planet();
        let mut eq_temps = 0.0;
        let mut polar_temps = 0.0;
        for j in 0..40 {
            let lon = j as f64 / 40.0 * std::f64::consts::TAU - std::f64::consts::PI;
            let eq = planet.climate(0.0, lon);
            let po = planet.climate(1.35, lon);
            assert!((0.0..=1.0).contains(&eq.precip));
            assert!((0.0..=1.0).contains(&po.precip));
            assert!(eq.temp_c.is_finite() && po.temp_c.is_finite());
            eq_temps += eq.sea_level_temp_c;
            polar_temps += po.sea_level_temp_c;
        }
        assert!(
            eq_temps / 40.0 > polar_temps / 40.0 + 30.0,
            "equator should be much warmer than 77°N"
        );
        // Whittaker corners behave.
        assert_eq!(classify_biome(25.0, 0.9), Biome::TropicalRainforest);
        assert_eq!(classify_biome(25.0, 0.05), Biome::Desert);
        assert_eq!(classify_biome(-20.0, 0.5), Biome::IceCap);
        assert_eq!(classify_biome(12.0, 0.55), Biome::TemperateForest);
    }

    #[test]
    fn every_river_reaches_the_sea() {
        // The Phase 4 milestone, taken literally: from every land cell —
        // not just river cells — following downstream pointers must reach
        // the ocean, without cycles, along a never-ascending water surface.
        let h = planet().hydrology();
        let (down, fill, ocean) = (h.downstream(), h.fill_levels(), h.ocean_mask());
        let n = down.len();
        let mut state = vec![0u8; n]; // 0 unknown · 1 reaches sea · 2 on path
        let mut path = Vec::new();
        for start in 0..n {
            if ocean[start] || state[start] == 1 {
                continue;
            }
            let mut c = start;
            path.clear();
            loop {
                if ocean[c] || state[c] == 1 {
                    break;
                }
                assert_ne!(state[c], 2, "drainage cycle through cell {c}");
                state[c] = 2;
                path.push(c);
                let d = down[c];
                assert_ne!(d, u32::MAX, "land cell {c} drains nowhere");
                let d = d as usize;
                assert!(
                    fill[d] <= fill[c] + 1e-9,
                    "water flows uphill: {} -> {}",
                    fill[c],
                    fill[d]
                );
                c = d;
            }
            for &p in &path {
                state[p] = 1;
            }
        }
    }

    #[test]
    fn rivers_and_lakes_exist_in_sane_numbers() {
        let h = planet().hydrology();
        let rivers = h.rivers().len();
        let land = h.ocean_mask().iter().filter(|&&o| !o).count();
        assert!(
            rivers > 300 && rivers < land / 10,
            "{rivers} river edges on {land} land cells"
        );
        assert!(h.lake_cell_count() > 10, "a noise planet should pond somewhere");
        assert!(h.rivers().iter().all(|rv| (1..=6).contains(&rv.w)));
        // Subdivision refines: level k has 2^k + 1 points, prefix-consistent.
        let coarse = h.river_polyline(0, 2);
        let fine = h.river_polyline(0, 4);
        assert_eq!((coarse.len(), fine.len()), (5, 17));
        assert_eq!(coarse[2], fine[8], "deeper meander moved an existing point");
    }

    #[test]
    fn carving_never_raises_and_stays_finite() {
        let planet = planet();
        let h = planet.hydrology();
        // Walk straight across the first decently wide river's midpoint.
        let rv = h.rivers().iter().position(|r| r.w >= 3).expect("a wide river");
        let pts = h.river_polyline(rv, 2);
        let mid = pts[pts.len() / 2];
        let (lat0, lon0) = world_core::geo::unit_to_lat_lon(mid);
        for i in -40i32..=40 {
            let lat = lat0 + i as f64 * 2e-4;
            let raw = planet.elevation_raw(lat, lon0, 8);
            let carved = planet.elevation(lat, lon0, 8);
            assert!(carved.is_finite());
            assert!(carved <= raw + 1e-12, "carving raised terrain");
        }
    }

    #[test]
    fn civilization_is_plausible() {
        let planet = planet();
        let h = planet.hydrology();
        let civ = planet.civilization();

        let cities = civ
            .settlements
            .iter()
            .filter(|s| s.kind == SettlementKind::City)
            .count();
        let ports = civ.settlements.iter().filter(|s| s.port).count();
        assert!((8..=40).contains(&cities), "{cities} cities");
        assert!(
            civ.settlements.len() > cities * 5,
            "only {} settlements",
            civ.settlements.len()
        );
        assert!(ports > 0, "a seafaring-ready planet has harbors somewhere");
        assert!(!civ.roads.is_empty(), "no roads at all");

        let mut names = std::collections::HashSet::new();
        for s in &civ.settlements {
            let c = s.cell as usize;
            assert!(
                !h.ocean_mask()[c],
                "settlement {} placed in the ocean",
                s.name
            );
            // Phase 13b: the position itself stands on dry ground — no
            // port in open water, no river town inside its own channel.
            let (lat, lon) = world_core::geo::unit_to_lat_lon(s.pos);
            let e = planet.elevation(lat, lon, 8);
            assert!(e > 0.0, "{} stands below the waterline", s.name);
            assert!(
                !planet.water_level(lat, lon).is_some_and(|w| e < w - 5e-4),
                "{} stands in flood water",
                s.name
            );
            assert!(!s.name.is_empty() && names.insert(s.name.clone()), "dup/empty name");
            assert!(!s.realm.is_empty());
        }

        // Roads start and end exactly at settlement positions.
        let positions: Vec<[f64; 3]> = civ.settlements.iter().map(|s| s.pos).collect();
        for r in &civ.roads {
            assert!(r.pts.len() >= 2);
            for end in [r.pts[0], *r.pts.last().unwrap()] {
                assert!(
                    positions.iter().any(|p| *p == end),
                    "road endpoint is not a settlement"
                );
            }
        }
    }

    #[test]
    fn civilization_is_deterministic() {
        let a = Planet::new(42);
        let b = Planet::new(42);
        let (ca, cb) = (a.civilization(), b.civilization());
        assert_eq!(ca.settlements.len(), cb.settlements.len());
        assert_eq!(ca.roads.len(), cb.roads.len());
        for (x, y) in ca.settlements.iter().zip(&cb.settlements) {
            assert_eq!((x.cell, &x.name, &x.realm), (y.cell, &y.name, &y.realm));
        }
    }

    #[test]
    fn history_has_unbroken_dynasties_and_sane_lifespans() {
        let planet = planet();
        let civ = planet.civilization();
        let hist = planet.history();
        let mut reigns = Vec::new();
        for cap in civ.settlements.iter().filter(|s| s.capital) {
            let r = hist.realm(cap.cell).expect("every realm has annals");
            let mut year = r.founding_year;
            for ruler in &r.rulers {
                assert_eq!(ruler.accession, year, "gap or overlap in the succession");
                assert!(ruler.death > ruler.accession);
                assert!(ruler.death <= PRESENT_YEAR);
                reigns.push((ruler.death - ruler.accession) as f64);
                year = ruler.death.max(year + 1);
            }
            assert_eq!(
                r.rulers.last().unwrap().death,
                PRESENT_YEAR,
                "someone must hold the seat today"
            );
            assert!(!r.annals.is_empty());
            assert!(r.annals.iter().all(|a| a.year <= PRESENT_YEAR));
            assert!(r.annals.windows(2).all(|w| w[0].year <= w[1].year));
        }
        // Gompertz sanity: medieval reigns average out somewhere plausible.
        let mean = reigns.iter().sum::<f64>() / reigns.len() as f64;
        assert!(
            (8.0..40.0).contains(&mean),
            "mean reign of {mean:.1} years is not medieval"
        );
    }

    #[test]
    fn wars_are_agreed_upon_by_both_sides() {
        let planet = planet();
        let civ = planet.civilization();
        let hist = planet.history();
        let capitals: Vec<_> = civ.settlements.iter().filter(|s| s.capital).collect();
        let mut total_wars = 0;
        for a in &capitals {
            let ah = hist.realm(a.cell).unwrap();
            for b in &capitals {
                if a.cell == b.cell {
                    continue;
                }
                // War annals begin with the needle; a ruler falling in that
                // war merely mentions it mid-sentence.
                let needle = format!("war with the Realm of {} over", b.name);
                for annal in ah.annals.iter().filter(|x| x.text.starts_with(&needle)) {
                    total_wars += 1;
                    // The other side must record the same war, same year.
                    let bh = hist.realm(b.cell).unwrap();
                    let counter = format!("war with the Realm of {} over", a.name);
                    assert!(
                        bh.annals
                            .iter()
                            .any(|x| x.year == annal.year && x.text.starts_with(&counter)),
                        "the Realm of {} does not remember its year-{} war with {}",
                        b.name,
                        annal.year,
                        a.name
                    );
                }
            }
        }
        assert!(total_wars > 10, "a 500-year era with almost no wars");
    }

    #[test]
    fn history_is_deterministic() {
        let a = Planet::new(42);
        let b = Planet::new(42);
        let cap = a
            .civilization()
            .settlements
            .iter()
            .find(|s| s.capital)
            .unwrap()
            .cell;
        let (ha, hb) = (a.history().realm(cap).unwrap(), b.history().realm(cap).unwrap());
        assert_eq!(ha.annals.len(), hb.annals.len());
        for (x, y) in ha.annals.iter().zip(&hb.annals) {
            assert_eq!((x.year, &x.text), (y.year, &y.text));
        }
    }

    #[test]
    fn interiors_are_deterministic_and_scale_with_population() {
        let planet = planet();
        let civ = planet.civilization();
        let city = civ
            .settlements
            .iter()
            .position(|s| s.capital && s.port && s.population > 5000)
            .expect("a big port city");
        let village = civ
            .settlements
            .iter()
            .position(|s| s.kind == SettlementKind::Village)
            .expect("a village");

        let a = interior(planet, city);
        let b = interior(planet, city);
        assert_eq!(
            a.notables.iter().map(|n| (&n.name, &n.role, n.age)).collect::<Vec<_>>(),
            b.notables.iter().map(|n| (&n.name, &n.role, n.age)).collect::<Vec<_>>(),
            "interiors must be pure functions of the world"
        );

        let s = &civ.settlements[city];
        // Isolation's support values: one shoemaker per 150 souls (±1 marginal).
        let shoemakers = a.trades.iter().find(|t| t.name == "shoemakers").unwrap();
        let expected = s.population / 150;
        assert!(
            (shoemakers.count as i64 - expected as i64).abs() <= 1,
            "{} shoemakers for {} souls",
            shoemakers.count,
            s.population
        );
        assert!(
            a.wards.iter().any(|w| w.kind == "patriciate"),
            "a city of {} must have a patriciate ward",
            s.population
        );
        assert!(a.notables.iter().any(|n| n.role == "harbormaster"));
        assert!(a.notables.iter().any(|n| n.role == "castellan of the seat"));
        assert!(a.notables.iter().all(|n| (24..=70).contains(&n.age)));

        let v = interior(planet, village);
        assert!(v.wards.is_empty(), "villages have no wards");
        assert!(v.notables.iter().any(|n| n.role == "reeve"));
        assert!(
            v.trades.iter().map(|t| t.count).sum::<u32>()
                < a.trades.iter().map(|t| t.count).sum::<u32>(),
            "a village cannot out-trade a city"
        );
    }

    #[test]
    fn the_peerage_holds_together() {
        let planet = planet();
        let civ = planet.civilization();
        let peers = planet.peerage();

        let mut former_royals = 0;
        for s in &civ.settlements {
            if s.capital {
                assert!(peers.holding(s.cell).is_none(), "the crown holds of no one");
                let houses = peers.houses(s.cell).expect("every realm has houses");
                assert!(houses.len() >= 4);
                assert_eq!(
                    houses.iter().filter(|h| h.reigning).count(),
                    1,
                    "exactly one house reigns"
                );
                // Every house that once reigned matches the dynasty record.
                let rh = planet.history().realm(s.cell).unwrap();
                for h in houses.iter().filter(|h| h.held_seat.is_some()) {
                    assert!(
                        rh.rulers.iter().any(|r| r.house == h.name),
                        "House {} claims a seat the annals never gave it",
                        h.name
                    );
                    if !h.reigning {
                        former_royals += 1;
                    }
                }
                continue;
            }
            let hold = peers
                .holding(s.cell)
                .expect("every lesser settlement is held by someone");
            // Tenure chains terminate at the capital in at most two steps.
            let liege = hold.liege_cell;
            if liege != s.realm_capital {
                let up = peers.holding(liege).expect("liege town is itself held");
                assert_eq!(
                    up.liege_cell, s.realm_capital,
                    "a village's town liege must hold of the crown"
                );
            }
            if hold.cadet {
                let up = peers.holding(liege);
                let liege_house = up.map(|u| u.house.as_str()).unwrap_or("");
                assert!(
                    liege_house == hold.house || liege == s.realm_capital,
                    "a cadet must share the liege's house"
                );
            }
            assert!((30..=70).contains(&hold.age));
        }
        assert!(
            former_royals > 0,
            "five centuries of broken lines must leave fallen houses somewhere"
        );

        // Deterministic, like everything else.
        let other = Planet::new(42);
        let s = civ.settlements.iter().find(|s| !s.capital).unwrap();
        let (a, b) = (
            peers.holding(s.cell).unwrap(),
            other.peerage().holding(s.cell).unwrap(),
        );
        assert_eq!(
            (&a.house, &a.holder, a.age, a.liege_cell),
            (&b.house, &b.holder, b.age, b.liege_cell)
        );
    }

    #[test]
    fn geography_names_the_world() {
        let planet = planet();
        let geo = planet.geography();
        let count = |k: NaturalKind| geo.features.iter().filter(|f| f.kind == k).count();

        assert_eq!(count(NaturalKind::Ocean), 1, "one world ocean");
        assert!(count(NaturalKind::Continent) >= 1, "no continents");
        assert!(count(NaturalKind::Range) >= 3, "a tectonic world needs ranges");
        assert!(count(NaturalKind::Forest) >= 3);
        assert!(count(NaturalKind::Island) >= 3);
        assert!(count(NaturalKind::Gulf) >= 1);

        // Names are unique and axes stay near their features.
        let mut names = std::collections::HashSet::new();
        for f in &geo.features {
            assert!(names.insert(f.name.clone()), "duplicate name {}", f.name);
            assert!(f.min_zoom <= 4);
            let (a, b) = f.axis;
            let d = |p: [f64; 3], q: [f64; 3]| {
                ((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2) + (p[2] - q[2]).powi(2)).sqrt()
            };
            assert!(d(a, f.center) < 0.6 && d(b, f.center) < 0.6);
        }

        // Nearly every settlement stands on named ground.
        let civ = planet.civilization();
        let housed = civ
            .settlements
            .iter()
            .filter(|s| geo.landmass_at(s.cell).is_some())
            .count();
        assert!(
            housed * 10 >= civ.settlements.len() * 9,
            "{housed}/{} settlements on named landmasses",
            civ.settlements.len()
        );

        // Deterministic like everything else.
        let other = Planet::new(42);
        assert_eq!(geo.features.len(), other.geography().features.len());
        assert_eq!(geo.features[0].name, other.geography().features[0].name);
    }

    #[test]
    fn households_are_era_true_and_deterministic() {
        let planet = planet();
        let civ = planet.civilization();
        let (mut widows, mut lost_total, mut people_seen) = (0u32, 0u32, 0u32);

        for (i, s) in civ.settlements.iter().enumerate().step_by(7) {
            for (_, role, slot) in people_of(planet, i) {
                let head = person_at(planet, s.cell, slot).expect("listed people resolve");
                let hh = household(planet, s.cell, slot, &head);
                let hh2 = household(planet, s.cell, slot, &head);
                people_seen += 1;

                // Determinism.
                assert_eq!(
                    hh.children.iter().map(|c| &c.name).collect::<Vec<_>>(),
                    hh2.children.iter().map(|c| &c.name).collect::<Vec<_>>()
                );

                // Parents are a real generation older, or dead at adult ages.
                for p in [&hh.parents.0, &hh.parents.1] {
                    if p.alive {
                        assert!(p.age >= head.age + 20 && p.age <= head.age + 35);
                    } else {
                        assert!((21..=85).contains(&p.age));
                    }
                }

                // The cloth stays celibate; the wed stay era-plausible.
                if role.contains("priest") {
                    assert!(hh.spouse.is_none(), "a priest with a spouse");
                }
                if let Some(sp) = &hh.spouse {
                    assert!((14..=85).contains(&sp.age));
                    if !sp.alive {
                        widows += 1;
                    }
                    // Children born in wedlock, spaced like the era spaces
                    // them, none older than the marriage.
                    let mut prev: Option<u32> = None;
                    for c in &hh.children {
                        assert!(c.age < head.age.saturating_sub(12));
                        if let Some(p) = prev {
                            assert!(p >= c.age + 2, "birth interval under two years");
                        }
                        prev = Some(c.age);
                    }
                } else {
                    assert!(hh.children.is_empty(), "children out of wedlock");
                }
                lost_total += hh.lost;
            }
        }
        assert!(people_seen > 100, "sample too small: {people_seen}");
        assert!(widows > 0, "the era's mortality must widow someone");
        assert!(lost_total > 0, "23% infant mortality must show somewhere");

        // Every capital's reigning monarch resolves as a person.
        let cap = civ.settlements.iter().find(|s| s.capital).unwrap();
        let ruler = person_at(planet, cap.cell, people::SLOT_RULER).unwrap();
        assert!(ruler.role.contains("Realm of"));
        assert!(household_lines(planet, cap.cell, people::SLOT_RULER).is_some());
    }

    #[test]
    fn the_economy_flows_and_balances() {
        let planet = planet();
        let civ = planet.civilization();
        let econ = planet.economy();

        // Everyone makes something; ports make salt; imports come from
        // real producers and only for goods not made at home.
        for (i, s) in civ.settlements.iter().enumerate() {
            assert!(!econ.produces[i].is_empty(), "{} makes nothing", s.name);
            if s.port {
                assert!(econ.produces[i].contains(&Good::Salt));
            }
            for &(good, src) in &econ.imports[i] {
                assert!(
                    econ.produces[src].contains(&good),
                    "{} imports {} from a non-producer",
                    s.name,
                    good.word()
                );
                assert!(
                    !econ.produces[i].contains(&good),
                    "{} imports what it already makes",
                    s.name
                );
            }
        }

        // Trade actually moves: roads carry goods, lanes exist between
        // ports, and every wealth class is represented somewhere.
        assert!(
            econ.road_flows.iter().filter(|f| !f.is_empty()).count() > 50,
            "the roads are empty"
        );
        assert!(!econ.lanes.is_empty(), "a world of ports without shipping");
        let h = planet.hydrology();
        let polyline_len = |pts: &[[f64; 3]]| -> f64 {
            pts.windows(2)
                .map(|w| {
                    let d = [w[0][0] - w[1][0], w[0][1] - w[1][1], w[0][2] - w[1][2]];
                    (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
                })
                .sum()
        };
        let mut rounds_a_cape = false;
        for l in &econ.lanes {
            assert!(civ.settlements[l.a].port && civ.settlements[l.b].port);
            // A lane is a voyage: every point clear of the harbor mouths
            // must be over ocean — no lane crosses a landmass.
            for &p in &l.pts[3..l.pts.len().saturating_sub(3)] {
                let c = h.grid.point_to_cell(p) as usize;
                assert!(
                    h.ocean_mask()[c],
                    "the lane {} ↔ {} sails over dry land",
                    civ.settlements[l.a].name,
                    civ.settlements[l.b].name
                );
            }
            let straight = {
                let (a, b) = (civ.settlements[l.a].pos, civ.settlements[l.b].pos);
                let d = [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
                (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
            };
            if polyline_len(&l.pts) > straight * 1.25 {
                rounds_a_cape = true;
            }
        }
        assert!(
            rounds_a_cape,
            "no lane deviates from the chord — routing is doing nothing"
        );
        for class in 1..=5u8 {
            assert!(
                econ.wealth.iter().any(|&w| w == class),
                "no settlement is {}",
                Economy::wealth_word(class)
            );
        }

        // The manor roll: every settlement's land yields something, dues
        // are exactly the third penny of what reaches each holding, and
        // every mark in a crown's ledger traces to the manor that paid it.
        let peers = planet.peerage();
        for (i, s) in civ.settlements.iter().enumerate() {
            assert!(econ.manor_income[i] > 0, "{} yields nothing", s.name);
            if s.capital {
                assert_eq!(econ.manor_sends[i], 0, "the crown pays no one");
                assert_eq!(
                    econ.realm_ledger[&s.cell],
                    econ.manor_income[i] + econ.manor_receives[i],
                    "{}'s ledger is not demesne plus dues",
                    s.name
                );
            } else {
                assert_eq!(
                    econ.manor_sends[i],
                    (econ.manor_income[i] + econ.manor_receives[i]) / 3,
                    "{} pays other than the third penny",
                    s.name
                );
            }
            // What arrives is exactly what the vassals sent.
            let sent_here: u64 = civ
                .settlements
                .iter()
                .enumerate()
                .filter(|(_, t)| {
                    peers
                        .holding(t.cell)
                        .is_some_and(|h| h.liege_cell == s.cell)
                })
                .map(|(j, _)| econ.manor_sends[j])
                .sum();
            assert_eq!(econ.manor_receives[i], sent_here, "dues leak at {}", s.name);
        }
        // Conservation to the last penny: everything the land yields is
        // either kept at some rung or sits in a crown's ledger.
        let yielded: u64 = econ.manor_income.iter().sum();
        let kept: u64 = (0..civ.settlements.len())
            .filter(|&i| !civ.settlements[i].capital)
            .map(|i| econ.manor_income[i] + econ.manor_receives[i] - econ.manor_sends[i])
            .sum();
        let crowns: u64 = econ.realm_ledger.values().sum();
        assert_eq!(yielded, kept + crowns, "marks leaked in the climb");
        assert!(econ.realm_ledger.values().all(|&v| v > 0));
        // Somewhere a town lord is collecting dues — the web carries money.
        assert!(
            (0..civ.settlements.len())
                .any(|i| !civ.settlements[i].capital && econ.manor_receives[i] > 0),
            "no lord in the world collects a due"
        );

        // Deterministic.
        let other = Planet::new(42);
        assert_eq!(econ.wealth, other.economy().wealth);
    }

    #[test]
    fn street_level_holds_together() {
        use citymap::GroundKind;
        let planet = planet();
        let civ = planet.civilization();
        let dist = |a: [f64; 3], b: [f64; 3]| {
            ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
        };

        for (i, s) in civ.settlements.iter().enumerate().step_by(5) {
            let plan = citymap::plan(planet, i);
            assert_eq!(plan.settlement, i);
            assert!(plan.radius > 0.0 && plan.radius <= citymap::MAX_RADIUS);
            assert_eq!(plan.wall.is_some(), s.population > 900, "{}", s.name);
            // Every interior ward has a seat; even a hamlet can be walked.
            let inside = interior(planet, i);
            if inside.wards.is_empty() {
                assert!(plan.wards.len() >= 2, "{} cannot be walked", s.name);
            } else {
                assert_eq!(plan.wards.len(), inside.wards.len());
            }
            for w in &plan.wards {
                assert!(dist(w.pos, plan.center) <= plan.radius * 1.001);
            }
            if let Some(wall_r) = plan.wall {
                assert!(!plan.gates.is_empty(), "{} is walled without gates", s.name);
                for g in &plan.gates {
                    assert!((dist(g.pos, plan.center) - wall_r).abs() < wall_r * 0.02);
                }
            }
            // Rooms are one walkable component: everywhere reaches everywhere.
            let rooms = citymap::rooms(&plan);
            let mut seen = vec![false; rooms.len()];
            let mut stack = vec![0usize];
            seen[0] = true;
            while let Some(c) = stack.pop() {
                for e in citymap::exits(&plan, rooms[c].k) {
                    let j = rooms.iter().position(|r| r.k == e).expect("exit resolves");
                    if !seen[j] {
                        seen[j] = true;
                        stack.push(j);
                    }
                }
            }
            assert!(
                seen.iter().all(|&v| v),
                "{} has rooms no street reaches",
                s.name
            );
            // The town is where the town is — on dry ground.
            assert!(plan.ground_at(plan.center, 0.1).is_some());
            {
                let (lat, lon) = world_core::geo::unit_to_lat_lon(plan.center);
                assert!(
                    planet.elevation(lat, lon, 8) > 0.0,
                    "{} anchors underwater",
                    s.name
                );
            }

            // Deterministic, like everything else.
            let again = citymap::plan(planet, i);
            for (a, b) in plan.wards.iter().zip(&again.wards) {
                assert_eq!((a.name.clone(), a.pos), (b.name.clone(), b.pos));
            }
        }

        // Classify a grid over the biggest city: all the anatomy shows.
        let (big, s) = civ
            .settlements
            .iter()
            .enumerate()
            .max_by_key(|(_, s)| s.population)
            .expect("settlements exist");
        let plan = citymap::plan(planet, big);
        let (lat0, lon0) = world_core::geo::unit_to_lat_lon(plan.center);
        let r = plan.radius * 1.2;
        let mut counts = std::collections::HashMap::new();
        let mut outside = 0;
        for j in -40i32..=40 {
            for i in -40i32..=40 {
                let lat = lat0 + j as f64 * r / 40.0;
                let lon = lon0 + i as f64 * r / 40.0 / lat0.cos();
                let p = lat_lon_to_unit(lat, lon);
                match plan.ground_at(p, 0.1) {
                    Some(g) => *counts.entry(g.kind).or_insert(0u32) += 1,
                    None => outside += 1,
                }
            }
        }
        for kind in [
            GroundKind::Plot,
            GroundKind::Street,
            GroundKind::Wall,
            GroundKind::Square,
        ] {
            assert!(
                counts.get(&kind).copied().unwrap_or(0) > 0,
                "the biggest city has no {kind:?}"
            );
        }
        assert!(
            counts[&GroundKind::Plot] > counts[&GroundKind::Street],
            "more street than city"
        );
        assert!(outside > 0, "the city must end somewhere");

        // The milestone, taken literally: in some port town, walk from the
        // harborside to the inn without leaving the street graph.
        let mut walked = false;
        for (i, _) in civ.settlements.iter().enumerate() {
            let plan = citymap::plan(planet, i);
            let Some(quay) = plan.wards.iter().position(|w| w.kind == "harborside") else {
                continue;
            };
            let Some((_, _)) = &plan.inn else { continue };
            let inn_k = plan.wards.len();
            // BFS from the quay to the inn's door.
            let rooms = citymap::rooms(&plan);
            let mut seen = vec![false; rooms.len()];
            let mut queue = std::collections::VecDeque::from([quay]);
            seen[quay] = true;
            while let Some(c) = queue.pop_front() {
                if rooms[c].k == inn_k {
                    walked = true;
                    break;
                }
                for e in citymap::exits(&plan, rooms[c].k) {
                    let j = rooms.iter().position(|r| r.k == e).unwrap();
                    if !seen[j] {
                        seen[j] = true;
                        queue.push_back(j);
                    }
                }
            }
            if walked {
                break;
            }
        }
        assert!(walked, "no port town can walk from its quay to its inn");
    }

    #[test]
    fn the_fourth_coordinate_holds() {
        let planet = planet();
        let civ = planet.civilization();
        let hist = planet.history();

        // The present is canon: at year 500 every function of the year
        // agrees exactly with what the other phases already established.
        for (i, s) in civ.settlements.iter().enumerate() {
            let f = founded_in(planet, i);
            assert!((20..=470).contains(&f), "{} founded in year {f}", s.name);
            assert_eq!(realm_in(planet, i, PRESENT_YEAR), Some(s.realm_capital));
            assert_eq!(population_in(planet, i, PRESENT_YEAR), s.population);
            assert_eq!(realm_in(planet, i, f - 1), None, "held before it existed");
            assert_eq!(population_in(planet, i, f - 1), 0);
            assert!(
                population_in(planet, i, f) < s.population,
                "{} must have grown since its founding",
                s.name
            );
        }

        // Allegiance changes hands somewhere, and only ever at the end of
        // a decisive war between exactly the two realms involved.
        let mut moved = 0usize;
        for (i, _) in civ.settlements.iter().enumerate() {
            let mut prev: Option<u32> = None;
            for year in founded_in(planet, i)..=PRESENT_YEAR {
                let now = realm_in(planet, i, year).expect("exists from founding");
                if let Some(p) = prev {
                    if p != now {
                        moved += 1;
                        assert!(
                            hist.wars.iter().any(|w| w.end == year
                                && w.victor == Some(now)
                                && ((w.a == p && w.b == now) || (w.a == now && w.b == p))),
                            "allegiance moved in year {year} without a war to move it"
                        );
                    }
                }
                prev = Some(now);
            }
        }
        assert!(moved > 0, "five centuries of war moved no borders at all");

        // Every year from the founding has exactly one ruler on the seat,
        // and it is the one whose reign spans that year.
        for cap in civ.settlements.iter().filter(|s| s.capital) {
            let rh = hist.realm(cap.cell).unwrap();
            assert!(hist.ruler_in(cap.cell, rh.founding_year - 1).is_none());
            for year in rh.founding_year..=PRESENT_YEAR {
                let r = hist.ruler_in(cap.cell, year).expect("someone holds the seat");
                assert!(r.accession <= year && year <= r.death);
            }
            assert_eq!(
                hist.ruler_in(cap.cell, PRESENT_YEAR).unwrap().name,
                hist.current_ruler(cap.cell).unwrap().name
            );
        }

        // Plagues bite: crossing an arrival year drops a capital's count.
        let mut dipped = false;
        for (i, s) in civ.settlements.iter().enumerate().filter(|(_, s)| s.capital) {
            let rh = hist.realm(s.realm_capital).unwrap();
            for p in &rh.plagues {
                if p.arrival > founded_in(planet, i) + 1
                    && population_in(planet, i, p.arrival) < population_in(planet, i, p.arrival - 1)
                {
                    dipped = true;
                }
            }
        }
        assert!(dipped, "two great plagues must show in somebody's numbers");

        // Deterministic, like everything else.
        let other = Planet::new(42);
        for i in (0..civ.settlements.len()).step_by(11) {
            assert_eq!(realm_in(planet, i, 250), realm_in(&other, i, 250));
            assert_eq!(population_in(planet, i, 250), population_in(&other, i, 250));
        }
    }

    #[test]
    fn tectonics_is_sane() {
        let planet = planet();
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

