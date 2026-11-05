use axum::{
    extract::{DefaultBodyLimit, Path, Query, Request, State},
    http::{header, HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Extension, Json, Router,
};
use iris_auth as auth;
use iris_config::limits::{
    DEFAULT_LIST_LIMIT, MAX_ATTACHMENT_BYTES, MAX_FAILED_LOGINS, MAX_FAILED_LOGINS_PER_IP,
    MAX_HTML_BODY_BYTES, MAX_LIST_LIMIT, MAX_PREVIEW_BYTES, MAX_RAW_BYTES, MAX_READS_PER_WINDOW,
    MAX_SEARCH_QUERY_CHARS, MAX_TIMELESS_RESET_BODY_BYTES, MAX_TRANSACTIONAL_EMAIL_BODY_BYTES,
    READ_WINDOW_MINUTES,
};
use iris_config::Config;
use iris_mail::{
    extract_text_attachment, Attachment, Compose, JmapClient, ListPage, MailError, MessageDetail,
    ResendClient,
};
use serde::Deserialize;
use serde_json::json;
use sqlx::SqlitePool;
use std::sync::Arc;

mod cleanup;
mod compose;
mod email_address;
mod html_body;
mod integration_apps;
mod integration_keys;
mod temp;
mod transactional_email;

pub(crate) struct AppState {
    pool: SqlitePool,
    config: Config,
    jmap: JmapClient,
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
        html_bodies: html_body::HtmlBodyStore::new(),
        jmap: JmapClient::new(config.stalwart_api_url.clone()),
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
        .route("/auth/logout", post(logout))
        .route("/api/v1/mailboxes", get(mailboxes))
        .route("/api/v1/mailboxes/:mailbox_id/messages", get(list_messages))
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
            get(compose::folders),
        )
        .route(
            "/api/v1/mailboxes/:mailbox_id/messages/:message_id/move",
            post(compose::move_message),
        )
        .route(
            "/api/v1/mailboxes/:mailbox_id/messages/:message_id/seen",
            post(compose::set_seen),
        )
        .route(
            "/api/v1/mailboxes/:mailbox_id/drafts",
            post(compose::save_draft),
        )
        .route("/api/v1/mailboxes/:mailbox_id/send", post(compose::send))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_session,
        ));

    let app = Router::new()
        .route("/health", get(|| async { "OK" }))
        .route("/ready", get(ready))
        .route("/auth/csrf", get(csrf))
        .route("/auth/login", post(login))
        .route(
            "/internal/v1/timeless-password-reset",
            post(timeless_password_reset)
                .layer(DefaultBodyLimit::max(MAX_TIMELESS_RESET_BODY_BYTES)),
        )
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TimelessPasswordResetBody {
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
    s.config.iris_timeless_service_token.len() >= 32
        && valid_email(&s.config.iris_timeless_email_from)
        && valid_email(&s.config.iris_timeless_email_to)
        && valid_dns_name(&s.config.timeless_public_origin)
        && s.relay.is_some()
}

async fn timeless_password_reset(
    State(s): S,
    headers: HeaderMap,
    Json(body): Json<TimelessPasswordResetBody>,
) -> ApiResult {
    let supplied = headers
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    if !supplied.is_some_and(|value| auth::ct_eq(&s.config.iris_timeless_service_token, value)) {
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
        s.config.timeless_public_origin, body.token
    );
    let message = Compose {
        from: s.config.iris_timeless_email_from.clone(),
        to: vec![s.config.iris_timeless_email_to.clone()],
        subject: "Reset your Timeless editor password".to_string(),
        text: format!("A password reset was requested for the Timeless editor account.\n\nUse this one-time link within 15 minutes:\n{link}\n\nIf you did not request this, ignore this message."),
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
        tracing::warn!("Timeless password reset email relay failed; details redacted");
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

async fn session(Extension(u): Extension<CurrentUser>) -> Json<serde_json::Value> {
    Json(json!({ "authenticated": true, "email": u.email }))
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
    folder: Option<String>,
    q: Option<String>,
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
    let folder = compose::folder_role(p.folder.as_deref().unwrap_or("inbox"))?;
    // Search terms are capped so a huge `q` is never forwarded verbatim to JMAP.
    let q =
        p.q.as_deref()
            .map(str::trim)
            .filter(|q| !q.is_empty())
            .map(|q| q.chars().take(MAX_SEARCH_QUERY_CHARS).collect::<String>());
    let (messages, total) = s
        .jmap
        .list_folder(
            &u.creds.user,
            &u.creds.pass,
            &account,
            folder,
            ListPage {
                limit,
                position,
                query: q.as_deref(),
            },
        )
        .await?;
    Ok(Json(json!({ "messages": messages, "total": total, "position": position })).into_response())
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

fn sanitized_filename(name: &str) -> String {
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
mod timeless_password_reset_tests {
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
            jmap: JmapClient::new("http://127.0.0.1:9".into()),
            relay,
            html_bodies: html_body::HtmlBodyStore::new(),
            config: Config {
                env: "test".into(),
                host: "127.0.0.1".into(),
                port: 8000,
                database_url: "sqlite::memory:".into(),
                session_secret: "test-secret".into(),
                stalwart_api_url: "http://127.0.0.1:9".into(),
                temp_inbox_domain: "example.test".into(),
                temp_inbox_default_expiry_hours: 1,
                temp_inbox_mail_user: String::new(),
                temp_inbox_mail_password: String::new(),
                send_local_domains: vec![],
                send_external: false,
                resend_api_key: "test-resend-key".into(),
                integration_email_from: "no-reply@meowlabs.id".into(),
                integration_email_enabled: true,
                iris_timeless_service_token: service_key.into(),
                iris_timeless_email_from: sender.into(),
                iris_timeless_email_to: recipient.into(),
                timeless_public_origin: origin.into(),
                resend_api_url: String::new(),
                mail_content_origin: String::new(),
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
            "no-reply@meowlabs.id",
            "recovery@example.test",
            "timeless.meowlabs.id",
            Some(ResendClient::with_endpoint(
                "test-key".into(),
                relay.endpoint.clone(),
            )),
        )
        .await;
        let missing = timeless_password_reset(
            State(app.clone()),
            HeaderMap::new(),
            Json(TimelessPasswordResetBody {
                token: GOOD_TOKEN.into(),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(missing.0, StatusCode::UNAUTHORIZED);
        let wrong = timeless_password_reset(
            State(app),
            auth_headers("wrong-service-key"),
            Json(TimelessPasswordResetBody {
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
            "no-reply@meowlabs.id",
            "recovery@example.test",
            "timeless.meowlabs.id",
            Some(ResendClient::with_endpoint(
                "test-key".into(),
                relay.endpoint.clone(),
            )),
        )
        .await;
        let result = timeless_password_reset(
            State(app),
            auth_headers(SERVICE_KEY),
            Json(TimelessPasswordResetBody {
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
        let unauthenticated = timeless_password_reset(
            State(app.clone()),
            HeaderMap::new(),
            Json(TimelessPasswordResetBody {
                token: GOOD_TOKEN.into(),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(unauthenticated.0, StatusCode::UNAUTHORIZED);
        let authenticated = timeless_password_reset(
            State(app),
            auth_headers(SERVICE_KEY),
            Json(TimelessPasswordResetBody {
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
            "admin@meowlabs.id",
            "",
            "timeless.meowlabs.id",
            Some(ResendClient::with_endpoint(
                "test-key".into(),
                relay.endpoint.clone(),
            )),
        )
        .await;
        let result = timeless_password_reset(
            State(app),
            auth_headers(SERVICE_KEY),
            Json(TimelessPasswordResetBody {
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
            "admin@meowlabs.id",
            "recovery@example.test",
            "timeless.meowlabs.id",
            Some(ResendClient::with_endpoint(
                "test-key".into(),
                relay.endpoint.clone(),
            )),
        )
        .await;
        let response = timeless_password_reset(
            State(app),
            auth_headers(SERVICE_KEY),
            Json(TimelessPasswordResetBody {
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
        assert_eq!(payload["from"], "admin@meowlabs.id");
        assert_eq!(payload["to"], json!(["recovery@example.test"]));
        assert_eq!(payload["subject"], "Reset your Timeless editor password");
        let text = payload["text"].as_str().unwrap();
        assert!(text.contains(&format!(
            "https://timeless.meowlabs.id/admin/reset#token={GOOD_TOKEN}"
        )));
        assert!(!text.contains("?token="));
        relay.task.abort();
    }

    #[tokio::test]
    async fn transactional_email_is_fail_closed_when_delivery_is_disabled() {
        let mut relay = fake_relay().await;
        let mut app = state(
            SERVICE_KEY,
            "no-reply@meowlabs.id",
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
            .bind("gencv")
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
                action_url: "https://gencv.meowlabs.id/verify?token=opaque".into(),
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
            "no-reply@meowlabs.id",
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
            .bind("gencv")
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
            action_url: "https://gencv.meowlabs.id/verify?token=opaque".into(),
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
        assert_eq!(payload["from"], "no-reply@meowlabs.id");
        assert_eq!(payload["to"], json!(["user@example.test"]));
        assert_eq!(payload["subject"], "Verify your GenCV email");
        assert!(payload["text"]
            .as_str()
            .unwrap()
            .contains("https://gencv.meowlabs.id/verify?token=opaque"));

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
                action_url: "https://gencv.meowlabs.id/verify?token=opaque".into(),
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
            "no-reply@meowlabs.id",
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
            .bind("gencv")
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
            action_url: "https://gencv.meowlabs.id/verify?token=opaque".into(),
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
            "no-reply@meowlabs.id",
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
            .bind("gencv")
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
            action_url: "https://gencv.meowlabs.id/verify?token=abc".into(),
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
            "no-reply@meowlabs.id",
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
            .bind("gencv")
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
                action_url: "https://gencv.meowlabs.id/verify?token=abc".into(),
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
        sqlx::query("INSERT INTO integration_keys (key_id,app_id,token_hash,scopes_json,created_at) VALUES ('test-key','gencv',?,'[\\\"email:template:auth.verify\\\"]',?)")
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
            jmap: JmapClient::new("http://127.0.0.1:9".into()),
            relay: None,
            html_bodies: html_body::HtmlBodyStore::new(),
            config: Config {
                env: "test".into(),
                host: "127.0.0.1".into(),
                port: 8000,
                database_url: "sqlite::memory:".into(),
                session_secret: "test-secret".into(),
                stalwart_api_url: "http://127.0.0.1:9".into(),
                temp_inbox_domain: "example.test".into(),
                temp_inbox_default_expiry_hours: 1,
                temp_inbox_mail_user: String::new(),
                temp_inbox_mail_password: String::new(),
                send_local_domains: vec![],
                send_external: false,
                resend_api_key: String::new(),
                integration_email_from: "no-reply@meowlabs.id".into(),
                integration_email_enabled: false,
                iris_timeless_service_token: String::new(),
                iris_timeless_email_from: String::new(),
                iris_timeless_email_to: String::new(),
                timeless_public_origin: String::new(),
                resend_api_url: String::new(),
                mail_content_origin: String::new(),
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
        /// Email/get answer for get_message (None = empty list -> NotFound).
        message: Option<Value>,
        /// Bytes served by GET /jmap/download/... for any blob id.
        blob: Vec<u8>,
    }

    impl Default for JmapScript {
        fn default() -> Self {
            Self {
                roles: vec!["inbox", "sent", "drafts", "archive", "trash"],
                email_set: EmailSet::Created,
                query_total: 0,
                query_list: vec![],
                message: None,
                blob: vec![],
            }
        }
    }

    struct FakeJmap {
        base_url: String,
        relay_url: String,
        /// Parsed JSON bodies of every POST /jmap/ request, in order.
        requests: Arc<Mutex<Vec<Value>>>,
        relay_calls: Arc<AtomicUsize>,
    }

    fn folder_json(role: &str) -> Value {
        json!({"id": format!("f-{role}"), "role": role, "name": role, "totalEmails": 0, "unreadEmails": 0})
    }

    /// One JMAP `methodResponses` body for the request the client posted.
    fn jmap_response(script: &JmapScript, body: &Value) -> String {
        let calls = body["methodCalls"].as_array().cloned().unwrap_or_default();
        let first = calls
            .first()
            .and_then(|c| c[0].as_str())
            .unwrap_or_default()
            .to_string();
        let responses: Vec<Value> = match first.as_str() {
            "Mailbox/get" => vec![
                json!(["Mailbox/get", {"list": script.roles.iter().map(|r| folder_json(r)).collect::<Vec<_>>()}, "m"]),
            ],
            "Mailbox/set" => {
                vec![json!(["Mailbox/set", {"created": {}, "notCreated": {}}, "s"])]
            }
            "Identity/get" => vec![
                json!(["Identity/get", {"list": [{"id": "ident-1", "email": "alice@meowlabs.id"}]}, "i"]),
            ],
            "Identity/set" => vec![
                json!(["Identity/set", {"created": {"i": {"id": "ident-1"}}, "notCreated": {}}, "s"]),
            ],
            "Email/query" => vec![
                json!(["Email/query", {"total": script.query_total, "ids": script.query_list.iter().map(|m| m["id"].clone()).collect::<Vec<_>>()}, "q"]),
                json!(["Email/get", {"list": script.query_list}, "g"]),
            ],
            "Email/get" => {
                let list = script.message.clone().map(|m| vec![m]).unwrap_or_default();
                vec![json!(["Email/get", {"list": list}, "g"])]
            }
            // jmap.send submits Email/set + EmailSubmission/set in one batch.
            "Email/set" if calls.len() == 2 => vec![
                json!(["Email/set", {"created": {"n": {"id": "msg-1"}}, "notCreated": {}}, "e"]),
                json!(["EmailSubmission/set", {"created": {"s": {"id": "sub-1"}}, "notCreated": {}}, "x"]),
            ],
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
        let task_requests = requests.clone();
        let task_relay = relay_calls.clone();
        drop(tokio::spawn(async move {
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
                        (
                            "200 OK",
                            "application/json",
                            jmap_response(&script, &v).into_bytes(),
                        )
                    } else if method == "POST" && path == "/emails" {
                        task_relay.fetch_add(1, Ordering::SeqCst);
                        (
                            "200 OK",
                            "application/json",
                            br#"{"id":"re_mock_1"}"#.to_vec(),
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
        }
    }

    async fn test_state(jmap: &FakeJmap, relay: Option<ResendClient>) -> Arc<AppState> {
        let pool = iris_database::init_pool("sqlite::memory:").await.unwrap();
        sqlx::query("INSERT INTO app_users (id,email,display_name,created_at,updated_at) VALUES ('u-alice','alice@meowlabs.id','Alice','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO permanent_mailboxes (id,user_id,email_address,mail_server_account_id,status,created_at,updated_at) VALUES ('mbx-1','u-alice','alice@meowlabs.id','acct-1','active','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')")
            .execute(&pool)
            .await
            .unwrap();
        Arc::new(AppState {
            pool,
            jmap: JmapClient::new(jmap.base_url.clone()),
            relay,
            html_bodies: html_body::HtmlBodyStore::new(),
            config: Config {
                env: "test".into(),
                host: "127.0.0.1".into(),
                port: 8000,
                database_url: "sqlite::memory:".into(),
                session_secret: "test-secret".into(),
                stalwart_api_url: "http://127.0.0.1:9".into(),
                temp_inbox_domain: "example.test".into(),
                temp_inbox_default_expiry_hours: 1,
                temp_inbox_mail_user: String::new(),
                temp_inbox_mail_password: String::new(),
                send_local_domains: vec!["meowlabs.id".into()],
                send_external: false,
                resend_api_key: "test-resend-key".into(),
                integration_email_from: "no-reply@meowlabs.id".into(),
                integration_email_enabled: false,
                iris_timeless_service_token: String::new(),
                iris_timeless_email_from: String::new(),
                iris_timeless_email_to: String::new(),
                timeless_public_origin: String::new(),
                resend_api_url: String::new(),
                mail_content_origin: String::new(),
            },
        })
    }

    fn user() -> CurrentUser {
        CurrentUser {
            user_id: "u-alice".into(),
            email: "alice@meowlabs.id".into(),
            creds: Creds {
                user: "alice@meowlabs.id".into(),
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
        json!({"id": id, "from": [{"email": "a@example.com"}], "to": [{"email": "alice@meowlabs.id"}], "subject": subject, "preview": "p", "receivedAt": "2026-11-04T00:00:00Z", "keywords": {"$seen": true}, "size": 100, "hasAttachment": false})
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
            compose_body("bob@example.com"),
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
            compose_body("bob@example.com"),
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
            compose_body("alice@meowlabs.id"),
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
            "to": [{"email": "alice@meowlabs.id"}],
            "subject": "Report domain: meowlabs.id",
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
}
