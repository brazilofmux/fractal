# fractal

A planet as a function, not a file.

An Earth-sized procedurally generated world, derived on demand from a seed —
viewable from orbit down to street level, and (eventually) populated lazily
with towns, roads, ports, rivers, and stories. A spiritual successor to
ProFantasy's Fractal Terrains, built to avoid the ways it failed: no
pre-rendered raster hoard, no single-precision deep-zoom corruption, no
hand-authoring wall.

See [PLAN.md](PLAN.md) for the full design and phase roadmap.

## Status: Phase 0

Seeded planet — continents, oceans, mountain potential — served as standard
Web Mercator XYZ raster tiles and rendered in the browser on a MapLibre globe.

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

- `crates/world-core` — positional hashing, gradient noise, sphere/Mercator geometry
- `crates/world-gen` — the generation pipeline (Phase 0: elevation)
- `crates/world-tiles` — per-pixel tile rendering, hypsometric tint, PNG encoding
- `crates/world-server` — axum HTTP tile server
- `web/` — MapLibre GL globe viewer
