mod notes;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;
use tower_http::services::ServeDir;
use world_gen::{Planet, GEN_VERSION};

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
            | "roads" | "labels"
    )
        || z > MAX_ZOOM
        || x >= (1u32 << z.min(31))
        || y >= (1u32 << z.min(31))
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    let (ext, mime) = if matches!(layer.as_str(), "rivers" | "settlements" | "roads" | "labels") {
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
        _ => world_tiles::render_elevation_tile(&render_app.planet, z, x, y),
    })
    .await
    .expect("render task");

    write_cache(&path, &body).await;
    tile_response(body, mime)
}

/// Lore endpoint: cached canon returns immediately; unwritten entries start
/// generating in the background and the client polls until ready.
async fn lore_entry(State(app): State<Arc<App>>, Path(id): Path<String>) -> Response {
    let planet = app.planet.clone();
    let engine = app.lore.clone();
    let req_id = id.clone();
    let status = tokio::task::spawn_blocking(move || engine.request(&planet, &req_id))
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
    // every state, canon whether or not the chronicle is written yet.
    if let Some(cell) = id.strip_prefix('r').and_then(|c| c.parse::<u32>().ok()) {
        if let Some(rh) = app.planet.history().realm(cell) {
            let mut lines: Vec<String> = Vec::new();
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
    // people as clickable atlas references.
    if let Some(cell) = id.strip_prefix('s').and_then(|c| c.parse::<u32>().ok()) {
        let civ = app.planet.civilization();
        if let Some(i) = civ.settlements.iter().position(|s| s.cell == cell) {
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
