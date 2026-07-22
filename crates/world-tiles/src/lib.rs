//! Raster tile rendering: sample the planet into an elevation grid (with a
//! one-pixel border so gradients never need a neighboring tile), hillshade,
//! hypsometric-tint, encode PNG. Rows render in parallel.

use image::ImageEncoder;
use rayon::prelude::*;
use world_core::geo::tile_pixel_to_lat_lon;
use world_gen::Planet;

pub const TILE_SIZE: usize = 256;
const BORDER: usize = 1;
const GRID: usize = TILE_SIZE + 2 * BORDER;

/// Slope steepness for shading, in normalized-elevation units per pixel.
/// Map-style rather than physically metric: per-pixel gradients of fBm are
/// nearly scale-invariant, so one constant reads well at every zoom.
const SHADE_STRENGTH: f64 = 220.0;

pub fn render_elevation_tile(planet: &Planet, z: u32, x: u32, y: u32) -> Vec<u8> {
    // More synthesis depth as you zoom; capped well below f64's limits.
    let octaves = (z + 6).min(22);

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

                let tint = hypsometric(e);
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

/// Unit vector toward the light. Azimuth in degrees clockwise from north,
/// altitude above the horizon; screen space is x-east, y-south.
fn light_vector(azimuth_deg: f64, altitude_deg: f64) -> [f64; 3] {
    let az = azimuth_deg.to_radians();
    let alt = altitude_deg.to_radians();
    [az.sin() * alt.cos(), -az.cos() * alt.cos(), alt.sin()]
}

/// Elevation → color. Stops below sea level run deep abyss to shelf; above,
/// lowland green through arid brown to snow.
fn hypsometric(e: f64) -> [u8; 3] {
    const OCEAN: [(f64, [u8; 3]); 4] = [
        (-0.90, [6, 16, 42]),
        (-0.45, [12, 38, 82]),
        (-0.12, [24, 68, 122]),
        (0.00, [60, 116, 158]),
    ];
    const LAND: [(f64, [u8; 3]); 6] = [
        (0.00, [88, 138, 90]),
        (0.10, [122, 152, 92]),
        (0.25, [168, 160, 104]),
        (0.42, [150, 118, 88]),
        (0.60, [130, 108, 100]),
        (0.75, [236, 240, 244]),
    ];
    if e <= 0.0 {
        gradient(&OCEAN, e)
    } else {
        gradient(&LAND, e)
    }
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
