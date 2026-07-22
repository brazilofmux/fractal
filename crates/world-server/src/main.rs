use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use tower_http::services::ServeDir;
use world_gen::Planet;

const MAX_ZOOM: u32 = 24;

#[tokio::main]
async fn main() {
    let seed: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(42);
    let planet = Arc::new(Planet::new(seed));

    let app = Router::new()
        .route("/tiles/elevation/{z}/{x}/{y}", get(elevation_tile))
        .fallback_service(ServeDir::new("web"))
        .with_state(planet);

    let addr = "127.0.0.1:8632";
    println!("seed {seed} → http://{addr}");
    let listener = tokio::net::TcpListener::bind(addr).await.expect("bind");
    axum::serve(listener, app).await.expect("serve");
}

async fn elevation_tile(
    State(planet): State<Arc<Planet>>,
    Path((z, x, y)): Path<(u32, u32, String)>,
) -> Response {
    let Ok(y) = y.trim_end_matches(".png").parse::<u32>() else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    if z > MAX_ZOOM || x >= (1u32 << z.min(31)) || y >= (1u32 << z.min(31)) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let png = tokio::task::spawn_blocking(move || {
        world_tiles::render_elevation_tile(&planet, z, x, y)
    })
    .await
    .expect("render task");

    (
        [
            (header::CONTENT_TYPE, "image/png"),
            (header::CACHE_CONTROL, "public, max-age=3600"),
        ],
        png,
    )
        .into_response()
}
