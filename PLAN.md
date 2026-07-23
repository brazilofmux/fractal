# Fractal — a planet as a function, not a file

A procedurally generated, Earth-sized world that is derived on demand from a seed,
viewable from orbit down to street level, and populated — lazily, as you look at it —
with towns, roads, ports, rivers, and stories.

Spiritual successor to ProFantasy's Fractal Terrains, built to not fail the ways it failed:
no pre-rendered raster hoard, no single-precision zoom corruption, no hand-authoring wall.

## Principles

1. **The world is `f(seed, coordinates)`.** Nothing is stored that can be re-derived.
   Same seed → same planet, forever. Caches are an optimization, never a source of truth.
2. **Coarse constrains fine.** Each zoom level is generated *subject to* what the level
   above it already established. A river that exists at zoom 6 is a constraint on the
   terrain synthesized at zoom 12 — never contradicted, only refined.
3. **All randomness is positional.** No global RNG state anywhere. Every random value is
   a hash of `(seed, stage, coordinates)` — this is what makes lazy, out-of-order,
   parallel generation possible.
4. **f64 everywhere, integers for addressing.** Tiles are addressed as `(zoom, x, y)`
   integers; positions within a tile compute to lat/lon in f64. At zoom 30 that's
   sub-millimeter precision. FT3's deep-zoom garbage is structurally impossible here.
   A regression test zooms to maximum depth and asserts continuity.
5. **Speak standard formats.** XYZ raster tiles + Mapbox Vector Tiles over HTTP.
   MapLibre, Leaflet, and QGIS are all free frontends. Optional PMTiles export for
   sharing a "baked" world as a single file.
6. **AI generates the generator and the lore — never the pixels.** Terrain is math.
   Stories are language. Each tool does what it's for.

## Architecture

```
fractal/
├── crates/
│   ├── world-core      # cube-sphere geometry, positional hashing, noise, f64 coordinate math
│   ├── world-gen       # the pipeline: tectonics → elevation → climate → hydrology → biomes → civilization
│   ├── world-tiles     # raster rendering (hypsometric tint, hillshade) + vector tile encoding
│   └── world-server    # axum HTTP server: /tiles/{layer}/{z}/{x}/{y}, /feature/{id}, /lore/{id}
├── lore/               # lore engine: feature identity, context assembly, Claude API calls, SQLite cache
├── web/                # MapLibre GL viewer: layers, styling, click-a-town lore panel
└── PLAN.md
```

### Two domains, cleanly separated

- **Generation domain: the cube-sphere.** All synthesis happens on a seamless sphere
  (cube faces projected to the sphere, quadtree-subdivided). No poles-singularity, no
  seams, no projection distortion in the *math*.
- **Serving domain: Web Mercator XYZ.** The tile server answers standard slippy-map
  requests by sampling the sphere per pixel: `tile(z,x,y) pixel → lat/lon (f64) → f(seed, …)`.
  The projection is a *view* of the world, not its representation. (MapLibre's globe
  mode consumes the same tiles for an orbital view.)

### The constraint cascade

Each pipeline stage runs at the coarsest zoom where its physics makes sense, and its
output becomes immutable input to everything finer:

| Stage | Runs at | Produces |
|---|---|---|
| Tectonics | global, ~coarse grid | plates (spherical Voronoi), boundary types, uplift field |
| Elevation | any zoom, on demand | heightfield = uplift-shaped noise, refined per LOD |
| Climate | global, coarse | temperature (latitude + altitude), winds, precipitation, rain shadows |
| Hydrology | coarse graph, refined downward | drainage network, rivers, lakes; carves valleys as constraints |
| Biomes | derived | Whittaker classification from temperature × precipitation |
| Civilization | region-level, on demand | settlements (suitability scoring), roads (least-cost paths), ports (settlement ∩ natural harbor) |
| Lore | per feature, on first view | names, histories, tensions — LLM-written, deterministic context, cached |

Hydrology is the acknowledged hard part: rivers must flow downhill *across tile
boundaries* and reach the sea. Solved by generating the drainage graph coarse-first
(flow accumulation on the global grid), then treating river presence as a carving
constraint during fine-level elevation synthesis. This is the make-or-break stage —
prototyped early, not last.

### The lore engine

- Every generated feature (settlement, river, mountain range, region) gets a **stable ID**
  derived from `(seed, stage, location)` — the same town has the same ID forever.
- First time a feature is viewed, the engine assembles its *deterministic context*
  (biome, climate, what the river's called, neighboring settlements, trade-route
  position, regional history so far) and asks Claude to write its entry. Result is
  cached in SQLite keyed by feature ID.
- Lore is generated top-down like the terrain: region history first, then towns
  *within* that history, then families and taverns within the town. The same fractal
  property — coherent detail at every zoom — applied to fiction.
- The world works fully offline with lore disabled; the lore layer is additive.

## Phases

Each phase ends with something you can look at.

- **Phase 0 — A planet on screen.** Workspace scaffold. Cube-sphere math + positional
  hash noise in `world-core`. Elevation-only tiles (hypsometric tint) from axum.
  MapLibre viewer with globe mode. *Milestone: pan and zoom a seeded planet in the browser.*
- **Phase 1 — Trustworthy depth.** Hillshading. LOD refinement that is seam-free and
  artifact-free at extreme zoom, with regression tests (the anti-FT3 phase). On-disk
  tile cache.
- **Phase 2 — Geology.** Spherical-Voronoi plates, boundary classification
  (convergent/divergent/transform), uplift field shaping the noise. Mountain ranges
  now exist for reasons.
- **Phase 3 — Climate & biomes.** Insolation, prevailing winds, orographic
  precipitation, rain shadows → Whittaker biomes → the planet gets its colors honestly.
- **Phase 4 — Water.** The hard one. Coarse global drainage graph, flow accumulation,
  rivers and lakes as vector tiles, valley-carving constraint into elevation.
  *Milestone: every river reaches the sea.*
- **Phase 5 — People.** Suitability-scored settlement placement, least-cost-path roads,
  ports at natural harbors, region/settlement vector layers with generated names.
- **Phase 6 — Stories.** Feature IDs, context assembly, Claude API integration, SQLite
  lore cache, click-a-town lore panel in the viewer.
- **Phase 7 — Beyond (optional).** History simulation (wars, migrations, trade — DF-style
  but LLM-narrated). Player annotations/edits stored as overlays. PMTiles world export.

*Phases 0–7 complete. What follows was planned after the world existed,
which is why it reads less like engineering and more like appetite.*

## Phases 8+ — deeper in

The founding principles hold: everything is `f(seed, coordinates)` — and where
time enters, `f(seed, coordinates, year)`. Nothing below breaks a phase that
shipped; each layer reads the ones beneath it and writes none of them.

- **Phase 8 — Names on the land.** The map has towns and rivers but the land
  itself is mute. Derive and name the natural features: seas and gulfs from
  ocean components, mountain ranges from tectonic belt segments, forests and
  deserts from contiguous biome patches, islands from small landmasses. Each
  becomes a lore-capable feature with a stable id, labeled on the map at the
  right zooms (curved along ranges, spread across seas). The chronicler learns
  geography: "east of the Thornfell Range, on the Gulf of Herring…"
  *Milestone: zoom out and the map reads like a map.*

- **Phase 9 — Everyone has a name.** Port WorldMaker's PersonTables outright:
  Gompertz lifespans, marriage ages, birth intervals. Give every notable a
  household — spouse, children, ages all era-true; give rulers consorts,
  heirs, and sibling rivalries; let the annals record royal births, marriages
  and suspicious deaths. People become lore features (`p{...}`): click the
  harbormaster, get her story — which must agree with her family tree.
  *Milestone: a family tree you can walk, three generations deep, for anyone.*

- **Phase 10 — The price of salt.** Isolation's other half: economics. Manor
  incomes from land and biome; taxes flowing up the tenure web (the crown's
  ledger is the sum of its grudging lords); goods with sources — salt at
  ports, timber in forests, iron in mountains — flowing along the actual
  roads and sea lanes to actual markets. Wealth reshapes the lore: a town on
  a salt road is rich and knows it; a realm cut off from the sea by a rival's
  border remembers exactly which war did it.
  *Milestone: click a road and see what travels it.*

- **Phase 11 — The fourth coordinate.** Make the present year a parameter.
  The history engine already simulates 500 years; expose them: a time slider
  where populations grow, rulers succeed each other, plagues empty the ports,
  and war outcomes actually move border villages between realms (allegiance
  becomes a function of year, seeded by the wars both sides already agree
  on). The chronicler writes from whatever year you're parked in.
  *Milestone: drag the slider and watch a realm lose the war you read about.*

- **Phase 12 — Street level.** The original promise was orbit down to street
  level; this is the last flight of stairs. Diagram the cities: ward
  geometry, walls and gates, the market square, the harbor — deterministic
  city maps rendered from the interiors that already exist. Then let people
  in: rooms generated on demand ("You stand on Herring Quay; Maldwyn is
  counting hulls"), lore-narrated, cached as canon — and a bridge that speaks
  MUX, because a world this stubborn about determinism deserves players
  who type.
  *Milestone: walk from the Saltwharf to the Gilded Ram without leaving text.*

- **Phase 13 — The honest ledger.** Street level exposed what coarse zooms
  forgave: geometry and money that were plausible from orbit and wrong up
  close. Three debts, paid in order:
  - *13a — Lanes that sail.* Sea lanes are straight port-to-port chords
    today; some cross continents, and the trade solver prices the voyage as
    if they could. Route them as least-cost paths over the ocean cells —
    the water-borne twin of the Phase-5 road A* — hugging coasts the way
    era shipping did, priced at the distance actually sailed. Trade
    re-derives itself honestly: the cape adds cost, the strait earns its
    toll, and some port pair that traded cheaply through a continent
    discovers the long way round. *Milestone: click a lane and watch it
    round the cape.*
  - *13b — Roads that arrive.* Settlement positions were cell-scale
    jitter; Phase 12 anchored the towns to dry ground but the Phase-5 road
    polylines still run to the old nominal points — a high road can stop
    kilometers short of the wall it means to reach, or end at sea. Snap
    road endpoints to the anchors (and re-check the ford points), so every
    road at street zoom arrives at a gate. *Milestone: follow any road at
    z14 and it ends at a town.*
  - *13c — The manor roll.* Phase 10 promised manor incomes and delivered
    a flat head-tax straight to the crown. Give every holding of the
    Phase-7c tenure web an income derived from its land — arable, pasture,
    woodland, fishery, by biome and climate — of which the holder keeps
    their share, the liege takes their cut, and the crown gets what
    survives the climb. Ledgers at every rung: a knight's manor is worth
    so many marks, the Lord of the town so many manors and rents, the
    crown the sum of its grudging lords — conservation tested, so taxes
    still don't leak. The chronicler learns what everyone is worth, and a
    cadet who keeps a poor manor for a rich liege now has a number to
    resent. *Milestone: trace any mark in the crown's ledger down to the
    manor that paid it.*

### Risks, named early

- **Time-dependence (11) is the dangerous one** — it threatens every cache
  key and tempts every layer to become stateful. Rule: year is an *input*,
  never a state. Anything that can't be expressed as `f(seed, place, year)`
  doesn't ship.
- **Person-level lore (9) multiplies canon.** The SQLite cache handles scale,
  but consistency pressure grows: a person's entry must agree with their
  family tree, their settlement, their realm, and their era. Context assembly
  stays the sole source of truth; the chronicler never gets to improvise
  facts that generators could have supplied.
- **Street level (12) is unbounded.** Scope it like the planet: coarse
  constrains fine. Ward diagrams before room text; room text before anything
  interactive; MUX last, and only if it still sounds fun when we get there.

## Risks, burned down early

- **Hydrology across scales** → prototype the coarse drainage graph in Phase 4's first
  week; if the carving approach fights the noise, fall back to precomputing a global
  medium-resolution hydrology layer (still seed-derived, just eagerly cached).
- **Tile latency in Rust** → budget: <50 ms/tile uncached. Rayon per-pixel parallelism;
  cache aggressively; tiles are embarrassingly parallel.
- **Lore consistency drift** → lore prompts receive only deterministic context + already-
  cached neighboring lore; contradictions are structurally hard, and the cache is the
  canon.
