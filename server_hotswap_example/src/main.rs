use std::sync::{Arc, LazyLock, Mutex};

use axum::{
    body::Body,
    http::{Response, StatusCode, Uri},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::Serialize;
use tower_http::services::ServeDir;
use tracing::{error, info};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Serialize)]
struct IndexResponse {
    hello: String,
}

async fn index() -> impl IntoResponse {
    Json(IndexResponse {
        hello: "world".to_string(),
    })
}

static COUNTER: Mutex<i32> = Mutex::new(0);

async fn increment() -> impl IntoResponse {
    let mut counter = COUNTER.lock().unwrap();

    *counter += 1;
    // *counter *= 2;

    Json(*counter)
}

// https://github.com/DioxusLabs/dioxus/pull/4588

#[tokio::main]
async fn main() {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    dioxus_devtools::serve_subsecond(sub_main).await;
}

async fn sub_main() {
    log::info!("Doing initialization");

    let app = Router::new()
        .route("/", get(index))
        .route("/increment", post(increment));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000")
        .await
        .unwrap();
    info!("listening on http://{}", listener.local_addr().unwrap());
    if let Err(err) = axum::serve(listener, app).await {
        error!("server error: {}", err);
    }
}
