

use axum::{
    body::Body,
    http::{Response, StatusCode, Uri},
    response::IntoResponse,
    routing::get,
    Router,
};
use tower_http::services::ServeDir;
use tracing::{error, info};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

async fn index() -> impl IntoResponse {
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/json; charset=utf-8")
        .body(Body::from("{\"hello\":\"world\"}"))
        .unwrap()
}

#[tokio::main]
async fn main() {
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with(tracing_subscriber::fmt::layer())
        .init();


    let app = Router::new()
        .route("/", get(index));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000").await.unwrap();
    info!("listening on http://{}", listener.local_addr().unwrap());
    if let Err(err) = axum::serve(listener, app).await {
        error!("server error: {}", err);
    }
}


