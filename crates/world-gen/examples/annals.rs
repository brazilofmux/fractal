//! Debug probe: print a realm's annals.
//! cargo run -p world-gen --example annals -- <capital-name>...

fn main() {
    let planet = world_gen::Planet::new(42);
    let civ = planet.civilization();
    let hist = planet.history();
    for name in std::env::args().skip(1) {
        let cap = civ
            .settlements
            .iter()
            .find(|s| s.capital && s.name == name)
            .unwrap_or_else(|| panic!("no capital named {name}"));
        let r = hist.realm(cap.cell).unwrap();
        println!("== Realm of {} (r{}) — founded year {}", cap.name, cap.cell, r.founding_year);
        for a in &r.annals {
            println!("  Year {:>3} — {}", a.year, a.text);
        }
        println!();
    }
}
