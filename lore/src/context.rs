//! Deterministic context assembly. Everything the chronicler is told about a
//! feature comes from the generators — biome, climate, rivers, neighbors,
//! demographics — so the same feature always yields the same brief, and the
//! stories it anchors cannot drift from the map.
//!
//! The demographic constants are the Medieval profile from TinyMUX.WorldMaker
//! (the author's earlier demographic simulation library) — era-true numbers
//! so the prose gets villages of hundreds, not metropolises of millions.

use world_core::geo::unit_to_lat_lon;
use world_gen::{
    civilization, classify_biome, Biome, Planet, Settlement, SettlementKind, PRESENT_YEAR,
};

pub const SYSTEM_PROMPT: &str = "You are the chronicler of a vast, real world \
you have walked end to end. You write atlas entries: grounded, specific, \
quietly evocative, in third person present tense. The facts provided to you \
are canon and must never be contradicted. You may invent small texture — a \
named person, a local custom, one notable event in living memory — so long as \
it fits the facts and the era's demographics. No purple prose, no exclamation \
marks, and never any hint that the world is invented. Do not repeat the raw \
facts as a list; weave the ones that matter into prose. Write plain \
paragraphs only — no markdown, no headings, no lists, no title line; the \
entry appears under its own name.";

#[derive(Clone, Copy)]
pub enum FeatureRef {
    /// Index into `civilization().settlements`.
    Settlement(usize),
    /// Index of the capital settlement of a realm.
    Realm(usize),
    /// Index into `geography().features`.
    Natural(usize),
    /// A person: (settlement cell, person slot).
    Person(u32, u8),
    /// A room of a settlement's walk: (settlement cell, room index).
    Room(u32, u8),
}

/// Parse a feature id: `s{cell}` for a settlement, `r{cell}` for the realm
/// whose capital sits on that cell, `n{index}` for a natural feature,
/// `p{cell}x{slot}` for a person, `w{cell}x{k}` for a room.
pub fn parse_id(planet: &Planet, id: &str) -> Option<FeatureRef> {
    let (kind, rest) = id.split_at(1);
    if kind == "p" {
        let (cell, slot) = rest.split_once('x')?;
        let (cell, slot) = (cell.parse().ok()?, slot.parse().ok()?);
        return world_gen::person_at(planet, cell, slot)
            .map(|_| FeatureRef::Person(cell, slot));
    }
    if kind == "w" {
        let (cell, k) = rest.split_once('x')?;
        let (cell, k): (u32, u8) = (cell.parse().ok()?, k.parse().ok()?);
        return world_gen::citymap::room_at(planet, cell, k as usize)
            .map(|_| FeatureRef::Room(cell, k));
    }
    let num: u32 = rest.parse().ok()?;
    let civ = planet.civilization();
    match kind {
        "s" => civ
            .settlements
            .iter()
            .position(|s| s.cell == num)
            .map(FeatureRef::Settlement),
        "r" => civ
            .settlements
            .iter()
            .position(|s| s.cell == num && s.capital)
            .map(FeatureRef::Realm),
        "n" => ((num as usize) < planet.geography().features.len())
            .then_some(FeatureRef::Natural(num as usize)),
        _ => None,
    }
}

pub fn feature_name(planet: &Planet, fref: FeatureRef) -> String {
    let civ = planet.civilization();
    match fref {
        FeatureRef::Settlement(i) => civ.settlements[i].name.clone(),
        FeatureRef::Realm(i) => format!("Realm of {}", civ.settlements[i].name),
        FeatureRef::Natural(i) => planet.geography().features[i].name.clone(),
        FeatureRef::Person(cell, slot) => world_gen::person_at(planet, cell, slot)
            .map(|h| h.name)
            .unwrap_or_default(),
        FeatureRef::Room(cell, k) => world_gen::citymap::room_at(planet, cell, k as usize)
            .map(|(_, r)| r.name)
            .unwrap_or_default(),
    }
}

/// The realm a feature belonged to in a given year — a settlement asked
/// about an earlier year answers to whichever crown held it then.
pub fn realm_of_in(planet: &Planet, fref: FeatureRef, year: u32) -> Option<(String, String)> {
    if year >= PRESENT_YEAR {
        return realm_of(planet, fref);
    }
    match fref {
        FeatureRef::Settlement(i) => {
            let cap = world_gen::realm_in(planet, i, year)?;
            let name = planet
                .civilization()
                .settlements
                .iter()
                .find(|s| s.cell == cap)
                .map(|s| s.name.clone())?;
            Some((format!("r{cap}"), format!("Realm of {name}")))
        }
        _ => realm_of(planet, fref),
    }
}

/// The id of the realm a feature belongs to, and the realm's display name
/// (natural features answer to no crown).
pub fn realm_of(planet: &Planet, fref: FeatureRef) -> Option<(String, String)> {
    let civ = planet.civilization();
    let s = match fref {
        FeatureRef::Settlement(i) | FeatureRef::Realm(i) => &civ.settlements[i],
        FeatureRef::Person(cell, slot) => {
            let head = world_gen::person_at(planet, cell, slot)?;
            &civ.settlements[head.settlement_index]
        }
        FeatureRef::Room(cell, _) => civ.settlements.iter().find(|s| s.cell == cell)?,
        FeatureRef::Natural(_) => return None,
    };
    Some((format!("r{}", s.realm_capital), format!("Realm of {}", s.realm)))
}

/// The user-turn prompt for a feature: instruction + canon facts (+ the
/// already-written realm chronicle for settlements, so town stories nest
/// inside their realm's history instead of contradicting it). The year is
/// the fourth coordinate: at the present the brief is exactly what it has
/// always been (canon written before Phase 11 stays valid); parked in an
/// earlier year the chronicler is told only what that year could know.
pub fn prompt_for(planet: &Planet, fref: FeatureRef, realm_body: Option<&str>, year: u32) -> String {
    match fref {
        FeatureRef::Settlement(i) if year < PRESENT_YEAR => {
            let s = &planet.civilization().settlements[i];
            let mut p = format!(
                "Write the atlas entry (120-180 words) for the {} of {}, as it \
                 stands in year {}. You write from within that year: nothing \
                 later has happened yet, and nothing later may be hinted \
                 at.\n\nCanon facts:\n{}",
                kind_word(s),
                s.name,
                year,
                settlement_facts_in(planet, i, year),
            );
            if let Some(realm) = realm_body {
                p.push_str("\n\nThe chronicle of its realm (canon, do not contradict):\n");
                p.push_str(realm);
            }
            p
        }
        FeatureRef::Realm(i) if year < PRESENT_YEAR => {
            let s = &planet.civilization().settlements[i];
            format!(
                "Write the chronicle (180-260 words) of the Realm of {}, as it \
                 stands in year {} — its origin, its character, and how the \
                 annals below shaped it. You write from within year {}: \
                 nothing later has happened yet, and nothing later may be \
                 hinted at. The annals are canon: cite their people, wars and \
                 years freely, elaborate them, never contradict them, and end \
                 on whatever tension the latest entries leave alive.\n\nCanon facts:\n{}",
                s.name,
                year,
                year,
                realm_facts_in(planet, i, year),
            )
        }
        FeatureRef::Settlement(i) => {
            let s = &planet.civilization().settlements[i];
            let mut p = format!(
                "Write the atlas entry (120-180 words) for the {} of {}.\n\nCanon facts:\n{}",
                kind_word(s),
                s.name,
                settlement_facts(planet, i),
            );
            if let Some(realm) = realm_body {
                p.push_str("\n\nThe chronicle of its realm (canon, do not contradict):\n");
                p.push_str(realm);
            }
            p
        }
        FeatureRef::Realm(i) => {
            let s = &planet.civilization().settlements[i];
            format!(
                "Write the chronicle (180-260 words) of the Realm of {} — its \
                 origin, its character, and how the annals below shaped it. \
                 The annals are canon: cite their people, wars and years \
                 freely, elaborate them, never contradict them, and end on \
                 whatever tension the recent entries leave alive.\n\nCanon facts:\n{}",
                s.name,
                realm_facts(planet, i),
            )
        }
        FeatureRef::Natural(i) => {
            let f = &planet.geography().features[i];
            format!(
                "Write the atlas entry (100-160 words) for {}, a {}. Write it \
                 as geography first — what the land or water is like to cross, \
                 what lives or fails to live there — and let any human history \
                 stay at the edges.\n\nCanon facts:\n{}",
                f.name,
                f.kind.word(),
                natural_facts(planet, i),
            )
        }
        FeatureRef::Room(cell, k) => {
            let (i, room) = world_gen::citymap::room_at(planet, cell, k as usize)
                .expect("room parsed, so it resolves");
            let mut p = format!(
                "Write the street-level view (60-110 words) from {}, {} of the \
                 {} of {}. For this entry only, write in the second person, \
                 present tense — the reader stands there now, seeing and \
                 hearing the place. Name at most one of the people below in \
                 passing, doing something ordinary. No greetings, no \
                 addresses to the reader, no purple prose.\n\nCanon facts:\n{}",
                room.name,
                ward_phrase(&room.kind),
                kind_word(&planet.civilization().settlements[i]),
                planet.civilization().settlements[i].name,
                room_facts(planet, i, cell, k as usize),
            );
            if let Some(town) = realm_body {
                p.push_str("\n\nThe atlas entry of its town (canon, do not contradict):\n");
                p.push_str(town);
            }
            p
        }
        FeatureRef::Person(cell, slot) => {
            let (head, lines) = world_gen::household_lines(planet, cell, slot)
                .expect("person parsed, so they resolve");
            let civ = planet.civilization();
            let s = &civ.settlements[head.settlement_index];
            let mut facts = String::new();
            line(&mut facts, format!("{}, aged {}", head.role, head.age));
            line(
                &mut facts,
                format!(
                    "lives in the {} of {} (population ~{}), Realm of {}",
                    match s.kind {
                        SettlementKind::City => "city",
                        SettlementKind::Town => "town",
                        SettlementKind::Village => "village",
                    },
                    s.name,
                    round_pop(s.population),
                    s.realm
                ),
            );
            for l in &lines {
                line(&mut facts, l.clone());
            }
            line(
                &mut facts,
                "era: pre-industrial; households of this kind are the norm, \
                 and the ages above are already era-true"
                    .into(),
            );
            format!(
                "Write the atlas note (80-140 words) on {}, {}. A life, not a \
                 legend: their standing, their household, and one habit or \
                 worry the town knows them by. The household facts are canon \
                 and every name in them may be used.\n\nCanon facts:\n{}",
                head.name, head.role, facts
            )
        }
    }
}

fn natural_facts(planet: &Planet, i: usize) -> String {
    let geo = planet.geography();
    let civ = planet.civilization();
    let f = &geo.features[i];
    let (lat, lon) = unit_to_lat_lon(f.center);
    let cl = planet.climate(lat, lon);

    let mut out = String::new();
    line(
        &mut out,
        format!(
            "a {} roughly {} leagues across",
            f.kind.word(),
            world_gen::geography::breadth_leagues(f.cells)
        ),
    );
    line(
        &mut out,
        format!(
            "at {}; mean {:.0} °C, {} rainfall",
            latitude_word(lat),
            cl.temp_c,
            precip_word(cl.precip)
        ),
    );
    if let Some(land) = geo.landmass_at(f.anchor) {
        if land.name != f.name {
            line(&mut out, format!("it lies on {}", land.name));
        }
    }
    // The nearest three settlements orient the reader.
    let mut near: Vec<(f64, &world_gen::Settlement)> = civ
        .settlements
        .iter()
        .map(|s| (chord(f.center, s.pos), s))
        .collect();
    near.sort_by(|a, b| a.0.total_cmp(&b.0));
    for (d, s) in near.iter().take(3) {
        line(
            &mut out,
            format!(
                "{} km {} lies {} (Realm of {})",
                (d * 6371.0).round() as u32,
                compass(f.center, s.pos),
                s.name,
                s.realm
            ),
        );
    }
    out
}

fn kind_word(s: &Settlement) -> &'static str {
    match (s.port, s.kind) {
        (true, SettlementKind::City) => "port city",
        (true, _) => "harbor town",
        (false, SettlementKind::City) => "city",
        (false, SettlementKind::Town) => "town",
        (false, SettlementKind::Village) => "village",
    }
}

fn settlement_facts(planet: &Planet, i: usize) -> String {
    let civ = planet.civilization();
    let h = planet.hydrology();
    let s = &civ.settlements[i];
    let (lat, lon) = unit_to_lat_lon(s.pos);
    let cl = planet.climate(lat, lon);
    let biome = classify_biome(cl.temp_c, cl.precip);
    let e = planet.bulk_elevation(lat, lon);

    let mut f = String::new();
    let mut line = |t: String| {
        f.push_str("- ");
        f.push_str(&t);
        f.push('\n');
    };

    line(format!(
        "{} of about {} people (roughly {} households), in the {}",
        kind_word(s),
        round_pop(s.population),
        (s.population as f64 / 4.6).round() as u32,
        s.realm_display()
    ));
    line(format!(
        "landscape: {} {} at {}",
        terrain_word(e),
        biome_word(biome),
        latitude_word(lat)
    ));
    // Named geography: where this place stands in the world's own terms.
    let geo = planet.geography();
    if let Some(land) = geo.landmass_at(s.cell) {
        if land.kind == world_gen::NaturalKind::Island {
            line(format!("it stands on {}", land.name));
        }
    }
    if let Some(cov) = geo.cover_at(s.cell) {
        line(format!("it lies within {}", cov.name));
    }
    if let Some(rel) = geo.relief_at(s.cell) {
        line(format!("it sits among {}", rel.name));
    } else if let Some(rng) = geo.nearest_range(s.pos, h.max_cell_size() * 6.0) {
        line(format!("{} rises {} of it", rng.name, compass(s.pos, rng.center)));
    }
    if s.port {
        if let Some(w) = geo.harbor_water(planet, s.cell) {
            line(format!("its harbor opens on {}", w.name));
        }
    }
    line(format!(
        "climate: mean {:.0} °C, {} rainfall",
        cl.temp_c,
        precip_word(cl.precip)
    ));
    if let Some(river) = civilization::river_name(planet.seed, h, s.cell) {
        let class = h.river_class(s.cell).unwrap_or(1);
        line(format!(
            "sits on the {river}, {}",
            if class >= 4 {
                "a great navigable river"
            } else if class >= 2 {
                "a working river"
            } else {
                "a modest river"
            }
        ));
    }
    if s.port {
        line("a sheltered natural harbor; livelihood tied to the sea".into());
    }
    if s.capital {
        line("seat of its realm".into());
    }
    if let Some(r) = planet.history().current_ruler(s.realm_capital) {
        line(format!(
            "the realm is ruled by {} {} of House {}, on the seat since year {} \
             (the present year is {})",
            r.title, r.name, r.house, r.accession, PRESENT_YEAR
        ));
    }
    // Who holds this place, and of whom — the tenure web is canon.
    if let Some(hold) = planet.peerage().holding(s.cell) {
        let liege = if hold.liege_cell == s.realm_capital {
            "the crown itself".to_string()
        } else {
            civ.settlements
                .iter()
                .find(|t| t.cell == hold.liege_cell)
                .map(|t| format!("the lord of {}", t.name))
                .unwrap_or_else(|| "the crown".to_string())
        };
        line(format!(
            "held by {} {} of House {}{}, aged {}, who holds of {}",
            hold.title,
            hold.holder,
            hold.house,
            if hold.cadet { " (a cadet of the liege's line)" } else { "" },
            hold.age,
            liege
        ));
        // A holder from a fallen royal house carries that weight.
        if let Some(houses) = planet.peerage().houses(s.realm_capital) {
            if let Some(h) = houses
                .iter()
                .find(|h| h.name == hold.house && !h.reigning && h.held_seat.is_some())
            {
                let (a, b) = h.held_seat.unwrap();
                line(format!(
                    "House {} held the realm's seat from year {} to {}, and today {}",
                    h.name, a, b, h.disposition
                ));
            }
        }
    }
    // What living memory holds: the realm's last few decades.
    if let Some(rh) = planet.history().realm(s.realm_capital) {
        for a in rh.annals.iter().filter(|a| a.year + 40 >= PRESENT_YEAR).take(2) {
            line(format!("in living memory (year {}): {}", a.year, a.text));
        }
    }

    // Nearest neighbors, with distance and direction — the social geography.
    let mut near: Vec<(f64, usize)> = civ
        .settlements
        .iter()
        .enumerate()
        .filter(|&(j, _)| j != i)
        .map(|(j, t)| (chord(s.pos, t.pos), j))
        .collect();
    near.sort_by(|a, b| a.0.total_cmp(&b.0));
    for &(d, j) in near.iter().take(3) {
        let t = &civ.settlements[j];
        line(format!(
            "{} km {} lies the {} of {} ({})",
            (d * 6371.0).round() as u32,
            compass(s.pos, t.pos),
            kind_word(t),
            t.name,
            if t.realm_capital == s.realm_capital {
                "same realm"
            } else {
                "a neighboring realm"
            }
        ));
    }
    let roads = civ
        .roads
        .iter()
        .filter(|r| r.a as usize == i || r.b as usize == i)
        .count();
    line(match roads {
        0 => "no made roads; travel is by track or water".into(),
        1 => "one road connects it to the wider world".into(),
        n => format!("{n} roads meet here"),
    });

    // Inside the walls: wards, trades, and people worth naming (canon).
    let inside = world_gen::interior(planet, i);
    if !inside.wards.is_empty() {
        let names: Vec<&str> = inside.wards.iter().map(|w| w.name.as_str()).collect();
        line(format!("its wards: {}", names.join(", ")));
    }
    if !inside.trades.is_empty() {
        let top: Vec<String> = inside
            .trades
            .iter()
            .take(5)
            .map(|t| format!("{} {}", t.count, t.name))
            .collect();
        line(format!("among its trades: {}", top.join(", ")));
    }
    for n in inside.notables.iter().take(5) {
        line(format!("{}, {} — aged {}", n.name, n.role, n.age));
    }

    // The economy: what it makes, buys, and lacks (canon).
    let econ = planet.economy();
    line(format!(
        "by the standards of the age it is {}",
        world_gen::Economy::wealth_word(econ.wealth[i])
    ));
    // The manor roll: what the land yields and where the money goes.
    if s.capital {
        line(format!(
            "the crown's demesne here yields some {} marks a year, and {} more \
             arrive in dues from the realm's manors",
            econ.manor_income[i], econ.manor_receives[i]
        ));
    } else {
        let sends = econ.manor_sends[i];
        let keeps = econ.manor_income[i] + econ.manor_receives[i] - sends;
        let liege = planet
            .peerage()
            .holding(s.cell)
            .and_then(|h| civ.settlements.iter().find(|t| t.cell == h.liege_cell))
            .map(|t| {
                if t.capital {
                    "the crown".to_string()
                } else {
                    format!("the lord of {}", t.name)
                }
            })
            .unwrap_or_else(|| "the crown".to_string());
        line(format!(
            "the manor roll: its lands yield some {} marks a year; the third \
             penny — {} marks — goes up to {}; {} marks stay with the holder",
            econ.manor_income[i], sends, liege, keeps
        ));
        let vassal_manors = civ
            .settlements
            .iter()
            .filter(|t| {
                planet
                    .peerage()
                    .holding(t.cell)
                    .is_some_and(|h| h.liege_cell == s.cell)
            })
            .count();
        if econ.manor_receives[i] > 0 {
            line(format!(
                "{} lesser manors are held of its lord, paying {} marks in dues",
                vassal_manors, econ.manor_receives[i]
            ));
        }
    }
    if !econ.produces[i].is_empty() {
        line(format!(
            "it produces {}",
            econ.produces[i].iter().map(|g| g.word()).collect::<Vec<_>>().join(", ")
        ));
    }
    if !econ.imports[i].is_empty() {
        let buys: Vec<String> = econ.imports[i]
            .iter()
            .take(4)
            .map(|(g, src)| format!("{} from {}", g.word(), civ.settlements[*src].name))
            .collect();
        line(format!("it buys {}", buys.join(", ")));
    }
    if !econ.wanting[i].is_empty() {
        line(format!(
            "it goes without {} — no road or lane reaches a source",
            econ.wanting[i].iter().map(|g| g.word()).collect::<Vec<_>>().join(", ")
        ));
    }

    // Era demographics (Medieval profile, TinyMUX.WorldMaker).
    line("era: pre-industrial. Life expectancy ~33 at birth (mid-40s if \
          childhood is survived); roughly a fifth of infants do not live a \
          year; women marry at 14-25 and men at 18-30; births come about \
          every two and a half years; few see 70"
        .into());
    f
}

fn realm_facts(planet: &Planet, capital: usize) -> String {
    let civ = planet.civilization();
    let cap = &civ.settlements[capital];

    let members: Vec<&Settlement> = civ
        .settlements
        .iter()
        .filter(|s| s.realm_capital == cap.cell)
        .collect();
    let towns = members
        .iter()
        .filter(|s| s.kind == SettlementKind::Town)
        .count();
    let villages = members
        .iter()
        .filter(|s| s.kind == SettlementKind::Village)
        .count();
    let ports = members.iter().filter(|s| s.port).count();
    let people: u32 = members.iter().map(|s| s.population).sum();
    let reach = members
        .iter()
        .map(|s| chord(cap.pos, s.pos))
        .fold(0.0f64, f64::max);

    let mut f = String::new();
    line(
        &mut f,
        format!("the realm is held from {}, its capital and only city", cap.name),
    );
    line(
        &mut f,
        format!(
            "it counts {} towns and {} villages — some {} souls — reaching about {} km from the capital",
            towns,
            villages,
            round_pop(people),
            (reach * 6371.0).round() as u32
        ),
    );
    if ports > 0 {
        line(&mut f, format!("{ports} of its settlements are working ports"));
    }
    f.push_str(&settlement_facts(planet, capital));

    // The nearest foreign capital: every realm needs a neighbor to define it.
    if let Some((d, other)) = civ
        .settlements
        .iter()
        .filter(|s| s.capital && s.cell != cap.cell)
        .map(|s| (chord(cap.pos, s.pos), s))
        .min_by(|a, b| a.0.total_cmp(&b.0))
    {
        line(
            &mut f,
            format!(
                "the nearest foreign power is the Realm of {}, {} km {}",
                other.name,
                (d * 6371.0).round() as u32,
                compass(cap.pos, other.pos)
            ),
        );
    }

    // The crown's income, for a chronicle that knows what a war costs —
    // and where every mark of it was raised.
    if let Some(marks) = planet.economy().realm_ledger.get(&cap.cell) {
        let econ = planet.economy();
        line(
            &mut f,
            format!(
                "the crown's ledger runs to some {marks} marks a year — {} from \
                 the royal demesne, {} in dues climbed up from the realm's manors",
                econ.manor_income[capital], econ.manor_receives[capital]
            ),
        );
    }

    // The great houses and their tempers.
    if let Some(houses) = planet.peerage().houses(cap.cell) {
        f.push_str("\nThe great houses of the realm:\n");
        for h in houses {
            let seat = match h.held_seat {
                Some((a, _)) if h.reigning => format!("royal since year {a}"),
                Some((a, b)) => format!("held the seat {a}–{b}"),
                None => "never royal".to_string(),
            };
            line(&mut f, format!("House {} — {}; {}", h.name, seat, h.disposition));
        }
    }

    // The annals: the realm's simulated five centuries, entry by entry.
    if let Some(rh) = planet.history().realm(cap.cell) {
        line(
            &mut f,
            format!(
                "the seat has passed through {} reigns since the founding in year {}; \
                 the present year is {}",
                rh.rulers.len(),
                rh.founding_year,
                PRESENT_YEAR
            ),
        );
        f.push_str("\nThe annals:\n");
        for a in &rh.annals {
            line(&mut f, format!("Year {} — {}", a.year, a.text));
        }
    }
    f
}

fn ward_phrase(kind: &str) -> &'static str {
    match kind {
        "market" => "the market ward",
        "military" => "the castle ward",
        "odoriferous businesses" => "the ward of the stinking trades",
        "craftsmen" => "a craftsmen's ward",
        "harborside" => "the harborside",
        "riverside" => "the riverside ward",
        "patriciate" => "the patricians' ward",
        "merchant" => "a merchants' ward",
        "administration" => "the ward of courts and scriveners",
        "gate" => "a gate ward",
        "slum" => "the poorest ward",
        "green" => "the village green",
        "lane" => "the village lane",
        "church" => "the church end",
        "inn" => "an inn",
        _ => "a ward",
    }
}

/// The street's brief: where the reader stands, who is in sight, what the
/// town around them is — all generator truth, so the second-person prose
/// can never wander off the map.
fn room_facts(planet: &Planet, i: usize, _cell: u32, k: usize) -> String {
    let civ = planet.civilization();
    let s = &civ.settlements[i];
    let plan = world_gen::citymap::plan(planet, i);
    let rooms = world_gen::citymap::rooms(&plan);
    let room = rooms.iter().find(|r| r.k == k).expect("room exists");
    let (lat, lon) = unit_to_lat_lon(s.pos);
    let cl = planet.climate(lat, lon);
    let econ = planet.economy();

    let mut f = String::new();
    line(
        &mut f,
        format!(
            "{} is {} of {}, a {} of about {} souls in the Realm of {}",
            room.name,
            ward_phrase(&room.kind),
            s.name,
            kind_word(s),
            round_pop(s.population),
            s.realm
        ),
    );
    line(
        &mut f,
        format!(
            "by the standards of the age the town is {}",
            world_gen::Economy::wealth_word(econ.wealth[i])
        ),
    );
    for (name, role, slot) in world_gen::people_of(planet, i) {
        if world_gen::citymap::room_of_role(&plan, &role, slot) == k {
            line(&mut f, format!("in sight here: {name}, {role}"));
        }
    }
    if matches!(
        room.kind.as_str(),
        "market" | "craftsmen" | "odoriferous businesses" | "merchant"
    ) {
        let inside = world_gen::interior(planet, i);
        let trades: Vec<String> = inside
            .trades
            .iter()
            .take(4)
            .map(|t| format!("{} {}", t.count, t.name))
            .collect();
        if !trades.is_empty() {
            line(&mut f, format!("among the town's trades: {}", trades.join(", ")));
        }
    }
    if room.kind == "inn" {
        line(
            &mut f,
            "an inn: hearth and taproom below, beds above, a stable yard behind".into(),
        );
    }
    if room.kind == "harborside" {
        line(
            &mut f,
            "the quay: hulls and cargo and gulls; the sea is the town's living".into(),
        );
    }
    if let Some((inn, host)) = &plan.inn {
        if *host == k {
            line(&mut f, format!("{inn} stands in this ward"));
        }
    }
    let ways: Vec<String> = world_gen::citymap::exits(&plan, k)
        .iter()
        .filter_map(|&e| rooms.iter().find(|r| r.k == e))
        .map(|r| r.name.clone())
        .collect();
    if !ways.is_empty() {
        line(&mut f, format!("ways lead on to: {}", ways.join(", ")));
    }
    line(
        &mut f,
        format!(
            "climate: mean {:.0} °C, {} rainfall",
            cl.temp_c,
            precip_word(cl.precip)
        ),
    );
    line(
        &mut f,
        "era: pre-industrial; streets are earth or cobble, the light is daylight or tallow"
            .into(),
    );
    f
}

/// The annals of a realm as the given year knew them: entries after the
/// year have not happened; a war still burning must not have its outcome
/// told, so its entry is rewritten without one.
pub fn annal_lines_in(planet: &Planet, capital_cell: u32, year: u32) -> Vec<String> {
    let hist = planet.history();
    let Some(rh) = hist.realm(capital_cell) else {
        return Vec::new();
    };
    let civ = planet.civilization();
    let mut out = Vec::new();
    for a in rh.annals.iter().filter(|a| a.year <= year) {
        if let Some(w) = hist.wars.iter().find(|w| {
            (w.a == capital_cell || w.b == capital_cell)
                && w.start == a.year
                && w.end > year
                && a.text.starts_with("war with")
        }) {
            let other = if w.a == capital_cell { w.b } else { w.a };
            let other_name = civ
                .settlements
                .iter()
                .find(|s| s.cell == other)
                .map(|s| s.name.as_str())
                .unwrap_or("the lost realm");
            out.push(format!(
                "Year {} — war with the Realm of {} breaks out over {}; it burns still",
                a.year, other_name, w.cause
            ));
            continue;
        }
        out.push(format!("Year {} — {}", a.year, a.text));
    }
    out
}

/// A settlement's brief as an earlier year knew it: the timeless ground —
/// landscape, water, named geography — plus only what that year could say
/// of people, crowns and numbers. No economy, no interiors, no household
/// ages: those are functions of the present.
fn settlement_facts_in(planet: &Planet, i: usize, year: u32) -> String {
    let civ = planet.civilization();
    let h = planet.hydrology();
    let s = &civ.settlements[i];
    let (lat, lon) = unit_to_lat_lon(s.pos);
    let cl = planet.climate(lat, lon);
    let biome = classify_biome(cl.temp_c, cl.precip);
    let e = planet.bulk_elevation(lat, lon);
    let cap = world_gen::realm_in(planet, i, year).unwrap_or(s.realm_capital);
    let realm_name = civ
        .settlements
        .iter()
        .find(|c| c.cell == cap)
        .map(|c| c.name.clone())
        .unwrap_or_else(|| s.realm.clone());
    let founded = world_gen::founded_in(planet, i);

    let mut f = String::new();
    line(&mut f, format!("the year is {year}"));
    line(
        &mut f,
        format!(
            "{} of about {} people (roughly {} households), in the Realm of {}",
            kind_word(s),
            round_pop(world_gen::population_in(planet, i, year)),
            (world_gen::population_in(planet, i, year) as f64 / 4.6).round() as u32,
            realm_name
        ),
    );
    line(
        &mut f,
        if year < founded + 40 {
            format!("a young place, founded only in year {founded}")
        } else {
            format!("founded in year {founded}")
        },
    );
    line(
        &mut f,
        format!(
            "landscape: {} {} at {}",
            terrain_word(e),
            biome_word(biome),
            latitude_word(lat)
        ),
    );
    let geo = planet.geography();
    if let Some(land) = geo.landmass_at(s.cell) {
        if land.kind == world_gen::NaturalKind::Island {
            line(&mut f, format!("it stands on {}", land.name));
        }
    }
    if let Some(cov) = geo.cover_at(s.cell) {
        line(&mut f, format!("it lies within {}", cov.name));
    }
    if let Some(rel) = geo.relief_at(s.cell) {
        line(&mut f, format!("it sits among {}", rel.name));
    }
    line(
        &mut f,
        format!(
            "climate: mean {:.0} °C, {} rainfall",
            cl.temp_c,
            precip_word(cl.precip)
        ),
    );
    if let Some(river) = civilization::river_name(planet.seed, h, s.cell) {
        line(&mut f, format!("sits on the {river}"));
    }
    if s.port {
        line(
            &mut f,
            "a sheltered natural harbor; livelihood tied to the sea".into(),
        );
    }
    if s.capital {
        line(&mut f, "seat of its realm".into());
    }
    if let Some(r) = planet.history().ruler_in(cap, year) {
        line(
            &mut f,
            format!(
                "the realm is ruled by {} {} of House {}, on the seat since year {}",
                r.title, r.name, r.house, r.accession
            ),
        );
    }
    // What living memory holds in that year.
    for a in annal_lines_in(planet, cap, year)
        .iter()
        .rev()
        .filter(|a| {
            a.strip_prefix("Year ")
                .and_then(|t| t.split(' ').next())
                .and_then(|y| y.parse::<u32>().ok())
                .is_some_and(|y| y + 40 >= year)
        })
        .take(2)
    {
        line(&mut f, format!("in living memory: {a}"));
    }
    // Neighbors that already stood in that year.
    let mut near: Vec<(f64, usize)> = civ
        .settlements
        .iter()
        .enumerate()
        .filter(|&(j, _)| j != i && world_gen::founded_in(planet, j) <= year)
        .map(|(j, t)| (chord(s.pos, t.pos), j))
        .collect();
    near.sort_by(|a, b| a.0.total_cmp(&b.0));
    for &(d, j) in near.iter().take(3) {
        let t = &civ.settlements[j];
        line(
            &mut f,
            format!(
                "{} km {} lies the {} of {} ({})",
                (d * 6371.0).round() as u32,
                compass(s.pos, t.pos),
                kind_word(t),
                t.name,
                if world_gen::realm_in(planet, j, year) == Some(cap) {
                    "same realm"
                } else {
                    "a neighboring realm"
                }
            ),
        );
    }
    line(
        &mut f,
        "era: pre-industrial. Life expectancy ~33 at birth (mid-40s if \
          childhood is survived); roughly a fifth of infants do not live a \
          year; women marry at 14-25 and men at 18-30; births come about \
          every two and a half years; few see 70"
            .into(),
    );
    f
}

/// A realm's brief as an earlier year knew it.
fn realm_facts_in(planet: &Planet, capital: usize, year: u32) -> String {
    let civ = planet.civilization();
    let cap = &civ.settlements[capital];
    let hist = planet.history();

    let held: Vec<usize> = (0..civ.settlements.len())
        .filter(|&j| j != capital && world_gen::realm_in(planet, j, year) == Some(cap.cell))
        .collect();
    let towns = held
        .iter()
        .filter(|&&j| civ.settlements[j].kind == SettlementKind::Town)
        .count();
    let villages = held.len() - towns;
    let people: u32 = world_gen::population_in(planet, capital, year)
        + held
            .iter()
            .map(|&j| world_gen::population_in(planet, j, year))
            .sum::<u32>();

    let mut f = String::new();
    line(&mut f, format!("the year is {year}"));
    line(
        &mut f,
        format!("the realm is held from {}, its capital and only city", cap.name),
    );
    line(
        &mut f,
        format!(
            "in this year it counts {} towns and {} villages — some {} souls in all",
            towns,
            villages,
            round_pop(people)
        ),
    );
    if let Some(rh) = hist.realm(cap.cell) {
        let reigns = rh.rulers.iter().filter(|r| r.accession <= year).count();
        line(
            &mut f,
            format!(
                "the seat has passed through {} reigns since the founding in year {}",
                reigns, rh.founding_year
            ),
        );
    }
    if let Some(r) = hist.ruler_in(cap.cell, year) {
        line(
            &mut f,
            format!(
                "the seat is held by {} {} of House {}, since year {}",
                r.title, r.name, r.house, r.accession
            ),
        );
    }
    for w in hist.wars.iter().filter(|w| {
        (w.a == cap.cell || w.b == cap.cell) && w.start <= year && w.end > year
    }) {
        let other = if w.a == cap.cell { w.b } else { w.a };
        if let Some(o) = civ.settlements.iter().find(|s| s.cell == other) {
            line(
                &mut f,
                format!(
                    "a war with the Realm of {} over {} burns as this is written",
                    o.name, w.cause
                ),
            );
        }
    }
    f.push_str("\nThe annals:\n");
    for a in annal_lines_in(planet, cap.cell, year) {
        line(&mut f, a);
    }
    f
}

fn line(f: &mut String, t: String) {
    f.push_str("- ");
    f.push_str(&t);
    f.push('\n');
}

trait RealmDisplay {
    fn realm_display(&self) -> String;
}
impl RealmDisplay for Settlement {
    fn realm_display(&self) -> String {
        format!("Realm of {}", self.realm)
    }
}

fn round_pop(p: u32) -> u32 {
    if p >= 1000 {
        p / 100 * 100
    } else {
        p / 10 * 10
    }
}

fn terrain_word(e: f64) -> &'static str {
    if e < 0.08 {
        "lowland"
    } else if e < 0.20 {
        "hill-country"
    } else if e < 0.35 {
        "highland"
    } else {
        "mountain"
    }
}

fn biome_word(b: Biome) -> &'static str {
    match b {
        Biome::Ocean => "coast",
        Biome::IceCap => "ice fields",
        Biome::Tundra => "tundra",
        Biome::ColdSteppe => "cold steppe",
        Biome::Boreal => "boreal forest",
        Biome::Desert => "desert",
        Biome::Grassland => "grassland",
        Biome::TemperateForest => "temperate forest",
        Biome::TemperateRainforest => "rain-soaked forest",
        Biome::Savanna => "savanna",
        Biome::TropicalForest => "tropical forest",
        Biome::TropicalRainforest => "tropical rainforest",
    }
}

fn latitude_word(lat: f64) -> String {
    let deg = lat.to_degrees();
    let band = match deg.abs() {
        d if d < 15.0 => "equatorial latitudes",
        d if d < 35.0 => "subtropical latitudes",
        d if d < 55.0 => "temperate latitudes",
        _ => "far northern latitudes",
    };
    if deg.abs() >= 55.0 && deg < 0.0 {
        return "far southern latitudes".into();
    }
    band.into()
}

fn precip_word(p: f64) -> &'static str {
    match p {
        p if p < 0.15 => "scant",
        p if p < 0.40 => "modest",
        p if p < 0.70 => "generous",
        _ => "relentless",
    }
}

fn compass(from: [f64; 3], to: [f64; 3]) -> &'static str {
    let (lat1, lon1) = unit_to_lat_lon(from);
    let (lat2, lon2) = unit_to_lat_lon(to);
    let dlon = (lon2 - lon1 + std::f64::consts::PI).rem_euclid(std::f64::consts::TAU)
        - std::f64::consts::PI;
    let y = dlon.sin() * lat2.cos();
    let x = lat1.cos() * lat2.sin() - lat1.sin() * lat2.cos() * dlon.cos();
    let bearing = y.atan2(x).to_degrees().rem_euclid(360.0);
    const WINDS: [&str; 8] = [
        "north",
        "northeast",
        "east",
        "southeast",
        "south",
        "southwest",
        "west",
        "northwest",
    ];
    WINDS[((bearing + 22.5) / 45.0) as usize % 8]
}

#[inline]
fn chord(a: [f64; 3], b: [f64; 3]) -> f64 {
    let d = [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
}
