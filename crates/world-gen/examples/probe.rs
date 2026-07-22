//! Debug probe: what does hydrology think about a lat/lon (degrees)?
//! cargo run -p world-gen --example probe -- <lat> <lon> [<lat> <lon> ...]

fn main() {
    let planet = world_gen::Planet::new(42);
    planet.hydrology();
    let args: Vec<f64> = std::env::args()
        .skip(1)
        .map(|a| a.parse().expect("lat/lon degrees"))
        .collect();
    for pair in args.chunks(2) {
        let (lat, lon) = (pair[0].to_radians(), pair[1].to_radians());
        let raw = planet.elevation_raw(lat, lon, 8);
        let carved = planet.elevation(lat, lon, 8);
        let water = planet.water_level(lat, lon);
        let bulk = planet.bulk_elevation(lat, lon);
        let t = planet.tectonics_at(lat, lon);
        println!(
            "({:7.3}, {:8.3})  raw {:+.8}  carved {:+.8}  bulk {:+.6}  water {:?}  plate {} edge {:.4} conv {:+.3} belt {:.3}",
            pair[0], pair[1], raw, carved, bulk, water, t.plate, t.edge, t.convergence, t.belt,
        );
    }
}
