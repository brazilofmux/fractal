//! Gradient noise over R^3, evaluated at points on the unit sphere so the
//! result is seamless and pole-free by construction. All math in f64.

use crate::hash::{hash3, splitmix64};

const GRADS: [[f64; 3]; 16] = [
    [1.0, 1.0, 0.0],
    [-1.0, 1.0, 0.0],
    [1.0, -1.0, 0.0],
    [-1.0, -1.0, 0.0],
    [1.0, 0.0, 1.0],
    [-1.0, 0.0, 1.0],
    [1.0, 0.0, -1.0],
    [-1.0, 0.0, -1.0],
    [0.0, 1.0, 1.0],
    [0.0, -1.0, 1.0],
    [0.0, 1.0, -1.0],
    [0.0, -1.0, -1.0],
    [1.0, 1.0, 0.0],
    [-1.0, 1.0, 0.0],
    [0.0, -1.0, 1.0],
    [0.0, -1.0, -1.0],
];

#[inline]
fn fade(t: f64) -> f64 {
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

#[inline]
fn grad_dot(seed: u64, ix: i64, iy: i64, iz: i64, dx: f64, dy: f64, dz: f64) -> f64 {
    let g = GRADS[(hash3(seed, ix, iy, iz) & 15) as usize];
    g[0] * dx + g[1] * dy + g[2] * dz
}

#[inline]
fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + t * (b - a)
}

/// Classic Perlin gradient noise, roughly in [-1, 1].
pub fn perlin3(seed: u64, p: [f64; 3]) -> f64 {
    let (x0, y0, z0) = (p[0].floor(), p[1].floor(), p[2].floor());
    let (ix, iy, iz) = (x0 as i64, y0 as i64, z0 as i64);
    let (fx, fy, fz) = (p[0] - x0, p[1] - y0, p[2] - z0);
    let (u, v, w) = (fade(fx), fade(fy), fade(fz));

    let mut n = [0.0f64; 8];
    for (c, slot) in n.iter_mut().enumerate() {
        let (cx, cy, cz) = ((c & 1) as i64, ((c >> 1) & 1) as i64, ((c >> 2) & 1) as i64);
        *slot = grad_dot(
            seed,
            ix + cx,
            iy + cy,
            iz + cz,
            fx - cx as f64,
            fy - cy as f64,
            fz - cz as f64,
        );
    }

    let nx0 = lerp(n[0], n[1], u);
    let nx1 = lerp(n[2], n[3], u);
    let nx2 = lerp(n[4], n[5], u);
    let nx3 = lerp(n[6], n[7], u);
    lerp(lerp(nx0, nx1, v), lerp(nx2, nx3, v), w) * 1.15
}

/// Fractal Brownian motion: octaves of perlin3, each with its own derived seed.
/// Normalized to roughly [-1, 1].
pub fn fbm(seed: u64, p: [f64; 3], octaves: u32, lacunarity: f64, gain: f64) -> f64 {
    let mut freq = 1.0;
    let mut amp = 1.0;
    let mut sum = 0.0;
    let mut norm = 0.0;
    for i in 0..octaves {
        let s = splitmix64(seed ^ (i as u64).wrapping_mul(0xA24B_AED4_963E_E407));
        sum += amp * perlin3(s, [p[0] * freq, p[1] * freq, p[2] * freq]);
        norm += amp;
        freq *= lacunarity;
        amp *= gain;
    }
    sum / norm
}

/// Ridged multifractal in [0, 1] — sharp crests, stands in for orogeny until
/// real tectonic uplift arrives in Phase 2.
pub fn ridged(seed: u64, p: [f64; 3], octaves: u32, lacunarity: f64, gain: f64) -> f64 {
    let mut freq = 1.0;
    let mut amp = 1.0;
    let mut sum = 0.0;
    let mut norm = 0.0;
    for i in 0..octaves {
        let s = splitmix64(seed ^ (i as u64).wrapping_mul(0x9FB2_1C65_1E98_DF25));
        let v = perlin3(s, [p[0] * freq, p[1] * freq, p[2] * freq]);
        sum += amp * (1.0 - v.abs().min(1.0));
        norm += amp;
        freq *= lacunarity;
        amp *= gain;
    }
    sum / norm
}
