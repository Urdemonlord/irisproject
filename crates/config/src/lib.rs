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
    /// Catch-all sink mailbox for the temp domain (not an admin account).
    pub temp_inbox_mail_user: String,
    pub temp_inbox_mail_password: String,
    /// Domains IRIS may send to while no outbound relay is configured.
    pub send_local_domains: Vec<String>,
    /// Allow sending to any domain (set once the mail server has a working outbound relay).
    pub send_external: bool,
    /// Resend API key; when set, mail to other domains is relayed through Resend.
    pub resend_api_key: String,
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
            temp_inbox_mail_user: std::env::var("TEMP_INBOX_MAIL_USER").unwrap_or_default(),
            temp_inbox_mail_password: std::env::var("TEMP_INBOX_MAIL_PASSWORD").unwrap_or_default(),
            send_local_domains: std::env::var("SEND_LOCAL_DOMAINS")
                .unwrap_or_else(|_| "meowlabs.id,iris.meowlabs.id,inbox.meowlabs.id".to_string())
                .split(',').map(|d| d.trim().to_lowercase()).filter(|d| !d.is_empty()).collect(),
            send_external: std::env::var("SEND_EXTERNAL").is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true")),
            resend_api_key: std::env::var("RESEND_API_KEY").unwrap_or_default(),
        })
    }
}
