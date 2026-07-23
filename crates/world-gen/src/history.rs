//! Phase 7: history. Five hundred years of deterministic annals, simulated
//! realm by realm from the seed — dynasties whose rulers live and die by a
//! Gompertz-Makeham mortality curve (ported from TinyMUX.WorldMaker's
//! medieval demographic profile), wars that both belligerents' annals agree
//! on to the year, plagues that land at the ports first and walk inland,
//! famines that prefer the dry realms. Geography constrains history the way
//! coarse constrains fine everywhere else in this world: the simulation
//! explains the present, it never contradicts it. The annals then feed the
//! lore engine as canon, so neighboring chronicles cite the same wars.

use std::collections::HashMap;

use world_core::geo::unit_to_lat_lon;
use world_core::hash::{hash3, splitmix64};

use crate::Planet;

const STAGE_HISTORY: u64 = 0xA2_2A15;
/// The annals run from year 1 to the present day.
pub const PRESENT_YEAR: u32 = 500;

// Medieval mortality (TinyMUX.WorldMaker): Gompertz base hazard, calibrated
// so an adult who reaches 20 dies around 55 on average, hard cap 85.
const GOMPERTZ_A: f64 = 0.00002;
const ADULT_LIFE_EXPECTANCY: f64 = 55.0;
const MAX_LIFESPAN: u32 = 85;

pub struct Ruler {
    pub name: String,
    pub title: &'static str,
    pub house: String,
    pub accession: u32,
    /// Age at accession — with the year, this dates every royal birthday.
    pub accession_age: u32,
    /// Year the reign ended; `PRESENT_YEAR` means they reign today.
    pub death: u32,
    /// How the reign ended, when it ended memorably.
    pub note: Option<String>,
}

pub struct Annal {
    pub year: u32,
    pub text: String,
}

pub struct RealmHistory {
    pub founding_year: u32,
    pub rulers: Vec<Ruler>,
    pub annals: Vec<Annal>,
}

pub struct History {
    realms: HashMap<u32, RealmHistory>,
}

struct War {
    a: u32, // capital cells
    b: u32,
    start: u32,
    end: u32,
    cause: &'static str,
    /// Winner's capital cell; None is a white peace.
    victor: Option<u32>,
}

impl History {
    pub fn build(planet: &Planet) -> Self {
        let civ = planet.civilization();
        let h = planet.hydrology();
        let seed = splitmix64(planet.seed ^ STAGE_HISTORY);
        let b_gompertz = calibrate_gompertz_b();

        // Realm roster: capital cell, position, population, port count.
        struct Realm {
            cell: u32,
            pos: [f64; 3],
            population: u64,
            has_port: bool,
            precip: f64,
        }
        let mut realms: Vec<Realm> = civ
            .settlements
            .iter()
            .filter(|s| s.capital)
            .map(|cap| {
                let members: Vec<_> = civ
                    .settlements
                    .iter()
                    .filter(|s| s.realm_capital == cap.cell)
                    .collect();
                let (lat, lon) = unit_to_lat_lon(cap.pos);
                Realm {
                    cell: cap.cell,
                    pos: cap.pos,
                    population: members.iter().map(|s| s.population as u64).sum(),
                    has_port: members.iter().any(|s| s.port),
                    precip: planet.climate(lat, lon).precip,
                }
            })
            .collect();
        realms.sort_by_key(|r| r.cell);

        // ---- Wars: generated once per neighboring pair, so both sides'
        // annals cite the same conflict, year for year. -------------------
        let max_reach = h.max_cell_size() * 70.0;
        let mut wars: Vec<War> = Vec::new();
        for i in 0..realms.len() {
            for j in i + 1..realms.len() {
                let (ra, rb) = (&realms[i], &realms[j]);
                let d = chord(ra.pos, rb.pos);
                if d > max_reach {
                    continue;
                }
                let ps = hash3(seed, ra.cell as i64, rb.cell as i64, 1);
                // Close neighbors quarrel more.
                let n_wars = (ps % 4).saturating_sub(if d > max_reach / 2.0 { 1 } else { 0 });
                for w in 0..n_wars {
                    let hw = hash3(seed, ra.cell as i64, rb.cell as i64, 10 + w as i64);
                    let start = 60 + (hw % (PRESENT_YEAR as u64 - 140)) as u32;
                    let end = start + 1 + (splitmix64(hw) % 6) as u32;
                    let causes = [
                        "the ford tolls",
                        "the salt trade",
                        "a broken betrothal",
                        "the herring grounds",
                        "a disputed succession",
                        "cattle raids on the border",
                        "the timber rights",
                    ];
                    let cause = causes[(hw >> 8) as usize % causes.len()];
                    // The bigger realm usually wins; sometimes nobody does.
                    let roll = (hw >> 16) % 10;
                    let victor = if roll < 3 {
                        None
                    } else if (hw >> 24) % (ra.population + rb.population)
                        < ra.population
                    {
                        Some(ra.cell)
                    } else {
                        Some(rb.cell)
                    };
                    wars.push(War {
                        a: ra.cell,
                        b: rb.cell,
                        start,
                        end,
                        cause,
                        victor,
                    });
                }
            }
        }

        // ---- Two great plagues sweep the world; ports sicken first. ------
        let plague_years = [
            120 + (hash3(seed, 1, 0, 0) % 80) as u32,
            320 + (hash3(seed, 2, 0, 0) % 90) as u32,
        ];

        // ---- Per-realm histories. ----------------------------------------
        let mut map = HashMap::new();
        for realm in &realms {
            let cap = civ
                .settlements
                .iter()
                .find(|s| s.cell == realm.cell)
                .expect("capital exists");
            let rs = splitmix64(seed ^ realm.cell as u64);
            let founding_year = 20 + (hash3(rs, 0, 0, 0) % 60) as u32;

            let realm_wars: Vec<&War> = wars
                .iter()
                .filter(|w| w.a == realm.cell || w.b == realm.cell)
                .collect();

            // Dynasty: an unbroken chain of reigns from founding to today.
            let mut rulers = Vec::new();
            let mut house = person_house(rs, rulers.len() as u32);
            let mut year = founding_year;
            let mut k = 0i64;
            while year < PRESENT_YEAR {
                let hr = hash3(rs, k, 1, 0);
                let female = hr % 5 < 2;
                let accession_age = if k == 0 {
                    22 + (hr >> 8) as u32 % 18
                } else {
                    16 + (hr >> 8) as u32 % 22
                };
                let natural_span =
                    death_age(splitmix64(hr), accession_age, b_gompertz) - accession_age;
                let mut death = year + natural_span.max(1);
                let mut note = None;
                // A ruler whose reign crosses a war may fall in it.
                if let Some(w) = realm_wars
                    .iter()
                    .find(|w| w.start > year && w.start < death && (hr >> 40) % 6 == 0)
                {
                    death = w.end.min(death);
                    let other = if w.a == realm.cell { w.b } else { w.a };
                    note = Some(format!(
                        "fell in the war with the Realm of {}",
                        realm_name_of(civ, other)
                    ));
                } else if death < PRESENT_YEAR && (hr >> 44) % 12 == 0 {
                    note = Some("died of the sweating sickness".into());
                }
                rulers.push(Ruler {
                    name: person_name(splitmix64(hr ^ 0xBEEF), female),
                    title: if female { "Queen" } else { "King" },
                    house: house.clone(),
                    accession: year,
                    accession_age,
                    death: death.min(PRESENT_YEAR),
                    note,
                });
                year = death;
                k += 1;
                // Now and then the line breaks and a new house takes the seat.
                if (hash3(rs, k, 2, 0)) % 6 == 0 {
                    house = person_house(rs, k as u32);
                }
            }

            // ---- Assemble the annals, year-ordered. -----------------------
            let mut annals = Vec::new();
            annals.push(Annal {
                year: founding_year,
                text: format!(
                    "{} founded; {} {} of House {} takes the seat",
                    cap.name, rulers[0].title, rulers[0].name, rulers[0].house
                ),
            });
            for w in &realm_wars {
                let other = if w.a == realm.cell { w.b } else { w.a };
                let other_name = realm_name_of(civ, other);
                let outcome = match w.victor {
                    None => "ending in an exhausted peace".to_string(),
                    Some(v) if v == realm.cell => {
                        format!("won; the border villages paid tribute to {}", cap.name)
                    }
                    Some(_) => format!("lost, and dearly, to {other_name}"),
                };
                annals.push(Annal {
                    year: w.start,
                    text: format!(
                        "war with the Realm of {other_name} over {}; fought until year {}, {}",
                        w.cause, w.end, outcome
                    ),
                });
            }
            for (i, &py) in plague_years.iter().enumerate() {
                let arrival = if realm.has_port {
                    py
                } else {
                    py + 1 + (hash3(rs, i as i64, 3, 0) % 2) as u32
                };
                let toll = ["a third", "a quarter", "one soul in five"]
                    [(hash3(rs, i as i64, 4, 0) % 3) as usize];
                annals.push(Annal {
                    year: arrival,
                    text: format!(
                        "the great plague comes{}; it takes {} before it burns out",
                        if realm.has_port {
                            " ashore with the trading ships"
                        } else {
                            " up the roads from the coast"
                        },
                        toll
                    ),
                });
            }
            // Dry realms hunger more often.
            let famines = if realm.precip < 0.18 {
                3
            } else if realm.precip < 0.35 {
                2
            } else {
                1
            };
            for i in 0..famines {
                let fy = founding_year
                    + 30
                    + (hash3(rs, i as i64, 5, 0) % (PRESENT_YEAR - founding_year - 40) as u64)
                        as u32;
                annals.push(Annal {
                    year: fy,
                    text: "the harvest fails; the lean years are still spoken of".into(),
                });
            }
            for r in rulers.iter().filter(|r| r.note.is_some()) {
                annals.push(Annal {
                    year: r.death,
                    text: format!("{} {} {}", r.title, r.name, r.note.as_ref().unwrap()),
                });
            }
            let last = rulers.last().expect("at least one ruler");
            annals.push(Annal {
                year: last.accession,
                text: format!(
                    "{} {} of House {} takes the seat, and holds it today",
                    last.title, last.name, last.house
                ),
            });
            annals.sort_by_key(|a| (a.year, a.text.len()));

            map.insert(
                realm.cell,
                RealmHistory {
                    founding_year,
                    rulers,
                    annals,
                },
            );
        }

        Self { realms: map }
    }

    pub fn realm(&self, capital_cell: u32) -> Option<&RealmHistory> {
        self.realms.get(&capital_cell)
    }

    /// The reigning ruler of the realm holding `capital_cell`.
    pub fn current_ruler(&self, capital_cell: u32) -> Option<&Ruler> {
        self.realms.get(&capital_cell).and_then(|r| r.rulers.last())
    }
}

fn realm_name_of(civ: &crate::Civilization, capital_cell: u32) -> String {
    civ.settlements
        .iter()
        .find(|s| s.cell == capital_cell)
        .map(|s| s.name.clone())
        .unwrap_or_else(|| "the lost realm".into())
}

/// Calibrate the Gompertz slope so that the mean age of adult death matches
/// the profile's life expectancy at 20 — same procedure as WorldMaker's
/// `CalibrateGompertzB`, run once per build (it is deterministic).
pub(crate) fn calibrate_gompertz_b() -> f64 {
    let (mut lo, mut hi) = (0.02f64, 0.25f64);
    for _ in 0..40 {
        let b = 0.5 * (lo + hi);
        if mean_adult_death_age(b) > ADULT_LIFE_EXPECTANCY {
            lo = b; // living too long: steepen the hazard
        } else {
            hi = b;
        }
    }
    0.5 * (lo + hi)
}

fn mean_adult_death_age(b: f64) -> f64 {
    let mut surviving = 1.0;
    let mut sum = 0.0;
    let mut mass = 0.0;
    for age in 20..MAX_LIFESPAN {
        let m = (GOMPERTZ_A * (b * age as f64).exp()).min(1.0);
        let dying = surviving * m;
        sum += dying * age as f64;
        mass += dying;
        surviving -= dying;
    }
    sum += surviving * MAX_LIFESPAN as f64;
    mass += surviving;
    sum / mass
}

/// Sample an age of death, conditional on being alive at `from_age` —
/// inverse-CDF walk down the same yearly hazard used for calibration.
pub(crate) fn death_age(h: u64, from_age: u32, b: f64) -> u32 {
    let u = (h >> 11) as f64 / (1u64 << 53) as f64;
    let mut surviving = 1.0;
    for age in from_age..MAX_LIFESPAN {
        let m = (GOMPERTZ_A * (b * age as f64).exp()).min(1.0);
        surviving *= 1.0 - m;
        if surviving < u {
            return age.max(from_age + 1);
        }
    }
    MAX_LIFESPAN
}

// ---- People's names (rulers get their own banks, distinct from places) ---

const P_ONSETS: [&str; 20] = [
    "Ald", "Bern", "Ced", "Dag", "Ed", "Gund", "Hild", "Ing", "Leof", "Mald",
    "Os", "Rag", "Sig", "Theod", "Ulf", "Wil", "Aeth", "Brand", "Erm", "God",
];
const P_MALE_ENDS: [&str; 8] = ["ric", "mund", "gar", "wald", "helm", "bert", "win", "red"];
const P_FEMALE_ENDS: [&str; 8] = ["a", "wyn", "ith", "gard", "run", "eth", "ild", "is"];

pub(crate) fn person_name(h: u64, female: bool) -> String {
    let onset = P_ONSETS[(h % P_ONSETS.len() as u64) as usize];
    let ends = if female { P_FEMALE_ENDS } else { P_MALE_ENDS };
    format!("{onset}{}", ends[((h >> 16) % ends.len() as u64) as usize])
}

fn person_house(rs: u64, k: u32) -> String {
    let h = hash3(rs, k as i64, 6, 0);
    let onset = P_ONSETS[(h % P_ONSETS.len() as u64) as usize];
    const HOUSE_ENDS: [&str; 6] = ["ing", "mark", "stone", "field", "born", "hall"];
    format!("{onset}{}", HOUSE_ENDS[((h >> 16) % 6) as usize])
}

#[inline]
fn chord(a: [f64; 3], b: [f64; 3]) -> f64 {
    let d = [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
}
