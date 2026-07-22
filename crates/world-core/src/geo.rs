//! Coordinate plumbing between the serving domain (Web Mercator XYZ tiles)
//! and the generation domain (points on the unit sphere). Tiles are addressed
//! by integers; everything continuous is f64 — at z30 that is sub-millimeter,
//! so deep zoom never runs out of mantissa.

use std::f64::consts::{PI, TAU};

/// (lat, lon) in radians → point on the unit sphere.
#[inline]
pub fn lat_lon_to_unit(lat: f64, lon: f64) -> [f64; 3] {
    let cl = lat.cos();
    [cl * lon.cos(), cl * lon.sin(), lat.sin()]
}

/// Center of pixel (px, py) of Web Mercator tile (z, x, y) → (lat, lon) radians.
#[inline]
pub fn tile_pixel_to_lat_lon(z: u32, x: u32, y: u32, px: f64, py: f64, tile_size: f64) -> (f64, f64) {
    let n = (1u64 << z) as f64;
    let xn = (x as f64 + px / tile_size) / n;
    let yn = (y as f64 + py / tile_size) / n;
    let lon = xn * TAU - PI;
    let lat = (PI * (1.0 - 2.0 * yn)).sinh().atan();
    (lat, lon)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deep_zoom_keeps_precision() {
        // The anti-FT3 test: at extreme zoom, adjacent pixels must still map to
        // distinct, monotonically ordered coordinates — no mantissa exhaustion.
        let z = 30;
        let x = 536_870_912; // middle of the range at z30
        let y = 536_870_912;
        let (_, lon_a) = tile_pixel_to_lat_lon(z, x, y, 0.5, 0.5, 256.0);
        let (_, lon_b) = tile_pixel_to_lat_lon(z, x, y, 1.5, 0.5, 256.0);
        assert!(lon_b > lon_a, "adjacent deep-zoom pixels collapsed");
    }
}
