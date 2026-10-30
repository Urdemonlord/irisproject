use axum::{
    extract::{Path, Query, Request, State},
    http::{header, HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Extension, Json, Router,
};
use iris_auth as auth;
use iris_config::Config;
use iris_mail::{JmapClient, MailError};
use serde::Deserialize;
use serde_json::json;
use sqlx::SqlitePool;
use std::sync::Arc;

mod cleanup;
mod compose;
mod temp;

pub(crate) struct AppState {
    pool: SqlitePool,
    config: Config,
    jmap: JmapClient,
}

#[derive(Clone)]
pub(crate) struct Creds { pub(crate) user: String, pub(crate) pass: String }

#[derive(Clone)]
pub(crate) struct CurrentUser { pub(crate) user_id: String, pub(crate) email: String, pub(crate) creds: Creds }

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
    if config.env == "production" && (config.session_secret.len() < 32 || config.session_secret.starts_with("dev-secret")) {
        panic!("SESSION_SECRET must be a random value of at least 32 chars in production");
    }
    let pool = iris_database::init_pool(&config.database_url).await.expect("database");
    let state = Arc::new(AppState {
        pool,
        jmap: JmapClient::new(config.stalwart_api_url.clone()),
        config: config.clone(),
    });
    tokio::spawn(cleanup::run(state.clone()));

    let protected = Router::new()
        .route("/api/v1/session", get(session))
        .route("/auth/logout", post(logout))
        .route("/api/v1/mailboxes", get(mailboxes))
        .route("/api/v1/mailboxes/:mailbox_id/messages", get(list_messages))
        .route("/api/v1/mailboxes/:mailbox_id/messages/:message_id", get(get_message).delete(compose::delete_message))
        .route("/api/v1/mailboxes/:mailbox_id/folders", get(compose::folders))
        .route("/api/v1/mailboxes/:mailbox_id/messages/:message_id/move", post(compose::move_message))
        .route("/api/v1/mailboxes/:mailbox_id/messages/:message_id/seen", post(compose::set_seen))
        .route("/api/v1/mailboxes/:mailbox_id/drafts", post(compose::save_draft))
        .route("/api/v1/mailboxes/:mailbox_id/send", post(compose::send))
        .route_layer(middleware::from_fn_with_state(state.clone(), require_session));

    let app = Router::new()
        .route("/health", get(|| async { "OK" }))
        .route("/ready", get(ready))
        .route("/auth/csrf", get(csrf))
        .route("/auth/login", post(login))
        .route("/api/v1/temp-inboxes", post(temp::create))
        .route("/api/v1/temp-inboxes/:id", get(temp::status))
        .route("/api/v1/temp-inboxes/:id/messages", get(temp::messages))
        .route("/api/v1/temp-inboxes/:id/messages/:message_id", get(temp::message))
        .merge(protected)
        .layer(middleware::from_fn_with_state(state.clone(), require_csrf))
        .with_state(state);

    let addr = format!("{}:{}", config.host, config.port);
    let listener = tokio::net::TcpListener::bind(&addr).await.expect("bind");
    tracing::info!("IRIS API listening on http://{addr}");
    axum::serve(listener, app.into_make_service_with_connect_info::<std::net::SocketAddr>()).await.expect("server");
}

async fn ready(State(s): S) -> Json<serde_json::Value> {
    Json(json!({ "ready": iris_database::health_check(&s.pool).await.is_ok() }))
}

fn secure(s: &AppState) -> bool {
    s.config.env == "production"
}

/// Issues (or re-issues) the double-submit CSRF cookie; the SPA calls this before its first write.
async fn csrf(State(s): S, headers: HeaderMap) -> Response {
    let existing = headers.get(header::COOKIE).and_then(|h| h.to_str().ok())
        .and_then(|c| auth::cookie(c, auth::CSRF_COOKIE)).filter(|t| t.len() == 64).map(str::to_string);
    let token = existing.unwrap_or_else(auth::new_token);
    ([(header::SET_COOKIE, auth::set_csrf_cookie(&token, secure(&s)))], Json(json!({ "csrf_token": token }))).into_response()
}

/// Unsafe methods need a matching CSRF header+cookie, unless authenticated by a bearer
/// capability token (temp inboxes), which a cross-site form cannot attach.
async fn require_csrf(headers: HeaderMap, req: Request, next: Next) -> Response {
    let m = req.method();
    let unsafe_method = !(m == axum::http::Method::GET || m == axum::http::Method::HEAD || m == axum::http::Method::OPTIONS);
    let bearer = headers.get(header::AUTHORIZATION).and_then(|h| h.to_str().ok()).is_some_and(|h| h.starts_with("Bearer "));
    let cookies = headers.get(header::COOKIE).and_then(|h| h.to_str().ok());
    let sent = headers.get(auth::CSRF_HEADER).and_then(|h| h.to_str().ok());
    if unsafe_method && !bearer && !auth::csrf_ok(cookies, sent) {
        return ApiError(StatusCode::FORBIDDEN, "csrf_failed").into_response();
    }
    next.run(req).await
}

#[derive(Deserialize)]
struct LoginBody { email: String, password: String }

const MAX_FAILED_LOGINS: i64 = 10; // per email per 15 minutes

async fn login(State(s): S, headers: HeaderMap, Json(b): Json<LoginBody>) -> ApiResult {
    let email = b.email.trim().to_lowercase();
    if email.is_empty() || !email.contains('@') || b.password.is_empty() || email.len() > 254 {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_request"));
    }
    let subject = auth::hash_token(&format!("login:{email}"));
    let since = (chrono::Utc::now() - chrono::Duration::minutes(15)).to_rfc3339();
    let failures: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM rate_limit_events WHERE subject_hash=? AND action='login' AND outcome='blocked' AND occurred_at>?",
    ).bind(&subject).bind(&since).fetch_one(&s.pool).await?;
    if failures >= MAX_FAILED_LOGINS {
        return Err(ApiError(StatusCode::TOO_MANY_REQUESTS, "too_many_attempts"));
    }

    let account_id = match s.jmap.login(&email, &b.password).await {
        Ok(a) => a,
        Err(MailError::Auth) => {
            sqlx::query("INSERT INTO rate_limit_events (id,subject_hash,action,occurred_at,outcome) VALUES (?,?,'login',?,'blocked')")
                .bind(uuid::Uuid::new_v4().to_string()).bind(&subject).bind(auth::now())
                .execute(&s.pool).await?;
            return Err(ApiError(StatusCode::UNAUTHORIZED, "invalid_credentials"));
        }
        Err(e) => return Err(e.into()),
    };

    // Per-session app password: the user's real password is used once here and never kept.
    let session_expiry = auth::expiry_at();
    let (cred_id, app_secret) = s.jmap
        .create_app_password(&email, &b.password, "IRIS web session", &session_expiry.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .await?;

    let token = auth::new_token();
    if let Err(e) = persist_login(&s, &email, &account_id, &token, &session_expiry, &app_secret, &cred_id).await {
        // Don't leak a live app password when the session row never got written.
        let _ = s.jmap.destroy_app_password(&email, &app_secret, &cred_id).await;
        return Err(e);
    }

    // Session rotation: any session this browser already held is revoked, never reused.
    if let Some(old) = headers.get(header::COOKIE).and_then(|h| h.to_str().ok()).and_then(auth::token_from_cookie) {
        revoke_by_token(&s, old).await;
    }

    let secure = secure(&s);
    let mut resp = Json(json!({ "authenticated": true, "email": email })).into_response();
    let h = resp.headers_mut();
    h.append(header::SET_COOKIE, auth::set_cookie(&token, secure).parse().unwrap());
    h.append(header::SET_COOKIE, auth::set_csrf_cookie(&auth::new_token(), secure).parse().unwrap());
    Ok(resp)
}

async fn persist_login(
    s: &AppState, email: &str, account_id: &str, token: &str, session_expiry: &chrono::DateTime<chrono::Utc>, app_secret: &str, cred_id: &str,
) -> Result<(), ApiError> {
    let now = auth::now();
    let mut tx = s.pool.begin().await?;
    sqlx::query("INSERT INTO app_users (id,email,display_name,created_at,updated_at) VALUES (?,?,?,?,?) ON CONFLICT(email) DO UPDATE SET updated_at=excluded.updated_at")
        .bind(uuid::Uuid::new_v4().to_string()).bind(email).bind(email).bind(&now).bind(&now)
        .execute(&mut *tx).await?;
    let (user_id, status): (String, String) = sqlx::query_as("SELECT id,status FROM app_users WHERE email=?")
        .bind(email).fetch_one(&mut *tx).await?;
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
    let Some((id, email, sealed, cred_id)) = row else { return };
    let _ = sqlx::query("UPDATE auth_sessions SET revoked_at=?, mail_credential_sealed=NULL WHERE id=?")
        .bind(auth::now()).bind(&id).execute(&s.pool).await;
    if let (Some(pass), Some(cred_id)) = (sealed.and_then(|x| auth::open(&s.config.session_secret, token, &x)), cred_id) {
        if let Err(e) = s.jmap.destroy_app_password(&email, &pass, &cred_id).await {
            tracing::warn!("app password revoke failed (expires on its own): {e}");
        }
    }
}

async fn require_session(State(s): S, headers: HeaderMap, mut req: Request, next: Next) -> Response {
    let unauthorized = || {
        ([(header::SET_COOKIE, auth::clear_cookie())], ApiError(StatusCode::UNAUTHORIZED, "unauthorized")).into_response()
    };
    let Some(token) = headers.get(header::COOKIE).and_then(|h| h.to_str().ok()).and_then(auth::token_from_cookie) else {
        return unauthorized();
    };
    let row: Option<(String, String, String, Option<String>)> = match sqlx::query_as(
        "SELECT s.id, s.user_id, u.email, s.mail_credential_sealed FROM auth_sessions s JOIN app_users u ON u.id=s.user_id
         WHERE s.token_hash=? AND s.revoked_at IS NULL AND s.expires_at>? AND u.status='active'",
    ).bind(auth::hash_token(token)).bind(auth::now()).fetch_optional(&s.pool).await {
        Ok(r) => r,
        Err(e) => return ApiError::from(e).into_response(),
    };
    let Some((session_id, user_id, email, sealed)) = row else { return unauthorized() };
    let Some(pass) = sealed.and_then(|x| auth::open(&s.config.session_secret, token, &x)) else { return unauthorized() };
    let creds = Creds { user: email.clone(), pass };
    let _ = sqlx::query("UPDATE auth_sessions SET last_seen_at=? WHERE id=?")
        .bind(auth::now()).bind(&session_id).execute(&s.pool).await;
    req.extensions_mut().insert(CurrentUser { user_id, email, creds });
    next.run(req).await
}

async fn session(Extension(u): Extension<CurrentUser>) -> Json<serde_json::Value> {
    Json(json!({ "authenticated": true, "email": u.email }))
}

async fn logout(State(s): S, headers: HeaderMap) -> ApiResult {
    if let Some(token) = headers.get(header::COOKIE).and_then(|h| h.to_str().ok()).and_then(auth::token_from_cookie) {
        revoke_by_token(&s, token).await;
    }
    Ok(([(header::SET_COOKIE, auth::clear_cookie())], StatusCode::NO_CONTENT).into_response())
}

async fn mailboxes(State(s): S, Extension(u): Extension<CurrentUser>) -> ApiResult {
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT id,email_address,status FROM permanent_mailboxes WHERE user_id=? ORDER BY created_at",
    ).bind(&u.user_id).fetch_all(&s.pool).await?;
    let list: Vec<_> = rows.into_iter().map(|(id, email, status)| json!({"id": id, "email_address": email, "status": status})).collect();
    Ok(Json(json!({ "mailboxes": list })).into_response())
}

/// Ownership check: returns the mail-server account id only if the mailbox belongs to the user.
pub(crate) async fn owned_account(s: &AppState, u: &CurrentUser, mailbox_id: &str) -> Result<String, ApiError> {
    sqlx::query_scalar("SELECT mail_server_account_id FROM permanent_mailboxes WHERE id=? AND user_id=? AND status='active'")
        .bind(mailbox_id).bind(&u.user_id).fetch_optional(&s.pool).await?
        .ok_or(ApiError(StatusCode::NOT_FOUND, "not_found"))
}

#[derive(Deserialize)]
struct Page { limit: Option<u32>, position: Option<u32>, folder: Option<String> }

async fn list_messages(State(s): S, Extension(u): Extension<CurrentUser>, Path(mailbox_id): Path<String>, Query(p): Query<Page>) -> ApiResult {
    let account = owned_account(&s, &u, &mailbox_id).await?;
    let limit = p.limit.unwrap_or(50).clamp(1, 100);
    let position = p.position.unwrap_or(0);
    let folder = compose::folder_role(p.folder.as_deref().unwrap_or("inbox"))?;
    let (messages, total) = s.jmap.list_folder(&u.creds.user, &u.creds.pass, &account, folder, limit, position).await?;
    Ok(Json(json!({ "messages": messages, "total": total, "position": position })).into_response())
}

pub(crate) fn valid_message_id(id: &str) -> bool {
    id.len() <= 128 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

async fn get_message(State(s): S, Extension(u): Extension<CurrentUser>, Path((mailbox_id, message_id)): Path<(String, String)>) -> ApiResult {
    let account = owned_account(&s, &u, &mailbox_id).await?;
    if !valid_message_id(&message_id) {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_request"));
    }
    let m = s.jmap.get_message(&u.creds.user, &u.creds.pass, &account, &message_id).await?;
    Ok(Json(json!({ "message": m })).into_response())
}
