use std::env;
use std::sync::{Arc, LazyLock, Mutex};
use std::sync::atomic::AtomicUsize;
use axum::{
    routing::{get, post},
};
use dioxus_devtools::subsecond;
use futures::FutureExt;

#[tokio::main]
async fn main() {
    dioxus_devtools::connect_subsecond();

    // https://github.com/DioxusLabs/dioxus/issues/4305#issuecomment-3204091426
    router_main().await;
}


async fn router_main() {
    use axum::{Router, routing::get};

    let app = Router::new().route("/", get(test_route));

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    println!("Server running on http://localhost:3000");

    axum::serve(listener, app.clone()).await.unwrap()
}

async fn test_route() -> axum::response::Html<String> {
    get_str().into()
}

static TEST: AtomicUsize = AtomicUsize::new(0);

fn get_str() -> String {
    subsecond::call(|| {
        format!("test {:?}", (&TEST) as *const AtomicUsize)
    })
}