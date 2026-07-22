# fractal

A planet as a function, not a file.

An Earth-sized procedurally generated world, derived on demand from a seed —
viewable from orbit down to street level, and (eventually) populated lazily
with towns, roads, ports, rivers, and stories. A spiritual successor to
ProFantasy's Fractal Terrains, built to avoid the ways it failed: no
pre-rendered raster hoard, no single-precision deep-zoom corruption, no
hand-authoring wall.

See [PLAN.md](PLAN.md) for the full design and phase roadmap.

## Status: Phase 4

Seeded planet served as standard Web Mercator XYZ raster tiles and rendered in
the browser on a MapLibre globe. Tiles are hillshaded (seam-free by
construction: border samples are bit-identical across neighboring tiles,
enforced by tests) and cached on disk under `cache/`, keyed by generator
version and seed so stale tiles self-invalidate.

Elevation is shaped by tectonics: spherical-Voronoi plates with Euler-pole
motion, boundaries classified by relative velocity, and uplift belts where
plates collide — mountain ranges exist for reasons, island arcs included.
Toggle the `plates` debug overlay in the viewer to see the mosaic (red seams
converge, blue seams rift) and check that ranges sit on collisions.

Land colors come from climate, not altitude: temperature from insolation
bands plus altitude lapse, precipitation from zonal bands (wet ITCZ, dry
subtropical highs, wet storm tracks) with continentality and true orographic
rain shadows — terrain is sampled upwind along prevailing winds, so the lee
of a mountain range is dry because the range is there. Whittaker
classification maps (temperature, precipitation) to biomes; snow and sea ice
appear where it is cold, not where it is high. `temperature` and
`precipitation` debug overlays in the viewer.

Water drains: a global drainage graph on a cube-sphere grid (6×256²
cells, ~40 km) is priority-flooded outward from the ocean, so every land
cell has a monotone downhill path to the sea — depressions become lakes
with a spill, and a test walks every cell's drainage chain to the coast.
Flow accumulation is weighted by the climate's actual precipitation;
cells above a discharge quantile become rivers, served as Mapbox Vector
Tiles (`/tiles/rivers/{z}/{x}/{y}.mvt`, hand-rolled encoder) with
deterministic meanders that refine with zoom. Rivers feed back into the
terrain: valleys are carved toward the water surface during elevation
synthesis (coarse constrains fine), channels flood to the river's level,
and lakes fill to theirs.

## Run it

```sh
cargo run --release -p world-server        # default seed 42
cargo run --release -p world-server 1337   # any u64 seed
```

Then open http://127.0.0.1:8632

Because the server speaks standard XYZ tiles, QGIS or any slippy-map client
works too: add `http://127.0.0.1:8632/tiles/elevation/{z}/{x}/{y}.png` as an
XYZ tile layer.

## Layout

- `crates/world-core` — positional hashing, gradient noise, sphere/Mercator geometry, cube-sphere grid
- `crates/world-gen` — the generation pipeline: tectonics → elevation → climate → hydrology
- `crates/world-tiles` — per-pixel tile rendering, hypsometric tint, PNG encoding, MVT encoding
- `crates/world-server` — axum HTTP tile server
- `web/` — MapLibre GL globe viewer
