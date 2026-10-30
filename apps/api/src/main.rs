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
use std::{collections::HashMap, sync::{Arc, RwLock}};

struct AppState {
    pool: SqlitePool,
    config: Config,
    jmap: JmapClient,
    // ponytail: mail credentials live only in process memory keyed by session id, so an
    // API restart forces re-login. Upgrade path: Stalwart app passwords / OAuth tokens.
    creds: RwLock<HashMap<String, Creds>>,
}

#[derive(Clone)]
struct Creds { user: String, pass: String }

#[derive(Clone)]
struct CurrentUser { user_id: String, session_id: String, email: String, creds: Creds }

type S = State<Arc<AppState>>;
type ApiResult = Result<Response, ApiError>;

struct ApiError(StatusCode, &'static str);
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
            _ => ApiError(StatusCode::BAD_GATEWAY, "mail_server_unavailable"),
        }
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    let config = Config::from_env().expect("config");
    let pool = iris_database::init_pool(&config.database_url).await.expect("database");
    let state = Arc::new(AppState {
        pool,
        jmap: JmapClient::new(config.stalwart_api_url.clone()),
        config: config.clone(),
        creds: RwLock::new(HashMap::new()),
    });

    let protected = Router::new()
        .route("/api/v1/session", get(session))
        .route("/auth/logout", post(logout))
        .route("/api/v1/mailboxes", get(mailboxes))
        .route("/api/v1/mailboxes/:mailbox_id/messages", get(list_messages))
        .route("/api/v1/mailboxes/:mailbox_id/messages/:message_id", get(get_message))
        .route_layer(middleware::from_fn_with_state(state.clone(), require_session));

    let app = Router::new()
        .route("/health", get(|| async { "OK" }))
        .route("/ready", get(ready))
        .route("/auth/login", post(login))
        .merge(protected)
        .with_state(state);

    let addr = format!("{}:{}", config.host, config.port);
    let listener = tokio::net::TcpListener::bind(&addr).await.expect("bind");
    tracing::info!("IRIS API listening on http://{addr}");
    axum::serve(listener, app).await.expect("server");
}

async fn ready(State(s): S) -> Json<serde_json::Value> {
    Json(json!({ "ready": iris_database::health_check(&s.pool).await.is_ok() }))
}

#[derive(Deserialize)]
struct LoginBody { email: String, password: String }

const MAX_FAILED_LOGINS: i64 = 10; // per email per 15 minutes

async fn login(State(s): S, Json(b): Json<LoginBody>) -> ApiResult {
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

    let now = auth::now();
    let mut tx = s.pool.begin().await?;
    sqlx::query("INSERT INTO app_users (id,email,display_name,created_at,updated_at) VALUES (?,?,?,?,?) ON CONFLICT(email) DO UPDATE SET updated_at=excluded.updated_at")
        .bind(uuid::Uuid::new_v4().to_string()).bind(&email).bind(&email).bind(&now).bind(&now)
        .execute(&mut *tx).await?;
    let (user_id, status): (String, String) = sqlx::query_as("SELECT id,status FROM app_users WHERE email=?")
        .bind(&email).fetch_one(&mut *tx).await?;
    if status != "active" {
        return Err(ApiError(StatusCode::FORBIDDEN, "account_disabled"));
    }
    sqlx::query("INSERT INTO permanent_mailboxes (id,user_id,email_address,mail_server_account_id,created_at,updated_at) VALUES (?,?,?,?,?,?) ON CONFLICT(email_address) DO UPDATE SET mail_server_account_id=excluded.mail_server_account_id, updated_at=excluded.updated_at WHERE user_id=excluded.user_id")
        .bind(uuid::Uuid::new_v4().to_string()).bind(&user_id).bind(&email).bind(&account_id).bind(&now).bind(&now)
        .execute(&mut *tx).await?;
    let token = auth::new_token();
    let session_id = uuid::Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO auth_sessions (id,user_id,token_hash,expires_at,created_at) VALUES (?,?,?,?,?)")
        .bind(&session_id).bind(&user_id).bind(auth::hash_token(&token)).bind(auth::expiry()).bind(&now)
        .execute(&mut *tx).await?;
    tx.commit().await?;
    s.creds.write().unwrap().insert(session_id, Creds { user: email.clone(), pass: b.password });

    let cookie = auth::set_cookie(&token, s.config.env == "production");
    Ok(([(header::SET_COOKIE, cookie)], Json(json!({ "authenticated": true, "email": email }))).into_response())
}

async fn require_session(State(s): S, headers: HeaderMap, mut req: Request, next: Next) -> Response {
    let unauthorized = || {
        ([(header::SET_COOKIE, auth::clear_cookie())], ApiError(StatusCode::UNAUTHORIZED, "unauthorized")).into_response()
    };
    let Some(token) = headers.get(header::COOKIE).and_then(|h| h.to_str().ok()).and_then(auth::token_from_cookie) else {
        return unauthorized();
    };
    let row: Option<(String, String, String)> = match sqlx::query_as(
        "SELECT s.id, s.user_id, u.email FROM auth_sessions s JOIN app_users u ON u.id=s.user_id
         WHERE s.token_hash=? AND s.revoked_at IS NULL AND s.expires_at>? AND u.status='active'",
    ).bind(auth::hash_token(token)).bind(auth::now()).fetch_optional(&s.pool).await {
        Ok(r) => r,
        Err(e) => return ApiError::from(e).into_response(),
    };
    let Some((session_id, user_id, email)) = row else { return unauthorized() };
    let Some(creds) = s.creds.read().unwrap().get(&session_id).cloned() else { return unauthorized() };
    let _ = sqlx::query("UPDATE auth_sessions SET last_seen_at=? WHERE id=?")
        .bind(auth::now()).bind(&session_id).execute(&s.pool).await;
    req.extensions_mut().insert(CurrentUser { user_id, session_id, email, creds });
    next.run(req).await
}

async fn session(Extension(u): Extension<CurrentUser>) -> Json<serde_json::Value> {
    Json(json!({ "authenticated": true, "email": u.email }))
}

async fn logout(State(s): S, Extension(u): Extension<CurrentUser>) -> ApiResult {
    sqlx::query("UPDATE auth_sessions SET revoked_at=? WHERE id=?")
        .bind(auth::now()).bind(&u.session_id).execute(&s.pool).await?;
    s.creds.write().unwrap().remove(&u.session_id);
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
async fn owned_account(s: &AppState, u: &CurrentUser, mailbox_id: &str) -> Result<String, ApiError> {
    sqlx::query_scalar("SELECT mail_server_account_id FROM permanent_mailboxes WHERE id=? AND user_id=? AND status='active'")
        .bind(mailbox_id).bind(&u.user_id).fetch_optional(&s.pool).await?
        .ok_or(ApiError(StatusCode::NOT_FOUND, "not_found"))
}

#[derive(Deserialize)]
struct Page { limit: Option<u32>, position: Option<u32> }

async fn list_messages(State(s): S, Extension(u): Extension<CurrentUser>, Path(mailbox_id): Path<String>, Query(p): Query<Page>) -> ApiResult {
    let account = owned_account(&s, &u, &mailbox_id).await?;
    let limit = p.limit.unwrap_or(50).clamp(1, 100);
    let position = p.position.unwrap_or(0);
    let (messages, total) = s.jmap.list_inbox(&u.creds.user, &u.creds.pass, &account, limit, position).await?;
    Ok(Json(json!({ "messages": messages, "total": total, "position": position })).into_response())
}

async fn get_message(State(s): S, Extension(u): Extension<CurrentUser>, Path((mailbox_id, message_id)): Path<(String, String)>) -> ApiResult {
    let account = owned_account(&s, &u, &mailbox_id).await?;
    if message_id.len() > 128 || !message_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_request"));
    }
    let m = s.jmap.get_message(&u.creds.user, &u.creds.pass, &account, &message_id).await?;
    Ok(Json(json!({ "message": m })).into_response())
}
