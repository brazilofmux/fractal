//! Bake a seeded world into shareable PMTiles archives — the map without
//! the server. Produces two files: a raster elevation pyramid and a vector
//! archive whose tiles carry the rivers, roads and settlements layers
//! together (protobuf lets sibling layers simply concatenate).
//!
//!   cargo run --release -p world-tiles --bin bake -- [seed] [raster-zoom]
//!
//! QGIS opens the results directly; MapLibre reads them via the pmtiles
//! protocol. Tile counts quadruple per zoom level: raster z5 is ~1.4k
//! tiles, z6 ~5.5k, z7 ~22k — pick accordingly.

use rayon::prelude::*;
use world_gen::Planet;
use world_tiles::pmtiles::{tile_id, Archive, TileType};

fn main() {
    let seed: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(42);
    let raster_max: u32 = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(5)
        .min(7);
    let vector_max: u32 = (raster_max + 2).min(8);

    let planet = Planet::new(seed);
    let t0 = std::time::Instant::now();
    planet.hydrology();
    planet.civilization();
    println!("world solved in {:.1?}", t0.elapsed());

    // ---- Raster: the elevation pyramid. -------------------------------
    let t0 = std::time::Instant::now();
    let mut raster = Archive::new();
    for z in 0..=raster_max {
        let n = 1u32 << z;
        let mut tiles: Vec<(u64, Vec<u8>)> = (0..n * n)
            .into_par_iter()
            .map(|i| {
                let (x, y) = (i % n, i / n);
                (
                    tile_id(z, x, y),
                    world_tiles::render_elevation_tile(&planet, z, x, y),
                )
            })
            .collect();
        tiles.sort_by_key(|t| t.0);
        for (id, bytes) in tiles {
            raster.add(id, &bytes);
        }
        println!("  raster z{z}: {} tiles", n * n);
    }
    let path = std::path::PathBuf::from(format!("world-s{seed}-raster.pmtiles"));
    let meta = format!(
        r#"{{"name":"fractal seed {seed} (elevation)","attribution":"fractal — a planet as a function"}}"#
    );
    let (addressed, contents, size) = raster
        .finish(&path, TileType::PNG, 0, raster_max as u8, &meta)
        .expect("write raster archive");
    println!(
        "{}: {addressed} tiles ({contents} unique) · {:.1} MB · {:.1?}",
        path.display(),
        size as f64 / 1e6,
        t0.elapsed()
    );

    // ---- Vector: rivers + roads + settlements in one tileset. ----------
    let t0 = std::time::Instant::now();
    let mut vector = Archive::new();
    for z in 0..=vector_max {
        let n = 1u32 << z;
        let mut tiles: Vec<(u64, Vec<u8>)> = (0..n * n)
            .into_par_iter()
            .map(|i| {
                let (x, y) = (i % n, i / n);
                // Sibling MVT layers concatenate into one tile.
                let mut bytes = world_tiles::render_rivers_tile(&planet, z, x, y);
                bytes.extend(world_tiles::render_roads_tile(&planet, z, x, y));
                bytes.extend(world_tiles::render_settlements_tile(&planet, z, x, y));
                bytes.extend(world_tiles::render_labels_tile(&planet, z, x, y));
                (tile_id(z, x, y), bytes)
            })
            .collect();
        tiles.sort_by_key(|t| t.0);
        for (id, bytes) in tiles {
            vector.add(id, &bytes);
        }
        println!("  vector z{z}: {} tiles", n * n);
    }
    let path = std::path::PathBuf::from(format!("world-s{seed}-features.pmtiles"));
    let meta = format!(
        r#"{{"name":"fractal seed {seed} (features)","vector_layers":[{{"id":"rivers","fields":{{"w":"Number","name":"String"}}}},{{"id":"roads","fields":{{"tier":"Number"}}}},{{"id":"settlements","fields":{{"name":"String","rank":"Number","port":"Number","capital":"Number","realm":"String","cell":"Number","pop":"Number"}}}},{{"id":"labels","fields":{{"name":"String","kind":"String","id":"String"}}}}]}}"#
    );
    let (addressed, contents, size) = vector
        .finish(&path, TileType::MVT, 0, vector_max as u8, &meta)
        .expect("write vector archive");
    println!(
        "{}: {addressed} tiles ({contents} unique) · {:.1} MB · {:.1?}",
        path.display(),
        size as f64 / 1e6,
        t0.elapsed()
    );
}
