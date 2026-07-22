//! Phase 4: hydrology. A coarse global drainage graph on the cube-sphere
//! grid: priority-flood depression filling outward from the ocean (so no
//! cell drains nowhere — what would be a pit becomes a lake with a spill),
//! flow accumulation weighted by real precipitation, rivers where
//! accumulated flow crosses a global quantile. The graph then feeds back
//! into terrain as constraints: valley carving pulls fine-scale elevation
//! down toward the river's water surface, and lakes flood to their fill
//! level. Coarse constrains fine — a river that exists on the global graph
//! is never contradicted by any zoom level below it.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use rayon::prelude::*;
use world_core::cubegrid::CubeGrid;
use world_core::geo::unit_to_lat_lon;
use world_core::hash::{hash3, splitmix64};

use crate::Planet;

/// Grid resolution per cube-face edge: 6·256² ≈ 393k cells, ~40 km at face
/// centers — the scale where "which way does the water go" gets decided.
const GRID_N: usize = 256;
const STAGE_RIVERS: u64 = 0x0D_5EA1;
/// A cell carries a river when its accumulated flow is above this quantile
/// of all land cells.
const RIVER_QUANTILE: f64 = 0.975;
/// Midpoint-displacement amplitude as a fraction of segment length.
const MEANDER: f64 = 0.55;
/// Subdivision depth of the polylines used for carving. Renderers may go
/// deeper — deeper levels only add wiggle smaller than the valley width, so
/// the drawn river always stays inside its carved valley.
const CARVE_LEVELS: u32 = 2;
const CARVE_PTS: usize = (1 << CARVE_LEVELS) + 1;

pub struct RiverEdge {
    /// Upstream cell.
    pub a: u32,
    /// Downstream cell (may be ocean — that's the river mouth).
    pub b: u32,
    /// Width class 1..=6, log-scaled in discharge (upstream end; used for
    /// vector styling).
    pub w: u8,
    /// Continuous width class at each end. Adjacent edges share the value at
    /// their common node, so channel width is continuous along a whole
    /// river and widens smoothly into each confluence.
    pub wa: f32,
    pub wb: f32,
}

pub struct Hydrology {
    seed: u64,
    pub(crate) grid: CubeGrid,
    /// Water-routing surface per cell: ≥ max(elevation, sea level), monotone
    /// non-increasing downstream. 0 on the ocean. Where fill > elevation,
    /// the cell is under a lake whose surface is the fill value.
    fill: Vec<f64>,
    down: Vec<u32>,
    pub(crate) ocean: Vec<bool>,
    // Per-cell samples and the flow solution, retained because the
    // civilization stage scores cells on exactly these fields — no point
    // sampling the planet twice.
    pub(crate) elev: Vec<f64>,
    pub(crate) precip: Vec<f64>,
    pub(crate) t_sea: Vec<f64>,
    pub(crate) acc: Vec<f64>,
    pub(crate) threshold: f64,
    /// Symmetrized 8-neighborhood as CSR (offsets into `adj_dat`).
    adj_off: Vec<u32>,
    adj_dat: Vec<u32>,
    rivers: Vec<RiverEdge>,
    /// CARVE_PTS-point polyline per river edge, flattened.
    carve_pts: Vec<[f64; 3]>,
    /// Per-cell river edges to consider when carving; empty away from rivers.
    bucket: Vec<Vec<u32>>,
    /// Max lake surface among a cell and its neighbors (−inf if none) — lets
    /// the renderer flood terrain sitting below a nearby lake's level.
    lake_near: Vec<f64>,
    lake_cells: usize,
}

impl Hydrology {
    pub fn build(planet: &Planet) -> Self {
        let grid = CubeGrid::new(GRID_N);
        let n = grid.cells();

        // Adjacency, symmetrized: geometric cross-face discovery can be
        // one-sided right at face seams, and the flood must be able to enter
        // every cell from any side.
        let mut adj: Vec<Vec<u32>> = (0..n as u32)
            .into_par_iter()
            .map(|c| grid.neighbors(c))
            .collect();
        let mut missing = Vec::new();
        for c in 0..n {
            for &nb in &adj[c] {
                if !adj[nb as usize].contains(&(c as u32)) {
                    missing.push((nb as usize, c as u32));
                }
            }
        }
        for (a, b) in missing {
            adj[a].push(b);
        }

        // Sample the planet at cell centers: macro elevation and climate.
        let samples: Vec<(f64, f64, f64)> = (0..n as u32)
            .into_par_iter()
            .map(|c| {
                let (lat, lon) = unit_to_lat_lon(grid.cell_center(c));
                let cl = planet.climate(lat, lon);
                (planet.bulk_elevation(lat, lon), cl.precip, cl.sea_level_temp_c)
            })
            .collect();
        let elev: Vec<f64> = samples.iter().map(|s| s.0).collect();

        // The ocean is the largest connected component of below-sea cells.
        // Smaller wet components are endorheic basins: they still must drain,
        // so the flood treats them as land and they fill into lakes.
        let ocean = largest_wet_component(&adj, &elev);

        // Priority flood (Barnes et al.): grow inland from the coast, always
        // expanding the lowest frontier cell. Each cell's fill is the lowest
        // water level that can reach the sea from there; its downstream
        // pointer is the neighbor that reached it. Every land cell therefore
        // gets a monotone non-ascending path to the ocean, by construction.
        let mut fill = vec![0.0f64; n];
        let mut down = vec![u32::MAX; n];
        let mut done = ocean.clone();
        let mut heap: BinaryHeap<Reverse<(K, u32, u32)>> = BinaryHeap::new();
        for c in 0..n {
            if !ocean[c] {
                continue;
            }
            for &nb in &adj[c] {
                if !done[nb as usize] {
                    heap.push(Reverse((K(elev[nb as usize].max(0.0)), nb, c as u32)));
                }
            }
        }
        let mut order = Vec::with_capacity(n);
        while let Some(Reverse((K(f), c, from))) = heap.pop() {
            let ci = c as usize;
            if done[ci] {
                continue;
            }
            done[ci] = true;
            fill[ci] = f;
            down[ci] = from;
            order.push(ci);
            for &nb in &adj[ci] {
                if !done[nb as usize] {
                    heap.push(Reverse((K(f.max(elev[nb as usize].max(0.0))), nb, c)));
                }
            }
        }

        // Flow accumulation in reverse pop order (upstream before downstream,
        // even across the flat fills of lakes). Precipitation-weighted, with
        // a trickle floor so desert basins still route, scaled by true cell
        // area so face corners don't undercount.
        let mut acc: Vec<f64> = (0..n)
            .map(|c| {
                if ocean[c] {
                    return 0.0;
                }
                let s = grid.local_cell_size(grid.cell_center(c as u32)) / grid.max_cell_size();
                (0.03 + samples[c].1) * s * s
            })
            .collect();
        for &c in order.iter().rev() {
            let d = down[c] as usize;
            acc[d] += acc[c];
        }

        // Rivers: land cells above the accumulation quantile, excluding
        // stretches fully under a lake (the lake renders the water there).
        let mut land_acc: Vec<f64> = (0..n).filter(|&c| !ocean[c]).map(|c| acc[c]).collect();
        land_acc.sort_by(f64::total_cmp);
        let threshold = land_acc[(land_acc.len() as f64 * RIVER_QUANTILE) as usize].max(1e-9);
        let is_lake = |c: usize| !ocean[c] && fill[c] > elev[c].max(0.0) + 1e-4;
        let class = |a: f64| (1.0 + (a / threshold).log2()).clamp(1.0, 6.5);
        let rivers: Vec<RiverEdge> = (0..n)
            .filter(|&c| !ocean[c] && acc[c] >= threshold)
            .filter(|&c| !(is_lake(c) && is_lake(down[c] as usize)))
            .map(|c| RiverEdge {
                a: c as u32,
                b: down[c],
                w: (class(acc[c]) as u8).min(6),
                wa: class(acc[c]) as f32,
                wb: class(acc[down[c] as usize]) as f32,
            })
            .collect();

        // Carving geometry and its spatial index. Register the 1-ring of
        // cells sampled densely along each polyline, so every point within a
        // valley half-width (< 1 local cell) of the line is guaranteed to
        // land in a registered cell — carving is continuous across cell and
        // face boundaries because no candidate edge is ever missing.
        let mut carve_pts = Vec::with_capacity(rivers.len() * CARVE_PTS);
        let mut bucket = vec![Vec::new(); n];
        let mut ring = Vec::new();
        for (ei, rv) in rivers.iter().enumerate() {
            let pts = subdivided(planet.seed, &grid, rv.a, rv.b, CARVE_LEVELS);
            ring.clear();
            for k in 0..pts.len() - 1 {
                for s in 0..=4 {
                    let t = s as f64 / 4.0;
                    let q = normalize(lerp3(pts[k], pts[k + 1], t));
                    let cq = grid.point_to_cell(q);
                    ring.push(cq);
                    ring.extend(grid.neighbors(cq));
                }
            }
            ring.sort_unstable();
            ring.dedup();
            for &cc in &ring {
                bucket[cc as usize].push(ei as u32);
            }
            carve_pts.extend_from_slice(&pts);
        }

        // Lakes, and the "flood level near me" field the renderer samples.
        let lake_cells = (0..n).filter(|&c| is_lake(c)).count();
        let lake_near: Vec<f64> = (0..n)
            .map(|c| {
                let mut w = if is_lake(c) { fill[c] } else { f64::NEG_INFINITY };
                for &nb in &adj[c] {
                    if is_lake(nb as usize) {
                        w = w.max(fill[nb as usize]);
                    }
                }
                w
            })
            .collect();

        // Flatten adjacency into CSR for cheap reuse by later stages.
        let mut adj_off = Vec::with_capacity(n + 1);
        let mut adj_dat = Vec::new();
        adj_off.push(0u32);
        for l in &adj {
            adj_dat.extend_from_slice(l);
            adj_off.push(adj_dat.len() as u32);
        }

        Self {
            seed: planet.seed,
            grid,
            fill,
            down,
            ocean,
            elev,
            precip: samples.iter().map(|s| s.1).collect(),
            t_sea: samples.iter().map(|s| s.2).collect(),
            acc,
            threshold,
            adj_off,
            adj_dat,
            rivers,
            carve_pts,
            bucket,
            lake_near,
            lake_cells,
        }
    }

    /// Symmetrized geometric 8-neighborhood of a cell.
    pub(crate) fn adj(&self, c: usize) -> &[u32] {
        &self.adj_dat[self.adj_off[c] as usize..self.adj_off[c + 1] as usize]
    }

    pub(crate) fn is_lake(&self, c: usize) -> bool {
        !self.ocean[c] && self.fill[c] > self.elev[c].max(0.0) + 1e-4
    }

    pub(crate) fn is_river(&self, c: usize) -> bool {
        !self.ocean[c] && self.acc[c] >= self.threshold
    }

    /// The jittered node position of a cell — the exact point the river
    /// network renders through, so anything placed "on the river" lands on
    /// the drawn line.
    pub fn node_position(&self, c: u32) -> [f64; 3] {
        node_pos(self.seed, &self.grid, c)
    }

    pub fn rivers(&self) -> &[RiverEdge] {
        &self.rivers
    }

    /// Downstream cell per cell (`u32::MAX` on the ocean itself).
    pub fn downstream(&self) -> &[u32] {
        &self.down
    }

    /// Water-routing surface per cell — monotone non-increasing downstream.
    pub fn fill_levels(&self) -> &[f64] {
        &self.fill
    }

    /// True for cells of the ocean (the largest connected below-sea body).
    pub fn ocean_mask(&self) -> &[bool] {
        &self.ocean
    }

    pub fn lake_cell_count(&self) -> usize {
        self.lake_cells
    }

    /// Upper bound on cell angular size, for renderers choosing tile margins.
    pub fn max_cell_size(&self) -> f64 {
        self.grid.max_cell_size()
    }

    /// The deterministic meandering polyline of a river edge as unit vectors.
    /// Deeper `levels` refine the same curve (earlier points are a prefix of
    /// the shape, never moved), so every zoom agrees with every other and
    /// with the carved valley.
    pub fn river_polyline(&self, edge: usize, levels: u32) -> Vec<[f64; 3]> {
        let rv = &self.rivers[edge];
        subdivided(self.seed, &self.grid, rv.a, rv.b, levels)
    }

    /// Nearest approach to one river edge's polyline: distance to the
    /// channel, valley half-width and channel half-width there (both follow
    /// the interpolated discharge, so they are continuous through nodes and
    /// confluences), and the water surface / incised channel floor.
    fn edge_hit(&self, p: [f64; 3], ei: usize, cell: f64) -> EdgeHit {
        let rv = &self.rivers[ei];
        let pts = &self.carve_pts[ei * CARVE_PTS..][..CARVE_PTS];
        let (mut best_d2, mut best_t) = (f64::INFINITY, 0.0);
        for k in 0..CARVE_PTS - 1 {
            let (d2, t) = point_segment(p, pts[k], pts[k + 1]);
            if d2 < best_d2 {
                best_d2 = d2;
                best_t = (k as f64 + t) / (CARVE_PTS - 1) as f64;
            }
        }
        let w = rv.wa as f64 + (rv.wb as f64 - rv.wa as f64) * best_t;
        // Valley half-width grows with discharge; capped safely below one
        // local cell so the bucket's coverage guarantee holds.
        let hw = cell * (0.16 + 0.07 * w);
        let surface = self.fill[rv.a as usize]
            + (self.fill[rv.b as usize] - self.fill[rv.a as usize]) * best_t;
        EdgeHit {
            d: best_d2.sqrt(),
            hw,
            channel: hw * (0.06 + 0.035 * w),
            surface,
            floor: surface - 0.002 - 0.0015 * w,
        }
    }

    /// Carve terrain toward the water surface of any nearby river. `e_raw`
    /// is the synthesized elevation at unit-sphere point `p`; the result is
    /// never above it. Distances are continuous, so the carve is too.
    pub fn carve(&self, p: [f64; 3], e_raw: f64) -> f64 {
        let edges = &self.bucket[self.grid.point_to_cell(p) as usize];
        if edges.is_empty() {
            return e_raw;
        }
        let cell = self.grid.local_cell_size(p);
        let mut e = e_raw;
        for &ei in edges {
            let hit = self.edge_hit(p, ei as usize, cell);
            // The channel floor sits slightly below the water surface so the
            // river stays in a notch even where terrain detail is rough.
            if hit.d < hit.hw && e_raw > hit.floor {
                let s = smoothstep01(hit.d / hit.hw);
                e = e.min(hit.floor + (e_raw - hit.floor) * s);
            }
        }
        e
    }

    /// Water surface at/near this point, if terrain below it should flood:
    /// a lake's fill level, or a river's water surface within its channel —
    /// terrain the raw synthesis left below a passing river's level *is*
    /// that river, which is what keeps the channel wet through hollows the
    /// carve never needed to touch.
    pub fn water_level(&self, p: [f64; 3]) -> Option<f64> {
        let c = self.grid.point_to_cell(p) as usize;
        let mut w = self.lake_near[c];
        let edges = &self.bucket[c];
        if !edges.is_empty() {
            let cell = self.grid.local_cell_size(p);
            for &ei in edges {
                let hit = self.edge_hit(p, ei as usize, cell);
                // Only the inner channel floods — the valley is carved out
                // to `hw`, but a valley floor is not a waterway.
                if hit.d < hit.channel {
                    w = w.max(hit.surface);
                }
            }
        }
        (w > f64::NEG_INFINITY).then_some(w)
    }
}

/// Node position of a cell in the river graph: the cell center plus a
/// deterministic tangent jitter, so confluences connect exactly while the
/// network stops looking grid-stamped.
fn node_pos(seed: u64, grid: &CubeGrid, c: u32) -> [f64; 3] {
    let ctr = grid.cell_center(c);
    let s = splitmix64(seed ^ STAGE_RIVERS);
    let u1 = unit_f64(hash3(s, c as i64, 1, 0)) * 2.0 - 1.0;
    let u2 = unit_f64(hash3(s, c as i64, 2, 0)) * 2.0 - 1.0;
    let up = if ctr[2].abs() < 0.9 {
        [0.0, 0.0, 1.0]
    } else {
        [1.0, 0.0, 0.0]
    };
    let t1 = normalize(cross(ctr, up));
    let t2 = cross(ctr, t1);
    let amp = grid.local_cell_size(ctr) * 0.35;
    normalize([
        ctr[0] + amp * (u1 * t1[0] + u2 * t2[0]),
        ctr[1] + amp * (u1 * t1[1] + u2 * t2[1]),
        ctr[2] + amp * (u1 * t1[2] + u2 * t2[2]),
    ])
}

/// Midpoint-displacement meander between two cell nodes. Each level inserts
/// hashed perpendicular offsets and keeps every existing point, so a deeper
/// subdivision refines — never contradicts — a shallower one.
fn subdivided(seed: u64, grid: &CubeGrid, a: u32, b: u32, levels: u32) -> Vec<[f64; 3]> {
    let s_edge = hash3(splitmix64(seed ^ STAGE_RIVERS), a as i64, b as i64, 7);
    let mut pts = vec![node_pos(seed, grid, a), node_pos(seed, grid, b)];
    for lvl in 0..levels {
        let mut next = Vec::with_capacity(pts.len() * 2 - 1);
        for k in 0..pts.len() - 1 {
            let (pa, pb) = (pts[k], pts[k + 1]);
            let chord = [pb[0] - pa[0], pb[1] - pa[1], pb[2] - pa[2]];
            let len = dot(chord, chord).sqrt();
            let mid = normalize(lerp3(pa, pb, 0.5));
            let m = if len > 1e-12 {
                let perp = normalize(cross(chord, mid));
                let h = unit_f64(hash3(s_edge, lvl as i64, k as i64, 0)) - 0.5;
                let d = h * MEANDER * len;
                normalize([
                    mid[0] + d * perp[0],
                    mid[1] + d * perp[1],
                    mid[2] + d * perp[2],
                ])
            } else {
                mid
            };
            next.push(pa);
            next.push(m);
        }
        next.push(*pts.last().unwrap());
        pts = next;
    }
    pts
}

/// Largest connected component of below-sea-level cells.
fn largest_wet_component(adj: &[Vec<u32>], elev: &[f64]) -> Vec<bool> {
    let n = elev.len();
    let mut comp = vec![u32::MAX; n];
    let mut sizes = Vec::new();
    let mut stack = Vec::new();
    for start in 0..n {
        if elev[start] > 0.0 || comp[start] != u32::MAX {
            continue;
        }
        let id = sizes.len() as u32;
        let mut size = 0usize;
        stack.push(start);
        comp[start] = id;
        while let Some(c) = stack.pop() {
            size += 1;
            for &nb in &adj[c] {
                let nb = nb as usize;
                if elev[nb] <= 0.0 && comp[nb] == u32::MAX {
                    comp[nb] = id;
                    stack.push(nb);
                }
            }
        }
        sizes.push(size);
    }
    let main = sizes
        .iter()
        .enumerate()
        .max_by_key(|(_, &s)| s)
        .map(|(i, _)| i as u32)
        .expect("a planet with no ocean at all");
    (0..n).map(|c| comp[c] == main).collect()
}

/// Squared chord distance from p to segment [a, b], and the parameter t of
/// the closest point. Chord ≈ arc at these scales.
fn point_segment(p: [f64; 3], a: [f64; 3], b: [f64; 3]) -> (f64, f64) {
    let ab = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let ap = [p[0] - a[0], p[1] - a[1], p[2] - a[2]];
    let den = dot(ab, ab);
    let t = if den > 1e-18 {
        (dot(ap, ab) / den).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let q = [ap[0] - t * ab[0], ap[1] - t * ab[1], ap[2] - t * ab[2]];
    (dot(q, q), t)
}

struct EdgeHit {
    d: f64,
    hw: f64,
    channel: f64,
    surface: f64,
    floor: f64,
}

/// f64 heap key with a total order.
#[derive(PartialEq)]
struct K(f64);
impl Eq for K {}
impl PartialOrd for K {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for K {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&other.0)
    }
}

#[inline]
fn smoothstep01(x: f64) -> f64 {
    let t = x.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[inline]
fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

#[inline]
fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

#[inline]
fn normalize(v: [f64; 3]) -> [f64; 3] {
    let inv = 1.0 / dot(v, v).sqrt();
    [v[0] * inv, v[1] * inv, v[2] * inv]
}

#[inline]
fn lerp3(a: [f64; 3], b: [f64; 3], t: f64) -> [f64; 3] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

#[inline]
fn unit_f64(h: u64) -> f64 {
    (h >> 11) as f64 / (1u64 << 53) as f64
}
