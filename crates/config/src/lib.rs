pub mod limits;

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
    /// Fixed sender identity for the Iris transactional-email API.
    pub integration_email_from: String,
    /// Explicit opt-in for integration email delivery; defaults to disabled.
    pub integration_email_enabled: bool,
    /// Dedicated capability shared only with Timeless for password-reset requests.
    pub iris_timeless_service_token: String,
    /// Fixed sender identity for Timeless password-reset messages.
    pub iris_timeless_email_from: String,
    /// Fixed recipient loaded from a server-side secret, never from request data.
    pub iris_timeless_email_to: String,
    /// DNS name only (no scheme/path); Iris always uses HTTPS for reset links.
    pub timeless_public_origin: String,
    /// Relay endpoint; production is pinned to the official Resend endpoint.
    pub resend_api_url: String,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        dotenvy::dotenv().ok();
        let env = std::env::var("IRIS_ENV").unwrap_or_else(|_| "development".to_string());
        let resend_api_url =
            resend_api_endpoint(&env, std::env::var("IRIS_RESEND_API_URL").ok().as_deref());
        Ok(Self {
            env,
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
                .split(',')
                .map(|d| d.trim().to_lowercase())
                .filter(|d| !d.is_empty())
                .collect(),
            send_external: std::env::var("SEND_EXTERNAL")
                .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true")),
            resend_api_key: std::env::var("RESEND_API_KEY").unwrap_or_default(),
            integration_email_from: std::env::var("IRIS_INTEGRATION_EMAIL_FROM")
                .unwrap_or_default(),
            integration_email_enabled: std::env::var("IRIS_INTEGRATION_EMAIL_ENABLED")
                .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true")),
            iris_timeless_service_token: std::env::var("IRIS_TIMELESS_SERVICE_TOKEN")
                .unwrap_or_default(),
            iris_timeless_email_from: std::env::var("IRIS_TIMELESS_EMAIL_FROM").unwrap_or_default(),
            iris_timeless_email_to: std::env::var("IRIS_TIMELESS_EMAIL_TO").unwrap_or_default(),
            timeless_public_origin: std::env::var("TIMELESS_PUBLIC_ORIGIN").unwrap_or_default(),
            resend_api_url,
        })
    }
}

fn is_local_http_endpoint(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("http://") else {
        return false;
    };
    let authority = rest.split('/').next().unwrap_or_default();
    if authority.is_empty() || authority.contains(['@', '?', '#', '\\']) {
        return false;
    }
    let Some((host, port)) = authority.rsplit_once(':') else {
        return false;
    };
    matches!(host, "localhost" | "127.0.0.1") && port.parse::<u16>().is_ok_and(|port| port > 0)
}

fn resend_api_endpoint(env: &str, override_url: Option<&str>) -> String {
    const RESEND_URL: &str = "https://api.resend.com/emails";
    if env.eq_ignore_ascii_case("production") {
        return RESEND_URL.to_string();
    }
    match override_url
        .map(str::trim)
        .filter(|url| is_local_http_endpoint(url))
    {
        Some(url) => url.to_string(),
        None => RESEND_URL.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_ignores_local_relay_override() {
        assert_eq!(
            resend_api_endpoint("production", Some("http://127.0.0.1:8765")),
            "https://api.resend.com/emails"
        );
    }

    #[test]
    fn development_uses_configured_local_relay_override() {
        assert_eq!(
            resend_api_endpoint("development", Some("http://127.0.0.1:8765")),
            "http://127.0.0.1:8765"
        );
    }

    #[test]
    fn rejects_localhost_userinfo_tricks_that_target_remote_hosts() {
        for url in [
            "http://localhost:8765@attacker.invalid/emails",
            "http://127.0.0.1:8765@attacker.invalid/emails",
        ] {
            assert_eq!(
                resend_api_endpoint("development", Some(url)),
                "https://api.resend.com/emails"
            );
        }
    }
}
