//! Phase 8: names on the land. The natural world, derived and named —
//! seas and gulfs from the ocean's own shape, mountain ranges from the
//! high ground the tectonics raised, forests and wastes from contiguous
//! biome patches, islands and continents from the landmasses themselves.
//! Every feature is a connected component over the same cube-sphere grid
//! hydrology solved, with a label axis fitted by principal component so
//! long ranges read lengthwise, and a stable place in the lore engine.

use world_core::hash::{hash3, splitmix64};

use crate::civilization::gen_name;
use crate::{classify_biome, Biome, Planet, LAPSE_C};

const STAGE_GEOGRAPHY: u64 = 0x6E_0217;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NaturalKind {
    Ocean,
    Sea,
    Gulf,
    Continent,
    Island,
    Range,
    Forest,
    Desert,
}

impl NaturalKind {
    pub fn word(self) -> &'static str {
        match self {
            NaturalKind::Ocean => "ocean",
            NaturalKind::Sea => "sea",
            NaturalKind::Gulf => "gulf",
            NaturalKind::Continent => "continent",
            NaturalKind::Island => "island",
            NaturalKind::Range => "mountain range",
            NaturalKind::Forest => "forest",
            NaturalKind::Desert => "desert",
        }
    }

    pub fn tag(self) -> &'static str {
        match self {
            NaturalKind::Ocean => "ocean",
            NaturalKind::Sea => "sea",
            NaturalKind::Gulf => "gulf",
            NaturalKind::Continent => "continent",
            NaturalKind::Island => "island",
            NaturalKind::Range => "range",
            NaturalKind::Forest => "forest",
            NaturalKind::Desert => "desert",
        }
    }
}

pub struct NaturalFeature {
    pub kind: NaturalKind,
    pub name: String,
    /// Smallest member cell — the component's stable anchor.
    pub anchor: u32,
    pub cells: u32,
    pub center: [f64; 3],
    /// Label axis endpoints, fitted along the component's long direction.
    pub axis: ([f64; 3], [f64; 3]),
    /// Elongation ×10 (10 = round, 30 = three times longer than wide).
    pub elong: u32,
    /// The lowest zoom at which the label should appear.
    pub min_zoom: u8,
}

pub struct Geography {
    pub features: Vec<NaturalFeature>,
    /// Per-cell feature indices (−1 = none): vegetation cover, relief,
    /// water body, landmass — so anything with a cell knows where it is.
    cover: Vec<i32>,
    relief: Vec<i32>,
    water: Vec<i32>,
    landmass: Vec<i32>,
}

impl Geography {
    pub fn build(planet: &Planet) -> Self {
        let h = planet.hydrology();
        let n = h.grid.cells();
        let seed = splitmix64(planet.seed ^ STAGE_GEOGRAPHY);

        let ocean: Vec<bool> = (0..n).map(|c| h.ocean[c]).collect();
        let elev = &h.elev;
        let temp: Vec<f64> = (0..n)
            .map(|c| h.t_sea[c] - LAPSE_C * elev[c].max(0.0))
            .collect();

        let mut features: Vec<NaturalFeature> = Vec::new();
        let mut cover = vec![-1i32; n];
        let mut relief = vec![-1i32; n];
        let mut water = vec![-1i32; n];
        let mut landmass = vec![-1i32; n];
        let mut used_stems = std::collections::HashSet::new();

        let register = |kind: NaturalKind,
                            comp: &[u32],
                            index_map: &mut Vec<i32>,
                            features: &mut Vec<NaturalFeature>,
                            used: &mut std::collections::HashSet<String>,
                            h: &crate::Hydrology| {
            let idx = features.len() as i32;
            for &c in comp {
                index_map[c as usize] = idx;
            }
            let anchor = *comp.iter().min().unwrap();
            let (center, axis, mut elong) = fit_axis(h, comp);
            // Planet-scale features read better as spread text at a point.
            if matches!(kind, NaturalKind::Ocean | NaturalKind::Continent) {
                elong = 10;
            }
            let cells = comp.len() as u32;
            features.push(NaturalFeature {
                kind,
                name: feature_name(seed, kind, anchor, used),
                anchor,
                cells,
                center,
                axis,
                elong,
                min_zoom: min_zoom_for(kind, cells),
            });
        };

        // ---- Water: the ocean, lesser seas, and the gulfs cut into them.
        let mut wet = components(h, |c| ocean[c]);
        wet.sort_by_key(|c| std::cmp::Reverse(c.len()));
        for (i, comp) in wet.iter().enumerate() {
            if comp.len() < 20 {
                continue;
            }
            let kind = if i == 0 { NaturalKind::Ocean } else { NaturalKind::Sea };
            register(kind, comp, &mut water, &mut features, &mut used_stems, h);
        }
        // Gulfs: ocean cells whose two-ring neighborhood is mostly land.
        let gulfish: Vec<bool> = (0..n)
            .map(|c| {
                if !ocean[c] {
                    return false;
                }
                let (mut land, mut total) = (0u32, 0u32);
                for &a in h.adj(c) {
                    total += 1;
                    if !ocean[a as usize] {
                        land += 1;
                    }
                    for &b in h.adj(a as usize) {
                        total += 1;
                        if !ocean[b as usize] {
                            land += 1;
                        }
                    }
                }
                land * 100 >= total * 38
            })
            .collect();
        for comp in components(h, |c| gulfish[c]) {
            if comp.len() >= 5 {
                register(NaturalKind::Gulf, &comp, &mut water, &mut features, &mut used_stems, h);
            }
        }

        // ---- Land masses: continents and isles.
        for comp in components(h, |c| !ocean[c]) {
            if comp.len() < 6 {
                continue; // skerries stay nameless
            }
            let kind = if comp.len() > 2000 {
                NaturalKind::Continent
            } else {
                NaturalKind::Island
            };
            register(kind, &comp, &mut landmass, &mut features, &mut used_stems, h);
        }

        // ---- Relief: the ranges.
        for comp in components(h, |c| !ocean[c] && elev[c] > 0.32) {
            if comp.len() >= 10 {
                register(NaturalKind::Range, &comp, &mut relief, &mut features, &mut used_stems, h);
            }
        }

        // ---- Cover: forests and deserts.
        let forestish = |b: Biome| {
            matches!(
                b,
                Biome::Boreal
                    | Biome::TemperateForest
                    | Biome::TemperateRainforest
                    | Biome::TropicalForest
                    | Biome::TropicalRainforest
            )
        };
        let biome: Vec<Biome> = (0..n)
            .map(|c| classify_biome(temp[c], h.precip[c]))
            .collect();
        for comp in components(h, |c| !ocean[c] && forestish(biome[c])) {
            if comp.len() >= 60 {
                register(NaturalKind::Forest, &comp, &mut cover, &mut features, &mut used_stems, h);
            }
        }
        for comp in components(h, |c| !ocean[c] && biome[c] == Biome::Desert) {
            if comp.len() >= 60 {
                register(NaturalKind::Desert, &comp, &mut cover, &mut features, &mut used_stems, h);
            }
        }

        Self {
            features,
            cover,
            relief,
            water,
            landmass,
        }
    }

    pub fn cover_at(&self, cell: u32) -> Option<&NaturalFeature> {
        self.lookup(&self.cover, cell)
    }
    pub fn relief_at(&self, cell: u32) -> Option<&NaturalFeature> {
        self.lookup(&self.relief, cell)
    }
    pub fn water_at(&self, cell: u32) -> Option<&NaturalFeature> {
        self.lookup(&self.water, cell)
    }
    pub fn landmass_at(&self, cell: u32) -> Option<&NaturalFeature> {
        self.lookup(&self.landmass, cell)
    }

    fn lookup(&self, map: &[i32], cell: u32) -> Option<&NaturalFeature> {
        let i = *map.get(cell as usize)?;
        (i >= 0).then(|| &self.features[i as usize])
    }

    /// The named water a harbor opens onto: the gulf/sea/ocean of the
    /// first ocean cell adjacent to this coastal cell.
    pub fn harbor_water<'a>(&'a self, planet: &Planet, cell: u32) -> Option<&'a NaturalFeature> {
        let h = planet.hydrology();
        h.adj(cell as usize)
            .iter()
            .find_map(|&a| self.lookup(&self.water, a))
    }

    /// The nearest range to a point, if any lies within `max_chord`.
    pub fn nearest_range(&self, pos: [f64; 3], max_chord: f64) -> Option<&NaturalFeature> {
        self.features
            .iter()
            .filter(|f| f.kind == NaturalKind::Range)
            .map(|f| (chord(pos, f.center), f))
            .filter(|(d, _)| *d <= max_chord)
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, f)| f)
    }
}

/// Connected components over the grid, by predicate.
fn components(h: &crate::Hydrology, pred: impl Fn(usize) -> bool) -> Vec<Vec<u32>> {
    let n = h.grid.cells();
    let mut seen = vec![false; n];
    let mut out = Vec::new();
    let mut stack = Vec::new();
    for start in 0..n {
        if seen[start] || !pred(start) {
            continue;
        }
        let mut comp = Vec::new();
        seen[start] = true;
        stack.push(start as u32);
        while let Some(c) = stack.pop() {
            comp.push(c);
            for &a in h.adj(c as usize) {
                let ai = a as usize;
                if !seen[ai] && pred(ai) {
                    seen[ai] = true;
                    stack.push(a);
                }
            }
        }
        out.push(comp);
    }
    out
}

/// Centroid plus a principal-axis label line in the tangent plane, so long
/// features are labeled along their length.
fn fit_axis(h: &crate::Hydrology, comp: &[u32]) -> ([f64; 3], ([f64; 3], [f64; 3]), u32) {
    let mut m = [0.0f64; 3];
    for &c in comp {
        let p = h.grid.cell_center(c);
        m = [m[0] + p[0], m[1] + p[1], m[2] + p[2]];
    }
    let m = normalize(m);
    let up = if m[2].abs() < 0.9 { [0.0, 0.0, 1.0] } else { [1.0, 0.0, 0.0] };
    let e1 = normalize(cross(m, up));
    let e2 = cross(m, e1);

    let (mut su, mut sv, mut suu, mut svv, mut suv) = (0.0, 0.0, 0.0, 0.0, 0.0);
    for &c in comp {
        let p = h.grid.cell_center(c);
        let (u, v) = (dot(p, e1), dot(p, e2));
        su += u;
        sv += v;
        suu += u * u;
        svv += v * v;
        suv += u * v;
    }
    let k = comp.len() as f64;
    let (mu, mv) = (su / k, sv / k);
    let (cuu, cvv, cuv) = (suu / k - mu * mu, svv / k - mv * mv, suv / k - mu * mv);
    let theta = 0.5 * (2.0 * cuv).atan2(cuu - cvv);
    let (ct, st) = (theta.cos(), theta.sin());
    let l1 = (cuu * ct * ct + 2.0 * cuv * ct * st + cvv * st * st).max(1e-12);
    let l2 = (cuu + cvv - l1).max(1e-12);
    // Cap the label line: past ~2000 km an axis stops being a label and
    // becomes a great-circle tour.
    let half = (1.4 * l1.sqrt()).min(0.32);
    let dir = [
        e1[0] * ct + e2[0] * st,
        e1[1] * ct + e2[1] * st,
        e1[2] * ct + e2[2] * st,
    ];
    let a = normalize([m[0] - dir[0] * half, m[1] - dir[1] * half, m[2] - dir[2] * half]);
    let b = normalize([m[0] + dir[0] * half, m[1] + dir[1] * half, m[2] + dir[2] * half]);
    let elong = ((l1 / l2).sqrt() * 10.0).clamp(10.0, 80.0) as u32;
    (m, (a, b), elong)
}

fn min_zoom_for(kind: NaturalKind, cells: u32) -> u8 {
    match kind {
        NaturalKind::Ocean | NaturalKind::Continent => 0,
        NaturalKind::Sea | NaturalKind::Desert => 2,
        NaturalKind::Range => {
            if cells >= 40 {
                2
            } else {
                3
            }
        }
        NaturalKind::Gulf => 3,
        NaturalKind::Forest => {
            if cells >= 150 {
                3
            } else {
                4
            }
        }
        NaturalKind::Island => {
            if cells >= 100 {
                3
            } else {
                4
            }
        }
    }
}

fn feature_name(
    seed: u64,
    kind: NaturalKind,
    anchor: u32,
    used: &mut std::collections::HashSet<String>,
) -> String {
    for attempt in 0..8i64 {
        let hn = hash3(seed, anchor as i64, attempt, 0);
        let stem = gen_name(splitmix64(hn), false, false);
        if !used.insert(stem.clone()) {
            continue;
        }
        let pick = (hn >> 24) % 3;
        return match kind {
            NaturalKind::Ocean => format!("the {stem} Ocean"),
            NaturalKind::Sea => format!("the Sea of {stem}"),
            NaturalKind::Gulf => {
                if pick == 0 {
                    format!("{stem} Bay")
                } else {
                    format!("the Gulf of {stem}")
                }
            }
            NaturalKind::Continent => stem,
            NaturalKind::Island => {
                if pick == 0 {
                    format!("the Isle of {stem}")
                } else {
                    format!("{stem} Isle")
                }
            }
            NaturalKind::Range => ["the {} Range", "the {} Mountains", "the {} Peaks"]
                [pick as usize]
                .replace("{}", &stem),
            NaturalKind::Forest => match pick {
                0 => format!("{stem}wood"),
                1 => format!("the {stem} Forest"),
                _ => format!("the Woods of {stem}"),
            },
            NaturalKind::Desert => ["the {} Waste", "the {} Sands", "the {} Expanse"]
                [pick as usize]
                .replace("{}", &stem),
        };
    }
    format!("the Unnamed ({anchor})")
}

/// A rough breadth in leagues for the lore context (a league being three
/// miles of hard walking).
pub fn breadth_leagues(cells: u32) -> u32 {
    ((cells as f64 * 1500.0).sqrt() / 4.8).round() as u32
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
fn chord(a: [f64; 3], b: [f64; 3]) -> f64 {
    let d = [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
}

// unit_to_lat_lon re-exported use keeps the import meaningful for callers
// that pull label positions out of features.
pub use world_core::geo::unit_to_lat_lon as label_lat_lon;
