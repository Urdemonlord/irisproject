pub mod limits;

use serde::{Deserialize, Serialize};

/// Cloudflare's Turnstile token-validation endpoint. Single source of
/// truth: `Config::turnstile_siteverify_url` defaults to it (and the
/// production config pins it to this URL — see
/// `turnstile_siteverify_endpoint`).
pub const TURNSTILE_SITEVERIFY_URL: &str =
    "https://challenges.cloudflare.com/turnstile/v0/siteverify";

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    pub env: String,
    pub host: String,
    pub port: u16,
    pub database_url: String,
    pub session_secret: String,
    /// Cloudflare Turnstile secret key for login verification. Empty =
    /// disabled: logins skip the Turnstile check (see
    /// `apps/api/src/turnstile.rs`). Read from `TURNSTILE_SECRET_KEY`
    /// (trimmed — a stray space would make Siteverify reject every
    /// login); never ship it to the browser — the public half is the
    /// `VITE_TURNSTILE_SITE_KEY` build-time variable of the web app.
    pub turnstile_secret_key: String,
    /// Frontend hostnames a Turnstile token must have been minted on.
    /// Read from `TURNSTILE_HOSTNAMES` (comma-separated, lowercased).
    /// While the secret is set, an empty list fails closed: every login
    /// is rejected (see `apps/api/src/turnstile.rs`). In production,
    /// loopback hostnames are rejected at startup by
    /// `parse_turnstile_hostnames` — a token minted on `localhost` must
    /// never pass this check; outside production they are allowed, so
    /// localhost Turnstile testing works.
    pub turnstile_hostnames: Vec<String>,
    /// Siteverify endpoint for Turnstile token checks. Read from
    /// `TURNSTILE_SITEVERIFY_URL`; unset = the real Cloudflare
    /// endpoint. Production is pinned to the official endpoint by
    /// `turnstile_siteverify_endpoint`; outside production an override
    /// is honoured only with an `https://` scheme, so the secret key
    /// is never POSTed to an arbitrary or plaintext URL.
    pub turnstile_siteverify_url: String,
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
    /// Bearer token that authorizes the optional password-reset webhook
    /// (`POST /internal/v1/password-reset-webhook`). Read from
    /// `IRIS_PASSWORD_RESET_SERVICE_TOKEN`, falling back to the deprecated
    /// `IRIS_TIMELESS_SERVICE_TOKEN`.
    pub password_reset_service_token: String,
    /// Fixed sender identity for password-reset messages. Read from
    /// `IRIS_PASSWORD_RESET_EMAIL_FROM`, falling back to the deprecated
    /// `IRIS_TIMELESS_EMAIL_FROM`.
    pub password_reset_email_from: String,
    /// Fixed recipient loaded from a server-side secret, never from request
    /// data. Read from `IRIS_PASSWORD_RESET_EMAIL_TO`, falling back to the
    /// deprecated `IRIS_TIMELESS_EMAIL_TO`.
    pub password_reset_email_to: String,
    /// DNS name only (no scheme/path) of the companion app that serves the
    /// reset page; Iris always uses HTTPS for reset links. Read from
    /// `IRIS_PASSWORD_RESET_PUBLIC_ORIGIN`, falling back to the deprecated
    /// `TIMELESS_PUBLIC_ORIGIN`.
    pub password_reset_public_origin: String,
    /// Relay endpoint; production is pinned to the official Resend endpoint.
    pub resend_api_url: String,
    /// Separate origin that serves HTML mail bodies (e.g.
    /// `https://mail-content.example.com`). Empty = disabled: the web keeps
    /// rendering HTML mail in a same-origin srcdoc iframe. Only used to
    /// build one-time render URLs; never a secret.
    pub mail_content_origin: String,
    /// Admin allow-list for the read-only admin dashboard
    /// (`GET /api/v1/admin/summary`). Read from `IRIS_ADMIN_EMAILS`
    /// (comma-separated, lowercased). Empty = nobody is an admin: every
    /// admin request answers 403. There is no admin role in the login
    /// flow and `app_users.role` is never read or written by any code,
    /// so an explicit env list is the gate: it is visible in the
    /// deployment config, needs no hand-editing of the production
    /// database, and fails closed when unset.
    pub admin_emails: Vec<String>,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        dotenvy::dotenv().ok();
        let env = std::env::var("IRIS_ENV").unwrap_or_else(|_| "development".to_string());
        let resend_api_url =
            resend_api_endpoint(&env, std::env::var("IRIS_RESEND_API_URL").ok().as_deref());
        let turnstile_siteverify_url = turnstile_siteverify_endpoint(
            &env,
            std::env::var("TURNSTILE_SITEVERIFY_URL").ok().as_deref(),
        );
        let turnstile_hostnames = parse_turnstile_hostnames(
            &env,
            &std::env::var("TURNSTILE_HOSTNAMES").unwrap_or_default(),
        )?;
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
            turnstile_secret_key: std::env::var("TURNSTILE_SECRET_KEY")
                .map(|v| v.trim().to_owned())
                .unwrap_or_default(),
            turnstile_hostnames,
            turnstile_siteverify_url,
            stalwart_api_url: std::env::var("STALWART_API_URL")
                .unwrap_or_else(|_| "http://localhost:8092".to_string()),
            temp_inbox_domain: std::env::var("TEMP_INBOX_DOMAIN")
                .unwrap_or_else(|_| "inbox.example.com".to_string()),
            temp_inbox_default_expiry_hours: std::env::var("TEMP_INBOX_DEFAULT_EXPIRY_HOURS")
                .ok()
                .and_then(|h| h.parse().ok())
                .unwrap_or(1),
            temp_inbox_mail_user: std::env::var("TEMP_INBOX_MAIL_USER").unwrap_or_default(),
            temp_inbox_mail_password: std::env::var("TEMP_INBOX_MAIL_PASSWORD").unwrap_or_default(),
            send_local_domains: std::env::var("SEND_LOCAL_DOMAINS")
                .unwrap_or_else(|_| "example.com".to_string())
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
            password_reset_service_token: resolve_env_var(
                std::env::var("IRIS_PASSWORD_RESET_SERVICE_TOKEN").ok(),
                std::env::var("IRIS_TIMELESS_SERVICE_TOKEN").ok(),
            ),
            password_reset_email_from: resolve_env_var(
                std::env::var("IRIS_PASSWORD_RESET_EMAIL_FROM").ok(),
                std::env::var("IRIS_TIMELESS_EMAIL_FROM").ok(),
            ),
            password_reset_email_to: resolve_env_var(
                std::env::var("IRIS_PASSWORD_RESET_EMAIL_TO").ok(),
                std::env::var("IRIS_TIMELESS_EMAIL_TO").ok(),
            ),
            password_reset_public_origin: resolve_env_var(
                std::env::var("IRIS_PASSWORD_RESET_PUBLIC_ORIGIN").ok(),
                std::env::var("TIMELESS_PUBLIC_ORIGIN").ok(),
            ),
            resend_api_url,
            mail_content_origin: normalize_mail_content_origin(
                std::env::var("MAIL_CONTENT_ORIGIN").ok().as_deref(),
            ),
            admin_emails: parse_admin_emails(
                &std::env::var("IRIS_ADMIN_EMAILS").unwrap_or_default(),
            ),
        })
    }
}

/// Parse `IRIS_ADMIN_EMAILS` (comma-separated, lowercased, trimmed).
/// Pure so it can be unit-tested without touching process env.
pub fn parse_admin_emails(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(|e| e.trim().to_lowercase())
        .filter(|e| !e.is_empty())
        .collect()
}

/// True when `email` is on the admin allow-list. The comparison is
/// case-insensitive (both sides lowercased); an empty list never
/// matches, so an unset `IRIS_ADMIN_EMAILS` fails closed.
pub fn is_admin_email(admin_emails: &[String], email: &str) -> bool {
    let email = email.trim().to_lowercase();
    !email.is_empty() && admin_emails.iter().any(|a| a == &email)
}

/// Resolve an env value from its current name, falling back to a deprecated
/// name when the current one is unset or empty. The deprecated name keeps
/// existing deployments working; new deployments should use the current
/// name. Pure (takes already-read values) so it can be unit-tested without
/// touching process env.
fn resolve_env_var(primary: Option<String>, fallback: Option<String>) -> String {
    match primary {
        Some(value) if !value.is_empty() => value,
        _ => fallback.unwrap_or_default(),
    }
}

/// Normalise the optional separate mail-content origin. Empty / unset means
/// the feature is off. Anything else must be a bare `https://host[:port]`
/// origin — no path, query, fragment or userinfo — so it can be dropped
/// into an iframe `src` without smuggling a path or credentials.
/// `http://` is accepted only for loopback, so a local dev server can
/// stand in for the content origin without a certificate. An invalid
/// value fails closed to empty (disabled), never to a guess.
pub fn normalize_mail_content_origin(raw: Option<&str>) -> String {
    let raw = match raw.map(str::trim) {
        Some(raw) if !raw.is_empty() => raw,
        _ => return String::new(),
    };
    let Some((scheme, rest)) = raw.split_once("://") else {
        return String::new();
    };
    if scheme != "https" && scheme != "http" {
        return String::new();
    }
    // No userinfo, path, query or fragment: the rest must be host[:port].
    if rest.is_empty() || rest.contains(['/', '?', '#', '@', '\\']) {
        return String::new();
    }
    let (host, port) = match rest.rsplit_once(':') {
        Some((host, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => {
            (host, Some(port))
        }
        _ => (rest, None),
    };
    let host = host.to_lowercase();
    if host.is_empty() || host.contains([':', ' ']) {
        return String::new();
    }
    // Plain http is only ever a loopback dev stand-in.
    if scheme == "http" && !matches!(host.as_str(), "localhost" | "127.0.0.1") {
        return String::new();
    }
    match port {
        Some(port) => format!("{scheme}://{host}:{port}"),
        None => format!("{scheme}://{host}"),
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

/// Parse `TURNSTILE_HOSTNAMES` (comma-separated, lowercased). In
/// production a loopback hostname is rejected: a token minted on
/// `localhost` must never pass the hostname check on a live deploy —
/// a deploy that copied a dev allowlist would otherwise accept
/// tokens from a local widget. Outside production loopback hostnames
/// are allowed, so localhost Turnstile testing works. Fails at
/// startup (`from_env` returns `Err`, and `main` panics on it),
/// never silently. `env` uses the same `IRIS_ENV` value as
/// `turnstile_siteverify_endpoint` / `resend_api_endpoint`
/// (`"production"` = production).
fn parse_turnstile_hostnames(env: &str, raw: &str) -> anyhow::Result<Vec<String>> {
    let hostnames: Vec<String> = raw
        .split(',')
        .map(|h| h.trim().to_lowercase())
        .filter(|h| !h.is_empty())
        .collect();
    if env.eq_ignore_ascii_case("production") {
        for host in &hostnames {
            if is_loopback_hostname(host) {
                return Err(anyhow::anyhow!(
                    "TURNSTILE_HOSTNAMES contains loopback hostname '{host}'; remove it — a Turnstile token minted on localhost must never be accepted in production"
                ));
            }
        }
    }
    Ok(hostnames)
}

/// True for hostnames that point at this machine: `localhost` (and
/// `*.localhost`), the whole `127.0.0.0/8` range, and IPv6 loopback
/// `::1` in any textual form (including `[::1]` and the expanded
/// `0:0:0:0:0:0:0:1`). `IpAddr::is_loopback` covers the IP forms;
/// the name forms need a string check.
fn is_loopback_hostname(host: &str) -> bool {
    let host = host.trim_end_matches('.');
    if host == "localhost" || host.ends_with(".localhost") {
        return true;
    }
    // Bracketed IPv6 literal: "[::1]" does not parse as IpAddr.
    let bare = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    bare.parse::<std::net::IpAddr>()
        .is_ok_and(|ip| ip.is_loopback())
}

/// Siteverify endpoint for Turnstile token checks. Mirrors
/// `resend_api_endpoint`: production is pinned to Cloudflare's
/// official endpoint, so a misconfigured deploy can never POST the
/// secret key to an arbitrary host (and a mock answering
/// `success:true` can never disable the gate). Outside production an
/// override is honoured only with an `https://` scheme — an `http://`
/// override, a bare host, or no override all fall back to the
/// official endpoint.
fn turnstile_siteverify_endpoint(env: &str, override_url: Option<&str>) -> String {
    if env.eq_ignore_ascii_case("production") {
        return TURNSTILE_SITEVERIFY_URL.to_string();
    }
    match override_url
        .map(str::trim)
        .filter(|url| url.starts_with("https://") && url.len() > "https://".len())
    {
        Some(url) => url.to_string(),
        None => TURNSTILE_SITEVERIFY_URL.to_string(),
    }
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

    #[test]
    fn turnstile_hostnames_reject_loopback() {
        // Production rejects every loopback form.
        for bad in [
            "localhost",
            "LOCALHOST",
            "localhost.",
            "foo.localhost",
            "127.0.0.1",
            "127.0.0.2",
            "127.9.9.9",
            "::1",
            "[::1]",
            "0:0:0:0:0:0:0:1",
        ] {
            assert!(
                parse_turnstile_hostnames("production", bad).is_err(),
                "{bad} must be rejected in production"
            );
        }
        // One loopback entry poisons the whole list in production.
        assert!(parse_turnstile_hostnames("production", "mail.example.com, localhost").is_err());
        // Outside production loopback hostnames are allowed, so
        // localhost Turnstile testing works.
        for env in ["development", "test", "staging"] {
            for host in ["localhost", "127.0.0.1", "::1", "[::1]"] {
                assert_eq!(
                    parse_turnstile_hostnames(env, host).unwrap(),
                    vec![host.to_string()],
                    "{host} must be allowed in {env}"
                );
            }
        }
        // Ordinary hostnames still parse (lowercased, trimmed).
        assert_eq!(
            parse_turnstile_hostnames("production", "mail.example.com, APP.Example.com ").unwrap(),
            vec!["mail.example.com", "app.example.com"]
        );
        // Empty stays empty (fails closed later, at verify time).
        assert!(parse_turnstile_hostnames("production", "")
            .unwrap()
            .is_empty());
        assert!(parse_turnstile_hostnames("production", " , ")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn production_pins_siteverify_to_cloudflare() {
        for override_url in [
            Some("https://mock.invalid/siteverify"),
            Some("http://127.0.0.1:8765"),
            Some("http://mock.invalid/siteverify"),
            None,
        ] {
            assert_eq!(
                turnstile_siteverify_endpoint("production", override_url),
                "https://challenges.cloudflare.com/turnstile/v0/siteverify",
                "production must ignore {override_url:?}"
            );
        }
    }

    #[test]
    fn development_allows_only_https_siteverify_override() {
        assert_eq!(
            turnstile_siteverify_endpoint("development", Some("https://mock.invalid/siteverify")),
            "https://mock.invalid/siteverify"
        );
        // http (even loopback), bare hosts and empty all fall back.
        for bad in [
            "http://127.0.0.1:8765",
            "http://mock.invalid/siteverify",
            "mock.invalid",
            "https://",
            "",
        ] {
            assert_eq!(
                turnstile_siteverify_endpoint("development", Some(bad)),
                "https://challenges.cloudflare.com/turnstile/v0/siteverify",
                "{bad} must fall back to the official endpoint"
            );
        }
        assert_eq!(
            turnstile_siteverify_endpoint("development", None),
            "https://challenges.cloudflare.com/turnstile/v0/siteverify"
        );
    }

    #[test]
    fn mail_content_origin_empty_means_disabled() {
        assert_eq!(normalize_mail_content_origin(None), "");
        assert_eq!(normalize_mail_content_origin(Some("")), "");
        assert_eq!(normalize_mail_content_origin(Some("   ")), "");
    }

    #[test]
    fn mail_content_origin_keeps_bare_https_origin() {
        assert_eq!(
            normalize_mail_content_origin(Some("https://mail-content.example.com")),
            "https://mail-content.example.com"
        );
        assert_eq!(
            normalize_mail_content_origin(Some("https://MAIL-Content.Example.COM")),
            "https://mail-content.example.com"
        );
    }

    #[test]
    fn mail_content_origin_rejects_paths_and_userinfo() {
        for bad in [
            "https://mail-content.example.com/",
            "https://mail-content.example.com/api",
            "https://mail-content.example.com?x=1",
            "https://user:pass@mail-content.example.com",
            "https://mail-content.example.com#frag",
            "mail-content.example.com",
            "ftp://mail-content.example.com",
        ] {
            assert_eq!(
                normalize_mail_content_origin(Some(bad)),
                "",
                "{bad} must fail closed"
            );
        }
    }

    #[test]
    fn mail_content_origin_http_only_for_loopback() {
        assert_eq!(
            normalize_mail_content_origin(Some("http://127.0.0.1:8000")),
            "http://127.0.0.1:8000"
        );
        assert_eq!(
            normalize_mail_content_origin(Some("http://localhost:5173")),
            "http://localhost:5173"
        );
        assert_eq!(
            normalize_mail_content_origin(Some("http://mail-content.example.com")),
            ""
        );
    }

    #[test]
    fn admin_emails_parse_lowercase_trim_and_drop_empty() {
        assert_eq!(
            parse_admin_emails("Admin@Example.com , ops@example.com,, "),
            vec!["admin@example.com", "ops@example.com"]
        );
        assert!(parse_admin_emails("").is_empty());
        assert!(parse_admin_emails(" , ").is_empty());
    }

    #[test]
    fn admin_check_is_case_insensitive_and_fails_closed() {
        let list = parse_admin_emails("admin@example.com");
        assert!(is_admin_email(&list, "admin@example.com"));
        assert!(is_admin_email(&list, "Admin@Example.COM"));
        assert!(is_admin_email(&list, "  admin@example.com  "));
        assert!(!is_admin_email(&list, "other@example.com"));
        assert!(!is_admin_email(&list, ""));
        // An empty allow-list never matches anyone.
        assert!(!is_admin_email(&[], "admin@example.com"));
    }

    #[test]
    fn resolve_env_var_prefers_primary_over_deprecated() {
        assert_eq!(
            resolve_env_var(Some("new".into()), Some("old".into())),
            "new"
        );
        // An empty primary is treated as unset, so the fallback still applies.
        assert_eq!(
            resolve_env_var(Some(String::new()), Some("old".into())),
            "old"
        );
        assert_eq!(resolve_env_var(None, Some("old".into())), "old");
        assert_eq!(resolve_env_var(None, None), "");
        assert_eq!(resolve_env_var(Some("new".into()), None), "new");
    }
}
