# fractal

A planet as a function, not a file.

An Earth-sized procedurally generated world, derived on demand from a seed —
viewable from orbit down to street level, and (eventually) populated lazily
with towns, roads, ports, rivers, and stories. A spiritual successor to
ProFantasy's Fractal Terrains, built to avoid the ways it failed: no
pre-rendered raster hoard, no single-precision deep-zoom corruption, no
hand-authoring wall.

See [PLAN.md](PLAN.md) for the full design and phase roadmap.

## Status: Phase 7 (history)

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

People settled it: every land cell is scored on what settlers want —
fresh water, workable climate, flat ground, natural harbors — and the
best sites become cities, towns and villages (tiered, spaced, ~500 of
them), named by a seeded syllable generator. Ports appear where a good
site meets a sheltered cove; roads are least-cost paths over the same
grid the rivers use, hugging valleys and paying to ford. Settlements
and roads ship as vector tiles (`/tiles/settlements/…`,
`/tiles/roads/…`); the viewer labels them, and clicking a settlement
shows its name, kind and realm (allegiance to the nearest city).

And the world tells stories: every settlement and realm has a stable
feature id, and the first time one is clicked, the lore engine assembles
its deterministic context — biome, climate, its river's name, its
neighbors with distances and bearings, era-true medieval demographics
(population, households, mortality; the demographic model follows
TinyMUX.WorldMaker) — and asks Claude to write its atlas entry. Realm
chronicles are written before the entries of their towns, so fiction
nests the way terrain does; everything is cached in SQLite
(`lore.sqlite`) keyed by seed and generator version — the cache is the
canon. Lore is additive: without credentials the world runs fully
offline and the panel says so.

To enable lore, start the server with `ANTHROPIC_API_KEY` set (or an
`ant auth login` profile). The model defaults to `claude-opus-4-8`;
override with `LORE_MODEL`.

And the world has a past: five hundred years of deterministic annals,
simulated realm by realm from the seed. Dynasties whose rulers live and
die by a Gompertz-Makeham mortality curve (ported from
TinyMUX.WorldMaker's medieval profile), wars both belligerents' annals
record identically to the year, plagues that come ashore at the ports
and walk inland a year later, famines that prefer the dry realms.
The annals render under every realm chronicle in the viewer, and they
feed the chronicler as canon — so neighboring realms' stories cite the
same wars, the same fallen kings, the same hungry years.

And every settlement opens: wards gated by population (a patriciate
above five thousand souls, a harborside ward for ports, the Shambles
kept downwind), trades by the classic support-value tables (one
shoemaker per 150 souls, one inn per 2000 — the algorithm is ported
from the author's Isolation kingdom simulator), and notable people
with names and ages — the harbormaster, the guild master, the keeper
of the inn. All of it is a pure function of the seed, computed on
demand, shown instantly in the panel, and fed to the chronicler as
canon so the prose and the roster never disagree.

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
- `crates/world-gen` — the generation pipeline: tectonics → elevation → climate → hydrology → civilization
- `crates/world-tiles` — per-pixel tile rendering, hypsometric tint, PNG encoding, MVT encoding
- `crates/world-server` — axum HTTP tile + lore server
- `lore/` — the lore engine: feature ids, context assembly, Claude API, SQLite canon
- `web/` — MapLibre GL globe viewer with the lore panel
