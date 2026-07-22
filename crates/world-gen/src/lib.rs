//! The generation pipeline. Phase 0 ships elevation only: a continental base,
//! ridged mountain potential (a stand-in for tectonic uplift until Phase 2),
//! and zoom-scaled fBm detail. Later stages (climate, hydrology, biomes,
//! civilization) slot in here as further functions of (seed, position).

use world_core::geo::lat_lon_to_unit;
use world_core::hash::splitmix64;
use world_core::noise::{fbm, ridged};

/// Bump whenever generated output changes — cached tiles are keyed on this,
/// so stale caches invalidate themselves.
pub const GEN_VERSION: u32 = 3;

// Stage tags: each pipeline stage draws from its own seed stream.
const STAGE_CONTINENTS: u64 = 0xC0_4713;
const STAGE_UPLIFT: u64 = 0x0F_11F7;
const STAGE_DETAIL: u64 = 0xDE_7A11;

pub struct Planet {
    pub seed: u64,
}

impl Planet {
    pub fn new(seed: u64) -> Self {
        Self { seed }
    }

    /// Normalized elevation at a point: negative is below sea level, positive
    /// above, roughly [-1, 1]. `detail_octaves` scales synthesis depth to the
    /// zoom level being rendered so detail keeps arriving as you descend.
    pub fn elevation(&self, lat: f64, lon: f64, detail_octaves: u32) -> f64 {
        let p = lat_lon_to_unit(lat, lon);

        // Where the landmasses are. Low frequency, few octaves.
        let c = fbm(
            splitmix64(self.seed ^ STAGE_CONTINENTS),
            [p[0] * 1.4, p[1] * 1.4, p[2] * 1.4],
            4,
            2.0,
            0.55,
        );
        // Bias toward ocean: Earth-like ~35-40% land.
        let base = c * 1.05 - 0.18;

        // Mountains belong on land; fade them in past the coast.
        let land_mask = smoothstep(-0.02, 0.18, base);
        let uplift = ridged(
            splitmix64(self.seed ^ STAGE_UPLIFT),
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

        base + uplift * uplift * 0.45 * land_mask + detail * (0.16 + 0.16 * land_mask)
    }
}

#[inline]
fn smoothstep(e0: f64, e1: f64, x: f64) -> f64 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}
