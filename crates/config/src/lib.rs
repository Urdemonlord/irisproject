use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    pub env: String,
    pub host: String,
    pub port: u16,
    pub database_url: String,
    pub session_secret: String,
    pub stalwart_api_url: String,
    pub temp_inbox_domain: String,
    pub temp_inbox_default_expiry_hours: u32,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        dotenv::dotenv().ok();
        Ok(Self {
            env: std::env::var("IRIS_ENV").unwrap_or_else(|_| "development".to_string()),
            host: std::env::var("IRIS_HOST").unwrap_or_else(|_| "127.0.0.1".to_string()),
            port: std::env::var("IRIS_PORT")
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or(8000),
            database_url: std::env::var("DATABASE_URL")
                .unwrap_or_else(|_| "sqlite:iris.db".to_string()),
            session_secret: std::env::var("SESSION_SECRET")
                .unwrap_or_else(|_| "dev-secret-change-in-prod".to_string()),
            stalwart_api_url: std::env::var("STALWART_API_URL")
                .unwrap_or_else(|_| "http://localhost:8091".to_string()),
            temp_inbox_domain: std::env::var("TEMP_INBOX_DOMAIN")
                .unwrap_or_else(|_| "inbox.meowlabs.id".to_string()),
            temp_inbox_default_expiry_hours: std::env::var("TEMP_INBOX_DEFAULT_EXPIRY_HOURS")
                .ok()
                .and_then(|h| h.parse().ok())
                .unwrap_or(1),
        })
    }
}
