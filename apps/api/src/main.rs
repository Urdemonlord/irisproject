use axum::{
    extract::{DefaultBodyLimit, Path, Query, Request, State},
    http::{header, HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{
        sse::{KeepAlive, Sse},
        IntoResponse, Response,
    },
    routing::{get, post},
    Extension, Json, Router,
};
use iris_auth as auth;
use iris_config::limits::{
    DEFAULT_LIST_LIMIT, MAIL_WATCH_KEEPALIVE_SECS, MAX_ATTACHMENT_BYTES, MAX_COMPOSE_JSON_BYTES,
    MAX_FAILED_LOGINS, MAX_FAILED_LOGINS_PER_IP, MAX_HTML_BODY_BYTES, MAX_LIST_LIMIT,
    MAX_PASSWORD_RESET_BODY_BYTES, MAX_PREVIEW_BYTES, MAX_RAW_BYTES, MAX_READS_PER_WINDOW,
    MAX_SEARCH_QUERY_CHARS, MAX_TRANSACTIONAL_EMAIL_BODY_BYTES, MAX_TURNSTILE_ATTEMPTS_PER_IP,
    READ_WINDOW_MINUTES,
};
use iris_config::Config;
use iris_mail::{
    extract_text_attachment, valid_label, Attachment, Compose, JmapClient, ListPage, MailError,
    MessageDetail, ResendClient, View,
};
use serde::Deserialize;
use serde_json::json;
use sqlx::SqlitePool;
use std::{sync::Arc, time::Duration};

mod admin;
mod cleanup;
mod compose;
mod contacts;
mod dmarc;
mod email_address;
mod filters;
mod html_body;
mod integration_apps;
mod integration_keys;
mod notify;
mod organize;
mod temp;
mod transactional_email;
mod turnstile;

pub(crate) struct AppState {
    pool: SqlitePool,
    config: Config,
    jmap: JmapClient,
    /// Serializes filter mutations and label/folder deletes. Each of those
    /// handlers is a check-then-act spanning SQLite and JMAP (insert a rule,
    /// then rebuild and install the Sieve script; check no filter uses a
    /// label, then delete it), so no SQLite transaction can make it atomic:
    /// the Sieve install and the JMAP delete live outside the database.
    /// One process serves this API, so a process-local mutex is enough.
    /// Hold it across the whole mutation, including the JMAP call.
    organize_lock: tokio::sync::Mutex<()>,
    /// Shared outbound HTTP client (Turnstile Siteverify). One client for
    /// the process, like `JmapClient`'s own; a fresh client per login
    /// would throw away connection reuse on every attempt.
    http: reqwest::Client,
    /// Shared mailbox watchers for the live event stream: one watcher
    /// task per subscribed mailbox, started on first subscribe and
    /// stopped when the last tab disconnects (see `notify`).
    mail_watch: notify::MailWatch,
    /// Outbound relay for other domains (None = only local domains can be reached).
    relay: Option<ResendClient>,
    /// One-time HTML bodies minted for the separate mail-content origin.
    /// Process-local; entries expire after `HTML_BODY_TOKEN_TTL_SECS`.
    html_bodies: html_body::HtmlBodyStore,
}

#[derive(Clone)]
pub(crate) struct Creds {
    pub(crate) user: String,
    pub(crate) pass: String,
}

#[derive(Clone)]
pub(crate) struct CurrentUser {
    pub(crate) user_id: String,
    pub(crate) email: String,
    pub(crate) creds: Creds,
}

pub(crate) type S = State<Arc<AppState>>;
pub(crate) type ApiResult = Result<Response, ApiError>;

#[derive(Debug)]
pub(crate) struct ApiError(pub(crate) StatusCode, pub(crate) &'static str);
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}
impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        tracing::error!("db: {e}");
        match e {
            // A query that found no row is a client-visible "not found",
            // not a bug in our own code.
            sqlx::Error::RowNotFound => ApiError(StatusCode::NOT_FOUND, "not_found"),
            // The pool is exhausted, closed, or its worker died: a
            // transient outage, retryable, and operationally distinct
            // from an internal error.
            sqlx::Error::PoolTimedOut | sqlx::Error::PoolClosed | sqlx::Error::WorkerCrashed => {
                ApiError(StatusCode::SERVICE_UNAVAILABLE, "database_unavailable")
            }
            _ => ApiError(StatusCode::INTERNAL_SERVER_ERROR, "internal_error"),
        }
    }
}
impl From<MailError> for ApiError {
    fn from(e: MailError) -> Self {
        // Only the generic code reaches the client; the detail in `{e}`
        // (upstream response bodies, transport errors) stays in the
        // server log, never in the response.
        tracing::warn!("mail: {e}");
        match e {
            MailError::Auth => ApiError(StatusCode::UNAUTHORIZED, "unauthorized"),
            MailError::NotFound => ApiError(StatusCode::NOT_FOUND, "not_found"),
            MailError::Rejected(_) => ApiError(StatusCode::UNPROCESSABLE_ENTITY, "send_rejected"),
            MailError::NotEmpty => ApiError(StatusCode::CONFLICT, "folder_not_empty"),
            // The mail server could not be reached at all.
            MailError::Unavailable(_) => {
                ApiError(StatusCode::BAD_GATEWAY, "mail_server_unavailable")
            }
            // The mail server answered, but with something unexpected
            // (bad JMAP response, undecodable blob): reachable yet
            // misbehaving, a different cause from a transport failure.
            MailError::Protocol(_) => ApiError(StatusCode::BAD_GATEWAY, "mail_server_error"),
        }
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    let config = Config::from_env().expect("config");
    if config.turnstile_secret_key.is_empty() {
        // Logged once at startup, not per login: an unconfigured
        // deployment skips Turnstile verification entirely (see
        // `turnstile::verify`), so this is the one place the operator
        // learns the login form is unprotected.
        tracing::warn!(
            "turnstile: TURNSTILE_SECRET_KEY is not set; login Turnstile verification is disabled"
        );
    } else if config.turnstile_hostnames.is_empty() {
        // Fail closed (see `turnstile::verify`): a secret without a
        // hostname allowlist rejects every login, so warn loudly.
        tracing::warn!(
            "turnstile: TURNSTILE_SECRET_KEY is set but TURNSTILE_HOSTNAMES is empty; login Turnstile verification will reject every login"
        );
    }
    if config.env == "production"
        && (config.session_secret.len() < 32 || config.session_secret.starts_with("dev-secret"))
    {
        panic!("SESSION_SECRET must be a random value of at least 32 chars in production");
    }
    let pool = iris_database::init_pool(&config.database_url)
        .await
        .expect("database");
    let mut args = std::env::args().skip(1);
    let first_arg = args.next();
    if first_arg.as_deref() == Some("integration-key") {
        let command: Vec<String> = args.collect();
        match integration_keys::run(&pool, &command).await {
            Ok(output) => print!("{output}"),
            Err(code) => {
                eprintln!("integration-key error: {code}");
                std::process::exit(2);
            }
        }
        return;
    }
    let state = Arc::new(AppState {
        pool,
        organize_lock: tokio::sync::Mutex::new(()),
        html_bodies: html_body::HtmlBodyStore::new(),
        jmap: JmapClient::new(config.stalwart_api_url.clone()),
        http: reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .expect("reqwest client"),
        mail_watch: notify::MailWatch::from_limits(),
        relay: (!config.resend_api_key.is_empty()).then(|| {
            ResendClient::with_endpoint(
                config.resend_api_key.clone(),
                config.resend_api_url.clone(),
            )
        }),
        config: config.clone(),
    });
    if first_arg.as_deref() == Some("cleanup-once") {
        // Out-of-process safety net (iris-cleanup.timer): one retention pass, then exit.
        // No HTTP server is started and the in-process loop is not spawned.
        match cleanup::run_once(&state).await {
            Ok(()) => return,
            Err(e) => {
                eprintln!("cleanup-once error: {e}");
                std::process::exit(1);
            }
        }
    }
    tokio::spawn(cleanup::run(state.clone()));

    let protected = Router::new()
        .route("/api/v1/session", get(session))
        .route("/api/v1/admin/summary", get(admin::summary))
        .route("/auth/logout", post(logout))
        .route("/api/v1/mailboxes", get(mailboxes))
        .route("/api/v1/mailboxes/:mailbox_id/messages", get(list_messages))
        .route("/api/v1/mailboxes/:mailbox_id/events", get(mailbox_events))
        .route(
            "/api/v1/mailboxes/:mailbox_id/threads/:thread_id",
            get(get_thread),
        )
        .route(
            "/api/v1/mailboxes/:mailbox_id/messages/:message_id",
            get(get_message).delete(compose::delete_message),
        )
        .route(
            "/api/v1/mailboxes/:mailbox_id/messages/:message_id/raw",
            get(get_message_raw),
        )
        .route(
            "/api/v1/mailboxes/:mailbox_id/messages/:message_id/html-body-token",
            post(mint_html_body_token),
        )
        .route(
            "/api/v1/mailboxes/:mailbox_id/messages/:message_id/attachments/:attachment_id",
            get(get_attachment),
        )
        .route(
            "/api/v1/mailboxes/:mailbox_id/messages/:message_id/attachments/:attachment_id/preview",
            get(get_attachment_preview),
        )
        .route(
            "/api/v1/mailboxes/:mailbox_id/folders",
            get(compose::folders).post(organize::create_folder),
        )
        .route(
            "/api/v1/mailboxes/:mailbox_id/folders/:folder_id",
            axum::routing::patch(organize::rename_folder).delete(organize::delete_folder),
        )
        .route(
            "/api/v1/mailboxes/:mailbox_id/labels",
            get(organize::list_labels).post(organize::create_label),
        )
        .route(
            "/api/v1/mailboxes/:mailbox_id/labels/:name",
            axum::routing::delete(organize::delete_label),
        )
        .route(
            "/api/v1/mailboxes/:mailbox_id/messages/:message_id/star",
            post(organize::set_star),
        )
        .route(
            "/api/v1/mailboxes/:mailbox_id/messages/:message_id/labels",
            post(organize::set_message_label),
        )
        .route(
            "/api/v1/mailboxes/:mailbox_id/filters",
            get(filters::list).post(filters::create),
        )
        .route(
            "/api/v1/contacts",
            get(contacts::list).post(contacts::create),
        )
        .route(
            "/api/v1/contacts/:contact_id",
            axum::routing::patch(contacts::update).delete(contacts::delete),
        )
        .route(
            "/api/v1/mailboxes/:mailbox_id/filters/:filter_id",
            axum::routing::delete(filters::delete),
        )
        .route(
            "/api/v1/mailboxes/:mailbox_id/messages/:message_id/move",
            post(compose::move_message),
        )
        .route(
            "/api/v1/mailboxes/:mailbox_id/dmarc/scan",
            post(dmarc::scan),
        )
        .route(
            "/api/v1/mailboxes/:mailbox_id/dmarc/summary",
            get(dmarc::summary),
        )
        .route(
            "/api/v1/mailboxes/:mailbox_id/dmarc/domains/:domain",
            get(dmarc::domain_detail),
        )
        .route(
            "/api/v1/mailboxes/:mailbox_id/messages/:message_id/seen",
            post(compose::set_seen),
        )
        .route(
            "/api/v1/mailboxes/:mailbox_id/drafts",
            // Base64 attachments inflate the JSON body past Axum's 2 MB
            // default; the cap lives in limits so a retune stays deliberate.
            post(compose::save_draft).layer(DefaultBodyLimit::max(MAX_COMPOSE_JSON_BYTES)),
        )
        .route(
            "/api/v1/mailboxes/:mailbox_id/send",
            post(compose::send).layer(DefaultBodyLimit::max(MAX_COMPOSE_JSON_BYTES)),
        )
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_session,
        ));

    let app = Router::new()
        .route("/health", get(|| async { "OK" }))
        .route("/ready", get(ready))
        .route("/auth/csrf", get(csrf))
        .route("/auth/login", post(login))
        .merge(password_reset_routes())
        .route(
            "/api/v1/integrations/email",
            post(transactional_email::send)
                .layer(DefaultBodyLimit::max(MAX_TRANSACTIONAL_EMAIL_BODY_BYTES)),
        )
        .route(
            "/api/v1/integrations/email-requests/:request_id",
            get(transactional_email::status),
        )
        .route("/api/v1/temp-inboxes", post(temp::create))
        .route("/api/v1/temp-inboxes/:id", get(temp::status))
        .route("/api/v1/temp-inboxes/:id/extend", post(temp::extend))
        .route("/api/v1/temp-inboxes/:id/delete", post(temp::delete))
        .route("/api/v1/temp-inboxes/:id/messages", get(temp::messages))
        .route(
            "/api/v1/temp-inboxes/:id/messages/:message_id",
            get(temp::message),
        )
        .route(
            "/api/v1/temp-inboxes/:id/messages/:message_id/raw",
            get(temp::message_raw),
        )
        .route(
            "/api/v1/mailboxes/:mailbox_id/messages/:message_id/html-body",
            get(get_html_body),
        )
        .route(
            "/api/v1/temp-inboxes/:id/messages/:message_id/attachments/:attachment_id",
            get(temp::message_attachment),
        )
        .route(
            "/api/v1/temp-inboxes/:id/messages/:message_id/attachments/:attachment_id/preview",
            get(temp::message_attachment_preview),
        )
        .merge(protected)
        .layer(middleware::from_fn_with_state(state.clone(), require_csrf))
        .with_state(state);

    let addr = format!("{}:{}", config.host, config.port);
    let listener = tokio::net::TcpListener::bind(&addr).await.expect("bind");
    tracing::info!("IRIS API listening on http://{addr}");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await
    .expect("server");
}

/// Both paths serve the same handler: the generic webhook path, plus the
/// deprecated companion-app path kept for the live caller (it allow-lists
/// that exact path — see README, "Password-reset webhook"). Remove the
/// deprecated route only after the caller has switched to the generic one.
fn password_reset_routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/internal/v1/password-reset-webhook",
            post(password_reset_webhook)
                .layer(DefaultBodyLimit::max(MAX_PASSWORD_RESET_BODY_BYTES)),
        )
        .route(
            "/internal/v1/timeless-password-reset",
            post(password_reset_webhook)
                .layer(DefaultBodyLimit::max(MAX_PASSWORD_RESET_BODY_BYTES)),
        )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PasswordResetWebhookBody {
    token: String,
}

fn valid_reset_token(token: &str) -> bool {
    token.len() == 43
        && token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn valid_dns_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 253 || !name.is_ascii() {
        return false;
    }
    let labels: Vec<_> = name.split('.').collect();
    labels.len() >= 2
        && labels.iter().all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label.as_bytes()[0].is_ascii_alphanumeric()
                && label.as_bytes()[label.len() - 1].is_ascii_alphanumeric()
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

fn reset_email_configured(s: &AppState) -> bool {
    let valid_email = |email: &str| {
        email.len() <= 254
            && email.contains('@')
            && !email.bytes().any(|b| {
                b.is_ascii_whitespace() || b == b'<' || b == b'>' || b == b'\r' || b == b'\n'
            })
    };
    s.config.password_reset_service_token.len() >= 32
        && valid_email(&s.config.password_reset_email_from)
        && valid_email(&s.config.password_reset_email_to)
        && valid_dns_name(&s.config.password_reset_public_origin)
        && s.relay.is_some()
}

async fn password_reset_webhook(
    State(s): S,
    headers: HeaderMap,
    Json(body): Json<PasswordResetWebhookBody>,
) -> ApiResult {
    let supplied = headers
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    if !supplied.is_some_and(|value| auth::ct_eq(&s.config.password_reset_service_token, value)) {
        return Err(ApiError(StatusCode::UNAUTHORIZED, "unauthorized"));
    }
    if !reset_email_configured(&s) {
        return Err(ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "service_unavailable",
        ));
    }
    if !valid_reset_token(&body.token) {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_request"));
    }

    let link = format!(
        "https://{}/admin/reset#token={}",
        s.config.password_reset_public_origin, body.token
    );
    let message = Compose {
        from: s.config.password_reset_email_from.clone(),
        to: vec![s.config.password_reset_email_to.clone()],
        subject: "Reset your editor password".to_string(),
        text: format!("A password reset was requested for the editor account.\n\nUse this one-time link within 15 minutes:\n{link}\n\nIf you did not request this, ignore this message."),
        ..Compose::default()
    };
    // `reset_email_configured` above already checks `s.relay.is_some()`;
    // answer 503 instead of panicking the request task if that ever drifts.
    let Some(relay) = s.relay.as_ref() else {
        return Err(ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "service_unavailable",
        ));
    };
    if relay.send(&message).await.is_err() {
        tracing::warn!("Password-reset webhook email relay failed; details redacted");
        return Err(ApiError(StatusCode::BAD_GATEWAY, "delivery_unavailable"));
    }
    Ok((StatusCode::ACCEPTED, Json(json!({ "accepted": true }))).into_response())
}

async fn ready(State(s): S) -> Json<serde_json::Value> {
    Json(json!({ "ready": iris_database::health_check(&s.pool).await.is_ok() }))
}

fn secure(s: &AppState) -> bool {
    s.config.env == "production"
}

/// Issues (or re-issues) the double-submit CSRF cookie; the SPA calls this before its first write.
async fn csrf(State(s): S, headers: HeaderMap) -> Response {
    let existing = headers
        .get(header::COOKIE)
        .and_then(|h| h.to_str().ok())
        .and_then(|c| auth::cookie(c, auth::CSRF_COOKIE))
        .filter(|t| t.len() == 64)
        .map(str::to_string);
    let token = existing.unwrap_or_else(auth::new_token);
    (
        [(
            header::SET_COOKIE,
            auth::set_csrf_cookie(&token, secure(&s)),
        )],
        Json(json!({ "csrf_token": token })),
    )
        .into_response()
}

/// Unsafe methods need a matching CSRF header+cookie, unless authenticated by a bearer
/// capability token (temp inboxes), which a cross-site form cannot attach.
async fn require_csrf(headers: HeaderMap, req: Request, next: Next) -> Response {
    let m = req.method();
    let unsafe_method = !(m == axum::http::Method::GET
        || m == axum::http::Method::HEAD
        || m == axum::http::Method::OPTIONS);
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .is_some_and(|h| h.starts_with("Bearer "));
    let cookies = headers.get(header::COOKIE).and_then(|h| h.to_str().ok());
    let sent = headers.get(auth::CSRF_HEADER).and_then(|h| h.to_str().ok());
    if unsafe_method && !bearer && !auth::csrf_ok(cookies, sent) {
        return ApiError(StatusCode::FORBIDDEN, "csrf_failed").into_response();
    }
    next.run(req).await
}

#[derive(Deserialize)]
struct LoginBody {
    email: String,
    password: String,
    /// Cloudflare Turnstile token from the login widget. Absent when the
    /// widget is off (no site key) or the client is not a browser; the
    /// server only requires it while `TURNSTILE_SECRET_KEY` is set.
    #[serde(default)]
    turnstile_token: Option<String>,
}

/// Peer IP; trust X-Real-IP only when the peer is the local reverse proxy.
pub(crate) fn client_ip(peer: std::net::SocketAddr, headers: &HeaderMap) -> String {
    let fwd = headers
        .get("x-real-ip")
        .and_then(|h| h.to_str().ok())
        .map(str::trim);
    match fwd {
        Some(ip) if peer.ip().is_loopback() && ip.parse::<std::net::IpAddr>().is_ok() => {
            ip.to_string()
        }
        _ => peer.ip().to_string(),
    }
}

async fn login(
    State(s): S,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    Json(b): Json<LoginBody>,
) -> ApiResult {
    let email = b.email.trim().to_lowercase();
    if email.is_empty() || !email.contains('@') || b.password.is_empty() || email.len() > 254 {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_request"));
    }
    let subject = auth::hash_token(&format!("login:{email}"));
    let ip = client_ip(peer, &headers);
    let ip_subject = auth::hash_token(&format!("login-ip:{ip}"));
    let since = (chrono::Utc::now() - chrono::Duration::minutes(15)).to_rfc3339();
    let failures: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM rate_limit_events WHERE subject_hash=? AND action='login' AND outcome='blocked' AND occurred_at>?",
    ).bind(&subject).bind(&since).fetch_one(&s.pool).await?;
    if failures >= MAX_FAILED_LOGINS {
        return Err(ApiError(StatusCode::TOO_MANY_REQUESTS, "too_many_attempts"));
    }
    let ip_failures: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM rate_limit_events WHERE subject_hash=? AND action='login' AND outcome='blocked' AND occurred_at>?",
    ).bind(&ip_subject).bind(&since).fetch_one(&s.pool).await?;
    if ip_failures >= MAX_FAILED_LOGINS_PER_IP {
        return Err(ApiError(StatusCode::TOO_MANY_REQUESTS, "too_many_attempts"));
    }

    // Bot gate, after the cheap rate-limit checks and before any JMAP
    // attempt: a failed check never touches the mail server and never
    // records a login failure. Off entirely while no secret is
    // configured (see `turnstile::verify`). The gate requires
    // success, the "login" action, and a hostname in
    // TURNSTILE_HOSTNAMES — a token minted anywhere else is rejected.
    //
    // Failed verifications get their own per-IP bucket: they never
    // reach JMAP, so the login buckets above never count them, and
    // without this cap every attempt would cost one outbound
    // Siteverify call. A Cloudflare outage (`Unavailable`) is not
    // the client's fault, so it is not recorded.
    if !s.config.turnstile_secret_key.is_empty() {
        let turnstile_ip_subject = auth::hash_token(&format!("turnstile-ip:{ip}"));
        let turnstile_failures: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM rate_limit_events WHERE subject_hash=? AND action='turnstile' AND outcome='blocked' AND occurred_at>?",
        ).bind(&turnstile_ip_subject).bind(&since).fetch_one(&s.pool).await?;
        if turnstile_failures >= MAX_TURNSTILE_ATTEMPTS_PER_IP {
            return Err(ApiError(StatusCode::TOO_MANY_REQUESTS, "too_many_attempts"));
        }
        match turnstile::verify(
            &s.http,
            &s.config.turnstile_secret_key,
            b.turnstile_token.as_deref(),
            &ip,
            turnstile::EXPECTED_ACTION,
            &s.config.turnstile_hostnames,
            &s.config.turnstile_siteverify_url,
        )
        .await
        {
            Ok(()) => {}
            Err(turnstile::TurnstileError::MissingToken)
            | Err(turnstile::TurnstileError::Invalid) => {
                sqlx::query("INSERT INTO rate_limit_events (id,subject_hash,action,occurred_at,outcome) VALUES (?,?,'turnstile',?,'blocked')")
                    .bind(uuid::Uuid::new_v4().to_string()).bind(&turnstile_ip_subject).bind(auth::now())
                    .execute(&s.pool).await?;
                return Err(ApiError(StatusCode::BAD_REQUEST, "turnstile_failed"));
            }
            Err(turnstile::TurnstileError::Unavailable) => {
                // Fail closed: a login that cannot be verified does not go
                // through, even though the credentials themselves are
                // still unchecked at this point.
                return Err(ApiError(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "turnstile_unavailable",
                ));
            }
        }
    }

    let account_id = match s.jmap.login(&email, &b.password).await {
        Ok(a) => a,
        Err(MailError::Auth) => {
            sqlx::query("INSERT INTO rate_limit_events (id,subject_hash,action,occurred_at,outcome) VALUES (?,?,'login',?,'blocked')")
                .bind(uuid::Uuid::new_v4().to_string()).bind(&subject).bind(auth::now())
                .execute(&s.pool).await?;
            sqlx::query("INSERT INTO rate_limit_events (id,subject_hash,action,occurred_at,outcome) VALUES (?,?,'login',?,'blocked')")
                .bind(uuid::Uuid::new_v4().to_string()).bind(&ip_subject).bind(auth::now())
                .execute(&s.pool).await?;
            return Err(ApiError(StatusCode::UNAUTHORIZED, "invalid_credentials"));
        }
        Err(e) => return Err(e.into()),
    };

    // Per-session app password: the user's real password is used once here and never kept.
    let session_expiry = auth::expiry_at();
    let (cred_id, app_secret) = s
        .jmap
        .create_app_password(
            &email,
            &b.password,
            "IRIS web session",
            &session_expiry.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        )
        .await?;

    let token = auth::new_token();
    if let Err(e) = persist_login(
        &s,
        &email,
        &account_id,
        &token,
        &session_expiry,
        &app_secret,
        &cred_id,
    )
    .await
    {
        // Don't leak a live app password when the session row never got written.
        let _ = s
            .jmap
            .destroy_app_password(&email, &app_secret, &cred_id)
            .await;
        return Err(e);
    }

    // Session rotation: any session this browser already held is revoked, never reused.
    if let Some(old) = headers
        .get(header::COOKIE)
        .and_then(|h| h.to_str().ok())
        .and_then(auth::token_from_cookie)
    {
        revoke_by_token(&s, old).await;
    }

    let secure = secure(&s);
    let mut resp = Json(json!({ "authenticated": true, "email": email })).into_response();
    let h = resp.headers_mut();
    // Built from hex tokens, so these parses cannot fail in practice;
    // map a failure to a 500 instead of panicking the request task (and
    // dropping an already-created session) if one ever did.
    h.append(
        header::SET_COOKIE,
        auth::set_cookie(&token, secure)
            .parse()
            .map_err(|_| ApiError(StatusCode::INTERNAL_SERVER_ERROR, "internal_error"))?,
    );
    h.append(
        header::SET_COOKIE,
        auth::set_csrf_cookie(&auth::new_token(), secure)
            .parse()
            .map_err(|_| ApiError(StatusCode::INTERNAL_SERVER_ERROR, "internal_error"))?,
    );
    Ok(resp)
}

async fn persist_login(
    s: &AppState,
    email: &str,
    account_id: &str,
    token: &str,
    session_expiry: &chrono::DateTime<chrono::Utc>,
    app_secret: &str,
    cred_id: &str,
) -> Result<(), ApiError> {
    let now = auth::now();
    let mut tx = s.pool.begin().await?;
    sqlx::query("INSERT INTO app_users (id,email,display_name,created_at,updated_at) VALUES (?,?,?,?,?) ON CONFLICT(email) DO UPDATE SET updated_at=excluded.updated_at")
        .bind(uuid::Uuid::new_v4().to_string()).bind(email).bind(email).bind(&now).bind(&now)
        .execute(&mut *tx).await?;
    let (user_id, status): (String, String) =
        sqlx::query_as("SELECT id,status FROM app_users WHERE email=?")
            .bind(email)
            .fetch_one(&mut *tx)
            .await?;
    if status != "active" {
        return Err(ApiError(StatusCode::FORBIDDEN, "account_disabled"));
    }
    sqlx::query("INSERT INTO permanent_mailboxes (id,user_id,email_address,mail_server_account_id,created_at,updated_at) VALUES (?,?,?,?,?,?) ON CONFLICT(email_address) DO UPDATE SET mail_server_account_id=excluded.mail_server_account_id, updated_at=excluded.updated_at WHERE user_id=excluded.user_id")
        .bind(uuid::Uuid::new_v4().to_string()).bind(&user_id).bind(email).bind(account_id).bind(&now).bind(&now)
        .execute(&mut *tx).await?;
    let session_id = uuid::Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO auth_sessions (id,user_id,token_hash,expires_at,created_at,mail_credential_sealed,mail_credential_id) VALUES (?,?,?,?,?,?,?)")
        .bind(&session_id).bind(&user_id).bind(auth::hash_token(token)).bind(session_expiry.to_rfc3339()).bind(&now)
        .bind(auth::seal(&s.config.session_secret, token, app_secret)).bind(cred_id)
        .execute(&mut *tx).await?;
    tx.commit().await?;

    Ok(())
}

/// Revokes the session behind `token` and its mail app password (best effort).
async fn revoke_by_token(s: &AppState, token: &str) {
    let row: Option<(String, String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT s.id, u.email, s.mail_credential_sealed, s.mail_credential_id FROM auth_sessions s JOIN app_users u ON u.id=s.user_id WHERE s.token_hash=? AND s.revoked_at IS NULL",
    ).bind(auth::hash_token(token)).fetch_optional(&s.pool).await.unwrap_or(None);
    let Some((id, email, sealed, cred_id)) = row else {
        return;
    };
    let _ = sqlx::query(
        "UPDATE auth_sessions SET revoked_at=?, mail_credential_sealed=NULL WHERE id=?",
    )
    .bind(auth::now())
    .bind(&id)
    .execute(&s.pool)
    .await;
    if let (Some(pass), Some(cred_id)) = (
        sealed.and_then(|x| auth::open(&s.config.session_secret, token, &x)),
        cred_id,
    ) {
        if let Err(e) = s.jmap.destroy_app_password(&email, &pass, &cred_id).await {
            tracing::warn!("app password revoke failed (expires on its own): {e}");
        }
    }
}

async fn require_session(
    State(s): S,
    headers: HeaderMap,
    mut req: Request,
    next: Next,
) -> Response {
    let unauthorized = || {
        (
            [(header::SET_COOKIE, auth::clear_cookie())],
            ApiError(StatusCode::UNAUTHORIZED, "unauthorized"),
        )
            .into_response()
    };
    let Some(token) = headers
        .get(header::COOKIE)
        .and_then(|h| h.to_str().ok())
        .and_then(auth::token_from_cookie)
    else {
        return unauthorized();
    };
    let row: Option<(String, String, String, Option<String>)> = match sqlx::query_as(
        "SELECT s.id, s.user_id, u.email, s.mail_credential_sealed FROM auth_sessions s JOIN app_users u ON u.id=s.user_id
         WHERE s.token_hash=? AND s.revoked_at IS NULL AND s.expires_at>? AND u.status='active'",
    ).bind(auth::hash_token(token)).bind(auth::now()).fetch_optional(&s.pool).await {
        Ok(r) => r,
        Err(e) => return ApiError::from(e).into_response(),
    };
    let Some((session_id, user_id, email, sealed)) = row else {
        return unauthorized();
    };
    let Some(pass) = sealed.and_then(|x| auth::open(&s.config.session_secret, token, &x)) else {
        return unauthorized();
    };
    let creds = Creds {
        user: email.clone(),
        pass,
    };
    let _ = sqlx::query("UPDATE auth_sessions SET last_seen_at=? WHERE id=?")
        .bind(auth::now())
        .bind(&session_id)
        .execute(&s.pool)
        .await;
    req.extensions_mut().insert(CurrentUser {
        user_id,
        email,
        creds,
    });
    next.run(req).await
}

async fn session(State(s): S, Extension(u): Extension<CurrentUser>) -> Json<serde_json::Value> {
    // `is_admin` only decides whether the web shows the Admin nav link;
    // the server-side gate is `admin::require_admin` on the admin routes.
    Json(json!({
        "authenticated": true,
        "email": u.email,
        "is_admin": iris_config::is_admin_email(&s.config.admin_emails, &u.email),
    }))
}

async fn logout(State(s): S, headers: HeaderMap) -> ApiResult {
    if let Some(token) = headers
        .get(header::COOKIE)
        .and_then(|h| h.to_str().ok())
        .and_then(auth::token_from_cookie)
    {
        revoke_by_token(&s, token).await;
    }
    Ok((
        [(header::SET_COOKIE, auth::clear_cookie())],
        StatusCode::NO_CONTENT,
    )
        .into_response())
}

async fn mailboxes(State(s): S, Extension(u): Extension<CurrentUser>) -> ApiResult {
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT id,email_address,status FROM permanent_mailboxes WHERE user_id=? ORDER BY created_at",
    ).bind(&u.user_id).fetch_all(&s.pool).await?;
    let list: Vec<_> = rows
        .into_iter()
        .map(|(id, email, status)| json!({"id": id, "email_address": email, "status": status}))
        .collect();
    Ok(Json(json!({ "mailboxes": list })).into_response())
}

/// Ownership check: returns the mail-server account id only if the mailbox belongs to the user.
pub(crate) async fn owned_account(
    s: &AppState,
    u: &CurrentUser,
    mailbox_id: &str,
) -> Result<String, ApiError> {
    sqlx::query_scalar("SELECT mail_server_account_id FROM permanent_mailboxes WHERE id=? AND user_id=? AND status='active'")
        .bind(mailbox_id).bind(&u.user_id).fetch_optional(&s.pool).await?
        .ok_or(ApiError(StatusCode::NOT_FOUND, "not_found"))
}

/// Fixed-window limiter for authenticated message reads, reusing the same
/// `rate_limit_events` mechanism as login (`login`), send (`compose::send`)
/// and temp-inbox creation (`temp::create`): count this user's `allowed`
/// `read` rows inside the window, refuse with 429 once the cap is hit,
/// otherwise log the read before doing the JMAP work.
///
/// The row is written *before* the JMAP call on purpose: concurrent
/// requests must see each other, and a failed mail-server call still
/// consumed a slot (same pay-for-the-attempt shape as login's `blocked`
/// rows). A refused read logs one `blocked` row for forensics — best
/// effort only, so a bookkeeping failure never turns a refusal into a 500;
/// only `allowed` rows count toward the cap. Rows older than 2 days are
/// pruned by `cleanup::mark`, so read volume cannot grow the table
/// unboundedly. Callers run this after the cheap local checks
/// (`owned_account`, id validation) so only reads about to hit JMAP pay.
pub(crate) async fn check_read_limit(s: &AppState, u: &CurrentUser) -> Result<(), ApiError> {
    let subject = auth::hash_token(&format!("read:{}", u.user_id));
    let since = (chrono::Utc::now() - chrono::Duration::minutes(READ_WINDOW_MINUTES)).to_rfc3339();
    let reads: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM rate_limit_events WHERE subject_hash=? AND action='read' AND outcome='allowed' AND occurred_at>?",
    )
    .bind(&subject)
    .bind(&since)
    .fetch_one(&s.pool)
    .await?;
    if reads >= MAX_READS_PER_WINDOW {
        if let Err(e) = sqlx::query("INSERT INTO rate_limit_events (id,subject_hash,action,occurred_at,outcome) VALUES (?,?,'read',?,'blocked')")
            .bind(uuid::Uuid::new_v4().to_string())
            .bind(&subject)
            .bind(auth::now())
            .execute(&s.pool)
            .await
        {
            tracing::error!("rate-limit event insert failed after read refusal: {e}");
        }
        return Err(ApiError(StatusCode::TOO_MANY_REQUESTS, "too_many_reads"));
    }
    sqlx::query("INSERT INTO rate_limit_events (id,subject_hash,action,occurred_at,outcome) VALUES (?,?,'read',?,'allowed')")
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(&subject)
        .bind(auth::now())
        .execute(&s.pool)
        .await?;
    Ok(())
}

#[derive(Deserialize)]
struct Page {
    limit: Option<u32>,
    position: Option<u32>,
    /// Standard folder role (default `inbox`).
    folder: Option<String>,
    /// Custom folder id. Giving both `folder` and `folder_id` is an error
    /// unless a `label` or `view` is also given: those win over the folder
    /// selectors, so the combination is only ambiguous on its own.
    folder_id: Option<String>,
    /// `starred`; wins over `folder` / `folder_id`.
    view: Option<String>,
    /// Label name; wins over everything else.
    label: Option<String>,
    q: Option<String>,
}

/// Which list a `Page` asks for. Every input is validated here.
fn page_view(p: &Page) -> Result<View<'_>, ApiError> {
    let bad = |code| ApiError(StatusCode::BAD_REQUEST, code);
    if p.folder.is_some() && p.folder_id.is_some() && p.label.is_none() && p.view.is_none() {
        return Err(bad("invalid_request"));
    }
    if let Some(l) = p.label.as_deref() {
        return if valid_label(l) {
            Ok(View::Label(l))
        } else {
            Err(bad("invalid_label"))
        };
    }
    match p.view.as_deref() {
        Some("starred") => return Ok(View::Starred),
        Some(_) => return Err(bad("invalid_view")),
        None => {}
    }
    if let Some(id) = p.folder_id.as_deref() {
        return if valid_message_id(id) {
            Ok(View::Folder(id))
        } else {
            Err(bad("invalid_folder"))
        };
    }
    Ok(View::Role(compose::folder_role(
        p.folder.as_deref().unwrap_or("inbox"),
    )?))
}

async fn list_messages(
    State(s): S,
    Extension(u): Extension<CurrentUser>,
    Path(mailbox_id): Path<String>,
    Query(p): Query<Page>,
) -> ApiResult {
    let account = owned_account(&s, &u, &mailbox_id).await?;
    check_read_limit(&s, &u).await?;
    let limit = p
        .limit
        .unwrap_or(DEFAULT_LIST_LIMIT)
        .clamp(1, MAX_LIST_LIMIT);
    let position = p.position.unwrap_or(0);
    let view = page_view(&p)?;
    // Search terms are capped so a huge `q` is never forwarded verbatim to JMAP.
    let q =
        p.q.as_deref()
            .map(str::trim)
            .filter(|q| !q.is_empty())
            .map(|q| q.chars().take(MAX_SEARCH_QUERY_CHARS).collect::<String>());
    let (messages, total) = s
        .jmap
        .list_view(
            &u.creds.user,
            &u.creds.pass,
            &account,
            view,
            ListPage {
                limit,
                position,
                query: q.as_deref(),
            },
        )
        .await?;
    Ok(Json(json!({ "messages": messages, "total": total, "position": position })).into_response())
}

/// One JMAP thread: every message in the conversation, oldest first.
/// A thread id is server-issued, same shape as a message id, so the
/// same validation applies; an unknown thread answers 404.
async fn get_thread(
    State(s): S,
    Extension(u): Extension<CurrentUser>,
    Path((mailbox_id, thread_id)): Path<(String, String)>,
) -> ApiResult {
    let account = owned_account(&s, &u, &mailbox_id).await?;
    if !valid_message_id(&thread_id) {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_request"));
    }
    check_read_limit(&s, &u).await?;
    let t = s
        .jmap
        .get_thread(&u.creds.user, &u.creds.pass, &account, &thread_id)
        .await?;
    Ok(Json(json!({ "thread": t })).into_response())
}

/// Live mailbox stream: server-sent events for new mail, so the web
/// does not have to poll the list. `EventSource` sends the session
/// cookie, and this route lives behind `require_session` like every
/// other mailbox read; a mailbox that is not yours answers 404 before
/// any watcher starts. A GET needs no CSRF token (safe method).
///
/// The handler does NOT consume a read-limit slot: the shared watcher
/// behind it polls JMAP once per `MAIL_WATCH_POLL_SECS` per mailbox
/// no matter how many tabs are open, and that bound is the stream's
/// rate limit (see `limits.rs`). The keep-alive comment keeps the
/// response open through a reverse proxy's idle timeout.
async fn mailbox_events(
    State(s): S,
    Extension(u): Extension<CurrentUser>,
    Path(mailbox_id): Path<String>,
) -> Result<Response, ApiError> {
    let account = owned_account(&s, &u, &mailbox_id).await?;
    let rx = s
        .mail_watch
        .subscribe(&s.jmap, &u.creds, &account, &u.user_id, &mailbox_id)
        .await?;
    // `x-accel-buffering: no` keeps nginx (and other buffering proxies)
    // from holding the stream until it ends, which for SSE is never.
    let mut resp = Sse::new(notify::events_stream(rx))
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(MAIL_WATCH_KEEPALIVE_SECS)))
        .into_response();
    resp.headers_mut().insert(
        "x-accel-buffering",
        axum::http::HeaderValue::from_static("no"),
    );
    Ok(resp)
}

pub(crate) fn valid_message_id(id: &str) -> bool {
    id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

async fn get_message(
    State(s): S,
    Extension(u): Extension<CurrentUser>,
    Path((mailbox_id, message_id)): Path<(String, String)>,
) -> ApiResult {
    let account = owned_account(&s, &u, &mailbox_id).await?;
    if !valid_message_id(&message_id) {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_request"));
    }
    check_read_limit(&s, &u).await?;
    let m = s
        .jmap
        .get_message(&u.creds.user, &u.creds.pass, &account, &message_id)
        .await?;
    Ok(Json(json!({ "message": m })).into_response())
}

/// JMAP part ids look like `2` or `2.1`: only these characters, so a
/// path segment can never smuggle a different blob id or a path.
pub(crate) fn valid_part_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

/// The attachment a request names must be one the opened message actually
/// carries. The blob id used for download always comes from here, never
/// from the request, so one message's attachments cannot be used to fetch
/// another message's blobs.
pub(crate) fn find_attachment<'a>(m: &'a MessageDetail, part_id: &str) -> Option<&'a Attachment> {
    m.attachments.iter().find(|a| a.part_id == part_id)
}

pub(crate) fn sanitized_filename(name: &str) -> String {
    let mut s: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    while s.starts_with('.') {
        s.remove(0);
    }
    s.truncate(128);
    if s.is_empty() {
        "attachment".to_string()
    } else {
        s
    }
}

/// View Original: the whole message blob, as plain text. Refused over
/// `MAX_RAW_BYTES` instead of buffering an arbitrarily large message.
pub(crate) async fn raw_response(
    s: &AppState,
    user: &str,
    pass: &str,
    account: &str,
    m: &MessageDetail,
) -> ApiResult {
    let blob_id = m
        .raw_blob_id
        .as_deref()
        .ok_or(ApiError(StatusCode::NOT_FOUND, "not_found"))?;
    if m.summary.size_bytes > MAX_RAW_BYTES {
        return Err(ApiError(StatusCode::PAYLOAD_TOO_LARGE, "too_large"));
    }
    let bytes = s
        .jmap
        .download_blob_bytes(user, pass, account, blob_id, "message.eml")
        .await?;
    Ok((
        [
            (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
            (header::CONTENT_DISPOSITION, "inline"),
        ],
        bytes,
    )
        .into_response())
}

/// One attachment, downloaded with the caller's own mail credentials and
/// served under the attachment's own (sanitized) name and content type.
pub(crate) async fn attachment_response(
    s: &AppState,
    user: &str,
    pass: &str,
    account: &str,
    m: &MessageDetail,
    part_id: &str,
) -> ApiResult {
    let a = find_attachment(m, part_id).ok_or(ApiError(StatusCode::NOT_FOUND, "not_found"))?;
    if a.size > MAX_ATTACHMENT_BYTES {
        return Err(ApiError(StatusCode::PAYLOAD_TOO_LARGE, "too_large"));
    }
    let bytes = s
        .jmap
        .download_blob_bytes(
            user,
            pass,
            account,
            &a.blob_id,
            &sanitized_filename(&a.name),
        )
        .await?;
    let content_type = a
        .content_type
        .parse()
        .unwrap_or_else(|_| "application/octet-stream".parse().unwrap());
    let disposition = format!("attachment; filename=\"{}\"", sanitized_filename(&a.name));
    Ok((
        [
            (header::CONTENT_TYPE, content_type),
            (header::CONTENT_DISPOSITION, disposition),
        ],
        bytes,
    )
        .into_response())
}

/// Decompressed text preview of a gzip / zip / XML attachment (DMARC
/// reports). Anything else answers 415, so the UI offers a download
/// instead of rendering binary bytes as text.
pub(crate) async fn attachment_preview_response(
    s: &AppState,
    user: &str,
    pass: &str,
    account: &str,
    m: &MessageDetail,
    part_id: &str,
) -> ApiResult {
    let a = find_attachment(m, part_id).ok_or(ApiError(StatusCode::NOT_FOUND, "not_found"))?;
    if a.size > MAX_PREVIEW_BYTES {
        return Err(ApiError(StatusCode::PAYLOAD_TOO_LARGE, "too_large"));
    }
    let bytes = s
        .jmap
        .download_blob_bytes(
            user,
            pass,
            account,
            &a.blob_id,
            &sanitized_filename(&a.name),
        )
        .await?;
    let out = match extract_text_attachment(&a.name, &a.content_type, &bytes) {
        Ok(out) => out,
        Err(MailError::Protocol(msg)) if msg == "not_previewable" => {
            return Err(ApiError(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "not_previewable",
            ));
        }
        Err(e) => return Err(ApiError::from(e)),
    };
    Ok(Json(json!({
        "attachment": {"name": a.name, "content_type": a.content_type, "size": a.size},
        "inner_name": out.inner_name, "text": out.text, "truncated": out.truncated,
    }))
    .into_response())
}

async fn get_message_raw(
    State(s): S,
    Extension(u): Extension<CurrentUser>,
    Path((mailbox_id, message_id)): Path<(String, String)>,
) -> ApiResult {
    let account = owned_account(&s, &u, &mailbox_id).await?;
    if !valid_message_id(&message_id) {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_request"));
    }
    check_read_limit(&s, &u).await?;
    let m = s
        .jmap
        .get_message(&u.creds.user, &u.creds.pass, &account, &message_id)
        .await?;
    raw_response(&s, &u.creds.user, &u.creds.pass, &account, &m).await
}

/// Body for minting a one-time HTML render URL.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HtmlBodyTokenBody {
    #[serde(default)]
    allow_remote: bool,
}

/// Query for claiming a one-time HTML render URL.
#[derive(Deserialize)]
struct HtmlBodyQuery {
    #[serde(default)]
    t: Option<String>,
}

/// Mint a one-time render URL for this message's HTML body, served from
/// the separate mail-content origin. The body is stored server-side under
/// a random token (never in the URL), so the render origin never needs
/// the session cookie — it stays host-only on the app origin. Without a
/// configured `MAIL_CONTENT_ORIGIN` this is refused with 409 and the web
/// keeps the srcdoc frame.
async fn mint_html_body_token(
    State(s): S,
    Extension(u): Extension<CurrentUser>,
    Path((mailbox_id, message_id)): Path<(String, String)>,
    Json(body): Json<HtmlBodyTokenBody>,
) -> ApiResult {
    let origin = s.config.mail_content_origin.clone();
    if origin.is_empty() {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "mail_content_origin_not_configured",
        ));
    }
    let account = owned_account(&s, &u, &mailbox_id).await?;
    if !valid_message_id(&message_id) {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_request"));
    }
    check_read_limit(&s, &u).await?;
    let m = s
        .jmap
        .get_message(&u.creds.user, &u.creds.pass, &account, &message_id)
        .await?;
    let html = m
        .html_body
        .ok_or(ApiError(StatusCode::NOT_FOUND, "not_found"))?;
    if html.len() as u64 > MAX_HTML_BODY_BYTES {
        return Err(ApiError(StatusCode::PAYLOAD_TOO_LARGE, "too_large"));
    }
    let token = s
        .html_bodies
        .mint(&mailbox_id, &message_id, body.allow_remote, html)
        .ok_or(ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "too_many_pending_bodies",
        ))?;
    Ok(Json(json!({
        "url": html_body::html_body_url(&origin, &mailbox_id, &message_id, &token)
    }))
    .into_response())
}

/// Serve a stored HTML body once, under a strict CSP. No session is
/// needed — and none is accepted: the one-time token is the whole
/// credential, it is deleted on first use (right or wrong message), and
/// it expires after `HTML_BODY_TOKEN_TTL_SECS`. Unknown or expired
/// tokens answer 404, indistinguishable from a missing body.
async fn get_html_body(
    State(s): S,
    Path((mailbox_id, message_id)): Path<(String, String)>,
    Query(q): Query<HtmlBodyQuery>,
) -> ApiResult {
    let token = q.t.as_deref().unwrap_or_default();
    if token.is_empty() || !valid_message_id(&message_id) {
        return Err(ApiError(StatusCode::NOT_FOUND, "not_found"));
    }
    let (html, allow_remote) = s
        .html_bodies
        .claim(token, &mailbox_id, &message_id)
        .ok_or(ApiError(StatusCode::NOT_FOUND, "not_found"))?;
    let csp = html_body::mail_body_csp(allow_remote);
    Ok((
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CONTENT_SECURITY_POLICY, csp.as_str()),
            (header::CACHE_CONTROL, "no-store"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        html,
    )
        .into_response())
}

async fn get_attachment(
    State(s): S,
    Extension(u): Extension<CurrentUser>,
    Path((mailbox_id, message_id, attachment_id)): Path<(String, String, String)>,
) -> ApiResult {
    let account = owned_account(&s, &u, &mailbox_id).await?;
    if !valid_message_id(&message_id) || !valid_part_id(&attachment_id) {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_request"));
    }
    check_read_limit(&s, &u).await?;
    let m = s
        .jmap
        .get_message(&u.creds.user, &u.creds.pass, &account, &message_id)
        .await?;
    attachment_response(
        &s,
        &u.creds.user,
        &u.creds.pass,
        &account,
        &m,
        &attachment_id,
    )
    .await
}

async fn get_attachment_preview(
    State(s): S,
    Extension(u): Extension<CurrentUser>,
    Path((mailbox_id, message_id, attachment_id)): Path<(String, String, String)>,
) -> ApiResult {
    let account = owned_account(&s, &u, &mailbox_id).await?;
    if !valid_message_id(&message_id) || !valid_part_id(&attachment_id) {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_request"));
    }
    check_read_limit(&s, &u).await?;
    let m = s
        .jmap
        .get_message(&u.creds.user, &u.creds.pass, &account, &message_id)
        .await?;
    attachment_preview_response(
        &s,
        &u.creds.user,
        &u.creds.pass,
        &account,
        &m,
        &attachment_id,
    )
    .await
}

#[cfg(test)]
mod password_reset_webhook_tests {
    use super::*;
    use serde_json::Value;
    use std::{
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
        time::Duration,
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        sync::oneshot,
        task::JoinHandle,
        time::timeout,
    };

    const SERVICE_KEY: &str = "test-service-key-never-use-in-production";
    const GOOD_TOKEN: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

    struct FakeRelay {
        endpoint: String,
        calls: Arc<AtomicUsize>,
        payload: oneshot::Receiver<Value>,
        task: JoinHandle<()>,
    }

    async fn fake_relay() -> FakeRelay {
        fake_relay_with_response("200 OK", r#"{"id":"ok"}"#).await
    }

    async fn fake_relay_with_response(
        status_line: &'static str,
        response_body: &'static str,
    ) -> FakeRelay {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let task_calls = calls.clone();
        let (tx, payload) = oneshot::channel();
        let task = tokio::spawn(async move {
            let Ok(Ok((mut stream, _))) = timeout(Duration::from_secs(2), listener.accept()).await
            else {
                return;
            };
            task_calls.fetch_add(1, Ordering::SeqCst);
            let mut bytes = Vec::new();
            let mut chunk = [0u8; 1024];
            let mut expected = None;
            loop {
                let Ok(n) = stream.read(&mut chunk).await else {
                    return;
                };
                if n == 0 {
                    return;
                }
                bytes.extend_from_slice(&chunk[..n]);
                if expected.is_none() {
                    if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..end]);
                        let length = headers
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().ok())
                                    .flatten()
                            })
                            .unwrap_or(0);
                        expected = Some(end + 4 + length);
                    }
                }
                if expected.is_some_and(|end| bytes.len() >= end) {
                    break;
                }
            }
            let body_start = bytes.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
            let body: Value = serde_json::from_slice(&bytes[body_start..]).unwrap();
            let _ = tx.send(body);
            let response = format!(
                "HTTP/1.1 {status_line}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{response_body}",
                response_body.len(),
            );
            let _ = stream.write_all(response.as_bytes()).await;
        });
        FakeRelay {
            endpoint: format!("http://{address}"),
            calls,
            payload,
            task,
        }
    }

    async fn state(
        service_key: &str,
        sender: &str,
        recipient: &str,
        origin: &str,
        relay: Option<ResendClient>,
    ) -> Arc<AppState> {
        let pool = iris_database::init_pool("sqlite::memory:").await.unwrap();
        Arc::new(AppState {
            pool,
            organize_lock: tokio::sync::Mutex::new(()),
            jmap: JmapClient::new("http://127.0.0.1:9".into()),
            http: reqwest::Client::new(),
            relay,
            html_bodies: html_body::HtmlBodyStore::new(),
            mail_watch: notify::MailWatch::from_limits(),
            config: Config {
                env: "test".into(),
                host: "127.0.0.1".into(),
                port: 8000,
                database_url: "sqlite::memory:".into(),
                session_secret: "test-secret".into(),
                turnstile_secret_key: String::new(),
                turnstile_hostnames: Vec::new(),
                turnstile_siteverify_url: String::new(),
                stalwart_api_url: "http://127.0.0.1:9".into(),
                temp_inbox_domain: "example.test".into(),
                temp_inbox_default_expiry_hours: 1,
                temp_inbox_mail_user: String::new(),
                temp_inbox_mail_password: String::new(),
                send_local_domains: vec![],
                send_external: false,
                resend_api_key: "test-resend-key".into(),
                integration_email_from: "no-reply@example.com".into(),
                integration_email_enabled: true,
                password_reset_service_token: service_key.into(),
                password_reset_email_from: sender.into(),
                password_reset_email_to: recipient.into(),
                password_reset_public_origin: origin.into(),
                resend_api_url: String::new(),
                mail_content_origin: String::new(),
                admin_emails: Vec::new(),
            },
        })
    }

    fn auth_headers(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            format!("Bearer {value}").parse().unwrap(),
        );
        headers
    }

    async fn assert_no_relay_call(relay: &mut FakeRelay) {
        assert!(timeout(Duration::from_millis(40), &mut relay.payload)
            .await
            .is_err());
        assert_eq!(relay.calls.load(Ordering::SeqCst), 0);
        relay.task.abort();
    }

    #[tokio::test]
    async fn rejects_missing_or_wrong_service_key() {
        let mut relay = fake_relay().await;
        let app = state(
            SERVICE_KEY,
            "no-reply@example.com",
            "recovery@example.test",
            "reset.example.com",
            Some(ResendClient::with_endpoint(
                "test-key".into(),
                relay.endpoint.clone(),
            )),
        )
        .await;
        let missing = password_reset_webhook(
            State(app.clone()),
            HeaderMap::new(),
            Json(PasswordResetWebhookBody {
                token: GOOD_TOKEN.into(),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(missing.0, StatusCode::UNAUTHORIZED);
        let wrong = password_reset_webhook(
            State(app),
            auth_headers("wrong-service-key"),
            Json(PasswordResetWebhookBody {
                token: GOOD_TOKEN.into(),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(wrong.0, StatusCode::UNAUTHORIZED);
        assert_no_relay_call(&mut relay).await;
    }

    #[tokio::test]
    async fn rejects_malformed_token_without_relay_call() {
        let mut relay = fake_relay().await;
        let app = state(
            SERVICE_KEY,
            "no-reply@example.com",
            "recovery@example.test",
            "reset.example.com",
            Some(ResendClient::with_endpoint(
                "test-key".into(),
                relay.endpoint.clone(),
            )),
        )
        .await;
        let result = password_reset_webhook(
            State(app),
            auth_headers(SERVICE_KEY),
            Json(PasswordResetWebhookBody {
                token: "not-a-reset-token".into(),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(result.0, StatusCode::BAD_REQUEST);
        assert_no_relay_call(&mut relay).await;
    }

    #[tokio::test]
    async fn rejects_missing_config_without_relay_call() {
        let mut relay = fake_relay().await;
        let app = state(
            SERVICE_KEY,
            "",
            "",
            "",
            Some(ResendClient::with_endpoint(
                "test-key".into(),
                relay.endpoint.clone(),
            )),
        )
        .await;
        let unauthenticated = password_reset_webhook(
            State(app.clone()),
            HeaderMap::new(),
            Json(PasswordResetWebhookBody {
                token: GOOD_TOKEN.into(),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(unauthenticated.0, StatusCode::UNAUTHORIZED);
        let authenticated = password_reset_webhook(
            State(app),
            auth_headers(SERVICE_KEY),
            Json(PasswordResetWebhookBody {
                token: GOOD_TOKEN.into(),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(authenticated.0, StatusCode::SERVICE_UNAVAILABLE);
        assert_no_relay_call(&mut relay).await;
    }

    #[tokio::test]
    async fn rejects_missing_recipient_secret_without_relay_call() {
        let mut relay = fake_relay().await;
        let app = state(
            SERVICE_KEY,
            "admin@example.com",
            "",
            "reset.example.com",
            Some(ResendClient::with_endpoint(
                "test-key".into(),
                relay.endpoint.clone(),
            )),
        )
        .await;
        let result = password_reset_webhook(
            State(app),
            auth_headers(SERVICE_KEY),
            Json(PasswordResetWebhookBody {
                token: GOOD_TOKEN.into(),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(result.0, StatusCode::SERVICE_UNAVAILABLE);
        assert_no_relay_call(&mut relay).await;
    }

    #[tokio::test]
    async fn builds_email_with_fixed_recipient_sender_and_fragment_link() {
        let relay = fake_relay().await;
        let app = state(
            SERVICE_KEY,
            "admin@example.com",
            "recovery@example.test",
            "reset.example.com",
            Some(ResendClient::with_endpoint(
                "test-key".into(),
                relay.endpoint.clone(),
            )),
        )
        .await;
        let response = password_reset_webhook(
            State(app),
            auth_headers(SERVICE_KEY),
            Json(PasswordResetWebhookBody {
                token: GOOD_TOKEN.into(),
            }),
        )
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let payload = timeout(Duration::from_secs(1), relay.payload)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(payload["from"], "admin@example.com");
        assert_eq!(payload["to"], json!(["recovery@example.test"]));
        assert_eq!(payload["subject"], "Reset your editor password");
        let text = payload["text"].as_str().unwrap();
        assert!(text.contains(&format!(
            "https://reset.example.com/admin/reset#token={GOOD_TOKEN}"
        )));
        assert!(!text.contains("?token="));
        relay.task.abort();
    }

    #[tokio::test]
    async fn generic_and_deprecated_routes_both_serve() {
        use axum::{body::Body, http::Request};
        use tower::ServiceExt;
        // The fake relay accepts a single connection, so each path gets a
        // fresh relay + state; both must answer 202.
        for path in [
            "/internal/v1/password-reset-webhook",
            "/internal/v1/timeless-password-reset",
        ] {
            let relay = fake_relay().await;
            let app = state(
                SERVICE_KEY,
                "admin@example.com",
                "recovery@example.test",
                "reset.example.com",
                Some(ResendClient::with_endpoint(
                    "test-key".into(),
                    relay.endpoint.clone(),
                )),
            )
            .await;
            let req = Request::builder()
                .method("POST")
                .uri(path)
                .header(header::AUTHORIZATION, format!("Bearer {SERVICE_KEY}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(format!("{{\"token\":\"{GOOD_TOKEN}\"}}")))
                .unwrap();
            let response = password_reset_routes()
                .with_state(app)
                .oneshot(req)
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::ACCEPTED, "{path} must serve");
            relay.task.abort();
        }
    }

    #[tokio::test]
    async fn transactional_email_is_fail_closed_when_delivery_is_disabled() {
        let mut relay = fake_relay().await;
        let mut app = state(
            SERVICE_KEY,
            "no-reply@example.com",
            "unused@example.test",
            "example.test",
            Some(ResendClient::with_endpoint(
                "test-relay-key".into(),
                relay.endpoint.clone(),
            )),
        )
        .await;
        Arc::get_mut(&mut app)
            .unwrap()
            .config
            .integration_email_enabled = false;
        let generated = auth::generate_api_key();
        sqlx::query("INSERT INTO integration_keys (key_id,app_id,token_hash,scopes_json,created_at) VALUES (?,?,?,?,?)")
            .bind(&generated.key_id)
            .bind("alpha")
            .bind(&generated.token_hash)
            .bind(json!(["email:template:auth.verify"]).to_string())
            .bind(auth::now())
            .execute(&app.pool)
            .await
            .unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            format!("Bearer {}", generated.token).parse().unwrap(),
        );
        headers.insert("idempotency-key", "disabled-check".parse().unwrap());
        let result = transactional_email::send(
            State(app.clone()),
            headers,
            Json(transactional_email::SendEmailBody {
                template_id: "auth.verify".into(),
                recipient: "user@example.test".into(),
                action_url: "https://alpha.example.com/verify?token=opaque".into(),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(result.0, StatusCode::SERVICE_UNAVAILABLE);
        let stored_requests: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM integration_email_requests")
                .fetch_one(&app.pool)
                .await
                .unwrap();
        assert_eq!(stored_requests, 0);
        assert_no_relay_call(&mut relay).await;
    }

    #[tokio::test]
    async fn transactional_email_uses_fixed_sender_and_deduplicates_retries() {
        let relay = fake_relay().await;
        let app = state(
            SERVICE_KEY,
            "no-reply@example.com",
            "unused@example.test",
            "example.test",
            Some(ResendClient::with_endpoint(
                "test-relay-key".into(),
                relay.endpoint.clone(),
            )),
        )
        .await;
        let generated = auth::generate_api_key();
        let expiry = (chrono::Utc::now() + chrono::Duration::days(1)).to_rfc3339();
        sqlx::query("INSERT INTO integration_keys (key_id,app_id,token_hash,scopes_json,created_at,expires_at,daily_send_limit) VALUES (?,?,?,?,?,?,10)")
            .bind(&generated.key_id)
            .bind("alpha")
            .bind(&generated.token_hash)
            .bind(json!(["email:template:auth.verify", "email:status"]).to_string())
            .bind(auth::now())
            .bind(expiry)
            .execute(&app.pool)
            .await
            .unwrap();

        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            format!("Bearer {}", generated.token).parse().unwrap(),
        );
        headers.insert("idempotency-key", "verify-user-42".parse().unwrap());
        let body = || transactional_email::SendEmailBody {
            template_id: "auth.verify".into(),
            recipient: "user@example.test".into(),
            action_url: "https://alpha.example.com/verify?token=opaque".into(),
        };
        let first = transactional_email::send(State(app.clone()), headers.clone(), Json(body()))
            .await
            .unwrap();
        assert_eq!(first.status(), StatusCode::ACCEPTED);
        let first_json: Value =
            serde_json::from_slice(&axum::body::to_bytes(first.into_body(), 4096).await.unwrap())
                .unwrap();
        let request_id = first_json["request_id"].as_str().unwrap().to_owned();
        assert_eq!(first_json["status"], "accepted");

        let payload = timeout(Duration::from_secs(1), relay.payload)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(payload["from"], "no-reply@example.com");
        assert_eq!(payload["to"], json!(["user@example.test"]));
        assert_eq!(payload["subject"], "Verify your email");
        assert!(payload["text"]
            .as_str()
            .unwrap()
            .contains("https://alpha.example.com/verify?token=opaque"));

        let retry = transactional_email::send(State(app.clone()), headers.clone(), Json(body()))
            .await
            .unwrap();
        assert_eq!(retry.status(), StatusCode::ACCEPTED);
        let retry_json: Value =
            serde_json::from_slice(&axum::body::to_bytes(retry.into_body(), 4096).await.unwrap())
                .unwrap();
        assert_eq!(retry_json["request_id"], request_id);
        assert_eq!(retry_json["status"], "accepted");

        let conflict = transactional_email::send(
            State(app.clone()),
            headers.clone(),
            Json(transactional_email::SendEmailBody {
                template_id: "auth.verify".into(),
                recipient: "different@example.test".into(),
                action_url: "https://alpha.example.com/verify?token=opaque".into(),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(conflict.0, StatusCode::CONFLICT);

        let request_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM integration_email_requests")
                .fetch_one(&app.pool)
                .await
                .unwrap();
        assert_eq!(request_count, 1);
        assert_eq!(relay.calls.load(Ordering::SeqCst), 1);

        let status = transactional_email::status(State(app), headers, Path(request_id))
            .await
            .unwrap();
        assert_eq!(status.status(), StatusCode::OK);
        relay.task.abort();
    }

    #[tokio::test]
    async fn transactional_email_unknown_relay_outcome_is_not_success_and_is_not_retried() {
        let relay = fake_relay_with_response(
            "500 Internal Server Error",
            r#"{"message":"temporary failure"}"#,
        )
        .await;
        let app = state(
            SERVICE_KEY,
            "no-reply@example.com",
            "unused@example.test",
            "example.test",
            Some(ResendClient::with_endpoint(
                "test-relay-key".into(),
                relay.endpoint.clone(),
            )),
        )
        .await;
        let generated = auth::generate_api_key();
        let expiry = (chrono::Utc::now() + chrono::Duration::days(1)).to_rfc3339();
        sqlx::query("INSERT INTO integration_keys (key_id,app_id,token_hash,scopes_json,created_at,expires_at,daily_send_limit) VALUES (?,?,?,?,?,?,10)")
            .bind(&generated.key_id)
            .bind("alpha")
            .bind(&generated.token_hash)
            .bind(json!(["email:template:auth.verify"]).to_string())
            .bind(auth::now())
            .bind(expiry)
            .execute(&app.pool)
            .await
            .unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            format!("Bearer {}", generated.token).parse().unwrap(),
        );
        headers.insert("idempotency-key", "unknown-relay-outcome".parse().unwrap());
        let body = || transactional_email::SendEmailBody {
            template_id: "auth.verify".into(),
            recipient: "user@example.test".into(),
            action_url: "https://alpha.example.com/verify?token=opaque".into(),
        };

        let first = transactional_email::send(State(app.clone()), headers.clone(), Json(body()))
            .await
            .unwrap();
        assert_eq!(first.status(), StatusCode::SERVICE_UNAVAILABLE);
        let first_json: Value =
            serde_json::from_slice(&axum::body::to_bytes(first.into_body(), 4096).await.unwrap())
                .unwrap();
        assert_eq!(first_json["status"], "unknown");
        let _payload = timeout(Duration::from_secs(1), relay.payload)
            .await
            .unwrap()
            .unwrap();

        let stored_status: String =
            sqlx::query_scalar("SELECT status FROM integration_email_requests WHERE id=?")
                .bind(first_json["request_id"].as_str().unwrap())
                .fetch_one(&app.pool)
                .await
                .unwrap();
        assert_eq!(stored_status, "unknown");

        let retry = transactional_email::send(State(app), headers, Json(body()))
            .await
            .unwrap();
        assert_eq!(retry.status(), StatusCode::SERVICE_UNAVAILABLE);
        let retry_json: Value =
            serde_json::from_slice(&axum::body::to_bytes(retry.into_body(), 4096).await.unwrap())
                .unwrap();
        assert_eq!(retry_json["status"], "unknown");
        assert_eq!(retry_json["request_id"], first_json["request_id"]);
        assert_eq!(relay.calls.load(Ordering::SeqCst), 1);
        relay.task.abort();
    }

    #[tokio::test]
    async fn transactional_email_failed_unknown_and_sending_attempts_do_not_consume_daily_quota() {
        let relay = fake_relay().await;
        let app = state(
            SERVICE_KEY,
            "no-reply@example.com",
            "unused@example.test",
            "example.test",
            Some(ResendClient::with_endpoint(
                "test-relay-key".into(),
                relay.endpoint.clone(),
            )),
        )
        .await;
        let generated = auth::generate_api_key();
        let expiry = (chrono::Utc::now() + chrono::Duration::days(1)).to_rfc3339();
        sqlx::query("INSERT INTO integration_keys (key_id,app_id,token_hash,scopes_json,created_at,expires_at,daily_send_limit) VALUES (?,?,?,?,?,?,1)")
            .bind(&generated.key_id)
            .bind("alpha")
            .bind(&generated.token_hash)
            .bind(json!(["email:template:auth.verify"]).to_string())
            .bind(auth::now())
            .bind(expiry)
            .execute(&app.pool)
            .await
            .unwrap();

        // One row per non-accepted outcome today, plus an accepted row from
        // yesterday: none of them may consume today's quota. (`sending` rows
        // of OTHER requests now DO consume quota to close the TOCTOU window,
        // so they are not seeded here.)
        let now = auth::now();
        let yesterday = (chrono::Utc::now() - chrono::Duration::days(1)).to_rfc3339();
        let insert = "INSERT INTO integration_email_requests (id,key_id,idempotency_key,template_id,recipient_hash,request_hash,status,created_at,updated_at) VALUES (?,?,?,?,?,?,?,?,?)";
        for (id, idempotency_key, status, created_at) in [
            ("seed-failed", "seed-failed", "failed", now.as_str()),
            ("seed-unknown", "seed-unknown", "unknown", now.as_str()),
            (
                "seed-old-accepted",
                "seed-old-accepted",
                "accepted",
                yesterday.as_str(),
            ),
        ] {
            sqlx::query(insert)
                .bind(id)
                .bind(&generated.key_id)
                .bind(idempotency_key)
                .bind("auth.verify")
                .bind("b".repeat(64))
                .bind("c".repeat(64))
                .bind(status)
                .bind(created_at)
                .bind(created_at)
                .execute(&app.pool)
                .await
                .unwrap();
        }

        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            format!("Bearer {}", generated.token).parse().unwrap(),
        );
        let body = || transactional_email::SendEmailBody {
            template_id: "auth.verify".into(),
            recipient: "user@example.test".into(),
            action_url: "https://alpha.example.com/verify?token=abc".into(),
        };

        headers.insert("idempotency-key", "quota-free-first".parse().unwrap());
        let first = transactional_email::send(State(app.clone()), headers.clone(), Json(body()))
            .await
            .unwrap();
        assert_eq!(first.status(), StatusCode::ACCEPTED);
        let _payload = timeout(Duration::from_secs(1), relay.payload)
            .await
            .unwrap()
            .unwrap();

        // That accepted send consumed the single daily slot, so the next
        // fresh request is rejected even though most rows today failed.
        headers.insert("idempotency-key", "quota-used-up".parse().unwrap());
        let rejected = transactional_email::send(State(app.clone()), headers, Json(body()))
            .await
            .unwrap_err();
        assert_eq!(rejected.0, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(rejected.1, "daily_limit_reached");
        assert_eq!(relay.calls.load(Ordering::SeqCst), 1);
        let request_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM integration_email_requests")
                .fetch_one(&app.pool)
                .await
                .unwrap();
        assert_eq!(request_count, 4);
        relay.task.abort();
    }

    #[tokio::test]
    async fn transactional_email_daily_quota_rejects_once_accepted_sends_reach_the_limit() {
        let mut relay = fake_relay().await;
        let app = state(
            SERVICE_KEY,
            "no-reply@example.com",
            "unused@example.test",
            "example.test",
            Some(ResendClient::with_endpoint(
                "test-relay-key".into(),
                relay.endpoint.clone(),
            )),
        )
        .await;
        let generated = auth::generate_api_key();
        let expiry = (chrono::Utc::now() + chrono::Duration::days(1)).to_rfc3339();
        sqlx::query("INSERT INTO integration_keys (key_id,app_id,token_hash,scopes_json,created_at,expires_at,daily_send_limit) VALUES (?,?,?,?,?,?,1)")
            .bind(&generated.key_id)
            .bind("alpha")
            .bind(&generated.token_hash)
            .bind(json!(["email:template:auth.verify"]).to_string())
            .bind(auth::now())
            .bind(expiry)
            .execute(&app.pool)
            .await
            .unwrap();

        // Exactly at the limit: one accepted send today already.
        let now = auth::now();
        sqlx::query("INSERT INTO integration_email_requests (id,key_id,idempotency_key,template_id,recipient_hash,request_hash,status,created_at,updated_at) VALUES (?,?,?,?,?,?,?,?,?)")
            .bind("seed-accepted")
            .bind(&generated.key_id)
            .bind("seed-accepted")
            .bind("auth.verify")
            .bind("b".repeat(64))
            .bind("c".repeat(64))
            .bind("accepted")
            .bind(&now)
            .bind(&now)
            .execute(&app.pool)
            .await
            .unwrap();

        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            format!("Bearer {}", generated.token).parse().unwrap(),
        );
        headers.insert("idempotency-key", "quota-at-limit".parse().unwrap());
        let rejected = transactional_email::send(
            State(app.clone()),
            headers,
            Json(transactional_email::SendEmailBody {
                template_id: "auth.verify".into(),
                recipient: "user@example.test".into(),
                action_url: "https://alpha.example.com/verify?token=abc".into(),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(rejected.0, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(rejected.1, "daily_limit_reached");
        let request_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM integration_email_requests")
                .fetch_one(&app.pool)
                .await
                .unwrap();
        assert_eq!(request_count, 1);
        assert_no_relay_call(&mut relay).await;
    }

    #[tokio::test]
    async fn migration_creates_scoped_integration_key_and_idempotency_tables() {
        let pool = iris_database::init_pool("sqlite::memory:").await.unwrap();
        let keys: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM integration_keys")
            .fetch_one(&pool)
            .await
            .unwrap();
        let sends: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM integration_email_requests")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(keys, 0);
        assert_eq!(sends, 0);
    }

    #[tokio::test]
    async fn migration_enforces_idempotency_per_integration_key() {
        let pool = iris_database::init_pool("sqlite::memory:").await.unwrap();
        sqlx::query("INSERT INTO integration_keys (key_id,app_id,token_hash,scopes_json,created_at) VALUES ('test-key','alpha',?,'[\\\"email:template:auth.verify\\\"]',?)")
            .bind("a".repeat(64)).bind(auth::now()).execute(&pool).await.unwrap();
        let insert = "INSERT INTO integration_email_requests (id,key_id,idempotency_key,template_id,recipient_hash,request_hash,status,created_at,updated_at) VALUES (?,?,?,?,?,?,'accepted',?,?)";
        let now = auth::now();
        sqlx::query(insert)
            .bind("request-1")
            .bind("test-key")
            .bind("same-key")
            .bind("auth.verify")
            .bind("b".repeat(64))
            .bind("c".repeat(64))
            .bind(&now)
            .bind(&now)
            .execute(&pool)
            .await
            .unwrap();
        let duplicate = sqlx::query(insert)
            .bind("request-2")
            .bind("test-key")
            .bind("same-key")
            .bind("auth.verify")
            .bind("b".repeat(64))
            .bind("c".repeat(64))
            .bind(&now)
            .bind(&now)
            .execute(&pool)
            .await;
        assert!(
            duplicate.is_err(),
            "a retry must not create a second provider request"
        );
    }
}

#[cfg(test)]
mod read_limit_tests {
    use super::*;

    async fn test_state() -> Arc<AppState> {
        let pool = iris_database::init_pool("sqlite::memory:").await.unwrap();
        Arc::new(AppState {
            pool,
            organize_lock: tokio::sync::Mutex::new(()),
            jmap: JmapClient::new("http://127.0.0.1:9".into()),
            http: reqwest::Client::new(),
            relay: None,
            html_bodies: html_body::HtmlBodyStore::new(),
            mail_watch: notify::MailWatch::from_limits(),
            config: Config {
                env: "test".into(),
                host: "127.0.0.1".into(),
                port: 8000,
                database_url: "sqlite::memory:".into(),
                session_secret: "test-secret".into(),
                turnstile_secret_key: String::new(),
                turnstile_hostnames: Vec::new(),
                turnstile_siteverify_url: String::new(),
                stalwart_api_url: "http://127.0.0.1:9".into(),
                temp_inbox_domain: "example.test".into(),
                temp_inbox_default_expiry_hours: 1,
                temp_inbox_mail_user: String::new(),
                temp_inbox_mail_password: String::new(),
                send_local_domains: vec![],
                send_external: false,
                resend_api_key: String::new(),
                integration_email_from: "no-reply@example.com".into(),
                integration_email_enabled: false,
                password_reset_service_token: String::new(),
                password_reset_email_from: String::new(),
                password_reset_email_to: String::new(),
                password_reset_public_origin: String::new(),
                resend_api_url: String::new(),
                mail_content_origin: String::new(),
                admin_emails: Vec::new(),
            },
        })
    }

    fn user(id: &str) -> CurrentUser {
        CurrentUser {
            user_id: id.to_string(),
            email: format!("{id}@example.test"),
            creds: Creds {
                user: id.to_string(),
                pass: "secret".into(),
            },
        }
    }

    /// Seed `n` allowed `read` rows for `user_id` at `occurred_at`, the way
    /// `check_read_limit` writes them.
    async fn seed_reads(pool: &SqlitePool, user_id: &str, n: i64, occurred_at: &str) {
        let subject = auth::hash_token(&format!("read:{user_id}"));
        for _ in 0..n {
            sqlx::query("INSERT INTO rate_limit_events (id,subject_hash,action,occurred_at,outcome) VALUES (?,?,'read',?,'allowed')")
                .bind(uuid::Uuid::new_v4().to_string())
                .bind(&subject)
                .bind(occurred_at)
                .execute(pool)
                .await
                .unwrap();
        }
    }

    fn assert_too_many_reads(r: Result<(), ApiError>) {
        let err = r.expect_err("a read past the cap must be refused");
        assert_eq!(err.0, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(err.1, "too_many_reads");
    }

    #[tokio::test]
    async fn refuses_at_cap_and_scopes_per_user() {
        let s = test_state().await;
        seed_reads(&s.pool, "alice", MAX_READS_PER_WINDOW - 1, &auth::now()).await;
        // The 600th read is still allowed; the 601st is refused.
        check_read_limit(&s, &user("alice")).await.unwrap();
        assert_too_many_reads(check_read_limit(&s, &user("alice")).await);
        // A refused read logs `blocked`, which never counts toward the cap,
        // and another user is unaffected by alice's cap.
        check_read_limit(&s, &user("bob")).await.unwrap();
    }

    #[tokio::test]
    async fn rows_older_than_window_do_not_count() {
        let s = test_state().await;
        let old =
            (chrono::Utc::now() - chrono::Duration::minutes(READ_WINDOW_MINUTES + 1)).to_rfc3339();
        seed_reads(&s.pool, "alice", MAX_READS_PER_WINDOW, &old).await;
        check_read_limit(&s, &user("alice")).await.unwrap();
    }
}

#[cfg(test)]
mod error_mapping_tests {
    use super::*;

    fn code(err: ApiError) -> (StatusCode, &'static str) {
        (err.0, err.1)
    }

    #[test]
    fn mail_errors_keep_their_category() {
        assert_eq!(
            code(ApiError::from(MailError::Auth)),
            (StatusCode::UNAUTHORIZED, "unauthorized")
        );
        assert_eq!(
            code(ApiError::from(MailError::NotFound)),
            (StatusCode::NOT_FOUND, "not_found")
        );
        assert_eq!(
            code(ApiError::from(MailError::Rejected("nope".into()))),
            (StatusCode::UNPROCESSABLE_ENTITY, "send_rejected")
        );
        assert_eq!(
            code(ApiError::from(MailError::Unavailable("down".into()))),
            (StatusCode::BAD_GATEWAY, "mail_server_unavailable")
        );
    }

    #[test]
    fn mail_protocol_errors_are_not_reported_as_unavailable() {
        // The server answered but with an unexpected response: reachable
        // yet misbehaving, a different cause from a transport failure.
        // The client-facing code stays generic; no detail leaks.
        assert_eq!(
            code(ApiError::from(MailError::Protocol(
                "unexpected response: internal detail".into()
            ))),
            (StatusCode::BAD_GATEWAY, "mail_server_error")
        );
    }

    #[test]
    fn db_errors_keep_their_category() {
        assert_eq!(
            code(ApiError::from(sqlx::Error::RowNotFound)),
            (StatusCode::NOT_FOUND, "not_found")
        );
        for e in [
            sqlx::Error::PoolTimedOut,
            sqlx::Error::PoolClosed,
            sqlx::Error::WorkerCrashed,
        ] {
            assert_eq!(
                code(ApiError::from(e)),
                (StatusCode::SERVICE_UNAVAILABLE, "database_unavailable")
            );
        }
    }
}

#[cfg(test)]
mod handler_integration_tests {
    use super::*;
    use axum::body::to_bytes;
    use iris_config::limits::{MAX_LIST_LIMIT, MAX_SEARCH_QUERY_CHARS, SENDS_PER_HOUR};
    use iris_mail::FILTER_SCRIPT_NAME;
    use serde_json::Value;
    use std::{
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc, Mutex,
        },
        time::Duration,
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        time::timeout,
    };

    /// What the fake JMAP server answers for an `Email/set` call.
    #[derive(Clone, Copy, PartialEq)]
    enum EmailSet {
        Created,
        NotCreated,
    }

    /// Script for one fake JMAP server: canned folders, list results, one
    /// message, one blob, and whether `Email/set` succeeds. No Stalwart, no
    /// real login: `JmapClient` just points at the loopback mock.
    struct JmapScript {
        /// Roles present in the Mailbox/get answer; default is all five.
        roles: Vec<&'static str>,
        email_set: EmailSet,
        /// Total for Email/query; the list is returned verbatim by Email/get.
        query_total: u64,
        query_list: Vec<Value>,
        /// Email/query answers the list once, then nothing: like a server
        /// whose matches disappear once cleared.
        query_drain: bool,
        /// When non-empty, the Nth Email/query call answers entry N
        /// (clamped to the last): (total, newest-first ids). Lets a test
        /// script a mailbox that gains mail between polls.
        query_sequence: Vec<(u64, Vec<&'static str>)>,
        /// Email/get answer for get_message (None = empty list -> NotFound).
        message: Option<Value>,
        /// Bytes served by GET /jmap/download/... for any blob id.
        blob: Vec<u8>,
        /// Custom folders (id, name): role-less mailboxes in Mailbox/get.
        custom: Vec<(&'static str, &'static str)>,
        /// Whether SieveScript/set succeeds.
        sieve_ok: bool,
        /// Existing `iris-filters` script for SieveScript/get: (id, is_active).
        sieve_existing: Option<(&'static str, bool)>,
        /// Whether the destroy in a script-removal request succeeds.
        sieve_destroy_ok: bool,
    }

    impl Default for JmapScript {
        fn default() -> Self {
            Self {
                roles: vec!["inbox", "sent", "drafts", "archive", "trash"],
                email_set: EmailSet::Created,
                query_total: 0,
                query_list: vec![],
                query_drain: false,
                query_sequence: vec![],
                message: None,
                blob: vec![],
                custom: vec![],
                sieve_ok: true,
                sieve_existing: None,
                sieve_destroy_ok: true,
            }
        }
    }

    struct FakeJmap {
        base_url: String,
        relay_url: String,
        /// Parsed JSON bodies of every POST /jmap/ request, in order.
        requests: Arc<Mutex<Vec<Value>>>,
        relay_calls: Arc<AtomicUsize>,
        /// GET /jmap/session hits — the JMAP login entry point. A
        /// handler that must not reach JMAP asserts this stays 0.
        session_calls: Arc<AtomicUsize>,
    }

    fn folder_json(role: &str) -> Value {
        json!({"id": format!("f-{role}"), "role": role, "name": role, "totalEmails": 0, "unreadEmails": 0})
    }

    /// One JMAP `methodResponses` body for the request the client posted.
    /// `query_calls` counts the Email/query calls before this one.
    fn jmap_response(script: &JmapScript, body: &Value, query_calls: usize) -> String {
        let calls = body["methodCalls"].as_array().cloned().unwrap_or_default();
        let first = calls
            .first()
            .and_then(|c| c[0].as_str())
            .unwrap_or_default()
            .to_string();
        let responses: Vec<Value> = match first.as_str() {
            "Mailbox/get" => {
                let mut list: Vec<Value> = script.roles.iter().map(|r| folder_json(r)).collect();
                list.extend(script.custom.iter().map(|(id, name)| {
                    json!({"id": id, "role": null, "name": name, "totalEmails": 0, "unreadEmails": 0})
                }));
                vec![json!(["Mailbox/get", {"list": list}, "m"])]
            }
            "SieveScript/get" => {
                let list: Vec<Value> = script
                    .sieve_existing
                    .map(|(id, active)| {
                        json!({"id": id, "name": FILTER_SCRIPT_NAME, "isActive": active})
                    })
                    .into_iter()
                    .collect();
                vec![json!(["SieveScript/get", {"list": list}, "g"])]
            }
            // Script removal sends deactivate + destroy in one request.
            // The mock answers one response per call, in order.
            "SieveScript/set" if calls.len() == 2 => {
                let destroy = if script.sieve_destroy_ok {
                    json!(["SieveScript/set", {"destroyed": ["sieve-1"], "notDestroyed": {}}, "d"])
                } else {
                    json!(["SieveScript/set", {"destroyed": [], "notDestroyed": {"sieve-1": {"type": "serverFail"}}}, "d"])
                };
                vec![json!(["SieveScript/set", {}, "a"]), destroy]
            }
            "SieveScript/set" if script.sieve_ok => vec![
                json!(["SieveScript/set", {"created": {"n": {"id": "sieve-1"}}, "notCreated": {}}, "s"]),
            ],
            "SieveScript/set" => vec![
                json!(["SieveScript/set", {"created": {}, "notCreated": {"n": {"type": "invalidSieve"}}}, "s"]),
            ],
            "Mailbox/set" => {
                vec![json!(["Mailbox/set", {"created": {}, "notCreated": {}}, "s"])]
            }
            "Identity/get" => vec![
                json!(["Identity/get", {"list": [{"id": "ident-1", "email": "alice@example.com"}]}, "i"]),
            ],
            "Identity/set" => vec![
                json!(["Identity/set", {"created": {"i": {"id": "ident-1"}}, "notCreated": {}}, "s"]),
            ],
            "Email/query" if !script.query_sequence.is_empty() => {
                let (total, ids) =
                    &script.query_sequence[query_calls.min(script.query_sequence.len() - 1)];
                let ids: Vec<Value> = ids.iter().map(|id| json!(id)).collect();
                let list: Vec<Value> = ids.iter().map(|id| json!({"id": id})).collect();
                vec![
                    json!(["Email/query", {"total": total, "ids": ids}, "q"]),
                    json!(["Email/get", {"list": list}, "g"]),
                ]
            }
            "Email/query" => {
                let drained = script.query_drain && query_calls > 0;
                let list: Vec<Value> = if drained {
                    vec![]
                } else {
                    script.query_list.clone()
                };
                let total = if drained { 0 } else { script.query_total };
                vec![
                    json!(["Email/query", {"total": total, "ids": list.iter().map(|m| m["id"].clone()).collect::<Vec<_>>()}, "q"]),
                    json!(["Email/get", {"list": list}, "g"]),
                ]
            }
            "Email/get" => {
                let list = script.message.clone().map(|m| vec![m]).unwrap_or_default();
                vec![json!(["Email/get", {"list": list}, "g"])]
            }
            // jmap.send submits Email/set + EmailSubmission/set in one batch.
            "Email/set" if calls.len() == 2 => vec![
                json!(["Email/set", {"created": {"n": {"id": "msg-1"}}, "notCreated": {}}, "e"]),
                json!(["EmailSubmission/set", {"created": {"s": {"id": "sub-1"}}, "notCreated": {}}, "x"]),
            ],
            "Email/set" if calls[0][1]["update"].is_object() => {
                let updated: serde_json::Map<String, Value> = calls[0][1]["update"]
                    .as_object()
                    .unwrap()
                    .keys()
                    .map(|k| (k.clone(), Value::Null))
                    .collect();
                vec![json!(["Email/set", {"updated": updated}, "u"])]
            }
            "Email/set" => match script.email_set {
                EmailSet::Created => {
                    vec![
                        json!(["Email/set", {"created": {"n": {"id": "msg-1"}}, "notCreated": {}}, "s"]),
                    ]
                }
                EmailSet::NotCreated => vec![
                    json!(["Email/set", {"created": {}, "notCreated": {"n": {"type": "forbidden", "description": "mock refuses the Sent copy"}}}, "s"]),
                ],
            },
            _ => vec![json!(["error", {"type": "unknownMethod"}, "e"])],
        };
        serde_json::to_string(&json!({"methodResponses": responses})).unwrap()
    }

    /// Loopback HTTP server answering JMAP on POST /jmap/, the relay on
    /// POST /emails, and blob downloads on GET /jmap/download/... One
    /// request per connection (`connection: close`); the task ends after
    /// 5 s without a connection, or when the test runtime drops.
    async fn fake_jmap(script: JmapScript) -> FakeJmap {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let relay_calls = Arc::new(AtomicUsize::new(0));
        let session_calls = Arc::new(AtomicUsize::new(0));
        let task_requests = requests.clone();
        let task_relay = relay_calls.clone();
        let task_session = session_calls.clone();
        drop(tokio::spawn(async move {
            let mut query_calls = 0usize;
            loop {
                let Ok(Ok((mut stream, _))) =
                    timeout(Duration::from_secs(5), listener.accept()).await
                else {
                    return;
                };
                // Read one HTTP request: headers, then Content-Length body.
                let mut bytes = Vec::new();
                let mut chunk = [0u8; 4096];
                let mut expected: Option<usize> = None;
                let mut header_end: Option<usize> = None;
                loop {
                    let Ok(n) = stream.read(&mut chunk).await else {
                        return;
                    };
                    if n == 0 {
                        return;
                    }
                    bytes.extend_from_slice(&chunk[..n]);
                    if header_end.is_none() {
                        if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                            header_end = Some(end);
                            let headers = String::from_utf8_lossy(&bytes[..end]);
                            let length = headers
                                .lines()
                                .find_map(|line| {
                                    let (name, value) = line.split_once(':')?;
                                    name.eq_ignore_ascii_case("content-length")
                                        .then(|| value.trim().parse::<usize>().ok())
                                        .flatten()
                                })
                                .unwrap_or(0);
                            expected = Some(end + 4 + length);
                        }
                    }
                    if let Some(end) = expected {
                        if bytes.len() >= end {
                            break;
                        }
                    }
                }
                let end = header_end.unwrap();
                let head = String::from_utf8_lossy(&bytes[..end]).to_string();
                let mut lines = head.lines();
                let request_line = lines.next().unwrap_or_default().to_string();
                let mut parts = request_line.split_whitespace();
                let method = parts.next().unwrap_or_default().to_string();
                let path = parts.next().unwrap_or_default().to_string();
                let body = bytes[end + 4..].to_vec();
                let (status, content_type, payload): (&str, &str, Vec<u8>) =
                    if method == "POST" && path == "/jmap/" {
                        let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                        task_requests.lock().unwrap().push(v.clone());
                        let first = v["methodCalls"][0][0].as_str().unwrap_or_default();
                        let calls = query_calls;
                        if first == "Email/query" {
                            query_calls += 1;
                        }
                        (
                            "200 OK",
                            "application/json",
                            jmap_response(&script, &v, calls).into_bytes(),
                        )
                    } else if method == "POST" && path == "/emails" {
                        task_relay.fetch_add(1, Ordering::SeqCst);
                        (
                            "200 OK",
                            "application/json",
                            br#"{"id":"re_mock_1"}"#.to_vec(),
                        )
                    } else if method == "GET" && path == "/jmap/session" {
                        // Counted, then 404 like any unknown route: no
                        // test needs a real session here, but the
                        // login-gate tests assert this is never hit.
                        task_session.fetch_add(1, Ordering::SeqCst);
                        (
                            "404 Not Found",
                            "application/json",
                            br#"{"error":"no such route"}"#.to_vec(),
                        )
                    } else if method == "POST" && path.starts_with("/jmap/upload/") {
                        task_requests
                            .lock()
                            .unwrap()
                            .push(json!({"upload": String::from_utf8_lossy(&body)}));
                        (
                            "200 OK",
                            "application/json",
                            br#"{"blobId":"blob-1"}"#.to_vec(),
                        )
                    } else if method == "GET" && path.starts_with("/jmap/download/") {
                        ("200 OK", "application/octet-stream", script.blob.clone())
                    } else {
                        (
                            "404 Not Found",
                            "application/json",
                            br#"{"error":"no such route"}"#.to_vec(),
                        )
                    };
                let response = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    payload.len()
                );
                let mut out = response.into_bytes();
                out.extend_from_slice(&payload);
                let _ = stream.write_all(&out).await;
                let _ = stream.shutdown().await;
            }
        }));
        FakeJmap {
            base_url: format!("http://{address}"),
            relay_url: format!("http://{address}/emails"),
            requests,
            relay_calls,
            session_calls,
        }
    }

    async fn test_state(jmap: &FakeJmap, relay: Option<ResendClient>) -> Arc<AppState> {
        let pool = iris_database::init_pool("sqlite::memory:").await.unwrap();
        sqlx::query("INSERT INTO app_users (id,email,display_name,created_at,updated_at) VALUES ('u-alice','alice@example.com','Alice','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO permanent_mailboxes (id,user_id,email_address,mail_server_account_id,status,created_at,updated_at) VALUES ('mbx-1','u-alice','alice@example.com','acct-1','active','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')")
            .execute(&pool)
            .await
            .unwrap();
        Arc::new(AppState {
            pool,
            organize_lock: tokio::sync::Mutex::new(()),
            jmap: JmapClient::new(jmap.base_url.clone()),
            http: reqwest::Client::new(),
            relay,
            html_bodies: html_body::HtmlBodyStore::new(),
            mail_watch: notify::MailWatch::from_limits(),
            config: Config {
                env: "test".into(),
                host: "127.0.0.1".into(),
                port: 8000,
                database_url: "sqlite::memory:".into(),
                session_secret: "test-secret".into(),
                turnstile_secret_key: String::new(),
                turnstile_hostnames: Vec::new(),
                turnstile_siteverify_url: String::new(),
                stalwart_api_url: "http://127.0.0.1:9".into(),
                temp_inbox_domain: "example.test".into(),
                temp_inbox_default_expiry_hours: 1,
                temp_inbox_mail_user: String::new(),
                temp_inbox_mail_password: String::new(),
                send_local_domains: vec!["example.com".into()],
                send_external: false,
                resend_api_key: "test-resend-key".into(),
                integration_email_from: "no-reply@example.com".into(),
                integration_email_enabled: false,
                password_reset_service_token: String::new(),
                password_reset_email_from: String::new(),
                password_reset_email_to: String::new(),
                password_reset_public_origin: String::new(),
                resend_api_url: String::new(),
                mail_content_origin: String::new(),
                admin_emails: Vec::new(),
            },
        })
    }

    fn user() -> CurrentUser {
        CurrentUser {
            user_id: "u-alice".into(),
            email: "alice@example.com".into(),
            creds: Creds {
                user: "alice@example.com".into(),
                pass: "secret".into(),
            },
        }
    }

    fn relay(jmap: &FakeJmap) -> ResendClient {
        ResendClient::with_endpoint("test-relay-key".into(), jmap.relay_url.clone())
    }

    async fn body_json(resp: Response) -> Value {
        let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn compose_body(to: &str) -> Json<compose::ComposeBody> {
        Json(
            serde_json::from_value(json!({
                "to": [to], "subject": "Hi", "text": "Hello from the test",
            }))
            .unwrap(),
        )
    }

    /// Seed `n` allowed `send` rows for alice within the last hour, the way
    /// `compose::send` counts them.
    async fn seed_sends(pool: &SqlitePool, n: i64) {
        let subject = auth::hash_token("send:u-alice");
        for _ in 0..n {
            sqlx::query("INSERT INTO rate_limit_events (id,subject_hash,action,occurred_at,outcome) VALUES (?,?,'send',?,'allowed')")
                .bind(uuid::Uuid::new_v4().to_string())
                .bind(&subject)
                .bind(auth::now())
                .execute(pool)
                .await
                .unwrap();
        }
    }

    async fn send_count(pool: &SqlitePool) -> i64 {
        let subject = auth::hash_token("send:u-alice");
        sqlx::query_scalar(
            "SELECT COUNT(*) FROM rate_limit_events WHERE subject_hash=? AND action='send' AND outcome='allowed'",
        )
        .bind(&subject)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    fn summary_json(id: &str, subject: &str) -> Value {
        json!({"id": id, "from": [{"email": "a@example.com"}], "to": [{"email": "alice@example.com"}], "subject": subject, "preview": "p", "receivedAt": "2026-11-04T00:00:00Z", "keywords": {"$seen": true}, "size": 100, "hasAttachment": false})
    }

    /// Compose -> relay -> store_sent fails: the message is already
    /// delivered, so the handler must still answer success, report the
    /// failed copy honestly (`sent_copy: false`), and still log the send.
    #[tokio::test]
    async fn relay_send_reports_sent_copy_false_when_store_sent_fails() {
        let jmap = fake_jmap(JmapScript {
            email_set: EmailSet::NotCreated,
            ..Default::default()
        })
        .await;
        let s = test_state(&jmap, Some(relay(&jmap))).await;
        let resp = compose::send(
            State(s.clone()),
            Extension(user()),
            Path("mbx-1".into()),
            compose_body("bob@example.test"),
        )
        .await
        .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["sent"], true);
        assert_eq!(v["sent_copy"], false);
        assert_eq!(jmap.relay_calls.load(Ordering::SeqCst), 1);
        assert_eq!(send_count(&s.pool).await, 1);
        // The failed copy really was an Email/set into Sent, not a skip.
        let reqs = jmap.requests.lock().unwrap();
        let set = reqs
            .iter()
            .find(|r| r["methodCalls"][0][0] == "Email/set")
            .expect("store_sent must attempt Email/set");
        assert_eq!(
            set["methodCalls"][0][1]["create"]["n"]["mailboxIds"]["f-sent"],
            true
        );
        assert_eq!(
            set["methodCalls"][0][1]["create"]["n"]["keywords"]["$seen"],
            true
        );
    }

    #[tokio::test]
    async fn relay_send_reports_sent_copy_true_when_store_sent_succeeds() {
        let jmap = fake_jmap(JmapScript::default()).await;
        let s = test_state(&jmap, Some(relay(&jmap))).await;
        let resp = compose::send(
            State(s.clone()),
            Extension(user()),
            Path("mbx-1".into()),
            compose_body("bob@example.test"),
        )
        .await
        .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["sent"], true);
        assert_eq!(v["sent_copy"], true);
        assert_eq!(jmap.relay_calls.load(Ordering::SeqCst), 1);
        assert_eq!(send_count(&s.pool).await, 1);
    }

    /// The hourly send quota is enforced before any relay or JMAP call:
    /// the 101st send in an hour is refused and nothing is delivered.
    #[tokio::test]
    async fn send_quota_refuses_at_cap_without_relay_or_jmap() {
        let jmap = fake_jmap(JmapScript::default()).await;
        let s = test_state(&jmap, Some(relay(&jmap))).await;
        seed_sends(&s.pool, SENDS_PER_HOUR).await;
        let err = compose::send(
            State(s.clone()),
            Extension(user()),
            Path("mbx-1".into()),
            compose_body("bob@example.com"),
        )
        .await
        .expect_err("100 sends in the last hour must refuse the 101st");
        assert_eq!(err.0, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(err.1, "too_many_sends");
        assert_eq!(jmap.relay_calls.load(Ordering::SeqCst), 0);
        assert!(jmap.requests.lock().unwrap().is_empty());
        assert_eq!(send_count(&s.pool).await, SENDS_PER_HOUR);
    }

    /// A local recipient with no relay goes through JMAP submission, which
    /// files Sent atomically: `sent_copy` is always true on that path.
    #[tokio::test]
    async fn local_send_via_jmap_files_sent_copy() {
        let jmap = fake_jmap(JmapScript::default()).await;
        let s = test_state(&jmap, None).await;
        let resp = compose::send(
            State(s.clone()),
            Extension(user()),
            Path("mbx-1".into()),
            compose_body("alice@example.com"),
        )
        .await
        .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["sent"], true);
        assert_eq!(v["sent_copy"], true);
        assert_eq!(jmap.relay_calls.load(Ordering::SeqCst), 0);
        let reqs = jmap.requests.lock().unwrap();
        let batch = reqs
            .iter()
            .find(|r| r["methodCalls"].as_array().is_some_and(|c| c.len() == 2))
            .expect("jmap send must submit Email/set + EmailSubmission/set in one batch");
        assert_eq!(batch["methodCalls"][1][0], "EmailSubmission/set");
        assert_eq!(
            batch["methodCalls"][1][1]["onSuccessUpdateEmail"]["#s"]["mailboxIds/f-sent"],
            true
        );
    }

    /// Search + pagination through the real handler: the query text,
    /// position and limit reach JMAP, and the total + page come back.
    #[tokio::test]
    async fn list_messages_forwards_search_and_pagination() {
        let jmap = fake_jmap(JmapScript {
            query_total: 5,
            query_list: vec![
                summary_json("m3", "Invoice 3"),
                summary_json("m4", "Invoice 4"),
            ],
            ..Default::default()
        })
        .await;
        let s = test_state(&jmap, None).await;
        let resp = list_messages(
            State(s.clone()),
            Extension(user()),
            Path("mbx-1".into()),
            Query(Page {
                limit: Some(2),
                position: Some(2),
                folder: None,
                folder_id: None,
                view: None,
                label: None,
                q: Some("invoice".into()),
            }),
        )
        .await
        .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["total"], 5);
        assert_eq!(v["position"], 2);
        assert_eq!(v["messages"].as_array().unwrap().len(), 2);
        assert_eq!(v["messages"][0]["subject"], "Invoice 3");
        let reqs = jmap.requests.lock().unwrap();
        let query = reqs
            .iter()
            .find(|r| r["methodCalls"][0][0] == "Email/query")
            .expect("list must query JMAP");
        let args = &query["methodCalls"][0][1];
        assert_eq!(args["filter"]["conditions"][0]["inMailbox"], "f-inbox");
        assert_eq!(args["filter"]["conditions"][1]["text"], "invoice");
        assert_eq!(args["position"], 2);
        assert_eq!(args["limit"], 2);
        assert_eq!(args["calculateTotal"], true);
    }

    /// The mailbox watcher shares one task per mailbox, reports the
    /// arrival, and stops when its last tab disconnects.
    #[tokio::test]
    async fn mail_watch_emits_new_mail_and_stops_when_idle() {
        let jmap = fake_jmap(JmapScript {
            query_sequence: vec![(1, vec!["m1"]), (2, vec!["m2"])],
            ..Default::default()
        })
        .await;
        let s = test_state(&jmap, None).await;
        let watch = notify::MailWatch::new(Duration::from_millis(50));
        let mut rx = watch
            .subscribe(&s.jmap, &user().creds, "acct-1", "u-alice", "mbx-1")
            .await
            .unwrap();
        let next_snapshot =
            async |rx: &mut tokio::sync::watch::Receiver<Option<iris_mail::InboxSnapshot>>| loop {
                rx.changed().await.unwrap();
                if let Some(snap) = rx.borrow().clone() {
                    break snap;
                }
            };
        // First snapshot is the baseline: one message, newest m1.
        let first = timeout(Duration::from_secs(5), next_snapshot(&mut rx))
            .await
            .unwrap();
        assert_eq!(first.total, 1);
        assert_eq!(first.newest_id.as_deref(), Some("m1"));
        // The next poll sees the arrival: total 2, newest m2.
        let second = timeout(Duration::from_secs(5), next_snapshot(&mut rx))
            .await
            .unwrap();
        assert_eq!(second.total, 2);
        assert_eq!(second.newest_id.as_deref(), Some("m2"));
        assert_eq!(iris_mail::new_mail_count(&first, &second), 1);
        // A second subscriber shares the watcher: no second task.
        let rx2 = watch
            .subscribe(&s.jmap, &user().creds, "acct-1", "u-alice", "mbx-1")
            .await
            .unwrap();
        assert_eq!(watch.active_watchers(), 1);
        drop(rx);
        drop(rx2);
        // With no subscribers the watcher stops and unregisters itself.
        timeout(Duration::from_secs(5), async {
            loop {
                if watch.active_watchers() == 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn mailbox_events_rejects_unknown_mailbox() {
        let jmap = fake_jmap(JmapScript::default()).await;
        let s = test_state(&jmap, None).await;
        let err = mailbox_events(State(s), Extension(user()), Path("nope".into()))
            .await
            .expect_err("unknown mailbox must fail");
        assert_eq!(err.0, StatusCode::NOT_FOUND);
        assert_eq!(err.1, "not_found");
    }

    fn page(f: impl FnOnce(&mut Page)) -> Query<Page> {
        let mut p = Page {
            limit: None,
            position: None,
            folder: None,
            folder_id: None,
            view: None,
            label: None,
            q: None,
        };
        f(&mut p);
        Query(p)
    }

    fn query_filter(jmap: &FakeJmap) -> Value {
        let reqs = jmap.requests.lock().unwrap();
        reqs.iter()
            .find(|r| r["methodCalls"][0][0] == "Email/query")
            .expect("list must query JMAP")["methodCalls"][0][1]["filter"]
            .clone()
    }

    #[tokio::test]
    async fn starred_view_queries_flagged_outside_trash() {
        let jmap = fake_jmap(JmapScript::default()).await;
        let s = test_state(&jmap, None).await;
        let p = page(|p| p.view = Some("starred".into()));
        list_messages(State(s), Extension(user()), Path("mbx-1".into()), p)
            .await
            .unwrap();
        let f = query_filter(&jmap);
        assert_eq!(f["conditions"][0]["hasKeyword"], "$flagged");
        assert_eq!(f["conditions"][1]["inMailboxOtherThan"][0], "f-trash");
    }

    #[tokio::test]
    async fn label_and_custom_folder_views() {
        let jmap = fake_jmap(JmapScript {
            custom: vec![("c-1", "Receipts")],
            ..Default::default()
        })
        .await;
        let s = test_state(&jmap, None).await;
        let p = page(|p| p.label = Some("work".into()));
        list_messages(State(s.clone()), Extension(user()), Path("mbx-1".into()), p)
            .await
            .unwrap();
        assert_eq!(query_filter(&jmap)["conditions"][0]["hasKeyword"], "work");

        jmap.requests.lock().unwrap().clear();
        let p = page(|p| p.folder_id = Some("c-1".into()));
        list_messages(State(s.clone()), Extension(user()), Path("mbx-1".into()), p)
            .await
            .unwrap();
        assert_eq!(query_filter(&jmap)["inMailbox"], "c-1");

        // A standard folder's id is not a custom folder.
        let p = page(|p| p.folder_id = Some("f-inbox".into()));
        let e = list_messages(State(s.clone()), Extension(user()), Path("mbx-1".into()), p)
            .await
            .unwrap_err();
        assert_eq!(e.0, StatusCode::NOT_FOUND);

        for p in [
            page(|p| p.label = Some("$seen".into())),
            page(|p| p.view = Some("junk".into())),
            page(|p| p.folder_id = Some("../x".into())),
            // Both a role and a custom folder id: ambiguous, refused
            // instead of silently listing one of them.
            page(|p| {
                p.folder = Some("inbox".into());
                p.folder_id = Some("c-1".into());
            }),
        ] {
            let e = list_messages(State(s.clone()), Extension(user()), Path("mbx-1".into()), p)
                .await
                .unwrap_err();
            assert_eq!(e.0, StatusCode::BAD_REQUEST);
        }

        // A higher-priority label or view wins over both folder selectors,
        // so the combination is only refused when neither is given.
        jmap.requests.lock().unwrap().clear();
        let p = page(|p| {
            p.folder = Some("inbox".into());
            p.folder_id = Some("c-1".into());
            p.label = Some("work".into());
        });
        list_messages(State(s.clone()), Extension(user()), Path("mbx-1".into()), p)
            .await
            .unwrap();
        assert_eq!(query_filter(&jmap)["conditions"][0]["hasKeyword"], "work");

        jmap.requests.lock().unwrap().clear();
        let p = page(|p| {
            p.folder = Some("inbox".into());
            p.folder_id = Some("c-1".into());
            p.view = Some("starred".into());
        });
        list_messages(State(s.clone()), Extension(user()), Path("mbx-1".into()), p)
            .await
            .unwrap();
        assert_eq!(
            query_filter(&jmap)["conditions"][0]["hasKeyword"],
            "$flagged"
        );
    }

    #[tokio::test]
    async fn star_and_label_set_keywords() {
        let jmap = fake_jmap(JmapScript::default()).await;
        let s = test_state(&jmap, None).await;
        let star: organize::StarBody = serde_json::from_value(json!({"starred": true})).unwrap();
        organize::set_star(
            State(s.clone()),
            Extension(user()),
            Path(("mbx-1".into(), "m1".into())),
            Json(star),
        )
        .await
        .unwrap();

        let body = || {
            Json(
                serde_json::from_value::<organize::MessageLabelBody>(
                    json!({"label": "Work", "on": true}),
                )
                .unwrap(),
            )
        };
        let e = organize::set_message_label(
            State(s.clone()),
            Extension(user()),
            Path(("mbx-1".into(), "m1".into())),
            body(),
        )
        .await
        .unwrap_err();
        assert_eq!(e.1, "unknown_label");

        let new: organize::LabelBody = serde_json::from_value(json!({"name": "work"})).unwrap();
        organize::create_label(
            State(s.clone()),
            Extension(user()),
            Path("mbx-1".into()),
            Json(new),
        )
        .await
        .unwrap();
        organize::set_message_label(
            State(s.clone()),
            Extension(user()),
            Path(("mbx-1".into(), "m1".into())),
            body(),
        )
        .await
        .unwrap();

        let reqs = jmap.requests.lock().unwrap();
        let updates: Vec<Value> = reqs
            .iter()
            .filter(|r| r["methodCalls"][0][0] == "Email/set")
            .map(|r| r["methodCalls"][0][1]["update"]["m1"].clone())
            .collect();
        assert_eq!(
            updates,
            vec![
                json!({"keywords/$flagged": true}),
                json!({"keywords/work": true})
            ]
        );
    }

    #[tokio::test]
    async fn move_to_custom_folder_checks_the_id() {
        let jmap = fake_jmap(JmapScript {
            custom: vec![("c-1", "Receipts")],
            ..Default::default()
        })
        .await;
        let s = test_state(&jmap, None).await;
        let mv = |id: &str| {
            Json(serde_json::from_value::<compose::MoveBody>(json!({"folder_id": id})).unwrap())
        };
        let e = compose::move_message(
            State(s.clone()),
            Extension(user()),
            Path(("mbx-1".into(), "m1".into())),
            mv("f-sent"),
        )
        .await
        .unwrap_err();
        assert_eq!(e.1, "invalid_folder");
        jmap.requests.lock().unwrap().clear();
        compose::move_message(
            State(s.clone()),
            Extension(user()),
            Path(("mbx-1".into(), "m1".into())),
            mv("c-1"),
        )
        .await
        .unwrap();
        let set = {
            let reqs = jmap.requests.lock().unwrap();
            reqs.iter()
                .find(|r| r["methodCalls"][0][0] == "Email/set")
                .unwrap()["methodCalls"][0][1]["update"]["m1"]["mailboxIds"]
                .clone()
        };
        assert_eq!(set, json!({"c-1": true}));
        jmap.requests.lock().unwrap().clear();

        // Moving back into Sent works: Undo restores a message to the
        // folder it came from, including Sent.
        let role = |r: &str| {
            Json(serde_json::from_value::<compose::MoveBody>(json!({"folder": r})).unwrap())
        };
        compose::move_message(
            State(s.clone()),
            Extension(user()),
            Path(("mbx-1".into(), "m1".into())),
            role("sent"),
        )
        .await
        .unwrap();
        let set = jmap
            .requests
            .lock()
            .unwrap()
            .iter()
            .find(|r| r["methodCalls"][0][0] == "Email/set")
            .unwrap()["methodCalls"][0][1]["update"]["m1"]["mailboxIds"]
            .clone();
        assert_eq!(set, json!({"f-sent": true}));
        // Drafts stays closed: a draft is an unsent composition, not a
        // folder to file mail into.
        let e = compose::move_message(
            State(s.clone()),
            Extension(user()),
            Path(("mbx-1".into(), "m1".into())),
            role("drafts"),
        )
        .await
        .unwrap_err();
        assert_eq!(e.1, "invalid_folder");
    }

    async fn filter_count(s: &AppState) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM mail_filters")
            .fetch_one(&s.pool)
            .await
            .unwrap()
    }

    fn new_rule(v: Value) -> Json<filters::NewRule> {
        Json(serde_json::from_value(v).unwrap())
    }

    #[tokio::test]
    async fn filter_create_installs_sieve_script() {
        let jmap = fake_jmap(JmapScript {
            custom: vec![("c-1", "Receipts")],
            ..Default::default()
        })
        .await;
        let s = test_state(&jmap, None).await;
        let resp = filters::create(
            State(s.clone()),
            Extension(user()),
            Path("mbx-1".into()),
            new_rule(
                json!({"field": "from", "value": "shop.test", "action": "move", "target": "c-1"}),
            ),
        )
        .await
        .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        assert_eq!(filter_count(&s).await, 1);
        {
            let reqs = jmap.requests.lock().unwrap();
            let script = reqs
                .iter()
                .find_map(|r| r["upload"].as_str())
                .expect("script uploaded");
            assert!(script.contains("fileinto :mailboxid \"c-1\" \"Receipts\";"));
            let set = reqs
                .iter()
                .find(|r| r["methodCalls"][0][0] == "SieveScript/set")
                .unwrap();
            assert_eq!(set["methodCalls"][0][1]["onSuccessActivateScript"], "#n");
            assert_eq!(set["methodCalls"][0][1]["create"]["n"]["blobId"], "blob-1");
        }

        // Inbox is not a filter target; a folder in use cannot be deleted.
        let e = filters::create(
            State(s.clone()),
            Extension(user()),
            Path("mbx-1".into()),
            new_rule(json!({"field": "from", "value": "x", "action": "move", "target": "f-inbox"})),
        )
        .await
        .unwrap_err();
        assert_eq!(e.1, "invalid_folder");
        let e = organize::delete_folder(
            State(s.clone()),
            Extension(user()),
            Path(("mbx-1".into(), "c-1".into())),
        )
        .await
        .unwrap_err();
        assert_eq!(e.1, "folder_in_use_by_filter");
    }

    #[tokio::test]
    async fn filter_is_rolled_back_when_the_script_is_refused() {
        let jmap = fake_jmap(JmapScript {
            sieve_ok: false,
            ..Default::default()
        })
        .await;
        let s = test_state(&jmap, None).await;
        let e = filters::create(
            State(s.clone()),
            Extension(user()),
            Path("mbx-1".into()),
            new_rule(json!({"field": "subject", "value": "news", "action": "read"})),
        )
        .await
        .unwrap_err();
        assert_eq!(e.0, StatusCode::BAD_GATEWAY);
        assert_eq!(filter_count(&s).await, 0);
        for bad in [
            json!({"field": "body", "value": "x", "action": "read"}),
            json!({"field": "from", "value": " ", "action": "read"}),
            json!({"field": "from", "value": "a\nb", "action": "read"}),
            json!({"field": "from", "value": "x", "action": "delete"}),
        ] {
            let e = filters::create(
                State(s.clone()),
                Extension(user()),
                Path("mbx-1".into()),
                new_rule(bad),
            )
            .await
            .unwrap_err();
            assert_eq!(e.1, "invalid_filter");
        }
    }

    /// Deleting the last filter when the server refuses to destroy the
    /// active script: the database rule is restored AND the script is
    /// reactivated, so the stored rules and the running script agree.
    #[tokio::test]
    async fn filter_delete_reactivates_script_when_destroy_fails() {
        let jmap = fake_jmap(JmapScript {
            sieve_existing: Some(("sieve-1", true)),
            sieve_destroy_ok: false,
            ..Default::default()
        })
        .await;
        let s = test_state(&jmap, None).await;
        sqlx::query(
            "INSERT INTO mail_filters (id, mailbox_id, field, value, action, target, created_at) \
             VALUES ('r-1', 'mbx-1', 'subject', 'news', 'read', NULL, '2026-01-01T00:00:00Z')",
        )
        .execute(&s.pool)
        .await
        .unwrap();
        let e = filters::delete(
            State(s.clone()),
            Extension(user()),
            Path(("mbx-1".into(), "r-1".into())),
        )
        .await
        .unwrap_err();
        assert_eq!(e.0, StatusCode::BAD_GATEWAY);
        // The rule is restored, so the delete did not happen.
        assert_eq!(filter_count(&s).await, 1);
        // And the script was reactivated after the failed destroy.
        let reqs = jmap.requests.lock().unwrap();
        assert!(
            reqs.iter()
                .any(|r| r["methodCalls"][0][1]["onSuccessActivateScript"] == "sieve-1"),
            "expected a reactivation call, got: {reqs:?}"
        );
    }

    /// Deleting a label clears the keyword from every message, not just
    /// the first query page: the mock answers the messages once, then
    /// nothing, like a server whose matches disappear once cleared.
    #[tokio::test]
    async fn delete_label_clears_the_keyword_from_every_message() {
        let jmap = fake_jmap(JmapScript {
            query_drain: true,
            query_total: 2,
            query_list: vec![json!({"id": "m1"}), json!({"id": "m2"})],
            ..Default::default()
        })
        .await;
        let s = test_state(&jmap, None).await;
        sqlx::query(
            "INSERT INTO mailbox_labels (mailbox_id, name, created_at) VALUES ('mbx-1', 'work', '2026-01-01T00:00:00Z')",
        )
        .execute(&s.pool)
        .await
        .unwrap();
        organize::delete_label(
            State(s.clone()),
            Extension(user()),
            Path(("mbx-1".into(), "work".into())),
        )
        .await
        .unwrap();
        assert!(!organize::label_exists(&s, "mbx-1", "work").await.unwrap());
        let reqs = jmap.requests.lock().unwrap();
        // Two queries: one found the messages, the second came back empty.
        assert_eq!(
            reqs.iter()
                .filter(|r| r["methodCalls"][0][0] == "Email/query")
                .count(),
            2
        );
        let updates: Vec<&Value> = reqs
            .iter()
            .filter(|r| r["methodCalls"][0][0] == "Email/set")
            .collect();
        assert_eq!(updates.len(), 1);
        assert!(updates[0]["methodCalls"][0][1]["update"]["m1"]["keywords/work"].is_null());
        assert!(updates[0]["methodCalls"][0][1]["update"]["m2"]["keywords/work"].is_null());
    }

    /// A filter create waits on the organize lock: while the lock is held
    /// it cannot reach the insert or the Sieve install, so two concurrent
    /// creates (or a create racing a label delete) can never interleave a
    /// stale rule read with a newer install.
    #[tokio::test]
    async fn filter_create_waits_for_the_organize_lock() {
        let jmap = fake_jmap(JmapScript::default()).await;
        let s = test_state(&jmap, None).await;
        let guard = s.organize_lock.lock().await;
        let s2 = s.clone();
        let mut create = tokio::spawn(async move {
            filters::create(
                State(s2),
                Extension(user()),
                Path("mbx-1".into()),
                new_rule(json!({"field": "subject", "value": "news", "action": "read"})),
            )
            .await
        });
        assert!(
            timeout(Duration::from_millis(200), &mut create)
                .await
                .is_err(),
            "create finished while the organize lock was held"
        );
        drop(guard);
        let resp = create.await.unwrap().unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
        assert_eq!(filter_count(&s).await, 1);
    }

    /// A label assign waits on the organize lock, like filter creates do:
    /// while the lock is held it cannot check the label or set the keyword,
    /// so it can never slip in between a label delete's keyword clear and
    /// row delete and leave an orphaned keyword behind.
    #[tokio::test]
    async fn label_assign_waits_for_the_organize_lock() {
        let jmap = fake_jmap(JmapScript::default()).await;
        let s = test_state(&jmap, None).await;
        let new: organize::LabelBody = serde_json::from_value(json!({"name": "work"})).unwrap();
        organize::create_label(
            State(s.clone()),
            Extension(user()),
            Path("mbx-1".into()),
            Json(new),
        )
        .await
        .unwrap();
        let guard = s.organize_lock.lock().await;
        let s2 = s.clone();
        let mut assign = tokio::spawn(async move {
            organize::set_message_label(
                State(s2),
                Extension(user()),
                Path(("mbx-1".into(), "m1".into())),
                Json(
                    serde_json::from_value::<organize::MessageLabelBody>(
                        json!({"label": "work", "on": true}),
                    )
                    .unwrap(),
                ),
            )
            .await
        });
        assert!(
            timeout(Duration::from_millis(200), &mut assign)
                .await
                .is_err(),
            "label assign finished while the organize lock was held"
        );
        drop(guard);
        let resp = assign.await.unwrap().unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }

    /// Handler-side clamps: a huge page is capped at MAX_LIST_LIMIT and a
    /// huge search term is truncated to MAX_SEARCH_QUERY_CHARS before JMAP.
    #[tokio::test]
    async fn list_messages_clamps_limit_and_truncates_search() {
        let jmap = fake_jmap(JmapScript::default()).await;
        let s = test_state(&jmap, None).await;
        let long_q = "x".repeat(300);
        let resp = list_messages(
            State(s.clone()),
            Extension(user()),
            Path("mbx-1".into()),
            Query(Page {
                limit: Some(500),
                position: None,
                folder: None,
                folder_id: None,
                view: None,
                label: None,
                q: Some(long_q),
            }),
        )
        .await
        .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let reqs = jmap.requests.lock().unwrap();
        let query = reqs
            .iter()
            .find(|r| r["methodCalls"][0][0] == "Email/query")
            .expect("list must query JMAP");
        let args = &query["methodCalls"][0][1];
        assert_eq!(args["limit"], MAX_LIST_LIMIT);
        assert_eq!(
            args["filter"]["conditions"][1]["text"]
                .as_str()
                .unwrap()
                .len(),
            MAX_SEARCH_QUERY_CHARS
        );
    }

    /// A valid folder role the server has no folder for is a 404, not a 500.
    #[tokio::test]
    async fn list_messages_missing_folder_role_is_not_found() {
        let jmap = fake_jmap(JmapScript {
            roles: vec!["inbox", "sent", "drafts", "trash"],
            ..Default::default()
        })
        .await;
        let s = test_state(&jmap, None).await;
        let err = list_messages(
            State(s.clone()),
            Extension(user()),
            Path("mbx-1".into()),
            Query(Page {
                limit: None,
                position: None,
                folder: Some("archive".into()),
                folder_id: None,
                view: None,
                label: None,
                q: None,
            }),
        )
        .await
        .expect_err("a role the server has no folder for must be 404");
        assert_eq!(err.0, StatusCode::NOT_FOUND);
        assert_eq!(err.1, "not_found");
    }

    /// One message carrying a single zip attachment (part "2", blob "b2").
    fn message_with_zip_attachment(zip_len: u64) -> Value {
        json!({
            "id": "m1",
            "from": [{"email": "noreply-dmarc-support@google.com"}],
            "to": [{"email": "alice@example.com"}],
            "subject": "Report domain: example.com",
            "preview": "", "receivedAt": "2026-11-04T00:00:00Z",
            "keywords": {"$seen": true}, "size": 12345, "hasAttachment": true,
            "blobId": "msg-blob-1",
            "textBody": [], "htmlBody": [], "bodyValues": {},
            "bodyStructure": {"type": "multipart/mixed", "subParts": [
                {"type": "text/plain", "partId": "1", "blobId": "b1", "size": 0},
                {"type": "application/zip", "partId": "2", "blobId": "b2", "size": zip_len,
                 "name": "report.zip", "disposition": "attachment"}
            ]},
            "attachments": [{"type": "application/zip", "partId": "2", "blobId": "b2", "size": zip_len,
                "name": "report.zip", "disposition": "attachment"}]
        })
    }

    /// A zip with no entries at all (end-of-central-directory only).
    const EMPTY_ZIP: &[u8] = &[
        80, 75, 5, 6, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ];

    /// A stored zip holding one `report.xml` DMARC report.
    const XML_ZIP: &[u8] = &[
        80, 75, 3, 4, 20, 0, 0, 0, 0, 0, 178, 104, 101, 93, 183, 125, 237, 223, 87, 0, 0, 0, 87, 0,
        0, 0, 10, 0, 0, 0, 114, 101, 112, 111, 114, 116, 46, 120, 109, 108, 60, 102, 101, 101, 100,
        98, 97, 99, 107, 62, 60, 114, 101, 112, 111, 114, 116, 95, 109, 101, 116, 97, 100, 97, 116,
        97, 62, 60, 111, 114, 103, 95, 110, 97, 109, 101, 62, 103, 111, 111, 103, 108, 101, 46, 99,
        111, 109, 60, 47, 111, 114, 103, 95, 110, 97, 109, 101, 62, 60, 47, 114, 101, 112, 111,
        114, 116, 95, 109, 101, 116, 97, 100, 97, 116, 97, 62, 60, 47, 102, 101, 101, 100, 98, 97,
        99, 107, 62, 80, 75, 1, 2, 20, 3, 20, 0, 0, 0, 0, 0, 178, 104, 101, 93, 183, 125, 237, 223,
        87, 0, 0, 0, 87, 0, 0, 0, 10, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 128, 1, 0, 0, 0, 0, 114,
        101, 112, 111, 114, 116, 46, 120, 109, 108, 80, 75, 5, 6, 0, 0, 0, 0, 1, 0, 1, 0, 56, 0, 0,
        0, 127, 0, 0, 0, 0, 0,
    ];

    /// Preview path end to end: get_message -> find_attachment -> blob
    /// download -> extract. A zip with no .xml entry answers 415, so the
    /// UI offers a download instead of rendering bogus text.
    #[tokio::test]
    async fn attachment_preview_of_zip_without_xml_answers_415() {
        let jmap = fake_jmap(JmapScript {
            message: Some(message_with_zip_attachment(EMPTY_ZIP.len() as u64)),
            blob: EMPTY_ZIP.to_vec(),
            ..Default::default()
        })
        .await;
        let s = test_state(&jmap, None).await;
        let err = get_attachment_preview(
            State(s.clone()),
            Extension(user()),
            Path(("mbx-1".into(), "m1".into(), "2".into())),
        )
        .await
        .expect_err("a zip with no .xml entry must not preview");
        assert_eq!(err.0, StatusCode::UNSUPPORTED_MEDIA_TYPE);
        assert_eq!(err.1, "not_previewable");
    }

    /// Same preview path, positive control: a zip with a .xml entry
    /// returns the extracted report text.
    #[tokio::test]
    async fn attachment_preview_of_zip_with_xml_returns_text() {
        let jmap = fake_jmap(JmapScript {
            message: Some(message_with_zip_attachment(XML_ZIP.len() as u64)),
            blob: XML_ZIP.to_vec(),
            ..Default::default()
        })
        .await;
        let s = test_state(&jmap, None).await;
        let resp = get_attachment_preview(
            State(s.clone()),
            Extension(user()),
            Path(("mbx-1".into(), "m1".into(), "2".into())),
        )
        .await
        .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let v = body_json(resp).await;
        assert_eq!(v["inner_name"], "report.xml");
        assert!(v["text"].as_str().unwrap().contains("google.com"));
    }

    /// Loopback fake for Cloudflare Siteverify: answers every POST with
    /// the same canned body and counts calls. The login handler points
    /// at it via `turnstile_siteverify_url` in the test `Config`, so
    /// these tests exercise the real gate without touching Cloudflare.
    struct FakeSiteverify {
        url: String,
        calls: Arc<AtomicUsize>,
    }

    async fn fake_siteverify_multi(
        status_line: &'static str,
        response_body: &'static str,
    ) -> FakeSiteverify {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let task_calls = calls.clone();
        let response_body = response_body.to_string();
        drop(tokio::spawn(async move {
            loop {
                let Ok(Ok((mut stream, _))) =
                    timeout(Duration::from_secs(5), listener.accept()).await
                else {
                    return;
                };
                // Read one request: headers, then Content-Length body.
                let mut bytes = Vec::new();
                let mut chunk = [0u8; 4096];
                let mut expected: Option<usize> = None;
                loop {
                    let Ok(n) = stream.read(&mut chunk).await else {
                        return;
                    };
                    if n == 0 {
                        return;
                    }
                    bytes.extend_from_slice(&chunk[..n]);
                    if expected.is_none() {
                        if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                            let headers = String::from_utf8_lossy(&bytes[..end]);
                            let length = headers
                                .lines()
                                .find_map(|line| {
                                    let (name, value) = line.split_once(':')?;
                                    name.eq_ignore_ascii_case("content-length")
                                        .then(|| value.trim().parse::<usize>().ok())
                                        .flatten()
                                })
                                .unwrap_or(0);
                            expected = Some(end + 4 + length);
                        }
                    }
                    if let Some(end) = expected {
                        if bytes.len() >= end {
                            break;
                        }
                    }
                }
                task_calls.fetch_add(1, Ordering::SeqCst);
                let response = format!(
                    "HTTP/1.1 {status_line}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{response_body}",
                    response_body.len(),
                );
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            }
        }));
        FakeSiteverify {
            url: format!("http://{address}"),
            calls,
        }
    }

    /// `test_state` with the Turnstile gate switched on: secret set,
    /// hostname allowlist `mail.example.com`, Siteverify pointed at
    /// the given mock URL.
    async fn turnstile_test_state(jmap: &FakeJmap, siteverify_url: &str) -> Arc<AppState> {
        let mut s = test_state(jmap, None).await;
        let state = Arc::get_mut(&mut s).expect("test_state is the only owner");
        state.config.turnstile_secret_key = "test-secret".into();
        state.config.turnstile_hostnames = vec!["mail.example.com".into()];
        state.config.turnstile_siteverify_url = siteverify_url.into();
        s
    }

    /// One login attempt against the handler, from 127.0.0.1.
    async fn login_attempt(s: &Arc<AppState>, token: Option<&str>) -> Result<Response, ApiError> {
        login(
            State(s.clone()),
            axum::extract::ConnectInfo(std::net::SocketAddr::from(([127, 0, 0, 1], 12345))),
            HeaderMap::new(),
            Json(LoginBody {
                email: "alice@example.com".into(),
                password: "secret".into(),
                turnstile_token: token.map(str::to_string),
            }),
        )
        .await
    }

    fn assert_api_error<T: std::fmt::Debug>(
        result: Result<T, ApiError>,
        status: StatusCode,
        code: &str,
    ) {
        let err = result.expect_err("expected an ApiError");
        assert_eq!(err.0, status);
        assert_eq!(err.1, code);
    }

    /// No JMAP request may happen on a gate failure: the login must
    /// die at the Turnstile check, before any mail-server contact.
    fn assert_jmap_untouched(jmap: &FakeJmap) {
        assert_eq!(
            jmap.session_calls.load(Ordering::SeqCst),
            0,
            "gate failure must not reach JMAP (GET /jmap/session)"
        );
        assert!(
            jmap.requests.lock().unwrap().is_empty(),
            "gate failure must not reach JMAP (POST /jmap/)"
        );
    }

    #[tokio::test]
    async fn login_gate_missing_token_returns_400_and_never_touches_jmap() {
        let siteverify = fake_siteverify_multi(
            "200 OK",
            r#"{"success":true,"action":"login","hostname":"mail.example.com"}"#,
        )
        .await;
        let jmap = fake_jmap(JmapScript::default()).await;
        let s = turnstile_test_state(&jmap, &siteverify.url).await;
        assert_api_error(
            login_attempt(&s, None).await,
            StatusCode::BAD_REQUEST,
            "turnstile_failed",
        );
        assert_eq!(siteverify.calls.load(Ordering::SeqCst), 0);
        assert_jmap_untouched(&jmap);
    }

    #[tokio::test]
    async fn login_gate_invalid_token_returns_400_and_never_touches_jmap() {
        let siteverify = fake_siteverify_multi(
            "200 OK",
            r#"{"success":false,"error-codes":["timeout-or-duplicate"]}"#,
        )
        .await;
        let jmap = fake_jmap(JmapScript::default()).await;
        let s = turnstile_test_state(&jmap, &siteverify.url).await;
        assert_api_error(
            login_attempt(&s, Some("stale-token")).await,
            StatusCode::BAD_REQUEST,
            "turnstile_failed",
        );
        assert_eq!(siteverify.calls.load(Ordering::SeqCst), 1);
        assert_jmap_untouched(&jmap);
    }

    #[tokio::test]
    async fn login_gate_unavailable_returns_503_and_never_touches_jmap() {
        let siteverify =
            fake_siteverify_multi("500 Internal Server Error", r#"{"success":false}"#).await;
        let jmap = fake_jmap(JmapScript::default()).await;
        let s = turnstile_test_state(&jmap, &siteverify.url).await;
        assert_api_error(
            login_attempt(&s, Some("token")).await,
            StatusCode::SERVICE_UNAVAILABLE,
            "turnstile_unavailable",
        );
        assert_eq!(siteverify.calls.load(Ordering::SeqCst), 1);
        assert_jmap_untouched(&jmap);
    }

    #[tokio::test]
    async fn login_gate_server_side_error_returns_503_and_never_touches_jmap() {
        let siteverify = fake_siteverify_multi(
            "200 OK",
            r#"{"success":false,"error-codes":["invalid-input-secret"]}"#,
        )
        .await;
        let jmap = fake_jmap(JmapScript::default()).await;
        let s = turnstile_test_state(&jmap, &siteverify.url).await;
        assert_api_error(
            login_attempt(&s, Some("token")).await,
            StatusCode::SERVICE_UNAVAILABLE,
            "turnstile_unavailable",
        );
        assert_jmap_untouched(&jmap);
    }

    #[tokio::test]
    async fn login_gate_valid_token_reaches_jmap() {
        // Positive control: a good token passes the gate, so the
        // handler contacts JMAP. The fake 404s /jmap/session, which
        // surfaces as 502 — the point is JMAP *was* reached.
        let siteverify = fake_siteverify_multi(
            "200 OK",
            r#"{"success":true,"action":"login","hostname":"mail.example.com"}"#,
        )
        .await;
        let jmap = fake_jmap(JmapScript::default()).await;
        let s = turnstile_test_state(&jmap, &siteverify.url).await;
        assert_api_error(
            login_attempt(&s, Some("good-token")).await,
            StatusCode::BAD_GATEWAY,
            "mail_server_unavailable",
        );
        assert_eq!(siteverify.calls.load(Ordering::SeqCst), 1);
        assert_eq!(jmap.session_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn login_gate_per_ip_attempt_limit_returns_429() {
        let siteverify = fake_siteverify_multi(
            "200 OK",
            r#"{"success":false,"error-codes":["timeout-or-duplicate"]}"#,
        )
        .await;
        let jmap = fake_jmap(JmapScript::default()).await;
        let s = turnstile_test_state(&jmap, &siteverify.url).await;
        for _ in 0..MAX_TURNSTILE_ATTEMPTS_PER_IP {
            assert_api_error(
                login_attempt(&s, Some("bad-token")).await,
                StatusCode::BAD_REQUEST,
                "turnstile_failed",
            );
        }
        // The next attempt is capped before Siteverify is called.
        assert_api_error(
            login_attempt(&s, Some("bad-token")).await,
            StatusCode::TOO_MANY_REQUESTS,
            "too_many_attempts",
        );
        assert_eq!(
            siteverify.calls.load(Ordering::SeqCst),
            MAX_TURNSTILE_ATTEMPTS_PER_IP as usize
        );
        assert_jmap_untouched(&jmap);
    }
}
