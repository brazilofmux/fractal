//! Phase 12: street level. The city arrives the way the planet always has —
//! as a function, not a file. Each settlement's ward layout is a tiny
//! spherical Voronoi (the same trick the tectonic plates use, at one
//! millionth the scale): named ward seats scattered by positional hash,
//! streets where two wards' claims meet, walls as a distance contour with
//! gates where the real Phase-5 roads cross it, the market square at the
//! heart, the quay where the harborside meets the water. Nothing is stored;
//! `ground_at` answers per pixel at render time, so the same city exists at
//! every zoom from orbit down to the cobbles.

use world_core::hash::{hash3, splitmix64};

use crate::{interior, Planet};

const STAGE_CITY: u64 = 0x57_2EE7;

/// City ground begins to render at this zoom; above it the dot suffices.
pub const GROUND_MIN_ZOOM: u32 = 12;
/// Ward names and room click-targets appear from this zoom.
pub const CITY_LAYER_MIN_ZOOM: u32 = 13;

const KM: f64 = 1.0 / 6371.0;
/// Cull margin for tile renderers: no city footprint exceeds this.
pub const MAX_RADIUS: f64 = 1.3 * KM;

const STREET_W: f64 = 0.008 * KM;
const AVENUE_W: f64 = 0.012 * KM;
const WALL_T: f64 = 0.006 * KM;
const GATE_HALF: f64 = 0.015 * KM;
/// Building-plot grid pitch (one burgage plot / roof per cell).
pub const PLOT_GRID: f64 = 0.016 * KM;

pub struct WardSeat {
    pub name: String,
    pub kind: &'static str,
    pub pos: [f64; 3],
}

pub struct Gate {
    pub name: String,
    pub pos: [f64; 3],
}

pub struct CityPlan {
    pub settlement: usize,
    pub center: [f64; 3],
    /// Footprint radius in chord units (≈ radians).
    pub radius: f64,
    /// Wall radius; villages go unwalled.
    pub wall: Option<f64>,
    pub square: [f64; 3],
    pub square_r: f64,
    /// The ward a visitor enters by: the market (or the village green).
    pub entry: usize,
    pub wards: Vec<WardSeat>,
    pub gates: Vec<Gate>,
    /// The inn and the ward it stands in.
    pub inn: Option<(String, usize)>,
    /// Ward adjacency (Voronoi neighbors) — the walkable street graph.
    pub adjacency: Vec<(usize, usize)>,
    east: [f64; 3],
    north: [f64; 3],
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum GroundKind {
    Plot,
    Street,
    Wall,
    Square,
    Quay,
    Green,
}

#[derive(Clone, Copy)]
pub struct Ground {
    pub ward: usize,
    pub kind: GroundKind,
}

/// Footprint radius from head count: medieval densities pack thousands
/// into each square kilometer inside the walls; hamlets get a floor so
/// they are visible at all.
pub fn radius_of(population: u32) -> f64 {
    (population as f64 / 28_000.0).sqrt().max(0.09) * KM
}

/// The deterministic town plan of a settlement, derived on demand.
/// Positions come ashore at placement (Phase 13b), so the settlement's own
/// position is the plan's center — one anchor for dots, roads and wards.
pub fn plan(planet: &Planet, i: usize) -> CityPlan {
    let civ = planet.civilization();
    let h = planet.hydrology();
    let s = &civ.settlements[i];
    let seed = splitmix64(planet.seed ^ STAGE_CITY ^ s.cell as u64);
    let center = s.pos;
    let radius = radius_of(s.population);
    let walled = s.population > 900;
    let wall_r = radius * 0.82;

    // Local compass: east and north tangents at the center.
    let east = normalize(cross([0.0, 0.0, 1.0], center));
    let north = cross(center, east);
    let at = |bearing: f64, d: f64| -> [f64; 3] {
        normalize([
            center[0] + d * (bearing.sin() * east[0] + bearing.cos() * north[0]),
            center[1] + d * (bearing.sin() * east[1] + bearing.cos() * north[1]),
            center[2] + d * (bearing.sin() * east[2] + bearing.cos() * north[2]),
        ])
    };
    let bearing_of = |q: [f64; 3]| -> f64 {
        let d = [q[0] - center[0], q[1] - center[1], q[2] - center[2]];
        dot(d, east).atan2(dot(d, north))
    };

    // The harbor lies toward the nearest ocean water.
    let harbor_bearing = h
        .adj(s.cell as usize)
        .iter()
        .find(|&&nb| h.ocean_mask()[nb as usize])
        .map(|&nb| bearing_of(h.grid.cell_center(nb)))
        .unwrap_or_else(|| unit_f64(hash3(seed, 0, 9, 0)) * std::f64::consts::TAU);

    // ---- Ward seats. Interior wards are the canon list; villages get a
    // green and a lane so every settlement can be walked.
    let inside = interior(planet, i);
    let named: Vec<(String, &'static str)> = if inside.wards.is_empty() {
        let mut v = vec![("the Green".to_string(), "green"), ("the Lane".to_string(), "lane")];
        if inside.notables.iter().any(|n| n.role.contains("priest")) {
            v.push(("the Church End".to_string(), "church"));
        }
        v
    } else {
        inside
            .wards
            .iter()
            .map(|w| (w.name.clone(), w.kind))
            .collect()
    };

    let mut wards: Vec<WardSeat> = Vec::with_capacity(named.len());
    for (k, (name, kind)) in named.iter().enumerate() {
        let hk = hash3(seed, k as i64, 1, 0);
        let (lo, hi) = radial_band(kind);
        let d = (lo + (hi - lo) * unit_f64(hk)) * radius;
        // Compass wards sit where their name points; the harborside faces
        // the water; everyone else takes a hashed bearing.
        let bearing = match compass_bearing(name) {
            Some(b) => b,
            None if *kind == "harborside" => harbor_bearing,
            None => unit_f64(splitmix64(hk)) * std::f64::consts::TAU,
        };
        wards.push(WardSeat {
            name: name.clone(),
            kind,
            pos: at(bearing, d),
        });
    }

    // The market ward (or the green) anchors the square.
    let host = wards
        .iter()
        .position(|w| w.kind == "market")
        .or_else(|| wards.iter().position(|w| w.kind == "green"))
        .unwrap_or(0);
    let square = wards[host].pos;
    let square_r = (radius * 0.09).clamp(0.025 * KM, 0.055 * KM);

    // ---- Gates: where the real roads cross the wall, plus any compass
    // gate the interior already named. Nearby bearings merge.
    let mut gates: Vec<Gate> = Vec::new();
    if walled {
        let mut bearings: Vec<(f64, Option<String>)> = wards
            .iter()
            .filter(|w| w.kind == "gate")
            .map(|w| (bearing_of(w.pos), Some(w.name.clone())))
            .collect();
        for r in &civ.roads {
            let (a, b) = (r.a as usize, r.b as usize);
            if a != i && b != i {
                continue;
            }
            let pts: Vec<[f64; 3]> = if a == i {
                r.pts.iter().copied().collect()
            } else {
                r.pts.iter().rev().copied().collect()
            };
            if let Some(&q) = pts.iter().find(|&&q| chord(q, center) > wall_r * 1.15) {
                let rb = bearing_of(q);
                if !bearings
                    .iter()
                    .any(|(b, _)| angle_between(*b, rb) < 0.45)
                {
                    bearings.push((rb, None));
                }
            }
        }
        bearings.sort_by(|a, b| a.0.total_cmp(&b.0));
        for (b, name) in bearings.into_iter().take(5) {
            gates.push(Gate {
                name: name.unwrap_or_else(|| octant_gate_name(b)),
                pos: at(b, wall_r),
            });
        }
        // Even a town no road has found keeps one gate for its own fields.
        if gates.is_empty() {
            let b = unit_f64(hash3(seed, 0, 3, 0)) * std::f64::consts::TAU;
            gates.push(Gate {
                name: octant_gate_name(b),
                pos: at(b, wall_r),
            });
        }
    }

    // ---- Adjacency: Voronoi neighbors by the midpoint test, stitched
    // connected so every ward can be walked to from every other.
    let mut adjacency: Vec<(usize, usize)> = Vec::new();
    for a in 0..wards.len() {
        for b in a + 1..wards.len() {
            let m = normalize(mid(wards[a].pos, wards[b].pos));
            let mut best = (usize::MAX, f64::MAX);
            let mut second = (usize::MAX, f64::MAX);
            for (k, w) in wards.iter().enumerate() {
                let d = chord(m, w.pos);
                if d < best.1 {
                    second = best;
                    best = (k, d);
                } else if d < second.1 {
                    second = (k, d);
                }
            }
            let pair = (best.0.min(second.0), best.0.max(second.0));
            if pair == (a, b) {
                adjacency.push((a, b));
            }
        }
    }
    stitch_connected(&wards, &mut adjacency);

    CityPlan {
        settlement: i,
        center,
        radius,
        wall: walled.then_some(wall_r),
        square,
        square_r,
        entry: host,
        wards,
        gates,
        inn: inside.inn.clone().map(|name| (name, host)),
        adjacency,
        east,
        north,
    }
}

impl CityPlan {
    /// What is underfoot at a point, or None where the town gives way to
    /// open country. Call only for land pixels — water stays water.
    pub fn ground_at(&self, p: [f64; 3], elevation: f64) -> Option<Ground> {
        let d = chord(p, self.center);
        if d > self.radius {
            return None;
        }
        let ward = self.ward_at(p);

        if let Some(wall_r) = self.wall {
            // The wall ring, pierced at the gates.
            if (d - wall_r).abs() < WALL_T {
                let in_gate = self.gates.iter().any(|g| chord(p, g.pos) < GATE_HALF);
                return Some(Ground {
                    ward,
                    kind: if in_gate {
                        GroundKind::Street
                    } else {
                        GroundKind::Wall
                    },
                });
            }
            // Beyond the wall only the roads to the gates are town ground.
            if d > wall_r {
                let on_road = self.gates.iter().any(|g| {
                    self.near_segment(p, g.pos, self.gate_approach(g), AVENUE_W)
                });
                return on_road.then_some(Ground {
                    ward,
                    kind: GroundKind::Street,
                });
            }
        }

        // The square is open ground; nothing is built on it.
        if chord(p, self.square) < self.square_r {
            return Some(Ground {
                ward,
                kind: GroundKind::Square,
            });
        }
        // Avenues run from the square to each gate.
        for g in &self.gates {
            if self.near_segment(p, self.square, g.pos, AVENUE_W) {
                return Some(Ground {
                    ward,
                    kind: GroundKind::Street,
                });
            }
        }
        // Streets are where two wards' claims meet; all else is plots.
        let (d1, d2) = self.two_nearest(p);
        if d2 - d1 < STREET_W {
            return Some(Ground {
                ward,
                kind: GroundKind::Street,
            });
        }
        let kind = match self.wards[ward].kind {
            "green" => GroundKind::Green,
            "harborside" if elevation < 0.0025 => GroundKind::Quay,
            _ => GroundKind::Plot,
        };
        Some(Ground { ward, kind })
    }

    pub fn ward_at(&self, p: [f64; 3]) -> usize {
        let mut best = (0usize, f64::MAX);
        for (k, w) in self.wards.iter().enumerate() {
            let d = chord(p, w.pos);
            if d < best.1 {
                best = (k, d);
            }
        }
        best.0
    }

    fn two_nearest(&self, p: [f64; 3]) -> (f64, f64) {
        let (mut d1, mut d2) = (f64::MAX, f64::MAX);
        for w in &self.wards {
            let d = chord(p, w.pos);
            if d < d1 {
                d2 = d1;
                d1 = d;
            } else if d < d2 {
                d2 = d;
            }
        }
        (d1, d2)
    }

    /// Where the inn's sign hangs: a few doors off its host ward's seat,
    /// so the two labels never collide.
    pub fn inn_pos(&self) -> Option<[f64; 3]> {
        let (_, host) = self.inn.as_ref()?;
        let hp = self.wards[*host].pos;
        let d = (0.12 * self.radius).max(0.02 * KM);
        Some(normalize([
            hp[0] + d * (self.east[0] + 0.4 * self.north[0]),
            hp[1] + d * (self.east[1] + 0.4 * self.north[1]),
            hp[2] + d * (self.east[2] + 0.4 * self.north[2]),
        ]))
    }

    /// Local planar coordinates (chord units) for tiny-scale geometry.
    pub fn local(&self, p: [f64; 3]) -> (f64, f64) {
        let d = [
            p[0] - self.center[0],
            p[1] - self.center[1],
            p[2] - self.center[2],
        ];
        (dot(d, self.east), dot(d, self.north))
    }

    /// Where a gate's road runs on to: straight out to the footprint edge.
    fn gate_approach(&self, g: &Gate) -> [f64; 3] {
        let d = [
            g.pos[0] - self.center[0],
            g.pos[1] - self.center[1],
            g.pos[2] - self.center[2],
        ];
        let len = (dot(d, d)).sqrt().max(1e-12);
        let f = self.radius / len;
        normalize([
            self.center[0] + d[0] * f,
            self.center[1] + d[1] * f,
            self.center[2] + d[2] * f,
        ])
    }

    fn near_segment(&self, p: [f64; 3], a: [f64; 3], b: [f64; 3], half_w: f64) -> bool {
        let (px, py) = self.local(p);
        let (ax, ay) = self.local(a);
        let (bx, by) = self.local(b);
        let (dx, dy) = (bx - ax, by - ay);
        let len2 = dx * dx + dy * dy;
        let t = if len2 > 1e-24 {
            (((px - ax) * dx + (py - ay) * dy) / len2).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let (cx, cy) = (ax + t * dx - px, ay + t * dy - py);
        (cx * cx + cy * cy).sqrt() < half_w
    }
}

/// The rooms of a settlement: every ward, plus the inn standing in its
/// host ward. Room `k` for `k < wards.len()` is the ward itself; the inn,
/// if any, is room `wards.len()`.
pub struct Room {
    pub k: usize,
    pub name: String,
    pub kind: String,
}

pub fn rooms(plan: &CityPlan) -> Vec<Room> {
    let mut out: Vec<Room> = plan
        .wards
        .iter()
        .enumerate()
        .map(|(k, w)| Room {
            k,
            name: w.name.clone(),
            kind: w.kind.to_string(),
        })
        .collect();
    if let Some((name, _)) = &plan.inn {
        out.push(Room {
            k: plan.wards.len(),
            name: name.clone(),
            kind: "inn".to_string(),
        });
    }
    out
}

/// Resolve a room id to (settlement index, room), if it exists.
pub fn room_at(planet: &Planet, cell: u32, k: usize) -> Option<(usize, Room)> {
    let civ = planet.civilization();
    let i = civ.settlements.iter().position(|s| s.cell == cell)?;
    let p = plan(planet, i);
    rooms(&p).into_iter().find(|r| r.k == k).map(|r| (i, r))
}

/// Walkable exits of a room: Voronoi neighbors for wards; the inn opens
/// onto its host ward and nowhere else.
pub fn exits(plan: &CityPlan, k: usize) -> Vec<usize> {
    let inn_k = plan.wards.len();
    let mut out = Vec::new();
    if k == inn_k {
        if let Some((_, host)) = &plan.inn {
            out.push(*host);
        }
        return out;
    }
    for &(a, b) in &plan.adjacency {
        if a == k {
            out.push(b);
        } else if b == k {
            out.push(a);
        }
    }
    if let Some((_, host)) = &plan.inn {
        if *host == k {
            out.push(inn_k);
        }
    }
    out.sort_unstable();
    out
}

/// The ward a person keeps to, by their role: the harbormaster is on the
/// quay, the castellan in the castle ward, the innkeeper at the inn.
pub fn room_of_role(plan: &CityPlan, role: &str, slot: u8) -> usize {
    let find = |kinds: &[&str]| -> Option<usize> {
        kinds
            .iter()
            .find_map(|k| plan.wards.iter().position(|w| w.kind == *k))
    };
    let inn_k = plan.wards.len();
    let picked = if role.contains("innkeeper") || role.contains("alewife") {
        plan.inn.as_ref().map(|_| inn_k)
    } else if role.contains("harbormaster") {
        find(&["harborside"])
    } else if role.contains("castellan") || role.contains("watch") || role.contains("King")
        || role.contains("Queen")
    {
        find(&["military", "gate"])
    } else if role.contains("guild") || role.contains("smith") {
        find(&["craftsmen", "lane"])
    } else if role.contains("miller") {
        find(&["riverside", "lane"])
    } else if role.contains("priest") {
        find(&["church", "administration"])
    } else if role.contains("mayor") || role.contains("reeve") {
        find(&["market", "green"])
    } else if role.contains("holder of") || role.contains("Lord") || role.contains("Lady") {
        find(&["patriciate", "military", "market"])
    } else {
        None
    };
    picked.unwrap_or_else(|| slot as usize % plan.wards.len())
}

fn radial_band(kind: &str) -> (f64, f64) {
    match kind {
        "market" | "green" => (0.06, 0.22),
        "patriciate" | "administration" | "merchant" => (0.25, 0.45),
        "military" => (0.30, 0.55),
        "craftsmen" | "riverside" | "church" | "lane" => (0.38, 0.62),
        "odoriferous businesses" | "slum" => (0.60, 0.78),
        "gate" => (0.74, 0.78),
        "harborside" => (0.80, 0.90),
        _ => (0.30, 0.65),
    }
}

fn compass_bearing(name: &str) -> Option<f64> {
    use std::f64::consts::PI;
    match name {
        n if n.starts_with("North") => Some(0.0),
        n if n.starts_with("East") => Some(PI / 2.0),
        n if n.starts_with("South") => Some(PI),
        n if n.starts_with("West") => Some(-PI / 2.0),
        _ => None,
    }
}

fn octant_gate_name(bearing: f64) -> String {
    const WINDS: [&str; 8] = [
        "North", "Northeast", "East", "Southeast", "South", "Southwest", "West", "Northwest",
    ];
    let deg = bearing.to_degrees().rem_euclid(360.0);
    format!("the {} Gate", WINDS[((deg + 22.5) / 45.0) as usize % 8])
}

fn angle_between(a: f64, b: f64) -> f64 {
    use std::f64::consts::{PI, TAU};
    ((a - b + PI).rem_euclid(TAU) - PI).abs()
}

/// Add the cheapest edges needed to make the ward graph one component.
fn stitch_connected(wards: &[WardSeat], adjacency: &mut Vec<(usize, usize)>) {
    let n = wards.len();
    if n == 0 {
        return;
    }
    loop {
        let mut seen = vec![false; n];
        let mut stack = vec![0usize];
        seen[0] = true;
        while let Some(c) = stack.pop() {
            for &(a, b) in adjacency.iter() {
                let o = if a == c { b } else if b == c { a } else { continue };
                if !seen[o] {
                    seen[o] = true;
                    stack.push(o);
                }
            }
        }
        let Some(orphan) = (0..n).find(|&k| !seen[k]) else {
            return;
        };
        // Closest reached ward becomes the orphan's neighbor.
        let best = (0..n)
            .filter(|&k| seen[k])
            .min_by(|&x, &y| {
                chord(wards[orphan].pos, wards[x].pos)
                    .total_cmp(&chord(wards[orphan].pos, wards[y].pos))
            })
            .expect("component is non-empty");
        adjacency.push((orphan.min(best), orphan.max(best)));
    }
}

// ---- Small math ------------------------------------------------------

#[inline]
fn chord(a: [f64; 3], b: [f64; 3]) -> f64 {
    let d = [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
}

#[inline]
fn mid(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        0.5 * (a[0] + b[0]),
        0.5 * (a[1] + b[1]),
        0.5 * (a[2] + b[2]),
    ]
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
    let inv = 1.0 / (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    [v[0] * inv, v[1] * inv, v[2] * inv]
}

#[inline]
fn unit_f64(h: u64) -> f64 {
    (h >> 11) as f64 / (1u64 << 53) as f64
}
