//! Phase 7: the peerage. Land is never just land — every settlement below
//! the capital is a holding, held by someone, of someone. The great houses
//! come out of the dynasty simulation: whenever a realm's line broke, the
//! deposed house kept its manors, and its disposition today depends on how
//! long it has had to swallow that. Town lords hold of the crown; village
//! manors go to cadet kinsmen of the town's house or to landed knights,
//! who hold of the town lord — a tenure web the chronicler treats as
//! canon, so the stories inherit the grudges the succession left behind.

use std::collections::HashMap;

use world_core::hash::{hash3, splitmix64};

use crate::history::person_name;
use crate::{Planet, SettlementKind, PRESENT_YEAR};

const STAGE_PEERAGE: u64 = 0x9E_E2A6;

pub struct House {
    pub name: String,
    /// The span of years this house held the realm's seat, if it ever did.
    pub held_seat: Option<(u32, u32)>,
    pub reigning: bool,
    pub disposition: String,
}

pub struct Holding {
    pub house: String,
    pub holder: String,
    pub title: &'static str,
    pub age: u32,
    /// Cell of the settlement whose lord this holder answers to; the
    /// capital means holding directly of the crown.
    pub liege_cell: u32,
    /// True when the holder is a kinsman of the liege town's house.
    pub cadet: bool,
}

pub struct Peerage {
    /// Capital cell → the realm's great houses.
    houses: HashMap<u32, Vec<House>>,
    /// Settlement cell → its holding (capitals excluded: the crown itself).
    holdings: HashMap<u32, Holding>,
}

impl Peerage {
    pub fn build(planet: &Planet) -> Self {
        let civ = planet.civilization();
        let hist = planet.history();
        let seed = splitmix64(planet.seed ^ STAGE_PEERAGE);

        let mut houses_by_realm: HashMap<u32, Vec<House>> = HashMap::new();
        let mut holdings: HashMap<u32, Holding> = HashMap::new();

        let capitals: Vec<&crate::Settlement> =
            civ.settlements.iter().filter(|s| s.capital).collect();
        for cap in capitals {
            let rh = hist.realm(cap.cell).expect("realm has history");
            let rs = splitmix64(seed ^ cap.cell as u64);

            // Group consecutive reigns into house tenures of the seat.
            let mut spans: Vec<(String, u32, u32)> = Vec::new();
            for r in &rh.rulers {
                match spans.last_mut() {
                    Some((name, _, end)) if *name == r.house => *end = r.death,
                    _ => spans.push((r.house.clone(), r.accession, r.death)),
                }
            }
            let reigning_house = spans.last().expect("dynasty exists").0.clone();

            // One entry per house name, keeping its full arc on the throne.
            let mut houses: Vec<House> = Vec::new();
            for (name, start, end) in &spans {
                if let Some(h) = houses.iter_mut().find(|h| &h.name == name) {
                    h.held_seat = h.held_seat.map(|(s, _)| (s, *end)).or(Some((*start, *end)));
                    continue;
                }
                let reigning = *name == reigning_house;
                let since_fall = PRESENT_YEAR.saturating_sub(*end);
                let disposition = if reigning {
                    "holds the seat".to_string()
                } else if since_fall < 80 {
                    let recent = ["still presses its claim", "openly restive", "barred from court"];
                    recent[(hash3(rs, name.len() as i64, 1, 0) % 3) as usize].to_string()
                } else {
                    let old = ["long reconciled", "proud and threadbare", "keeps to its lands"];
                    old[(hash3(rs, name.len() as i64, 2, 0) % 3) as usize].to_string()
                };
                houses.push(House {
                    name: name.clone(),
                    held_seat: Some((*start, *end)),
                    reigning,
                    disposition,
                });
            }
            // Pad the peerage with houses that never wore the crown.
            let mut k = 100i64;
            while houses.len() < 4 {
                let name = minor_house(rs, k);
                k += 1;
                if houses.iter().any(|h| h.name == name) {
                    continue;
                }
                let moods = [
                    "steadfast",
                    "ambitious beyond its rents",
                    "deep in debt",
                    "quietly pious",
                    "famously litigious",
                ];
                houses.push(House {
                    name,
                    held_seat: None,
                    reigning: false,
                    disposition: moods[(hash3(rs, k, 3, 0) % 5) as usize].to_string(),
                });
            }

            // ---- Holdings: towns hold of the crown; villages of a town.
            let members: Vec<&crate::Settlement> = civ
                .settlements
                .iter()
                .filter(|s| s.realm_capital == cap.cell && !s.capital)
                .collect();
            let towns: Vec<&&crate::Settlement> = members
                .iter()
                .filter(|s| s.kind == SettlementKind::Town)
                .collect();

            // Settlements are ordered city → town → village by placement,
            // so a village's liege town always has its holding already.
            for s in &members {
                let hs = hash3(rs, s.cell as i64, 4, 0);
                let (house, liege_cell, cadet, title_pair) = match s.kind {
                    SettlementKind::Town | SettlementKind::City => {
                        // A town seat goes to one of the great houses —
                        // deposed royal lines keep their lands.
                        let house = &houses[(hs % houses.len() as u64) as usize];
                        (house.name.clone(), cap.cell, false, ("Lord", "Lady"))
                    }
                    SettlementKind::Village => {
                        // Nearest town of the realm is the liege; failing
                        // that, the village holds directly of the crown.
                        let liege = towns
                            .iter()
                            .min_by(|a, b| {
                                chord(s.pos, a.pos).total_cmp(&chord(s.pos, b.pos))
                            })
                            .map(|t| t.cell)
                            .unwrap_or(cap.cell);
                        if hs % 5 < 3 {
                            // A cadet kinsman of the liege's own house.
                            let liege_house = holdings
                                .get(&liege)
                                .map(|h| h.house.clone())
                                .unwrap_or_else(|| reigning_house.clone());
                            (liege_house, liege, true, ("Ser", "Dame"))
                        } else {
                            (minor_house(rs, s.cell as i64), liege, false, ("Ser", "Dame"))
                        }
                    }
                };
                let female = (hs >> 8) % 10 < 3;
                holdings.insert(
                    s.cell,
                    Holding {
                        house,
                        holder: person_name(splitmix64(hs), female),
                        title: if female { title_pair.1 } else { title_pair.0 },
                        age: 30 + ((hs >> 16) % 24 + (hs >> 32) % 24) as u32 / 2 + 8,
                        liege_cell,
                        cadet,
                    },
                );
            }

            houses_by_realm.insert(cap.cell, houses);
        }

        Self {
            houses: houses_by_realm,
            holdings,
        }
    }

    pub fn houses(&self, capital_cell: u32) -> Option<&[House]> {
        self.houses.get(&capital_cell).map(|v| v.as_slice())
    }

    pub fn holding(&self, settlement_cell: u32) -> Option<&Holding> {
        self.holdings.get(&settlement_cell)
    }
}

fn minor_house(rs: u64, k: i64) -> String {
    const ONSETS: [&str; 12] = [
        "Ash", "Byrn", "Cor", "Dun", "Fal", "Grim", "Hol", "Mar", "Rook", "Stan", "Thorn", "Wex",
    ];
    const ENDS: [&str; 8] = ["ley", "wick", "combe", "worth", "den", "mere", "shaw", "by"];
    let h = hash3(rs, k, 5, 0);
    format!(
        "{}{}",
        ONSETS[(h % 12) as usize],
        ENDS[((h >> 8) % 8) as usize]
    )
}

#[inline]
fn chord(a: [f64; 3], b: [f64; 3]) -> f64 {
    let d = [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
}
