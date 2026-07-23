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
}

/// Parse a feature id: `s{cell}` for a settlement, `r{cell}` for the realm
/// whose capital sits on that cell.
pub fn parse_id(planet: &Planet, id: &str) -> Option<FeatureRef> {
    let (kind, cell) = id.split_at(1);
    let cell: u32 = cell.parse().ok()?;
    let civ = planet.civilization();
    match kind {
        "s" => civ
            .settlements
            .iter()
            .position(|s| s.cell == cell)
            .map(FeatureRef::Settlement),
        "r" => civ
            .settlements
            .iter()
            .position(|s| s.cell == cell && s.capital)
            .map(FeatureRef::Realm),
        _ => None,
    }
}

pub fn feature_name(planet: &Planet, fref: FeatureRef) -> String {
    let civ = planet.civilization();
    match fref {
        FeatureRef::Settlement(i) => civ.settlements[i].name.clone(),
        FeatureRef::Realm(i) => format!("Realm of {}", civ.settlements[i].name),
    }
}

/// The id of the realm a feature belongs to, and the realm's display name.
pub fn realm_of(planet: &Planet, fref: FeatureRef) -> (String, String) {
    let civ = planet.civilization();
    let s = match fref {
        FeatureRef::Settlement(i) | FeatureRef::Realm(i) => &civ.settlements[i],
    };
    (format!("r{}", s.realm_capital), format!("Realm of {}", s.realm))
}

/// The user-turn prompt for a feature: instruction + canon facts (+ the
/// already-written realm chronicle for settlements, so town stories nest
/// inside their realm's history instead of contradicting it).
pub fn prompt_for(planet: &Planet, fref: FeatureRef, realm_body: Option<&str>) -> String {
    match fref {
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
    }
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
