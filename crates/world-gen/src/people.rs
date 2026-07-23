//! Phase 9: everyone has a name. The full port of TinyMUX.WorldMaker's
//! PersonTables: any named person in the world — a settlement notable, a
//! manor holder, a reigning monarch — can be resolved to a household three
//! generations deep, computed on demand as a pure function of the seed.
//! Marriages happen at era ages (women 14–25, men 18–30), births arrive
//! every two-and-a-half years or so until fertility ends at 45, roughly a
//! fifth of infants are lost, childhood claims more, and spouses die on
//! the same Gompertz curve as kings — so widowhood is not written in
//! anywhere; it merely happens, deterministically.

use world_core::hash::{hash3, splitmix64};

use crate::history::{calibrate_gompertz_b, death_age, person_name};
use crate::{interior, Planet, SettlementKind, PRESENT_YEAR};

const STAGE_PEOPLE: u64 = 0x9E_0913;

// Medieval fertility and marriage (TinyMUX.WorldMaker profile).
const FEMALE_MARRIAGE: (u32, u32) = (14, 25);
const MALE_MARRIAGE: (u32, u32) = (18, 30);
const MAX_BIRTH_AGE: u32 = 45;
const INFANT_MORTALITY_PCT: u64 = 23;
const CHILDHOOD_MORTALITY_PCT: u64 = 9;

/// Slot numbers identify a person within a settlement: interior notables
/// use their own slots (10..=18), the manor holder is 30, the reigning
/// monarch (capitals only) is 40.
pub const SLOT_HOLDER: u8 = 30;
pub const SLOT_RULER: u8 = 40;

pub struct Head {
    pub name: String,
    pub role: String,
    pub female: bool,
    pub age: u32,
    pub settlement_index: usize,
}

pub struct Member {
    pub name: String,
    pub female: bool,
    pub age: u32,
    pub alive: bool,
    pub note: Option<String>,
}

pub struct HouseholdOf {
    pub parents: (Member, Member),
    pub spouse: Option<Member>,
    pub children: Vec<Member>,
    /// Infants and small children lost — counted, mourned, unnamed.
    pub lost: u32,
}

/// Resolve a person slot at a settlement to its head-of-household.
pub fn person_at(planet: &Planet, cell: u32, slot: u8) -> Option<Head> {
    let civ = planet.civilization();
    let i = civ.settlements.iter().position(|s| s.cell == cell)?;
    let s = &civ.settlements[i];
    match slot {
        SLOT_RULER => {
            let r = planet.history().current_ruler(s.realm_capital)?;
            (s.capital).then(|| Head {
                name: r.name.clone(),
                role: format!("{} of the Realm of {}", r.title, s.realm),
                female: r.title == "Queen",
                age: r.accession_age + (PRESENT_YEAR - r.accession),
                settlement_index: i,
            })
        }
        SLOT_HOLDER => {
            let hold = planet.peerage().holding(cell)?;
            Some(Head {
                name: hold.holder.clone(),
                role: format!("{} of House {}, holder of {}", hold.title, hold.house, s.name),
                female: matches!(hold.title, "Dame" | "Lady"),
                age: hold.age,
                settlement_index: i,
            })
        }
        _ => {
            let inside = interior(planet, i);
            let n = inside.notables.iter().find(|n| n.slot == slot)?;
            Some(Head {
                name: n.name.clone(),
                role: format!("{} of {}", n.role, s.name),
                female: n.female,
                age: n.age,
                settlement_index: i,
            })
        }
    }
}

/// The household of a person, three generations deep, derived from nothing
/// but the seed and who they already are.
pub fn household(planet: &Planet, cell: u32, slot: u8, head: &Head) -> HouseholdOf {
    let seed = splitmix64(planet.seed ^ STAGE_PEOPLE ^ ((cell as u64) << 8) ^ slot as u64);
    let b = calibrate_gompertz_b();

    // ---- Parents: a generation up, mostly in the ground by now.
    let parent = |key: i64, female: bool| {
        let hp = hash3(seed, key, 1, 0);
        let gap = 20 + (hp % 15) as u32; // parent's age at the head's birth
        let age_now = head.age + gap;
        let died_at = death_age(splitmix64(hp), 20, b);
        let alive = died_at > age_now && age_now < 85;
        Member {
            name: person_name(splitmix64(hp ^ 0xFA), female),
            female,
            age: if alive { age_now } else { died_at.max(21) },
            alive,
            note: (!alive).then(|| "gone these many years".to_string()),
        }
    };
    let parents = (parent(2, false), parent(3, true));

    // ---- Marriage. The cloth and the very young stay single.
    let hm = hash3(seed, 4, 0, 0);
    let celibate = head.role.contains("priest");
    let bounds = if head.female { FEMALE_MARRIAGE } else { MALE_MARRIAGE };
    let marriage_age = bounds.0 + (hm % (bounds.1 - bounds.0) as u64) as u32;
    let married = !celibate
        && head.age > marriage_age
        && (head.age >= 25 || hm % 2 == 0)
        && hm % 8 != 7; // some simply never wed
    let years_wed = if married { head.age - marriage_age } else { 0 };

    // The mother's clock governs childbearing; track it explicitly.
    let mut spouse = None;
    let mut mother_marriage_age = marriage_age;
    let mut mother_fertile_years = years_wed;
    if married {
        let hs = hash3(seed, 5, 0, 0);
        let sb = if head.female { MALE_MARRIAGE } else { FEMALE_MARRIAGE };
        let sma = sb.0 + (hs % (sb.1 - sb.0) as u64) as u32;
        let age_now = sma + years_wed;
        let died_at = death_age(splitmix64(hs), sma, b);
        let alive = died_at > age_now;
        if !head.female {
            mother_marriage_age = sma;
            mother_fertile_years = if alive {
                years_wed
            } else {
                died_at.saturating_sub(sma)
            };
        }
        spouse = Some(Member {
            name: person_name(splitmix64(hs ^ 0xFB), !head.female),
            female: !head.female,
            age: if alive { age_now } else { died_at },
            alive,
            note: (!alive).then(|| format!("buried {} years past", (age_now - died_at).max(1))),
        });
    }

    // ---- Children: the era's arithmetic, mercy not included.
    let mut children = Vec::new();
    let mut lost = 0u32;
    if spouse.is_some() {
        let fertile_years =
            mother_fertile_years.min(MAX_BIRTH_AGE.saturating_sub(mother_marriage_age));
        let mut t = 1 + (hash3(seed, 6, 0, 0) % 2) as u32;
        let mut k = 0i64;
        while t <= fertile_years.min(years_wed) {
            let hc = hash3(seed, 10 + k, 0, 0);
            let child_age = years_wed - t;
            if hc % 100 < INFANT_MORTALITY_PCT {
                lost += 1;
            } else if child_age > 15 && splitmix64(hc) % 100 < CHILDHOOD_MORTALITY_PCT {
                lost += 1;
            } else {
                let female = (hc >> 8) % 2 == 0;
                let grown = child_age >= if female { 16 } else { 19 };
                let wed = grown && (hc >> 16) % 3 != 0;
                children.push(Member {
                    name: person_name(splitmix64(hc ^ 0xFC), female),
                    female,
                    age: child_age,
                    alive: true,
                    note: wed.then(|| "married".to_string()),
                });
            }
            t += 2 + (splitmix64(hc ^ 0x11) % 2) as u32; // ~2.5-year intervals
            k += 1;
        }
    }

    HouseholdOf {
        parents,
        spouse,
        children,
        lost,
    }
}

/// Household rendered as atlas lines — the same text the lore context and
/// the panel both use, so prose and roster cannot drift apart.
pub fn household_lines(planet: &Planet, cell: u32, slot: u8) -> Option<(Head, Vec<String>)> {
    let head = person_at(planet, cell, slot)?;
    let hh = household(planet, cell, slot, &head);
    let mut lines = Vec::new();
    let (f, m) = &hh.parents;
    let fate = |p: &Member| {
        if p.alive {
            format!("living still, aged {}", p.age)
        } else {
            format!("dead at {}", p.age)
        }
    };
    lines.push(format!(
        "Born to {} ({}) and {} ({})",
        f.name,
        fate(f),
        m.name,
        fate(m)
    ));
    match &hh.spouse {
        Some(sp) if sp.alive => lines.push(format!(
            "Wed to {}, aged {}",
            sp.name, sp.age
        )),
        Some(sp) => lines.push(format!(
            "Widowed: {} was {}",
            sp.name,
            sp.note.clone().unwrap_or_else(|| "lost".into())
        )),
        None => lines.push("Unwed".to_string()),
    }
    for c in &hh.children {
        lines.push(format!(
            "{} {}, aged {}{}",
            if c.female { "Daughter" } else { "Son" },
            c.name,
            c.age,
            c.note.as_deref().map(|n| format!(" ({n})")).unwrap_or_default()
        ));
    }
    if hh.lost > 0 {
        lines.push(format!(
            "{} more {} lost young",
            hh.lost,
            if hh.lost == 1 { "child was" } else { "children were" }
        ));
    }
    Some((head, lines))
}

/// People worth a line in a settlement's panel: every slot that resolves.
pub fn people_of(planet: &Planet, settlement_index: usize) -> Vec<(String, String, u8)> {
    let civ = planet.civilization();
    let s = &civ.settlements[settlement_index];
    let mut out = Vec::new();
    if s.capital {
        if let Some(h) = person_at(planet, s.cell, SLOT_RULER) {
            out.push((h.name.clone(), h.role, SLOT_RULER));
        }
    }
    if let Some(h) = person_at(planet, s.cell, SLOT_HOLDER) {
        out.push((h.name.clone(), h.role, SLOT_HOLDER));
    }
    for n in interior(planet, settlement_index).notables {
        out.push((n.name, format!("{} of {}", n.role, s.name), n.slot));
    }
    let _ = matches!(s.kind, SettlementKind::Village); // kinds share one path
    out
}
