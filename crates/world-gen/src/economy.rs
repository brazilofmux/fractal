//! Phase 10: the price of salt. Goods have sources because geography says
//! so — salt and fish at the ports, timber in the named forests, iron
//! under the ranges, wool on the steppe, grain wherever the lowlands are
//! kind — and demand walks the actual road network (plus sea lanes, where
//! shipping is cheap the way it always has been) to the nearest producer,
//! by multi-source shortest paths. Every road remembers what crosses it;
//! every settlement knows what it makes, buys, and goes without; taxes
//! climb the tenure web until they reach a crown's ledger. Isolation's
//! other half, joined to the map.

use std::collections::HashMap;

use world_core::hash::{hash3, splitmix64};

use crate::{Biome, NaturalKind, Planet, SettlementKind, LAPSE_C};
use world_core::geo::unit_to_lat_lon;

const STAGE_ECONOMY: u64 = 0xEC_0A02;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Good {
    Salt,
    Fish,
    Grain,
    Timber,
    Iron,
    Wool,
    Wine,
    Furs,
    Spice,
}

impl Good {
    pub const ALL: [Good; 9] = [
        Good::Salt,
        Good::Fish,
        Good::Grain,
        Good::Timber,
        Good::Iron,
        Good::Wool,
        Good::Wine,
        Good::Furs,
        Good::Spice,
    ];

    pub fn word(self) -> &'static str {
        match self {
            Good::Salt => "salt",
            Good::Fish => "fish",
            Good::Grain => "grain",
            Good::Timber => "timber",
            Good::Iron => "iron",
            Good::Wool => "wool",
            Good::Wine => "wine",
            Good::Furs => "furs",
            Good::Spice => "spice",
        }
    }

    /// Luxuries are only demanded where there is money to want them.
    fn luxury(self) -> bool {
        matches!(self, Good::Wine | Good::Furs | Good::Spice)
    }
}

pub struct Lane {
    /// Settlement indices of the two ports.
    pub a: usize,
    pub b: usize,
    pub pts: Vec<[f64; 3]>,
    pub flows: Vec<(Good, u32)>,
}

pub struct Economy {
    /// Per settlement index.
    pub produces: Vec<Vec<Good>>,
    /// (good, producer settlement index) per settlement.
    pub imports: Vec<Vec<(Good, usize)>>,
    /// Goods a settlement wants and cannot reach.
    pub wanting: Vec<Vec<Good>>,
    /// Wealth class 1..=5 per settlement.
    pub wealth: Vec<u8>,
    /// Per road index: goods and volumes crossing it.
    pub road_flows: Vec<Vec<(Good, u32)>>,
    pub lanes: Vec<Lane>,
    /// Capital cell → yearly ledger in "marks" (arbitrary era coin).
    pub realm_ledger: HashMap<u32, u64>,
}

impl Economy {
    pub fn build(planet: &Planet) -> Self {
        let civ = planet.civilization();
        let h = planet.hydrology();
        let geo = planet.geography();
        let seed = splitmix64(planet.seed ^ STAGE_ECONOMY);
        let n = civ.settlements.len();

        // ---- Production: geography decides. ------------------------------
        let mut produces: Vec<Vec<Good>> = Vec::with_capacity(n);
        for s in &civ.settlements {
            let (lat, lon) = unit_to_lat_lon(s.pos);
            let cl = planet.climate(lat, lon);
            let e = planet.bulk_elevation(lat, lon);
            let temp = cl.sea_level_temp_c - LAPSE_C * e.max(0.0);
            let biome = crate::classify_biome(temp, cl.precip);
            let mut p = Vec::new();
            if s.port {
                p.push(Good::Salt);
                p.push(Good::Fish);
            } else if h.river_class(s.cell).map_or(false, |c| c >= 2) {
                p.push(Good::Fish);
            }
            if geo.cover_at(s.cell).map_or(false, |f| f.kind == NaturalKind::Forest) {
                p.push(Good::Timber);
            }
            if geo.relief_at(s.cell).is_some()
                || geo.nearest_range(s.pos, h.max_cell_size() * 3.0).is_some()
            {
                p.push(Good::Iron);
            }
            match biome {
                Biome::Grassland | Biome::ColdSteppe => p.push(Good::Wool),
                Biome::Boreal | Biome::Tundra => p.push(Good::Furs),
                Biome::TropicalForest | Biome::TropicalRainforest => p.push(Good::Spice),
                _ => {}
            }
            if matches!(
                biome,
                Biome::Grassland | Biome::TemperateForest | Biome::Savanna
            ) && e < 0.2
            {
                p.push(Good::Grain);
                if (10.0..=22.0).contains(&temp)
                    && (0.30..=0.70).contains(&cl.precip)
                    && hash3(seed, s.cell as i64, 1, 0) % 2 == 0
                {
                    p.push(Good::Wine);
                }
            }
            if p.is_empty() {
                p.push(Good::Grain); // subsistence, if nothing else
            }
            produces.push(p);
        }

        // ---- The trade graph: roads plus sea lanes. -----------------------
        // Edge list: (u, v, weight, road index or lane index offset).
        let road_len = |r: &crate::Road| -> f64 {
            r.pts.windows(2).map(|w| chord(w[0], w[1])).sum()
        };
        let mut edges: Vec<(usize, usize, f64)> = civ
            .roads
            .iter()
            .map(|r| (r.a as usize, r.b as usize, road_len(r)))
            .collect();
        let n_roads = edges.len();

        // Sea lanes: each port to its two nearest fellow ports. Shipping is
        // cheap: sea distance counts at less than half.
        let ports: Vec<usize> = (0..n).filter(|&i| civ.settlements[i].port).collect();
        let mut lanes: Vec<Lane> = Vec::new();
        let mut lane_pairs = std::collections::HashSet::new();
        for &a in &ports {
            let mut near: Vec<(f64, usize)> = ports
                .iter()
                .filter(|&&b| b != a)
                .map(|&b| (chord(civ.settlements[a].pos, civ.settlements[b].pos), b))
                .filter(|&(d, _)| d < h.max_cell_size() * 90.0)
                .collect();
            near.sort_by(|x, y| x.0.total_cmp(&y.0));
            for &(d, b) in near.iter().take(2) {
                if !lane_pairs.insert((a.min(b), a.max(b))) {
                    continue;
                }
                let (pa, pb) = (civ.settlements[a].pos, civ.settlements[b].pos);
                let pts = (0..=4)
                    .map(|k| slerp(pa, pb, k as f64 / 4.0))
                    .collect();
                edges.push((a, b, d * 0.45));
                lanes.push(Lane {
                    a,
                    b,
                    pts,
                    flows: Vec::new(),
                });
            }
        }

        let mut adj: Vec<Vec<(usize, f64, usize)>> = vec![Vec::new(); n];
        for (ei, &(u, v, w)) in edges.iter().enumerate() {
            adj[u].push((v, w, ei));
            adj[v].push((u, w, ei));
        }

        // ---- Flows: for each good, demand walks to the nearest producer.
        let mut road_flows: Vec<Vec<(Good, u32)>> = vec![Vec::new(); n_roads];
        let mut lane_flows: Vec<Vec<(Good, u32)>> = vec![Vec::new(); lanes.len()];
        let mut imports: Vec<Vec<(Good, usize)>> = vec![Vec::new(); n];
        let mut wanting: Vec<Vec<Good>> = vec![Vec::new(); n];
        let mut exports_vol = vec![0u64; n];
        let mut through_vol = vec![0u64; n];

        for good in Good::ALL {
            // Multi-source Dijkstra from every producer of this good.
            let mut dist = vec![f64::INFINITY; n];
            let mut root = vec![usize::MAX; n];
            let mut parent: Vec<(usize, usize)> = vec![(usize::MAX, usize::MAX); n]; // (node, edge)
            let mut heap = std::collections::BinaryHeap::new();
            for i in 0..n {
                if produces[i].contains(&good) {
                    dist[i] = 0.0;
                    root[i] = i;
                    heap.push(std::cmp::Reverse((K(0.0), i)));
                }
            }
            while let Some(std::cmp::Reverse((K(d), u))) = heap.pop() {
                if d > dist[u] {
                    continue;
                }
                for &(v, w, ei) in &adj[u] {
                    let nd = d + w;
                    if nd < dist[v] - 1e-12 {
                        dist[v] = nd;
                        root[v] = root[u];
                        parent[v] = (u, ei);
                        heap.push(std::cmp::Reverse((K(nd), v)));
                    }
                }
            }
            for d in 0..n {
                if produces[d].contains(&good) {
                    continue;
                }
                let s = &civ.settlements[d];
                if good.luxury() && s.population < 1200 {
                    continue; // villages do without wine
                }
                if dist[d].is_infinite() {
                    wanting[d].push(good);
                    continue;
                }
                let vol = (s.population / 1500 + 1) as u64;
                imports[d].push((good, root[d]));
                exports_vol[root[d]] += vol;
                // Walk home along the tree, tolling every crossing.
                let mut cur = d;
                while cur != root[d] {
                    let (pu, ei) = parent[cur];
                    let flows = if ei < n_roads {
                        &mut road_flows[ei]
                    } else {
                        &mut lane_flows[ei - n_roads]
                    };
                    match flows.iter_mut().find(|(g, _)| *g == good) {
                        Some((_, v)) => *v += vol as u32,
                        None => flows.push((good, vol as u32)),
                    }
                    if pu != root[d] {
                        through_vol[pu] += vol;
                    }
                    cur = pu;
                }
            }
        }
        for (l, f) in lanes.iter_mut().zip(lane_flows) {
            let mut f = f;
            f.sort_by(|a, b| b.1.cmp(&a.1));
            l.flows = f;
        }
        for f in road_flows.iter_mut() {
            f.sort_by(|a, b| b.1.cmp(&a.1));
        }

        // ---- Wealth: what you sell, what passes your gates, who you are.
        let mut score: Vec<f64> = (0..n)
            .map(|i| {
                let s = &civ.settlements[i];
                (s.population as f64).sqrt() * 0.6
                    + exports_vol[i] as f64 * 1.4
                    + through_vol[i] as f64 * 0.9
                    + if s.port { 12.0 } else { 0.0 }
            })
            .collect();
        let mut sorted = score.clone();
        sorted.sort_by(f64::total_cmp);
        let q = |p: f64| sorted[((sorted.len() - 1) as f64 * p) as usize];
        let (q1, q2, q3, q4) = (q(0.25), (q(0.5)), q(0.75), q(0.93));
        let wealth: Vec<u8> = score
            .iter_mut()
            .map(|s| match *s {
                x if x >= q4 => 5,
                x if x >= q3 => 4,
                x if x >= q2 => 3,
                x if x >= q1 => 2,
                _ => 1,
            })
            .collect();

        // ---- The ledgers: taxes climb the tenure web. ---------------------
        let mut realm_ledger: HashMap<u32, u64> = HashMap::new();
        for (i, s) in civ.settlements.iter().enumerate() {
            let take = s.population as u64 * wealth[i] as u64 / 40;
            *realm_ledger.entry(s.realm_capital).or_insert(0) += take;
        }

        Self {
            produces,
            imports,
            wanting,
            wealth,
            road_flows,
            lanes,
            realm_ledger,
        }
    }

    pub fn wealth_word(class: u8) -> &'static str {
        match class {
            5 => "rich",
            4 => "prosperous",
            3 => "comfortable",
            2 => "modest",
            _ => "struggling",
        }
    }
}

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

fn slerp(a: [f64; 3], b: [f64; 3], t: f64) -> [f64; 3] {
    let v = [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ];
    let inv = 1.0 / (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    [v[0] * inv, v[1] * inv, v[2] * inv]
}

#[inline]
fn chord(a: [f64; 3], b: [f64; 3]) -> f64 {
    let d = [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
}
