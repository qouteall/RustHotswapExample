use std::env;
use std::sync::{Arc, LazyLock, Mutex};

use axum::{
    routing::{get, post},
};
use futures::FutureExt;

#[tokio::main]
async fn main() {
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