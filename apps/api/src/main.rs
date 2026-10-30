use axum::{
    routing::{get, post},
    Json, Router, extract::State,
};
use std::sync::Arc;
use sqlx::SqlitePool;
use iris_config::Config;
use iris_database;
use iris_mail::JmapClient;
use tracing_subscriber;

struct AppState {
    pool: SqlitePool,
    config: Config,
    jmap_client: JmapClient,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let config = Config::from_env().expect("Failed to load config");
    let pool = iris_database::init_pool(&config.database_url)
        .await
        .expect("Failed to init database");

    let jmap_client = JmapClient::new(
        config.stalwart_api_url.clone(),
        "test@iris.meowlabs.id".to_string(),
        "test".to_string(),
    );

    let state = Arc::new(AppState {
        pool,
        config: config.clone(),
        jmap_client,
    });

    let app = Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .route("/api/v1/session", get(session))
        .with_state(state);

    let addr = format!("{}:{}", config.host, config.port)
        .parse::<std::net::SocketAddr>()
        .expect("Invalid address");
    
    tracing::info!("IRIS API listening on http://{}", addr);

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("Failed to bind");
    
    axum::serve(listener, app).await.expect("Server error");
}

async fn health() -> &'static str {
    "OK"
}

async fn ready(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    match iris_database::health_check(&state.pool).await {
        Ok(_) => Json(serde_json::json!({"ready": true})),
        Err(_) => Json(serde_json::json!({"ready": false})),
    }
}

async fn session(State(_state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    Json(serde_json::json!({"authenticated": false}))
}
