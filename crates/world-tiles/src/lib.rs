//! Raster tile rendering: sample the planet into an elevation grid (with a
//! one-pixel border so gradients never need a neighboring tile), hillshade,
//! hypsometric-tint, encode PNG. Rows render in parallel.

use image::ImageEncoder;
use rayon::prelude::*;
use world_core::geo::tile_pixel_to_lat_lon;
use world_gen::{classify_biome, Biome, Planet, LAPSE_C};

pub const TILE_SIZE: usize = 256;
const BORDER: usize = 1;
const GRID: usize = TILE_SIZE + 2 * BORDER;

/// Climate is sampled every this many pixels and bilinearly interpolated —
/// its features span kilometers, so per-pixel evaluation would be waste.
const CLIMATE_SPACING: usize = 4;

/// Bilinearly interpolated per-tile field of sea-level temperature and
/// precipitation. Temperature gets its altitude lapse applied per pixel from
/// the full-detail elevation, so biome edges follow real terrain.
struct ClimateGrid {
    n: usize,
    t_sea: Vec<f64>,
    precip: Vec<f64>,
}

fn build_climate_grid(planet: &Planet, z: u32, x: u32, y: u32) -> ClimateGrid {
    let n = TILE_SIZE / CLIMATE_SPACING + 1;
    let mut t_sea = vec![0.0f64; n * n];
    let mut precip = vec![0.0f64; n * n];
    t_sea
        .par_chunks_mut(n)
        .zip(precip.par_chunks_mut(n))
        .enumerate()
        .for_each(|(j, (tr, pr))| {
            for i in 0..n {
                let (lat, lon) = tile_pixel_to_lat_lon(
                    z,
                    x,
                    y,
                    (i * CLIMATE_SPACING) as f64,
                    (j * CLIMATE_SPACING) as f64,
                    TILE_SIZE as f64,
                );
                let cl = planet.climate(lat, lon);
                tr[i] = cl.sea_level_temp_c;
                pr[i] = cl.precip;
            }
        });
    ClimateGrid { n, t_sea, precip }
}

impl ClimateGrid {
    fn sample(&self, px: f64, py: f64) -> (f64, f64) {
        let max = (self.n - 1) as f64 - 1e-9;
        let gx = (px / CLIMATE_SPACING as f64).clamp(0.0, max);
        let gy = (py / CLIMATE_SPACING as f64).clamp(0.0, max);
        let (i, j) = (gx.floor() as usize, gy.floor() as usize);
        let (fx, fy) = (gx - i as f64, gy - j as f64);
        let idx = |jj: usize, ii: usize| jj * self.n + ii;
        let bil = |f: &[f64]| {
            let top = f[idx(j, i)] * (1.0 - fx) + f[idx(j, i + 1)] * fx;
            let bot = f[idx(j + 1, i)] * (1.0 - fx) + f[idx(j + 1, i + 1)] * fx;
            top * (1.0 - fy) + bot * fy
        };
        (bil(&self.t_sea), bil(&self.precip))
    }
}

/// Slope steepness for shading, in normalized-elevation units per pixel.
/// Map-style rather than physically metric: per-pixel gradients of fBm are
/// nearly scale-invariant, so one constant reads well at every zoom.
const SHADE_STRENGTH: f64 = 220.0;

pub fn render_elevation_tile(planet: &Planet, z: u32, x: u32, y: u32) -> Vec<u8> {
    // More synthesis depth as you zoom; capped well below f64's limits.
    let octaves = (z + 6).min(22);

    let climate = build_climate_grid(planet, z, x, y);

    // Elevation grid including a 1-px border. Border samples land outside this
    // tile, but positional determinism makes them bit-identical to the
    // neighboring tile's own samples — so shading is seam-free by construction.
    let mut elev = vec![0.0f64; GRID * GRID];
    elev.par_chunks_mut(GRID).enumerate().for_each(|(row, buf)| {
        for col in 0..GRID {
            let px = col as f64 - BORDER as f64 + 0.5;
            let py = row as f64 - BORDER as f64 + 0.5;
            let (lat, lon) = tile_pixel_to_lat_lon(z, x, y, px, py, TILE_SIZE as f64);
            buf[col] = planet.elevation(lat, lon, octaves);
        }
    });

    // Light from the northwest, 45° above the horizon. With fBm gain < 1/lacunarity,
    // per-pixel roughness shrinks ~0.55× per zoom level, so shading strength ramps
    // past z4 to keep relief legible at depth (net ~0.8×/zoom, a gentle mellowing).
    let light = light_vector(315.0, 45.0);
    let strength = SHADE_STRENGTH * 1.5f64.powi((z as i32 - 4).max(0));

    let mut pixels = vec![0u8; TILE_SIZE * TILE_SIZE * 3];
    pixels
        .par_chunks_mut(TILE_SIZE * 3)
        .enumerate()
        .for_each(|(row, buf)| {
            let g = |r: usize, c: usize| elev[r * GRID + c];
            for col in 0..TILE_SIZE {
                let (r, c) = (row + BORDER, col + BORDER);
                let e = g(r, c);

                let dzdx = (g(r, c + 1) - g(r, c - 1)) * strength / 2.0;
                let dzdy = (g(r + 1, c) - g(r - 1, c)) * strength / 2.0;
                let inv_len = 1.0 / (dzdx * dzdx + dzdy * dzdy + 1.0).sqrt();
                let diffuse = ((-dzdx * light[0] - dzdy * light[1] + light[2]) * inv_len).max(0.0);

                // Full relief on land, muted on the seafloor.
                let intensity = if e <= 0.0 {
                    0.75 + 0.25 * diffuse
                } else {
                    0.40 + 0.60 * diffuse
                };

                let (t_sea, precip) = climate.sample(col as f64 + 0.5, row as f64 + 0.5);
                let tint = surface_color(e, t_sea, precip);
                let out = &mut buf[col * 3..col * 3 + 3];
                for ch in 0..3 {
                    out[ch] = (tint[ch] as f64 * intensity).round().min(255.0) as u8;
                }
            }
        });

    encode_png(&pixels)
}

fn encode_png(pixels: &[u8]) -> Vec<u8> {
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(
            pixels,
            TILE_SIZE as u32,
            TILE_SIZE as u32,
            image::ExtendedColorType::Rgb8,
        )
        .expect("png encode");
    png
}

/// Debug layer: plate mosaic. Each plate gets a stable pastel; boundaries
/// darken, tinted red where convergent and blue where divergent — a direct
/// visual check that mountain belts sit on collisions.
pub fn render_plates_tile(planet: &Planet, z: u32, x: u32, y: u32) -> Vec<u8> {
    let mut pixels = vec![0u8; TILE_SIZE * TILE_SIZE * 3];
    pixels
        .par_chunks_mut(TILE_SIZE * 3)
        .enumerate()
        .for_each(|(row, buf)| {
            for col in 0..TILE_SIZE {
                let (lat, lon) = tile_pixel_to_lat_lon(
                    z,
                    x,
                    y,
                    col as f64 + 0.5,
                    row as f64 + 0.5,
                    TILE_SIZE as f64,
                );
                let t = planet.tectonics_at(lat, lon);
                let h = world_core::hash::hash3(0x71A7, t.plate as i64, 0, 0);
                let mut c = [
                    120.0 + (h & 0x7F) as f64,
                    120.0 + ((h >> 8) & 0x7F) as f64,
                    120.0 + ((h >> 16) & 0x7F) as f64,
                ];
                // Boundary tint: red = collision, blue = rift.
                let s = t.belt * t.convergence.abs();
                if t.convergence >= 0.0 {
                    c[0] += 90.0 * s;
                    c[1] -= 60.0 * s;
                    c[2] -= 60.0 * s;
                } else {
                    c[0] -= 60.0 * s;
                    c[1] -= 40.0 * s;
                    c[2] += 90.0 * s;
                }
                // Darken the seam itself.
                let dim = 1.0 - 0.45 * t.belt * t.belt;
                let out = &mut buf[col * 3..col * 3 + 3];
                for ch in 0..3 {
                    out[ch] = (c[ch] * dim).clamp(0.0, 255.0) as u8;
                }
            }
        });

    encode_png(&pixels)
}

/// Debug layer: ground temperature, blue (−40 °C) through white (0) to red (+35).
pub fn render_temperature_tile(planet: &Planet, z: u32, x: u32, y: u32) -> Vec<u8> {
    let climate = build_climate_grid(planet, z, x, y);
    render_climate_debug(planet, z, x, y, move |planet, lat, lon, px, py| {
        let (t_sea, _) = climate.sample(px, py);
        let t = t_sea - LAPSE_C * planet.bulk_elevation(lat, lon).max(0.0);
        if t < 0.0 {
            let f = (t / -40.0).clamp(0.0, 1.0);
            mix([235, 235, 235], [40, 70, 160], f)
        } else {
            let f = (t / 35.0).clamp(0.0, 1.0);
            mix([235, 235, 235], [185, 40, 30], f)
        }
    })
}

/// Debug layer: precipitation, parchment (dry) to deep blue (wet).
pub fn render_precipitation_tile(planet: &Planet, z: u32, x: u32, y: u32) -> Vec<u8> {
    let climate = build_climate_grid(planet, z, x, y);
    render_climate_debug(planet, z, x, y, move |_, _, _, px, py| {
        let (_, p) = climate.sample(px, py);
        mix([245, 242, 228], [25, 70, 140], p.clamp(0.0, 1.0))
    })
}

fn render_climate_debug(
    planet: &Planet,
    z: u32,
    x: u32,
    y: u32,
    color: impl Fn(&Planet, f64, f64, f64, f64) -> [u8; 3] + Sync,
) -> Vec<u8> {
    let mut pixels = vec![0u8; TILE_SIZE * TILE_SIZE * 3];
    pixels
        .par_chunks_mut(TILE_SIZE * 3)
        .enumerate()
        .for_each(|(row, buf)| {
            for col in 0..TILE_SIZE {
                let (px, py) = (col as f64 + 0.5, row as f64 + 0.5);
                let (lat, lon) = tile_pixel_to_lat_lon(z, x, y, px, py, TILE_SIZE as f64);
                let c = color(planet, lat, lon, px, py);
                buf[col * 3..col * 3 + 3].copy_from_slice(&c);
            }
        });
    encode_png(&pixels)
}

/// Unit vector toward the light. Azimuth in degrees clockwise from north,
/// altitude above the horizon; screen space is x-east, y-south.
fn light_vector(azimuth_deg: f64, altitude_deg: f64) -> [f64; 3] {
    let az = azimuth_deg.to_radians();
    let alt = altitude_deg.to_radians();
    [az.sin() * alt.cos(), -az.cos() * alt.cos(), alt.sin()]
}

/// Ground color: bathymetry below sea level (with sea ice where it's cold
/// enough), biome color on land (with rock showing through at altitude).
/// Snow is wherever temperature says, not wherever elevation says.
fn surface_color(e: f64, t_sea: f64, precip: f64) -> [u8; 3] {
    const OCEAN: [(f64, [u8; 3]); 4] = [
        (-0.90, [6, 16, 42]),
        (-0.45, [12, 38, 82]),
        (-0.12, [24, 68, 122]),
        (0.00, [60, 116, 158]),
    ];
    if e <= 0.0 {
        let c = gradient(&OCEAN, e);
        let ice = ((-6.0 - t_sea) / 10.0).clamp(0.0, 1.0);
        return mix(c, [222, 232, 240], ice);
    }
    let temp = t_sea - LAPSE_C * e;
    let mut c = biome_color(classify_biome(temp, precip));
    // Bare rock shows through on steep high terrain that isn't ice.
    let rocky = 0.5 * smoothstep01((e - 0.45) / 0.30);
    c = mix(c, [128, 118, 108], if temp < -13.0 { 0.0 } else { rocky });
    c
}

fn biome_color(b: Biome) -> [u8; 3] {
    match b {
        Biome::Ocean => [24, 68, 122],
        Biome::IceCap => [240, 246, 250],
        Biome::Tundra => [158, 152, 130],
        Biome::ColdSteppe => [172, 160, 132],
        Biome::Boreal => [62, 92, 66],
        Biome::Desert => [208, 182, 128],
        Biome::Grassland => [152, 162, 92],
        Biome::TemperateForest => [76, 122, 68],
        Biome::TemperateRainforest => [48, 102, 62],
        Biome::Savanna => [176, 166, 88],
        Biome::TropicalForest => [64, 130, 62],
        Biome::TropicalRainforest => [28, 106, 48],
    }
}

#[inline]
fn mix(a: [u8; 3], b: [u8; 3], t: f64) -> [u8; 3] {
    [
        lerp_u8(a[0], b[0], t),
        lerp_u8(a[1], b[1], t),
        lerp_u8(a[2], b[2], t),
    ]
}

#[inline]
fn smoothstep01(x: f64) -> f64 {
    let t = x.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn gradient(stops: &[(f64, [u8; 3])], v: f64) -> [u8; 3] {
    if v <= stops[0].0 {
        return stops[0].1;
    }
    for pair in stops.windows(2) {
        let (a, ca) = pair[0];
        let (b, cb) = pair[1];
        if v <= b {
            let t = (v - a) / (b - a);
            return [
                lerp_u8(ca[0], cb[0], t),
                lerp_u8(ca[1], cb[1], t),
                lerp_u8(ca[2], cb[2], t),
            ];
        }
    }
    stops[stops.len() - 1].1
}

#[inline]
fn lerp_u8(a: u8, b: u8, t: f64) -> u8 {
    (a as f64 + (b as f64 - a as f64) * t).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deep_zoom_elevation_is_continuous() {
        // Anti-FT3: at z22, adjacent pixels differ by a hair, never garbage.
        let planet = Planet::new(42);
        let (z, x, y) = (22u32, 2_000_000u32, 1_400_000u32);
        let mut prev: Option<f64> = None;
        for i in 0..256 {
            let (lat, lon) =
                tile_pixel_to_lat_lon(z, x, y, i as f64 + 0.5, 128.5, TILE_SIZE as f64);
            let e = planet.elevation(lat, lon, 22);
            if let Some(p) = prev {
                assert!(
                    (e - p).abs() < 0.01,
                    "deep-zoom discontinuity at pixel {i}: {p} → {e}"
                );
            }
            prev = Some(e);
        }
    }

    #[test]
    fn shared_border_elevations_match_exactly() {
        let planet = Planet::new(42);
        let (z, x, y) = (12u32, 1000u32, 1500u32);
        for i in 0..TILE_SIZE {
            let py = i as f64 + 0.5;
            let a = tile_pixel_to_lat_lon(z, x, y, TILE_SIZE as f64 + 0.5, py, TILE_SIZE as f64);
            let b = tile_pixel_to_lat_lon(z, x + 1, y, 0.5, py, TILE_SIZE as f64);
            assert_eq!(
                planet.elevation(a.0, a.1, 18),
                planet.elevation(b.0, b.1, 18),
                "border elevation diverged at row {i}"
            );
        }
    }
}
