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
    /// Per settlement: what its land yields in marks a year — the manor
    /// roll, from which every ledger below is built.
    pub manor_income: Vec<u64>,
    /// Per settlement: the third penny sent up to its liege each year.
    pub manor_sends: Vec<u64>,
    /// Per settlement: dues received from manors held of it.
    pub manor_receives: Vec<u64>,
    /// Capital cell → yearly ledger in "marks": the crown's demesne income
    /// plus every due that survived the climb up the tenure web.
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

        // Sea lanes: each port to its two nearest fellow ports — but a lane
        // is a voyage, not a chord. Least-cost paths over the ocean cells
        // (the water-borne twin of the road A*), hugging coasts the way era
        // shipping did. The league rate stays cheap; the league count is
        // now honest, so a strait is worth something, a cape costs its
        // rounding, and a continent in the way is a continent in the way.
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
                // No sea road, or one so long no shipper would ply it —
                // then there is no lane, and the goods go overland or not
                // at all. Honesty is the whole point.
                let Some((cells, sailed)) =
                    sea_route(h, civ.settlements[a].cell, civ.settlements[b].cell, d * 6.0)
                else {
                    continue;
                };
                let mut pts = Vec::with_capacity(cells.len() + 2);
                pts.push(civ.settlements[a].pos);
                pts.extend(cells.iter().map(|&c| h.grid.cell_center(c)));
                pts.push(civ.settlements[b].pos);
                let pts = crate::civilization::chaikin(&pts);
                edges.push((a, b, sailed * 0.45));
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

        // ---- The manor roll: what the land itself yields. -----------------
        // The goods a place produces are already geography's verdict
        // (arable is grain, pasture wool, woodland timber, fishery fish),
        // so the roll rides on them: a rate per thirty heads, worked by
        // however many heads there are.
        let manor_income: Vec<u64> = (0..n)
            .map(|i| {
                let mut rate: u64 = 4; // subsistence tillage, everywhere
                for g in &produces[i] {
                    rate += match g {
                        Good::Grain | Good::Salt => 3,
                        Good::Fish | Good::Timber | Good::Wool | Good::Furs => 2,
                        Good::Iron | Good::Wine => 4,
                        Good::Spice => 5,
                    };
                }
                civ.settlements[i].population as u64 * rate / 30
            })
            .collect();

        // ---- Dues climb the tenure web: the third penny. ------------------
        // Each holding sends a third of everything that reaches it — its
        // own land's yield plus the dues of manors held of it — up to its
        // liege. Villages pay their town lord, town lords pay the crown,
        // and the crown keeps what survives the climb. Integer arithmetic
        // throughout: every mark in a ledger is some manor's mark, and the
        // conservation test holds to the last penny.
        let peers = planet.peerage();
        let index_of: HashMap<u32, usize> = civ
            .settlements
            .iter()
            .enumerate()
            .map(|(i, s)| (s.cell, i))
            .collect();
        let mut manor_sends = vec![0u64; n];
        let mut manor_receives = vec![0u64; n];
        // Villages settle up before their liege towns do; chains are two
        // rungs at most, so rank order is settlement order enough.
        for rank in [3u8, 2u8] {
            for i in 0..n {
                let s = &civ.settlements[i];
                if s.capital || s.kind.rank() != rank {
                    continue;
                }
                let Some(hold) = peers.holding(s.cell) else {
                    continue;
                };
                let due = (manor_income[i] + manor_receives[i]) / 3;
                manor_sends[i] = due;
                if let Some(&li) = index_of.get(&hold.liege_cell) {
                    manor_receives[li] += due;
                }
            }
        }
        let mut realm_ledger: HashMap<u32, u64> = HashMap::new();
        for (i, s) in civ.settlements.iter().enumerate() {
            if s.capital {
                realm_ledger.insert(s.cell, manor_income[i] + manor_receives[i]);
            }
        }

        Self {
            produces,
            imports,
            wanting,
            wealth,
            road_flows,
            lanes,
            manor_income,
            manor_sends,
            manor_receives,
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

/// Least-cost path over ocean cells between two coastal settlements'
/// harbors. Coastal water sails at face value; open sea costs a third
/// more, so routes hug the shore where the shore cooperates — era
/// navigation in one multiplier. Returns the water cells crossed and the
/// effective sailed cost, or None when no voyage under `budget` exists.
fn sea_route(
    h: &crate::Hydrology,
    from: u32,
    to: u32,
    budget: f64,
) -> Option<(Vec<u32>, f64)> {
    use std::cmp::Reverse;
    use std::collections::{BinaryHeap, HashMap, HashSet};

    let ocean = h.ocean_mask();
    let starts: Vec<u32> = h
        .adj(from as usize)
        .iter()
        .copied()
        .filter(|&c| ocean[c as usize])
        .collect();
    let goals: HashSet<u32> = h
        .adj(to as usize)
        .iter()
        .copied()
        .filter(|&c| ocean[c as usize])
        .collect();
    if starts.is_empty() || goals.is_empty() {
        return None;
    }
    let goal_centers: Vec<[f64; 3]> = goals.iter().map(|&c| h.grid.cell_center(c)).collect();
    let heur = |p: [f64; 3]| -> f64 {
        goal_centers
            .iter()
            .map(|&g| chord(p, g))
            .fold(f64::INFINITY, f64::min)
    };
    let open_sea = |c: u32| -> bool {
        h.adj(c as usize).iter().all(|&nb| ocean[nb as usize])
    };

    let mut g: HashMap<u32, f64> = HashMap::new();
    let mut came: HashMap<u32, u32> = HashMap::new();
    let mut open: BinaryHeap<Reverse<(K, u32)>> = BinaryHeap::new();
    for &s in &starts {
        g.insert(s, 0.0);
        open.push(Reverse((K(heur(h.grid.cell_center(s))), s)));
    }
    let mut expanded = 0usize;
    while let Some(Reverse((K(f), c))) = open.pop() {
        if goals.contains(&c) {
            let mut path = vec![c];
            let mut cur = c;
            while let Some(&p) = came.get(&cur) {
                path.push(p);
                cur = p;
            }
            path.reverse();
            return Some((path, g[&c]));
        }
        if f > budget {
            return None;
        }
        expanded += 1;
        if expanded > 60_000 {
            return None;
        }
        let gc = g[&c];
        let pc = h.grid.cell_center(c);
        for &nb in h.adj(c as usize) {
            if !ocean[nb as usize] {
                continue;
            }
            let pn = h.grid.cell_center(nb);
            let mult = if open_sea(nb) { 1.35 } else { 1.0 };
            let ng = gc + chord(pc, pn) * mult;
            if g.get(&nb).is_none_or(|&old| ng < old - 1e-12) {
                g.insert(nb, ng);
                came.insert(nb, c);
                open.push(Reverse((K(ng + heur(pn)), nb)));
            }
        }
    }
    None
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

#[inline]
fn chord(a: [f64; 3], b: [f64; 3]) -> f64 {
    let d = [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
}
