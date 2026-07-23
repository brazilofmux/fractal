//! Phase 11: the fourth coordinate. The present every other phase describes
//! is year 500 of the annals; these functions answer the same questions for
//! any year — when a settlement rose, how many lived there, whose realm it
//! answered to — as pure functions of (seed, place, year). The iron rule:
//! year is an input, never a state. Nothing here is stored or simulated
//! forward; the present is canon, and the walk backward from it through
//! the wars both realms already agree on IS the history.

use world_core::hash::{hash3, splitmix64};

use crate::{Planet, SettlementKind, PRESENT_YEAR};

const STAGE_TIME: u64 = 0xF0_4C11;

/// How near the rival capital may be, relative to the home capital, for a
/// settlement to sit in the border band that wars are fought over.
const BORDER_RATIO: f64 = 1.7;

/// Years a plague's losses take to grow back.
const PLAGUE_RECOVERY_YEARS: f64 = 60.0;

/// The year a settlement was founded. Capitals rise with their realm;
/// towns follow within living memory of the founding; villages fill in
/// over the centuries. Everything stands well before the present.
pub fn founded_in(planet: &Planet, i: usize) -> u32 {
    let civ = planet.civilization();
    let s = &civ.settlements[i];
    let base = planet
        .history()
        .realm(s.realm_capital)
        .map(|r| r.founding_year)
        .unwrap_or(20);
    if s.capital {
        return base;
    }
    let h = hash3(splitmix64(planet.seed ^ STAGE_TIME), s.cell as i64, 1, 0);
    let span = match s.kind {
        SettlementKind::City | SettlementKind::Town => 120,
        SettlementKind::Village => 300,
    };
    (base + (h % span) as u32).min(470)
}

/// The capital cell of the realm that held a settlement in a given year,
/// or None before the settlement existed. The present allegiance is canon;
/// each decisive war between the home realm and its nearest rival handed
/// the border band to its victor, so walking those wars backward from
/// today yields the whole story — including ground that changed hands
/// more than once.
pub fn realm_in(planet: &Planet, i: usize, year: u32) -> Option<u32> {
    let civ = planet.civilization();
    let hist = planet.history();
    let s = &civ.settlements[i];
    if year < founded_in(planet, i) {
        return None;
    }
    let a = s.realm_capital;
    if s.capital || year >= PRESENT_YEAR {
        return Some(a);
    }
    // The rival: the second-nearest capital. Only the border band between
    // the two is worth marching for.
    let d_a = civ
        .settlements
        .iter()
        .find(|c| c.cell == a)
        .map(|c| chord(s.pos, c.pos))
        .unwrap_or(0.0);
    let (d_b, b) = civ
        .settlements
        .iter()
        .filter(|c| c.capital && c.cell != a)
        .map(|c| (chord(s.pos, c.pos), c.cell))
        .min_by(|x, y| x.0.total_cmp(&y.0))?;
    if d_b > d_a * BORDER_RATIO {
        return Some(a);
    }
    // Each decisive war handed the band to its victor, so the owner in any
    // year is the victor of the last war already ended — provided the whole
    // sequence lands on the present allegiance, which is canon. A sequence
    // that would end elsewhere never moved this ground at all. (A victor
    // must also have existed to win: wars the annals date before the
    // rival's founding cannot deliver villages to it.)
    let mut between: Vec<&crate::history::War> = hist
        .wars
        .iter()
        .filter(|w| {
            w.victor.is_some_and(|v| {
                hist.realm(v).is_some_and(|r| r.founding_year <= w.end)
            }) && ((w.a == a && w.b == b) || (w.a == b && w.b == a))
        })
        .collect();
    between.sort_by_key(|w| w.end);
    if between.last().map(|w| w.victor) != Some(Some(a)) {
        return Some(a);
    }
    let mut owner = a;
    for w in between.iter().filter(|w| w.end <= year) {
        owner = w.victor.expect("decisive wars only");
    }
    Some(owner)
}

/// Head count in a given year: growth from a modest founding population,
/// cut down by each plague that reached whichever realm held the place
/// that year and rebuilt over the two generations after — normalized so
/// the present count is exactly the canon one.
pub fn population_in(planet: &Planet, i: usize, year: u32) -> u32 {
    let civ = planet.civilization();
    let s = &civ.settlements[i];
    let f = founded_in(planet, i);
    if year < f {
        return 0;
    }
    let curve = |y: u32| -> f64 {
        let t = (y - f) as f64 / (PRESENT_YEAR - f) as f64;
        let mut v = 0.28f64.powf(1.0 - t);
        if let Some(rh) = realm_in(planet, i, y).and_then(|cap| planet.history().realm(cap)) {
            for p in rh.plagues.iter().filter(|p| p.arrival <= y) {
                let toll = p.toll_pct as f64 / 100.0;
                let regrown = ((y - p.arrival) as f64 / PLAGUE_RECOVERY_YEARS).min(1.0);
                v *= 1.0 - toll * (1.0 - regrown);
            }
        }
        v
    };
    (s.population as f64 * curve(year) / curve(PRESENT_YEAR))
        .round()
        .max(1.0) as u32
}

#[inline]
fn chord(a: [f64; 3], b: [f64; 3]) -> f64 {
    let d = [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
}
