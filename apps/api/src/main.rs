use axum::{
    routing::get,
    Router,
};
use std::net::SocketAddr;
use tracing_subscriber;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let app = Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready));

    let addr = SocketAddr::from(([127, 0, 0, 1], 8000));
    tracing::info!("listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

async fn health() -> &'static str {
    "OK"
}

async fn ready() -> &'static str {
    "OK"
}
