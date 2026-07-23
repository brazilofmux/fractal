mod notes;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;
use tower_http::services::ServeDir;
use world_core::geo::unit_to_lat_lon;
use world_gen::{Planet, GEN_VERSION, PRESENT_YEAR};

const MAX_ZOOM: u32 = 24;

struct App {
    planet: Arc<Planet>,
    cache_dir: PathBuf,
    lore: Arc<lore::LoreEngine>,
    notes: notes::NoteStore,
}

#[tokio::main]
async fn main() {
    let seed: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(42);
    let cache_dir = std::env::args()
        .nth(2)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("cache"));

    let lore = lore::LoreEngine::open(std::path::Path::new("lore.sqlite"), seed, GEN_VERSION)
        .expect("open lore cache");
    println!(
        "lore: {} · {} entries in canon · model {}",
        if lore.enabled() {
            "enabled"
        } else {
            "disabled (no ANTHROPIC_API_KEY / ant profile)"
        },
        lore.entries_written(),
        lore.model,
    );

    let notes = notes::NoteStore::open(std::path::Path::new("notes.sqlite"), seed)
        .expect("open notes store");
    println!("notes: {} marks on this world", notes.list().len());

    let app = Arc::new(App {
        planet: Arc::new(Planet::new(seed)),
        cache_dir,
        lore: Arc::new(lore),
        notes,
    });

    // Solve the global drainage graph before serving: every tile at every
    // zoom depends on it (carved valleys, lakes, river vectors), and it is
    // cheap enough to never be worth deferring.
    let t0 = std::time::Instant::now();
    let hydro = app.planet.hydrology();
    println!(
        "hydrology: {} river edges · {} lake cells · solved in {:.1?}",
        hydro.rivers().len(),
        hydro.lake_cell_count(),
        t0.elapsed()
    );

    let t0 = std::time::Instant::now();
    let civ = app.planet.civilization();
    let ports = civ.settlements.iter().filter(|s| s.port).count();
    let cities = civ.settlements.iter().filter(|s| s.capital).count();
    println!(
        "civilization: {} settlements ({cities} cities, {ports} ports) · {} roads · settled in {:.1?}",
        civ.settlements.len(),
        civ.roads.len(),
        t0.elapsed()
    );

    let router = Router::new()
        .route("/tiles/{layer}/{z}/{x}/{y}", get(tile))
        .route("/lore/{id}", get(lore_entry))
        .route("/state/{year}", get(state_in))
        .route("/notes", get(notes_list).post(notes_add))
        .route("/notes/{id}", delete(notes_delete))
        .fallback_service(ServeDir::new("web"))
        .with_state(app.clone());

    let addr = "127.0.0.1:8632";
    println!(
        "seed {seed} · gen v{GEN_VERSION} · cache {} → http://{addr}",
        app.cache_dir.display()
    );
    let listener = tokio::net::TcpListener::bind(addr).await.expect("bind");
    axum::serve(listener, router).await.expect("serve");
}

async fn tile(
    State(app): State<Arc<App>>,
    Path((layer, z, x, y)): Path<(String, u32, u32, String)>,
) -> Response {
    let Ok(y) = y
        .trim_end_matches(".png")
        .trim_end_matches(".mvt")
        .parse::<u32>()
    else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    if !matches!(
        layer.as_str(),
        "elevation" | "plates" | "temperature" | "precipitation" | "rivers" | "settlements"
            | "roads" | "labels" | "lanes"
    )
        || z > MAX_ZOOM
        || x >= (1u32 << z.min(31))
        || y >= (1u32 << z.min(31))
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    let (ext, mime) = if matches!(
        layer.as_str(),
        "rivers" | "settlements" | "roads" | "labels" | "lanes"
    ) {
        ("mvt", "application/x-protobuf")
    } else {
        ("png", "image/png")
    };

    // Cache is an optimization, never a source of truth: keyed on generator
    // version and seed, so nothing stale can survive a generator change.
    let path = app.cache_dir.join(format!(
        "v{GEN_VERSION}/{}/{layer}/{z}/{x}/{y}.{ext}",
        app.planet.seed
    ));
    if let Ok(bytes) = tokio::fs::read(&path).await {
        return tile_response(bytes, mime);
    }

    let render_app = app.clone();
    let body = tokio::task::spawn_blocking(move || match layer.as_str() {
        "plates" => world_tiles::render_plates_tile(&render_app.planet, z, x, y),
        "temperature" => world_tiles::render_temperature_tile(&render_app.planet, z, x, y),
        "precipitation" => world_tiles::render_precipitation_tile(&render_app.planet, z, x, y),
        "rivers" => world_tiles::render_rivers_tile(&render_app.planet, z, x, y),
        "settlements" => world_tiles::render_settlements_tile(&render_app.planet, z, x, y),
        "roads" => world_tiles::render_roads_tile(&render_app.planet, z, x, y),
        "labels" => world_tiles::render_labels_tile(&render_app.planet, z, x, y),
        "lanes" => world_tiles::render_lanes_tile(&render_app.planet, z, x, y),
        _ => world_tiles::render_elevation_tile(&render_app.planet, z, x, y),
    })
    .await
    .expect("render task");

    write_cache(&path, &body).await;
    tile_response(body, mime)
}

#[derive(Deserialize)]
struct YearQuery {
    year: Option<u32>,
}

/// Lore endpoint: cached canon returns immediately; unwritten entries start
/// generating in the background and the client polls until ready. The
/// optional `year` query is the fourth coordinate — the chronicler writes
/// from whatever year the viewer is parked in.
async fn lore_entry(
    State(app): State<Arc<App>>,
    Path(id): Path<String>,
    Query(q): Query<YearQuery>,
) -> Response {
    let year = q.year.unwrap_or(PRESENT_YEAR).clamp(1, PRESENT_YEAR);
    let planet = app.planet.clone();
    let engine = app.lore.clone();
    let req_id = id.clone();
    let status = tokio::task::spawn_blocking(move || engine.request(&planet, &req_id, year))
        .await
        .expect("lore task");

    let mut body = match status {
        lore::LoreStatus::Ready { name, body, realm } => json!({
            "status": "ready", "name": name, "body": body,
            "realm": realm.map(|(id, name)| json!({"id": id, "name": name})),
        }),
        lore::LoreStatus::Generating => json!({"status": "generating"}),
        lore::LoreStatus::Disabled { hint } => json!({"status": "disabled", "hint": hint}),
        lore::LoreStatus::Failed { message } => json!({"status": "failed", "message": message}),
        lore::LoreStatus::NotFound => {
            return (StatusCode::NOT_FOUND, "no such feature").into_response()
        }
    };
    // Realm annals come straight from the generator: instantly available in
    // every state, canon whether or not the chronicle is written yet. Parked
    // in an earlier year, the list stops where that year's knowledge does.
    if let Some(cell) = id.strip_prefix('r').and_then(|c| c.parse::<u32>().ok()) {
        if year < PRESENT_YEAR && app.planet.history().realm(cell).is_some() {
            body["annals"] = lore::context::annal_lines_in(&app.planet, cell, year)
                .into_iter()
                .map(|l| json!(l))
                .collect();
        } else if let Some(rh) = app.planet.history().realm(cell) {
            let mut lines: Vec<String> = Vec::new();
            if let Some(marks) = app.planet.economy().realm_ledger.get(&cell) {
                lines.push(format!("The crown's ledger: some {marks} marks a year"));
            }
            if let Some(houses) = app.planet.peerage().houses(cell) {
                for h in houses {
                    let seat = match h.held_seat {
                        Some((a, _)) if h.reigning => format!("royal since {a}"),
                        Some((a, b)) => format!("held the seat {a}–{b}"),
                        None => "never royal".into(),
                    };
                    lines.push(format!("House {} — {}; {}", h.name, seat, h.disposition));
                }
            }
            lines.extend(
                rh.annals
                    .iter()
                    .map(|a| format!("Year {} — {}", a.year, a.text)),
            );
            body["annals"] = lines.into_iter().map(|l| json!(l)).collect();
        }
    }
    // Likewise a settlement's interior: wards and trades as plain lines,
    // people as clickable atlas references. Interiors, trade and the living
    // are functions of the present; an earlier year shows what it can know.
    if let Some(cell) = id.strip_prefix('s').and_then(|c| c.parse::<u32>().ok()) {
        let civ = app.planet.civilization();
        if year < PRESENT_YEAR {
            if let Some(i) = civ.settlements.iter().position(|s| s.cell == cell) {
                let planet = &app.planet;
                let mut lines: Vec<String> = Vec::new();
                lines.push(format!(
                    "Founded in year {}",
                    world_gen::founded_in(planet, i)
                ));
                lines.push(format!(
                    "In year {year}: some {} souls",
                    world_gen::population_in(planet, i, year)
                ));
                if let Some(cap) = world_gen::realm_in(planet, i, year) {
                    if let Some(c) = civ.settlements.iter().find(|s| s.cell == cap) {
                        lines.push(format!("Held by the Realm of {}", c.name));
                        if let Some(r) = planet.history().ruler_in(cap, year) {
                            lines.push(format!(
                                "Ruled by {} {} of House {}, since year {}",
                                r.title, r.name, r.house, r.accession
                            ));
                        }
                    }
                }
                body["annals"] = lines.into_iter().map(|l| json!(l)).collect();
            }
        } else if let Some(i) = civ.settlements.iter().position(|s| s.cell == cell) {
            let inside = world_gen::interior(&app.planet, i);
            let mut lines: Vec<String> = Vec::new();
            if !inside.wards.is_empty() {
                let names: Vec<&str> = inside.wards.iter().map(|w| w.name.as_str()).collect();
                lines.push(format!("Wards: {}", names.join(" · ")));
            }
            if !inside.trades.is_empty() {
                let t: Vec<String> = inside
                    .trades
                    .iter()
                    .take(8)
                    .map(|t| format!("{} {}", t.count, t.name))
                    .collect();
                lines.push(format!("Trades: {}", t.join(", ")));
            }
            let econ = app.planet.economy();
            lines.push(format!(
                "Wealth: {}",
                world_gen::Economy::wealth_word(econ.wealth[i])
            ));
            if !econ.produces[i].is_empty() {
                lines.push(format!(
                    "Makes: {}",
                    econ.produces[i].iter().map(|g| g.word()).collect::<Vec<_>>().join(", ")
                ));
            }
            if !econ.imports[i].is_empty() {
                let buys: Vec<String> = econ.imports[i]
                    .iter()
                    .take(5)
                    .map(|(g, src)| format!("{} from {}", g.word(), civ.settlements[*src].name))
                    .collect();
                lines.push(format!("Buys: {}", buys.join(", ")));
            }
            if !econ.wanting[i].is_empty() {
                lines.push(format!(
                    "Goes without: {}",
                    econ.wanting[i].iter().map(|g| g.word()).collect::<Vec<_>>().join(", ")
                ));
            }
            body["annals"] = lines.into_iter().map(|l| json!(l)).collect();
            body["people"] = world_gen::people_of(&app.planet, i)
                .into_iter()
                .map(|(name, role, slot)| {
                    json!({ "id": format!("p{cell}x{slot}"), "text": format!("{name} — {role}") })
                })
                .collect();
        }
    }
    // A person's household is generator truth: shown instantly.
    if id.starts_with('p') {
        if let Some((cell, slot)) = id[1..]
            .split_once('x')
            .and_then(|(c, s)| Some((c.parse::<u32>().ok()?, s.parse::<u8>().ok()?)))
        {
            if let Some((_, lines)) = world_gen::household_lines(&app.planet, cell, slot) {
                body["annals"] = lines.into_iter().map(|l| json!(l)).collect();
            }
        }
    }
    Json(body).into_response()
}

/// The world as a given year knew it: every settlement already founded,
/// with that year's head count and allegiance. Small enough to compute on
/// demand — the fourth coordinate is an input, never a state.
async fn state_in(State(app): State<Arc<App>>, Path(year): Path<u32>) -> Response {
    let year = year.clamp(1, PRESENT_YEAR);
    let planet = app.planet.clone();
    let body = tokio::task::spawn_blocking(move || {
        let civ = planet.civilization();
        let settlements: Vec<serde_json::Value> = civ
            .settlements
            .iter()
            .enumerate()
            .filter_map(|(i, s)| {
                let cap = world_gen::realm_in(&planet, i, year)?;
                let realm = civ
                    .settlements
                    .iter()
                    .find(|c| c.cell == cap)
                    .map(|c| c.name.as_str())
                    .unwrap_or("");
                let (lat, lon) = unit_to_lat_lon(s.pos);
                Some(json!({
                    "cell": s.cell,
                    "name": s.name,
                    "rank": s.kind.rank(),
                    "port": s.port as u8,
                    "lat": lat.to_degrees(),
                    "lon": lon.to_degrees(),
                    "pop": world_gen::population_in(&planet, i, year),
                    "realm_capital": cap,
                    "realm": realm,
                }))
            })
            .collect();
        json!({ "year": year, "settlements": settlements })
    })
    .await
    .expect("state task");
    Json(body).into_response()
}

#[derive(Deserialize)]
struct NewNote {
    lat: f64,
    lng: f64,
    title: String,
    #[serde(default)]
    body: String,
}

async fn notes_list(State(app): State<Arc<App>>) -> Json<serde_json::Value> {
    Json(json!({ "notes": app.notes.list() }))
}

async fn notes_add(State(app): State<Arc<App>>, Json(n): Json<NewNote>) -> Response {
    match app.notes.add(n.lat, n.lng, &n.title, &n.body) {
        Ok(note) => Json(json!(note)).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, e).into_response(),
    }
}

async fn notes_delete(State(app): State<Arc<App>>, Path(id): Path<i64>) -> Response {
    if app.notes.delete(id) {
        StatusCode::NO_CONTENT.into_response()
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}

/// Best-effort atomic cache write: unique temp name, then rename, so a
/// concurrent reader can never see a torn PNG. Failures are ignored — the
/// tile can always be re-derived.
async fn write_cache(path: &std::path::Path, bytes: &[u8]) {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let Some(parent) = path.parent() else { return };
    if tokio::fs::create_dir_all(parent).await.is_err() {
        return;
    }
    let tmp = path.with_extension(format!(
        "tmp.{}.{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    if tokio::fs::write(&tmp, bytes).await.is_ok() {
        let _ = tokio::fs::rename(&tmp, path).await;
    }
}

fn tile_response(bytes: Vec<u8>, mime: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, mime),
            (header::CACHE_CONTROL, "public, max-age=3600"),
        ],
        bytes,
    )
        .into_response()
}
