use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use tower_http::services::ServeDir;
use world_gen::{Planet, GEN_VERSION};

const MAX_ZOOM: u32 = 24;

struct App {
    planet: Planet,
    cache_dir: PathBuf,
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

    let app = Arc::new(App {
        planet: Planet::new(seed),
        cache_dir,
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

    let router = Router::new()
        .route("/tiles/{layer}/{z}/{x}/{y}", get(tile))
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
        "elevation" | "plates" | "temperature" | "precipitation" | "rivers"
    )
        || z > MAX_ZOOM
        || x >= (1u32 << z.min(31))
        || y >= (1u32 << z.min(31))
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    let (ext, mime) = if layer == "rivers" {
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
        _ => world_tiles::render_elevation_tile(&render_app.planet, z, x, y),
    })
    .await
    .expect("render task");

    write_cache(&path, &body).await;
    tile_response(body, mime)
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
