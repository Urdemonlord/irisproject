use axum::{
    extract::{DefaultBodyLimit, Path, Query, Request, State},
    http::{header, HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Extension, Json, Router,
};
use iris_auth as auth;
use iris_config::Config;
use iris_mail::{
    extract_text_attachment, Attachment, Compose, JmapClient, MailError, MessageDetail,
    ResendClient, MAX_ATTACHMENT_BYTES, MAX_PREVIEW_BYTES, MAX_RAW_BYTES,
};
use serde::Deserialize;
use serde_json::json;
use sqlx::SqlitePool;
use std::sync::Arc;

mod cleanup;
mod compose;
mod integration_keys;
mod temp;
mod transactional_email;

pub(crate) struct AppState {
    pool: SqlitePool,
    config: Config,
    jmap: JmapClient,
    /// Outbound relay for other domains (None = only local domains can be reached).
    relay: Option<ResendClient>,
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
        ApiError(StatusCode::INTERNAL_SERVER_ERROR, "internal_error")
    }
}
impl From<MailError> for ApiError {
    fn from(e: MailError) -> Self {
        tracing::warn!("mail: {e}");
        match e {
            MailError::Auth => ApiError(StatusCode::UNAUTHORIZED, "unauthorized"),
            MailError::NotFound => ApiError(StatusCode::NOT_FOUND, "not_found"),
            MailError::Rejected(_) => ApiError(StatusCode::UNPROCESSABLE_ENTITY, "send_rejected"),
            _ => ApiError(StatusCode::BAD_GATEWAY, "mail_server_unavailable"),
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
    if args.next().as_deref() == Some("integration-key") {
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
        jmap: JmapClient::new(config.stalwart_api_url.clone()),
        relay: (!config.resend_api_key.is_empty()).then(|| {
            ResendClient::with_endpoint(
                config.resend_api_key.clone(),
                config.resend_api_url.clone(),
            )
        }),
        config: config.clone(),
    });
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
            post(timeless_password_reset).layer(DefaultBodyLimit::max(1024)),
        )
        .route(
            "/api/v1/integrations/email",
            post(transactional_email::send).layer(DefaultBodyLimit::max(8192)),
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
    let relay = s.relay.as_ref().expect("reset configuration checks relay");
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

const MAX_FAILED_LOGINS: i64 = 10; // per email per 15 minutes
const MAX_FAILED_LOGINS_PER_IP: i64 = 50; // per client IP per 15 minutes (tolerant of shared NAT)

/// Peer IP; trust X-Real-IP only when the peer is the local reverse proxy.
fn client_ip(peer: std::net::SocketAddr, headers: &HeaderMap) -> String {
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
    h.append(
        header::SET_COOKIE,
        auth::set_cookie(&token, secure).parse().unwrap(),
    );
    h.append(
        header::SET_COOKIE,
        auth::set_csrf_cookie(&auth::new_token(), secure)
            .parse()
            .unwrap(),
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

#[derive(Deserialize)]
struct Page {
    limit: Option<u32>,
    position: Option<u32>,
    folder: Option<String>,
}

async fn list_messages(
    State(s): S,
    Extension(u): Extension<CurrentUser>,
    Path(mailbox_id): Path<String>,
    Query(p): Query<Page>,
) -> ApiResult {
    let account = owned_account(&s, &u, &mailbox_id).await?;
    let limit = p.limit.unwrap_or(50).clamp(1, 100);
    let position = p.position.unwrap_or(0);
    let folder = compose::folder_role(p.folder.as_deref().unwrap_or("inbox"))?;
    let (messages, total) = s
        .jmap
        .list_folder(
            &u.creds.user,
            &u.creds.pass,
            &account,
            folder,
            limit,
            position,
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
    let m = s
        .jmap
        .get_message(&u.creds.user, &u.creds.pass, &account, &message_id)
        .await?;
    raw_response(&s, &u.creds.user, &u.creds.pass, &account, &m).await
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
