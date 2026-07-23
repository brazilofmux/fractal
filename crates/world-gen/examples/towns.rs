//! Debug probe: list the first settlements and their lore feature ids.
//! cargo run -p world-gen --example towns [-- count]

fn main() {
    let planet = world_gen::Planet::new(42);
    let civ = planet.civilization();
    let n: usize = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(8);
    for s in civ.settlements.iter().take(n) {
        println!(
            "s{:<8} {:22} {:?}{}  pop {:>6}  realm of {} (r{})",
            s.cell,
            s.name,
            s.kind,
            if s.port { " port" } else { "" },
            s.population,
            s.realm,
            s.realm_capital,
        );
    }
}
