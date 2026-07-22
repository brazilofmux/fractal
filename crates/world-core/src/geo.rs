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

/// Pixel (px, py) of Web Mercator tile (z, x, y) → (lat, lon) radians.
///
/// Computed in *global* pixel space: `x * tile_size + px` is exact in f64 for
/// any zoom we serve, so a sample point addressed through one tile (e.g. a
/// border pixel at px = -0.5) is bit-identical to the same point addressed
/// through its neighbor (px = tile_size - 0.5). Seams between tiles are
/// therefore structurally impossible, not just unlikely.
#[inline]
pub fn tile_pixel_to_lat_lon(z: u32, x: u32, y: u32, px: f64, py: f64, tile_size: f64) -> (f64, f64) {
    let world = tile_size * (1u64 << z) as f64;
    let gx = x as f64 * tile_size + px;
    let gy = y as f64 * tile_size + py;
    let lon = (gx / world) * TAU - PI;
    let lat = (PI * (1.0 - 2.0 * (gy / world))).sinh().atan();
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

    #[test]
    fn border_samples_are_bit_identical_across_tiles() {
        // The same point addressed through two neighboring tiles must produce
        // the exact same f64s — this is what guarantees seam-free hillshading.
        let (z, x, y) = (12, 1000, 1500);
        for i in 0..256 {
            let py = i as f64 + 0.5;
            let a = tile_pixel_to_lat_lon(z, x, y, 256.5, py, 256.0);
            let b = tile_pixel_to_lat_lon(z, x + 1, y, 0.5, py, 256.0);
            assert_eq!(a, b, "x-border sample diverged at row {i}");
            let c = tile_pixel_to_lat_lon(z, x, y, py, 256.5, 256.0);
            let d = tile_pixel_to_lat_lon(z, x, y + 1, py, 0.5, 256.0);
            assert_eq!(c, d, "y-border sample diverged at col {i}");
        }
    }
}
