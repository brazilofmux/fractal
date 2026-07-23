//! Phase 7: settlement interiors. Open any settlement and find its wards,
//! its tradesmen, and its notable people — computed on demand as a pure
//! function of (seed, settlement), never stored. The trade table and the
//! ward rules are ported from the user's Isolation kingdom simulator
//! (support values per the classic medieval-demographics tables: one
//! shoemaker per 150 souls, one inn per 2000, and so on; wards gated by
//! population with the same thresholds Isolation uses).

use world_core::hash::{hash3, splitmix64};

use crate::{Planet, SettlementKind};

const STAGE_INTERIOR: u64 = 0x1D_0025;

/// (name, one person supported per this many souls). Fishmongers only make
/// sense with water; the renderer filters them inland.
const TRADES: [(&str, u32); 33] = [
    ("shoemakers", 150),
    ("furriers", 250),
    ("tailors", 250),
    ("barbers", 350),
    ("jewelers", 400),
    ("taverns", 400),
    ("pastrycooks", 500),
    ("masons", 500),
    ("carpenters", 550),
    ("weavers", 600),
    ("chandlers", 700),
    ("mercers", 700),
    ("coopers", 700),
    ("bakers", 800),
    ("scabbard-makers", 850),
    ("wine-sellers", 900),
    ("saddlers", 1000),
    ("butchers", 1200),
    ("fishmongers", 1200),
    ("beer-sellers", 1400),
    ("spice merchants", 1400),
    ("blacksmiths", 1500),
    ("painters", 1500),
    ("doctors", 1700),
    ("roofers", 1800),
    ("locksmiths", 1900),
    ("ropemakers", 1900),
    ("bathhouses", 1900),
    ("inns", 2000),
    ("tanners", 2000),
    ("copyists", 2000),
    ("bookbinders", 3000),
    ("illuminators", 3900),
];

pub struct Trade {
    pub name: &'static str,
    pub count: u32,
}

pub struct Ward {
    pub name: String,
    pub kind: &'static str,
}

pub struct Notable {
    pub name: String,
    pub role: String,
    pub age: u32,
}

pub struct Interior {
    pub wards: Vec<Ward>,
    pub trades: Vec<Trade>,
    pub notables: Vec<Notable>,
    /// The settlement's inn (or alehouse), if it supports one.
    pub inn: Option<String>,
}

/// Everything behind a settlement's walls, derived on demand.
pub fn interior(planet: &Planet, settlement_index: usize) -> Interior {
    let civ = planet.civilization();
    let h = planet.hydrology();
    let s = &civ.settlements[settlement_index];
    let seed = splitmix64(planet.seed ^ STAGE_INTERIOR ^ s.cell as u64);
    let pop = s.population;
    let on_river = h.river_class(s.cell).is_some();
    let on_water = s.port || on_river;

    // ---- Trades: Isolation's support-value algorithm, made positional.
    // base = pop / support; a remainder past half the support value gives
    // an even chance of one marginal extra.
    let mut trades = Vec::new();
    for (i, &(name, support)) in TRADES.iter().enumerate() {
        if name == "fishmongers" && !on_water {
            continue;
        }
        let mut count = pop / support;
        let remainder = pop % support;
        if remainder > support / 2 && hash3(seed, i as i64, 0, 0) % 2 == 0 {
            count += 1; // the marginal shop, scraping by
        }
        if count > 0 {
            trades.push(Trade { name, count });
        }
    }
    trades.sort_by(|a, b| b.count.cmp(&a.count).then(a.name.cmp(b.name)));

    // ---- Wards: Isolation's population gates. Villages have none; towns
    // a few working quarters; cities the full civic anatomy.
    let mut wards = Vec::new();
    let mut add_ward = |kind: &'static str, names: &[&str], n: u32| {
        // Draw without replacement from this kind's name pool, so a city
        // never has two wards of the same name and the loop is bounded.
        let mut pool: Vec<&str> = names.to_vec();
        for k in 0..(n as usize).min(names.len()) {
            let pick =
                hash3(seed, kind.len() as i64 * 131 + k as i64, 1, 0) as usize % pool.len();
            wards.push(Ward {
                name: pool.remove(pick).to_string(),
                kind,
            });
        }
    };
    if pop > 900 {
        add_ward("military", &["Castle Ward", "the Garrison", "Drumgate"], 1);
        add_ward(
            "odoriferous businesses",
            &["the Shambles", "Tanners' Reach", "Slaughter Row"],
            1,
        );
        let craft = 1 + (pop / 2001).min(2);
        add_ward(
            "craftsmen",
            &["Weavers' Row", "Hammer Lane", "the Wrightyards", "Cooper's Walk"],
            craft,
        );
        add_ward(
            "market",
            &["the Great Market", "Penny Cross", "the Cloth Fair"],
            1 + (pop / 5001).min(1),
        );
        if s.port {
            add_ward("harborside", &["the Saltwharf", "Herring Quay", "the Strand"], 1);
        }
        if on_river {
            add_ward(
                "riverside",
                &["Bridgefoot", "the Watergate", "Millers' Bank"],
                1,
            );
        }
    }
    if pop > 5000 {
        add_ward(
            "patriciate",
            &["the High Ward", "Silver Hill", "the Old Court"],
            1,
        );
        add_ward(
            "merchant",
            &["Goldrow", "the Countinghouses", "Mercers' Walk"],
            1 + (pop / 12001).min(1),
        );
        add_ward("administration", &["the Law Courts", "Scriveners' Close"], 1);
        add_ward(
            "gate",
            &["Northgate", "Southgate", "Eastgate", "Westgate"],
            (1 + pop / 5001).min(4),
        );
        add_ward("slum", &["the Warrens", "Ragmarket", "the Mudflats"], 1);
    }

    // ---- The inn, if the town can keep one.
    let has_inn = trades.iter().any(|t| t.name == "inns");
    let inn = (has_inn || matches!(s.kind, SettlementKind::Village)).then(|| {
        const FIRST: [&str; 10] = [
            "Gilded", "Crooked", "Salt", "Drowned", "Merry", "Black", "White", "Copper",
            "Wandering", "Old",
        ];
        const SECOND: [&str; 10] = [
            "Ram", "Heron", "Anchor", "Kettle", "Stag", "Goose", "Lantern", "Wheel",
            "Mermaid", "Oak",
        ];
        let hi = hash3(seed, 7, 0, 0);
        format!(
            "the {} {}",
            FIRST[(hi % 10) as usize],
            SECOND[((hi >> 8) % 10) as usize]
        )
    });

    // ---- Notables: the people worth a line in the atlas.
    let mut notables = Vec::new();
    let mut add = |role: String, key: i64, notables: &mut Vec<Notable>| {
        let hn = hash3(seed, key, 2, 0);
        let female = hn % 5 < 2;
        // Triangular age distribution around the mid-forties.
        let age = 24 + (((hn >> 8) % 44 + (hn >> 24) % 44) / 2) as u32;
        notables.push(Notable {
            name: crate::history::person_name(splitmix64(hn), female),
            role,
            age,
        });
    };
    match s.kind {
        SettlementKind::City => {
            add("lord mayor".into(), 10, &mut notables);
            if s.capital {
                add("castellan of the seat".into(), 11, &mut notables);
            }
            add("captain of the watch".into(), 12, &mut notables);
            if let Some(t) = trades.iter().find(|t| t.count >= 3) {
                add(format!("master of the {}' guild", t.name), 13, &mut notables);
            }
        }
        SettlementKind::Town => {
            add("mayor".into(), 10, &mut notables);
            if let Some(t) = trades.iter().find(|t| t.count >= 3) {
                add(format!("master of the {}' guild", t.name), 13, &mut notables);
            }
            add("village priest".into(), 14, &mut notables);
        }
        SettlementKind::Village => {
            add("reeve".into(), 10, &mut notables);
            if on_river {
                add("miller".into(), 15, &mut notables);
            }
            add("smith".into(), 16, &mut notables);
            add("priest".into(), 14, &mut notables);
        }
    }
    if s.port {
        add("harbormaster".into(), 17, &mut notables);
    }
    if let Some(inn_name) = &inn {
        let role = if matches!(s.kind, SettlementKind::Village) && !has_inn {
            format!("alewife of {inn_name}")
        } else {
            format!("keeper of {inn_name}")
        };
        add(role, 18, &mut notables);
    }

    Interior {
        wards,
        trades,
        notables,
        inn,
    }
}
