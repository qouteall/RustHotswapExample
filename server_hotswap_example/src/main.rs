use std::env;
use std::sync::{Arc, LazyLock, Mutex};

use axum::{
    body::Body,
    http::{Response, StatusCode, Uri},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use futures::FutureExt;
use serde::Serialize;
use tower_http::services::ServeDir;
use tracing::{error, info};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() {

    for (key, value) in env::vars() {
        println!("{}: {}", key, value);
    }

    println!("Env vars done");

    // https://github.com/DioxusLabs/dioxus/issues/4305#issuecomment-3204091426
    let f =dioxus_devtools::serve_subsecond(router_main);
    let mut f_boxed = f.boxed_local();
    f_boxed.as_mut().await;
}


async fn router_main() {
    use axum::{Router, routing::get};

    let app = Router::new().route("/", get(test_route));

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    println!("Server running on http://localhost:3000");

    axum::serve(listener, app.clone()).await.unwrap()
}

async fn test_route() -> axum::response::Html<&'static str> {
    "axum works!!!!".into()
}