//! Phase 5: people. Settlements score every land cell on what humans have
//! always wanted — fresh water, workable climate, flat ground, shelter for
//! boats — and the best sites win, tier by tier, with spacing so cities
//! don't crowd. Ports emerge where a good site meets a natural harbor.
//! Roads are least-cost paths over the same grid the water uses: they hug
//! valleys, avoid mountains, and pay to ford rivers. Names are positional
//! hashes dressed in syllables; realms are simply "your nearest city".
//! Everything is a deterministic function of the seed.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};

use world_core::hash::{hash3, splitmix64};

use crate::hydrology::Hydrology;
use crate::{Planet, LAPSE_C};

const STAGE_CIV: u64 = 0xC1_1715;

/// Settlement quota: one city per this many land cells (clamped 8..=40),
/// towns and villages as multiples of the city count.
const CELLS_PER_CITY: usize = 5000;
const TOWNS_PER_CITY: usize = 5;
const VILLAGES_PER_CITY: usize = 18;

/// Minimum spacing between settlements of each tier, in cells.
const SEPARATION: [f64; 3] = [16.0, 6.5, 3.2];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SettlementKind {
    City,
    Town,
    Village,
}

impl SettlementKind {
    /// 1 = city, 2 = town, 3 = village — the vector-tile rank attribute.
    pub fn rank(self) -> u8 {
        match self {
            SettlementKind::City => 1,
            SettlementKind::Town => 2,
            SettlementKind::Village => 3,
        }
    }
}

pub struct Settlement {
    pub cell: u32,
    pub pos: [f64; 3],
    pub kind: SettlementKind,
    pub port: bool,
    pub capital: bool,
    pub name: String,
    /// Name of the nearest city — the realm this settlement belongs to.
    pub realm: String,
    /// Cell of that city, the realm's stable identity.
    pub realm_capital: u32,
    /// Deterministic head count, scaled to kind and fortune.
    pub population: u32,
}

pub struct Road {
    /// 1 = between cities, 2 = town link, 3 = village link.
    pub tier: u8,
    /// Indices into `settlements` of the two endpoints.
    pub a: u32,
    pub b: u32,
    pub pts: Vec<[f64; 3]>,
}

pub struct Civilization {
    pub settlements: Vec<Settlement>,
    pub roads: Vec<Road>,
}

impl Civilization {
    pub fn build(planet: &Planet) -> Self {
        let h = planet.hydrology();
        let n = h.grid.cells();
        let seed = splitmix64(planet.seed ^ STAGE_CIV);

        // ---- Suitability -------------------------------------------------
        let mut coast = vec![false; n];
        let mut harbor = vec![false; n];
        let mut score: Vec<f64> = vec![0.0; n];
        for c in 0..n {
            if h.ocean[c] || h.is_lake(c) || h.elev[c] <= 0.0 {
                continue;
            }
            let mut lake_adj = false;
            let mut relief = 0.0;
            for &nb in h.adj(c) {
                let nb = nb as usize;
                if h.ocean[nb] {
                    coast[c] = true;
                    // A sheltered anchorage: the adjacent water is nearly
                    // enclosed by land — a cove, not just any beach.
                    let enclosed = h.adj(nb).iter().filter(|&&m| !h.ocean[m as usize]).count();
                    if enclosed >= 5 {
                        harbor[c] = true;
                    }
                }
                lake_adj |= h.is_lake(nb);
                relief += (h.elev[nb] - h.elev[c]).abs();
            }
            relief /= h.adj(c).len() as f64;

            let temp = h.t_sea[c] - LAPSE_C * h.elev[c].max(0.0);
            let temp_s = (-((temp - 12.0) / 14.0) * ((temp - 12.0) / 14.0)).exp();
            let wet_s = smoothstep(0.06, 0.22, h.precip[c])
                * (1.0 - 0.3 * smoothstep(0.80, 1.00, h.precip[c]));
            let flat_s = (-(relief / 0.045) * (relief / 0.045)).exp();
            let alt_s = 1.0 - smoothstep(0.25, 0.55, h.elev[c]);
            let river_s = smoothstep(0.0, 3.0, (1.0 + h.acc[c] / h.threshold).ln());
            let water = river_s
                .max(if coast[c] { 0.60 } else { 0.0 })
                .max(if lake_adj { 0.45 } else { 0.0 });

            score[c] = temp_s
                * wet_s
                * flat_s
                * alt_s
                * (0.25 + 0.75 * water)
                * if harbor[c] { 1.25 } else { 1.0 }
                * (0.9 + 0.2 * unit_f64(hash3(seed, c as i64, 0, 0)));
        }

        // ---- Placement: greedy by score, tiered spacing -------------------
        let mut candidates: Vec<u32> = (0..n as u32).filter(|&c| score[c as usize] > 0.01).collect();
        candidates.sort_by(|&a, &b| {
            score[b as usize]
                .total_cmp(&score[a as usize])
                .then(a.cmp(&b))
        });
        let land = (0..n).filter(|&c| !h.ocean[c]).count();
        let n_cities = (land / CELLS_PER_CITY).clamp(8, 40);
        let quotas = [
            n_cities,
            n_cities * TOWNS_PER_CITY,
            n_cities * VILLAGES_PER_CITY,
        ];
        let cell_rad = h.grid.max_cell_size();

        let mut placed: Vec<(u32, [f64; 3], SettlementKind)> = Vec::new();
        for (tier, kind) in [
            SettlementKind::City,
            SettlementKind::Town,
            SettlementKind::Village,
        ]
        .into_iter()
        .enumerate()
        {
            let sep = SEPARATION[tier] * cell_rad;
            let mut taken = 0usize;
            for &c in &candidates {
                if taken >= quotas[tier] {
                    break;
                }
                let p = h.grid.cell_center(c);
                // Keep this tier's distance from everything already placed.
                if !placed
                    .iter()
                    .all(|(_, q, _)| chord(p, *q) >= sep.min(1.99))
                {
                    continue;
                }
                // A settlement needs standing room. Positions are jittered
                // at cell scale and the cell's own land-call is made at
                // drainage resolution — a cell can read as land coarse and
                // drown at full detail (a below-sea basin the raster has
                // always painted as sea). Walk the position ashore; if no
                // dry ground stands near, nobody founded anything here.
                let nominal = if h.is_river(c as usize) {
                    h.node_position(c)
                } else {
                    jitter(seed, &h.grid, c, p)
                };
                let Some(pos) = come_ashore(planet, h, c, nominal) else {
                    continue;
                };
                if chord(pos, nominal) > 0.0055 {
                    continue; // the nearest dry ground is another town's
                }
                placed.push((c, pos, kind));
                taken += 1;
            }
        }

        // ---- Identity: positions, names, realms ---------------------------
        let mut used_names = HashSet::new();
        let mut settlements: Vec<Settlement> = placed
            .iter()
            .map(|&(c, pos, kind)| {
                // The position was walked ashore at placement — one dry
                // position for dots, roads, lanes and wards alike.
                let hb = harbor[c as usize];
                let name = unique_name(
                    seed,
                    c,
                    hb,
                    h.is_river(c as usize),
                    &mut used_names,
                );
                // Population: kind sets the scale, a positional hash the
                // fortune, and a working port swells it by a quarter.
                let (base, span) = match kind {
                    SettlementKind::City => (8000u32, 20000u64),
                    SettlementKind::Town => (1000, 4000),
                    SettlementKind::Village => (120, 700),
                };
                let mut population = base + (hash3(seed, c as i64, 200, 0) % span) as u32;
                let port = hb && coast[c as usize];
                if port {
                    population += population / 4;
                }
                Settlement {
                    cell: c,
                    pos,
                    kind,
                    port,
                    capital: kind == SettlementKind::City,
                    name,
                    realm: String::new(),
                    realm_capital: 0,
                    population,
                }
            })
            .collect();
        let cities: Vec<(u32, [f64; 3], String)> = settlements
            .iter()
            .filter(|s| s.kind == SettlementKind::City)
            .map(|s| (s.cell, s.pos, s.name.clone()))
            .collect();
        for s in &mut settlements {
            let (capital, _, realm) = cities
                .iter()
                .min_by(|a, b| chord(s.pos, a.1).total_cmp(&chord(s.pos, b.1)))
                .expect("at least one city");
            s.realm = realm.clone();
            s.realm_capital = *capital;
        }

        // ---- Roads: least-cost paths between neighbors --------------------
        // Each settlement seeks a few nearest peers of its own or higher
        // tier; every accepted pair becomes an A* path over the land grid.
        let mut pairs: HashSet<(u32, u32)> = HashSet::new();
        let mut links: Vec<(usize, usize, u8)> = Vec::new();
        for (i, s) in settlements.iter().enumerate() {
            let (k, radius) = match s.kind {
                SettlementKind::City => (3, 60.0 * cell_rad),
                SettlementKind::Town => (2, 20.0 * cell_rad),
                SettlementKind::Village => (2, 10.0 * cell_rad),
            };
            let mut near: Vec<(f64, usize)> = settlements
                .iter()
                .enumerate()
                .filter(|&(j, t)| j != i && t.kind.rank() <= s.kind.rank())
                .map(|(j, t)| (chord(s.pos, t.pos), j))
                .filter(|&(d, _)| d < radius)
                .collect();
            near.sort_by(|a, b| a.0.total_cmp(&b.0));
            for &(_, j) in near.iter().take(k) {
                let key = (i.min(j) as u32, i.max(j) as u32);
                if pairs.insert(key) {
                    links.push((i, j, s.kind.rank()));
                }
            }
        }

        let mut roads = Vec::new();
        for (i, j, tier) in links {
            let (a, b) = (settlements[i].cell, settlements[j].cell);
            if let Some(cells) = astar(h, a, b) {
                let mut pts = Vec::with_capacity(cells.len());
                pts.push(settlements[i].pos);
                for &c in &cells[1..cells.len() - 1] {
                    pts.push(jitter(seed ^ 0x0A0D, &h.grid, c, h.grid.cell_center(c)));
                }
                pts.push(settlements[j].pos);
                let pts = chaikin(&chaikin(&pts));
                roads.push(Road {
                    tier,
                    a: i as u32,
                    b: j as u32,
                    pts,
                });
            }
        }

        Self { settlements, roads }
    }
}

/// Deterministic name of the river system that drains `cell`, if any. Every
/// settlement along one drainage shares the name, because the name hangs off
/// the river's mouth — the one cell the whole system agrees on.
pub fn river_name(planet_seed: u64, h: &Hydrology, cell: u32) -> Option<String> {
    h.river_class(cell)?;
    let mouth = h.river_mouth(cell)?;
    let seed = splitmix64(planet_seed ^ STAGE_CIV ^ 0x51E5);
    Some(format!(
        "River {}",
        gen_name(hash3(seed, mouth as i64, 300, 0), false, false)
    ))
}

/// A* over the land grid. Cost is distance times a terrain factor: slopes
/// and high ground are expensive, fording a river costs extra, oceans and
/// lakes are impassable. Returns the cell chain, endpoints included.
fn astar(h: &Hydrology, from: u32, to: u32) -> Option<Vec<u32>> {
    let goal = h.grid.cell_center(to);
    let start_h = chord(h.grid.cell_center(from), goal);
    let budget = start_h * 3.5 + 4.0 * h.grid.max_cell_size();

    let mut g: HashMap<u32, f64> = HashMap::new();
    let mut came: HashMap<u32, u32> = HashMap::new();
    let mut open: BinaryHeap<Reverse<(K, u32)>> = BinaryHeap::new();
    g.insert(from, 0.0);
    open.push(Reverse((K(start_h), from)));
    let mut expanded = 0usize;

    while let Some(Reverse((K(f), c))) = open.pop() {
        if c == to {
            let mut path = vec![c];
            let mut cur = c;
            while let Some(&p) = came.get(&cur) {
                path.push(p);
                cur = p;
            }
            path.reverse();
            return Some(path);
        }
        if f > budget {
            return None;
        }
        expanded += 1;
        if expanded > 40_000 {
            return None;
        }
        let gc = g[&c];
        let pc = h.grid.cell_center(c);
        for &nb in h.adj(c as usize) {
            let ni = nb as usize;
            if h.ocean[ni] || h.is_lake(ni) {
                continue;
            }
            let pn = h.grid.cell_center(nb);
            let d = chord(pc, pn);
            let slope = (h.elev[ni] - h.elev[c as usize]).abs() / d.max(1e-9);
            let mult = 1.0
                + 0.06 * slope
                + 2.0 * smoothstep(0.35, 0.60, h.elev[ni])
                + if h.is_river(ni) { 0.35 } else { 0.0 };
            let ng = gc + d * mult;
            if g.get(&nb).is_none_or(|&old| ng < old - 1e-12) {
                g.insert(nb, ng);
                came.insert(nb, c);
                open.push(Reverse((K(ng + chord(pn, goal)), nb)));
            }
        }
    }
    None
}

/// Walk a wet position onto dry ground: first along the line toward (and
/// past) the cell center — the cell is land, so a position jittered into
/// open water or a broad lake comes back onto its own ground — then, for
/// stubborn cases like a river town sitting in its own channel, rings
/// outward to the nearest dry point. None when no dry ground stands
/// within reach — the caller treats that as "no settlement here at all".
/// Deterministic, and cheap for the common case: a position already dry
/// returns after one sample.
fn come_ashore(planet: &Planet, h: &Hydrology, cell: u32, pos: [f64; 3]) -> Option<[f64; 3]> {
    const KM: f64 = 1.0 / 6371.0;
    let dry = |p: [f64; 3]| {
        let (lat, lon) = world_core::geo::unit_to_lat_lon(p);
        let e = planet.elevation(lat, lon, 8);
        e > 0.003 && !planet.water_level(lat, lon).is_some_and(|w| e < w - 5e-4)
    };
    if dry(pos) {
        return Some(pos);
    }
    let target = h.grid.cell_center(cell);
    for t in 1..=80 {
        let f = t as f64 / 40.0;
        let q = normalize([
            pos[0] + f * (target[0] - pos[0]),
            pos[1] + f * (target[1] - pos[1]),
            pos[2] + f * (target[2] - pos[2]),
        ]);
        if dry(q) {
            return Some(q);
        }
    }
    let east = normalize(cross([0.0, 0.0, 1.0], pos));
    let north = cross(pos, east);
    for ring in 1..=120 {
        let r = ring as f64 * 0.25 * KM;
        for b in 0..16 {
            let th = b as f64 * std::f64::consts::TAU / 16.0 + 0.13;
            let q = normalize([
                pos[0] + r * (th.sin() * east[0] + th.cos() * north[0]),
                pos[1] + r * (th.sin() * east[1] + th.cos() * north[1]),
                pos[2] + r * (th.sin() * east[2] + th.cos() * north[2]),
            ]);
            if dry(q) {
                return Some(q);
            }
        }
    }
    None
}

/// Deterministic tangent jitter off a cell center (its own stage stream, so
/// roads don't echo river geometry).
fn jitter(seed: u64, grid: &world_core::cubegrid::CubeGrid, c: u32, ctr: [f64; 3]) -> [f64; 3] {
    let u1 = unit_f64(hash3(seed, c as i64, 11, 0)) * 2.0 - 1.0;
    let u2 = unit_f64(hash3(seed, c as i64, 12, 0)) * 2.0 - 1.0;
    let up = if ctr[2].abs() < 0.9 {
        [0.0, 0.0, 1.0]
    } else {
        [1.0, 0.0, 0.0]
    };
    let t1 = normalize(cross(ctr, up));
    let t2 = cross(ctr, t1);
    let amp = grid.local_cell_size(ctr) * 0.30;
    normalize([
        ctr[0] + amp * (u1 * t1[0] + u2 * t2[0]),
        ctr[1] + amp * (u1 * t1[1] + u2 * t2[1]),
        ctr[2] + amp * (u1 * t1[2] + u2 * t2[2]),
    ])
}

/// One corner-cutting pass (Chaikin); endpoints stay fixed so roads still
/// meet their settlements exactly. (Sea lanes borrow it too.)
pub(crate) fn chaikin(pts: &[[f64; 3]]) -> Vec<[f64; 3]> {
    if pts.len() < 3 {
        return pts.to_vec();
    }
    let mut out = Vec::with_capacity(pts.len() * 2);
    out.push(pts[0]);
    for w in pts.windows(2) {
        out.push(normalize(mix3(w[0], w[1], 0.25)));
        out.push(normalize(mix3(w[0], w[1], 0.75)));
    }
    out.push(*pts.last().unwrap());
    out
}

// ---- Names -----------------------------------------------------------

const ONSETS: [&str; 24] = [
    "al", "ar", "bel", "bran", "cal", "dor", "el", "fen", "gal", "har", "kel", "lor", "mar",
    "nor", "or", "pel", "quil", "ran", "sel", "tar", "ul", "vor", "wil", "yr",
];
const MIDDLES: [&str; 8] = ["a", "e", "i", "o", "u", "ae", "ia", "en"];
const ENDS: [&str; 18] = [
    "ba", "dan", "dor", "fall", "gard", "holm", "ia", "mar", "mere", "mont", "na", "rath",
    "rick", "stead", "ton", "vale", "wick", "yn",
];
const RIVER_ENDS: [&str; 4] = ["ford", "bridge", "mouth", "bank"];
const PORT_ENDS: [&str; 3] = ["haven", "port", "quay"];

pub(crate) fn gen_name(mut x: u64, port: bool, river: bool) -> String {
    let mut pick = |m: usize| {
        x = splitmix64(x);
        (x % m as u64) as usize
    };
    let mut s = String::from(ONSETS[pick(ONSETS.len())]);
    if pick(3) == 0 {
        s.push_str(MIDDLES[pick(MIDDLES.len())]);
    }
    let roll = pick(10);
    if port && roll < 4 {
        s.push_str(PORT_ENDS[pick(PORT_ENDS.len())]);
    } else if river && roll < 3 {
        s.push_str(RIVER_ENDS[pick(RIVER_ENDS.len())]);
    } else {
        s.push_str(ENDS[pick(ENDS.len())]);
    }
    let mut chars = s.chars();
    let first = chars.next().unwrap().to_uppercase().to_string();
    first + chars.as_str()
}

fn unique_name(
    seed: u64,
    cell: u32,
    port: bool,
    river: bool,
    used: &mut HashSet<String>,
) -> String {
    for attempt in 0..8i64 {
        let name = gen_name(hash3(seed, cell as i64, 100 + attempt, 0), port, river);
        if used.insert(name.clone()) {
            return name;
        }
    }
    // Eight collisions in a row would be remarkable; qualify and move on.
    let name = format!(
        "{} {}",
        gen_name(hash3(seed, cell as i64, 99, 0), port, river),
        ["Vale", "Cross", "Rest", "March"][(cell % 4) as usize]
    );
    used.insert(name.clone());
    name
}

// ---- Small math ------------------------------------------------------

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
fn chord(a: [f64; 3], b: [f64; 3]) -> f64 {
    let d = [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
}

#[inline]
fn smoothstep(e0: f64, e1: f64, x: f64) -> f64 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
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
    let inv = 1.0 / (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    [v[0] * inv, v[1] * inv, v[2] * inv]
}

#[inline]
fn mix3(a: [f64; 3], b: [f64; 3], t: f64) -> [f64; 3] {
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
