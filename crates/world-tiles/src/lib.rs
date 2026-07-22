//! Raster tile rendering: sample the planet per pixel of a Web Mercator tile,
//! hypsometric-tint the elevation, encode PNG. Rows render in parallel.

use image::ImageEncoder;
use rayon::prelude::*;
use world_core::geo::tile_pixel_to_lat_lon;
use world_gen::Planet;

pub const TILE_SIZE: usize = 256;

pub fn render_elevation_tile(planet: &Planet, z: u32, x: u32, y: u32) -> Vec<u8> {
    // More synthesis depth as you zoom; capped well below f64's limits.
    let octaves = (z + 6).min(22);

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
                let e = planet.elevation(lat, lon, octaves);
                buf[col * 3..col * 3 + 3].copy_from_slice(&hypsometric(e));
            }
        });

    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(
            &pixels,
            TILE_SIZE as u32,
            TILE_SIZE as u32,
            image::ExtendedColorType::Rgb8,
        )
        .expect("png encode");
    png
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
