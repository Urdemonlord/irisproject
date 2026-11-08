//! Anonymous temporary inboxes: random receive-only address, capability token, hard expiry.
//! Mail for *@TEMP_INBOX_DOMAIN lands in one catch-all mailbox; each inbox sees only
//! messages whose To: header is its address and that arrived after it was created.
use crate::{
    attachment_preview_response, attachment_response, client_ip, raw_response, valid_message_id,
    valid_part_id, ApiError, ApiResult, S,
};
use axum::{
    extract::{ConnectInfo, Path},
    http::{header, HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use iris_auth as auth;
use iris_config::limits::{
    ACTIVE_PER_CLIENT, IP_CREATES_PER_HOUR, MAX_TEMP_EXTENSIONS, MAX_TEMP_INBOX_LIFETIME_HOURS,
    TEMP_EXTEND_HOURS, TEMP_INBOX_MESSAGE_LIMIT,
};
use serde_json::json;
use std::net::SocketAddr;

const CLIENT_COOKIE: &str = "iris_temp_client";

fn ts(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn cookie<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .map(str::trim)
        .find_map(|c| c.strip_prefix(name)?.strip_prefix('='))
}

/// 12 chars of [a-z0-9] (~62 bits) from two UUIDv4s.
fn random_local_part() -> String {
    const A: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let bytes = [
        uuid::Uuid::new_v4().into_bytes(),
        uuid::Uuid::new_v4().into_bytes(),
    ]
    .concat();
    bytes
        .iter()
        .filter(|b| **b < 252)
        .take(12)
        .map(|b| A[(*b % 36) as usize] as char)
        .collect()
}

pub async fn create(
    axum::extract::State(s): S,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> ApiResult {
    let now = Utc::now();
    let ip_subject = auth::hash_token(&format!(
        "{}:temp-ip:{}",
        s.config.session_secret,
        client_ip(peer, &headers)
    ));
    let client = cookie(&headers, CLIENT_COOKIE)
        .filter(|c| c.len() == 64 && c.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(str::to_string)
        .unwrap_or_else(auth::new_token);
    let client_subject = auth::hash_token(&format!("temp-client:{client}"));
    let expiry = Duration::hours(s.config.temp_inbox_default_expiry_hours.max(1) as i64);

    let count = |subject: &str, since: DateTime<Utc>| {
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM rate_limit_events WHERE subject_hash=? AND action='temp_create' AND outcome='allowed' AND occurred_at>?")
            .bind(subject.to_string()).bind(ts(since)).fetch_one(&s.pool)
    };
    let log = |subject: &str, outcome: &'static str| {
        sqlx::query("INSERT INTO rate_limit_events (id,subject_hash,action,occurred_at,outcome) VALUES (?,?,'temp_create',?,?)")
            .bind(uuid::Uuid::new_v4().to_string()).bind(subject.to_string()).bind(ts(now)).bind(outcome).execute(&s.pool)
    };
    if count(&ip_subject, now - Duration::hours(1)).await? >= IP_CREATES_PER_HOUR {
        log(&ip_subject, "blocked").await?;
        return Err(ApiError(StatusCode::TOO_MANY_REQUESTS, "rate_limited"));
    }
    // Inboxes live `expiry`, so creations within that window = currently active inboxes.
    if count(&client_subject, now - expiry).await? >= ACTIVE_PER_CLIENT {
        log(&client_subject, "blocked").await?;
        return Err(ApiError(StatusCode::TOO_MANY_REQUESTS, "quota_exceeded"));
    }

    let id = uuid::Uuid::new_v4().to_string();
    let token = auth::new_token();
    let address = format!("{}@{}", random_local_part(), s.config.temp_inbox_domain);
    let expires_at = now + expiry;
    sqlx::query("INSERT INTO temporary_inboxes (id,address,access_token_hash,created_at,expires_at,created_ip_hash) VALUES (?,?,?,?,?,?)")
        .bind(&id).bind(&address).bind(auth::hash_token(&token)).bind(ts(now)).bind(ts(expires_at)).bind(&ip_subject)
        .execute(&s.pool).await?;
    log(&ip_subject, "allowed").await?;
    log(&client_subject, "allowed").await?;

    let secure = if s.config.env == "production" {
        "; Secure"
    } else {
        ""
    };
    let set = format!("{CLIENT_COOKIE}={client}; Path=/api/v1/temp-inboxes; HttpOnly; SameSite=Strict; Max-Age=86400{secure}");
    Ok((
        StatusCode::CREATED,
        [(header::SET_COOKIE, set)],
        Json(json!({
            "id": id, "address": address, "access_token": token,
            "created_at": ts(now), "expires_at": ts(expires_at),
            "expires_in_seconds": expiry.num_seconds(),
        })),
    )
        .into_response())
}

struct Inbox {
    address: String,
    created_at: String,
    expires_at: String,
    /// 1-hour extensions used so far (migration 007).
    extensions: i64,
}

fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// Capability check: bearer token must match, inbox must be active and unexpired.
/// Expired inboxes are flipped to `expired` and answer 410 Gone.
async fn authorize(s: &crate::AppState, id: &str, headers: &HeaderMap) -> Result<Inbox, ApiError> {
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .ok_or(ApiError(StatusCode::UNAUTHORIZED, "unauthorized"))?;
    let row: Option<(String, String, String, String, String, i64)> = sqlx::query_as(
        "SELECT address,access_token_hash,status,created_at,expires_at,extensions FROM temporary_inboxes WHERE id=?",
    ).bind(id).fetch_optional(&s.pool).await?;
    // Same 404 for unknown id and wrong token: no inbox enumeration.
    let (address, _, status, created_at, expires_at, extensions) = row
        .filter(|r| r.1 == auth::hash_token(token))
        .ok_or(ApiError(StatusCode::NOT_FOUND, "not_found"))?;
    let now = ts(Utc::now());
    if status != "active" || expires_at <= now {
        if status == "active" {
            sqlx::query(
                "UPDATE temporary_inboxes SET status='expired' WHERE id=? AND status='active'",
            )
            .bind(id)
            .execute(&s.pool)
            .await?;
        }
        return Err(ApiError(StatusCode::GONE, "expired"));
    }
    sqlx::query("UPDATE temporary_inboxes SET last_accessed_at=? WHERE id=?")
        .bind(&now)
        .bind(id)
        .execute(&s.pool)
        .await?;
    Ok(Inbox {
        address,
        created_at,
        expires_at,
        extensions,
    })
}

fn seconds_left(expires_at: &str) -> i64 {
    DateTime::parse_from_rfc3339(expires_at)
        .map(|t| (t.with_timezone(&Utc) - Utc::now()).num_seconds().max(0))
        .unwrap_or(0)
}

pub async fn status(
    axum::extract::State(s): S,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> ApiResult {
    let i = authorize(&s, &id, &headers).await?;
    Ok(Json(json!({
        "id": id, "address": i.address, "status": "active",
        "created_at": i.created_at, "expires_at": i.expires_at,
        "expires_in_seconds": seconds_left(&i.expires_at),
        "extensions": i.extensions,
        "extensions_left": (MAX_TEMP_EXTENSIONS - i.extensions).max(0),
    }))
    .into_response())
}

/// New expiry for a 1-hour extension, or `None` when no further extension
/// is possible: the extension count is spent, or the lifetime cap leaves no
/// room past the current expiry. Pure, so the cap rules are testable
/// without a database.
fn extended_expiry(
    now: DateTime<Utc>,
    current_expires_at: DateTime<Utc>,
    created_at: DateTime<Utc>,
    extensions: i64,
) -> Option<DateTime<Utc>> {
    if extensions >= MAX_TEMP_EXTENSIONS {
        return None;
    }
    let base = now.max(current_expires_at);
    let capped = created_at + Duration::hours(MAX_TEMP_INBOX_LIFETIME_HOURS);
    let next = (base + Duration::hours(TEMP_EXTEND_HOURS)).min(capped);
    if next <= current_expires_at {
        return None;
    }
    Some(next)
}

/// Push the expiry forward by one hour from now (or from the current
/// expiry, whichever is later), capped at 24 hours total life and at five
/// extensions. Same Bearer-token check as every other temp endpoint, so
/// one browser can never extend another browser's inbox.
pub async fn extend(
    axum::extract::State(s): S,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> ApiResult {
    let i = authorize(&s, &id, &headers).await?;
    let now = Utc::now();
    let created_at = parse_ts(&i.created_at).ok_or(ApiError(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal_error",
    ))?;
    let expires_at = parse_ts(&i.expires_at).ok_or(ApiError(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal_error",
    ))?;
    let Some(next) = extended_expiry(now, expires_at, created_at, i.extensions) else {
        return Err(ApiError(StatusCode::CONFLICT, "extension_limit_reached"));
    };
    // Conditional UPDATE: if the row stopped being active between the
    // authorize check and this write (expiry, delete), nothing changes
    // and the caller hears 410 instead of a phantom extension.
    let updated = sqlx::query(
        "UPDATE temporary_inboxes SET expires_at=?, extensions=extensions+1 WHERE id=? AND status='active'",
    )
    .bind(ts(next))
    .bind(&id)
    .execute(&s.pool)
    .await?;
    if updated.rows_affected() == 0 {
        return Err(ApiError(StatusCode::GONE, "expired"));
    }
    let extensions = i.extensions + 1;
    Ok(Json(json!({
        "id": id, "address": i.address, "status": "active",
        "created_at": i.created_at, "expires_at": ts(next),
        "expires_in_seconds": (next - now).num_seconds().max(0),
        "extensions": extensions,
        "extensions_left": MAX_TEMP_EXTENSIONS - extensions,
    }))
    .into_response())
}

/// Delete an address early. The address stops working immediately:
/// `authorize` answers 410 for anything not `active`. The row goes to
/// `deletion_pending`, the queue the retention job already purges through
/// JMAP (mail to the address, then the row flips to `deleted`), so delete
/// reuses the expiry path instead of growing a second one.
pub async fn delete(
    axum::extract::State(s): S,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> ApiResult {
    let i = authorize(&s, &id, &headers).await?;
    let now = ts(Utc::now());
    let updated = sqlx::query(
        "UPDATE temporary_inboxes SET status='deletion_pending', expires_at=? WHERE id=? AND status='active'",
    )
    .bind(&now)
    .bind(&id)
    .execute(&s.pool)
    .await?;
    if updated.rows_affected() == 0 {
        return Err(ApiError(StatusCode::GONE, "expired"));
    }
    Ok(Json(json!({
        "id": id, "address": i.address, "status": "deleted", "deleted_at": now,
    }))
    .into_response())
}

async fn mail_account(s: &crate::AppState) -> Result<String, ApiError> {
    // ponytail: one session lookup per request; cache the account id if this gets hot.
    Ok(s.jmap
        .login(
            &s.config.temp_inbox_mail_user,
            &s.config.temp_inbox_mail_password,
        )
        .await?)
}

pub async fn messages(
    axum::extract::State(s): S,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> ApiResult {
    let i = authorize(&s, &id, &headers).await?;
    let c = &s.config;
    let account = mail_account(&s).await?;
    let list = s
        .jmap
        .list_to(
            &c.temp_inbox_mail_user,
            &c.temp_inbox_mail_password,
            &account,
            &i.address,
            &i.created_at,
            TEMP_INBOX_MESSAGE_LIMIT,
        )
        .await?;
    Ok(
        Json(json!({ "messages": list, "expires_in_seconds": seconds_left(&i.expires_at) }))
            .into_response(),
    )
}

pub async fn message(
    axum::extract::State(s): S,
    Path((id, message_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> ApiResult {
    let i = authorize(&s, &id, &headers).await?;
    if !valid_message_id(&message_id) {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_request"));
    }
    let c = &s.config;
    let account = mail_account(&s).await?;
    let m = s
        .jmap
        .get_message_to(
            &c.temp_inbox_mail_user,
            &c.temp_inbox_mail_password,
            &account,
            &message_id,
            &i.address,
        )
        .await?;
    if m.summary.received_at.as_str() < i.created_at.as_str() {
        return Err(ApiError(StatusCode::NOT_FOUND, "not_found"));
    }
    Ok(Json(json!({ "message": m })).into_response())
}

/// Shared detail lookup for the temp raw / attachment endpoints: same
/// capability-token check, same To: check, same received-after-creation
/// check as `message`, so a temp token can never open another inbox's mail.
/// Returns the catch-all account id alongside the detail, so callers do
/// not repeat the session lookup.
async fn temp_message(
    s: &crate::AppState,
    id: &str,
    message_id: &str,
    headers: &HeaderMap,
) -> Result<(String, iris_mail::MessageDetail), ApiError> {
    let i = authorize(s, id, headers).await?;
    if !valid_message_id(message_id) {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_request"));
    }
    let c = &s.config;
    let account = mail_account(s).await?;
    let m = s
        .jmap
        .get_message_to(
            &c.temp_inbox_mail_user,
            &c.temp_inbox_mail_password,
            &account,
            message_id,
            &i.address,
        )
        .await?;
    if m.summary.received_at.as_str() < i.created_at.as_str() {
        return Err(ApiError(StatusCode::NOT_FOUND, "not_found"));
    }
    Ok((account, m))
}

pub async fn message_raw(
    axum::extract::State(s): S,
    Path((id, message_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> ApiResult {
    let (account, m) = temp_message(&s, &id, &message_id, &headers).await?;
    let c = &s.config;
    raw_response(
        &s,
        &c.temp_inbox_mail_user,
        &c.temp_inbox_mail_password,
        &account,
        &m,
    )
    .await
}

pub async fn message_attachment(
    axum::extract::State(s): S,
    Path((id, message_id, attachment_id)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> ApiResult {
    if !valid_part_id(&attachment_id) {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_request"));
    }
    let (account, m) = temp_message(&s, &id, &message_id, &headers).await?;
    let c = &s.config;
    attachment_response(
        &s,
        &c.temp_inbox_mail_user,
        &c.temp_inbox_mail_password,
        &account,
        &m,
        &attachment_id,
    )
    .await
}

pub async fn message_attachment_preview(
    axum::extract::State(s): S,
    Path((id, message_id, attachment_id)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> ApiResult {
    if !valid_part_id(&attachment_id) {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_request"));
    }
    let (account, m) = temp_message(&s, &id, &message_id, &headers).await?;
    let c = &s.config;
    attachment_preview_response(
        &s,
        &c.temp_inbox_mail_user,
        &c.temp_inbox_mail_password,
        &account,
        &m,
        &attachment_id,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::State;
    use std::sync::Arc;

    #[test]
    fn local_part_shape() {
        let a = super::random_local_part();
        assert_eq!(a.len(), 12);
        assert!(a
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()));
        assert_ne!(a, super::random_local_part());
    }

    #[test]
    fn extended_expiry_adds_one_hour_from_later_of_now_and_expiry() {
        let now = Utc::now();
        let created = now - Duration::minutes(30);
        // Expiry still in the future: one hour past it.
        let next = super::extended_expiry(now, now + Duration::minutes(30), created, 0).unwrap();
        assert!((next - (now + Duration::minutes(90))).num_seconds().abs() <= 1);
        // Expiry already past: one hour from now, not from the stale expiry.
        let next = super::extended_expiry(now, now - Duration::minutes(10), created, 2).unwrap();
        assert!((next - (now + Duration::hours(1))).num_seconds().abs() <= 1);
    }

    #[test]
    fn extended_expiry_stops_at_count_cap() {
        let now = Utc::now();
        assert_eq!(
            super::extended_expiry(now, now + Duration::hours(1), now, MAX_TEMP_EXTENSIONS),
            None
        );
        assert!(super::extended_expiry(
            now,
            now + Duration::hours(1),
            now,
            MAX_TEMP_EXTENSIONS - 1
        )
        .is_some());
    }

    #[test]
    fn extended_expiry_stops_at_lifetime_cap() {
        let now = Utc::now();
        // Created 23 hours ago with 1 hour left: no room past the 24-hour cap.
        let created = now - Duration::hours(23);
        assert_eq!(
            super::extended_expiry(now, now + Duration::hours(1), created, 0),
            None
        );
        // Created 23.5 hours ago with 20 minutes left: the extension is
        // truncated to the 30 minutes left under the cap.
        let created = now - Duration::minutes(23 * 60 + 30);
        let next = super::extended_expiry(now, now + Duration::minutes(20), created, 0).unwrap();
        assert!((next - (now + Duration::minutes(30))).num_seconds().abs() <= 1);
    }

    async fn test_state() -> Arc<crate::AppState> {
        let pool = iris_database::init_pool("sqlite::memory:").await.unwrap();
        Arc::new(crate::AppState {
            pool,
            organize_lock: tokio::sync::Mutex::new(()),
            jmap: iris_mail::JmapClient::new("http://127.0.0.1:9".into()),
            http: reqwest::Client::new(),
            relay: None,
            html_bodies: crate::html_body::HtmlBodyStore::new(),
            mail_watch: crate::notify::MailWatch::from_limits(),
            config: iris_config::Config {
                env: "test".into(),
                admin_emails: Vec::new(),
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
            },
        })
    }

    fn auth_headers(token: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(
            header::AUTHORIZATION,
            format!("Bearer {token}").parse().unwrap(),
        );
        h
    }

    async fn seed_inbox(
        s: &Arc<crate::AppState>,
        id: &str,
        token: &str,
        created: DateTime<Utc>,
        expires: DateTime<Utc>,
        extensions: i64,
    ) {
        sqlx::query(
            "INSERT INTO temporary_inboxes (id,address,access_token_hash,status,created_at,expires_at,extensions) VALUES (?,?,?,'active',?,?,?)",
        )
        .bind(id)
        .bind(format!("{id}@example.test"))
        .bind(auth::hash_token(token))
        .bind(super::ts(created))
        .bind(super::ts(expires))
        .bind(extensions)
        .execute(&s.pool)
        .await
        .unwrap();
    }

    async fn body_json(resp: axum::response::Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(resp.into_body(), 1_000_000)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn extend_happy_path_pushes_expiry_and_counts() {
        let s = test_state().await;
        let now = Utc::now();
        seed_inbox(&s, "inbox-a", "tok-a", now, now + Duration::hours(1), 0).await;
        let v = body_json(
            super::extend(
                State(s.clone()),
                Path("inbox-a".to_string()),
                auth_headers("tok-a"),
            )
            .await
            .unwrap(),
        )
        .await;
        assert_eq!(v["status"], "active");
        assert_eq!(v["extensions"], 1);
        assert_eq!(v["extensions_left"], MAX_TEMP_EXTENSIONS - 1);
        let secs = v["expires_in_seconds"].as_i64().unwrap();
        assert!((6900..=7200).contains(&secs), "expires in {secs}s");
        let (expires, extensions): (String, i64) = sqlx::query_as(
            "SELECT expires_at,extensions FROM temporary_inboxes WHERE id='inbox-a'",
        )
        .fetch_one(&s.pool)
        .await
        .unwrap();
        assert_eq!(extensions, 1);
        assert!(
            (super::parse_ts(&expires).unwrap() - (now + Duration::hours(2)))
                .num_seconds()
                .abs()
                <= 2
        );
    }

    #[tokio::test]
    async fn extend_wrong_token_is_not_found() {
        let s = test_state().await;
        let now = Utc::now();
        seed_inbox(&s, "inbox-a", "tok-a", now, now + Duration::hours(1), 0).await;
        let err = super::extend(
            State(s.clone()),
            Path("inbox-a".to_string()),
            auth_headers("tok-other"),
        )
        .await
        .unwrap_err();
        assert_eq!((err.0, err.1), (StatusCode::NOT_FOUND, "not_found"));
    }

    #[tokio::test]
    async fn extend_token_cannot_touch_another_inbox() {
        let s = test_state().await;
        let now = Utc::now();
        seed_inbox(&s, "inbox-a", "tok-a", now, now + Duration::hours(1), 0).await;
        seed_inbox(&s, "inbox-b", "tok-b", now, now + Duration::hours(1), 0).await;
        let err = super::extend(
            State(s.clone()),
            Path("inbox-b".to_string()),
            auth_headers("tok-a"),
        )
        .await
        .unwrap_err();
        assert_eq!((err.0, err.1), (StatusCode::NOT_FOUND, "not_found"));
        // And inbox B is untouched.
        let extensions: i64 =
            sqlx::query_scalar("SELECT extensions FROM temporary_inboxes WHERE id='inbox-b'")
                .fetch_one(&s.pool)
                .await
                .unwrap();
        assert_eq!(extensions, 0);
    }

    #[tokio::test]
    async fn extend_expired_inbox_is_gone() {
        let s = test_state().await;
        let now = Utc::now();
        seed_inbox(
            &s,
            "inbox-a",
            "tok-a",
            now - Duration::hours(2),
            now - Duration::minutes(1),
            0,
        )
        .await;
        let err = super::extend(
            State(s.clone()),
            Path("inbox-a".to_string()),
            auth_headers("tok-a"),
        )
        .await
        .unwrap_err();
        assert_eq!((err.0, err.1), (StatusCode::GONE, "expired"));
        // authorize() flips the row to expired on the way out.
        let status: String =
            sqlx::query_scalar("SELECT status FROM temporary_inboxes WHERE id='inbox-a'")
                .fetch_one(&s.pool)
                .await
                .unwrap();
        assert_eq!(status, "expired");
    }

    #[tokio::test]
    async fn extend_at_count_cap_is_conflict() {
        let s = test_state().await;
        let now = Utc::now();
        seed_inbox(
            &s,
            "inbox-a",
            "tok-a",
            now,
            now + Duration::hours(1),
            MAX_TEMP_EXTENSIONS,
        )
        .await;
        let err = super::extend(
            State(s.clone()),
            Path("inbox-a".to_string()),
            auth_headers("tok-a"),
        )
        .await
        .unwrap_err();
        assert_eq!(
            (err.0, err.1),
            (StatusCode::CONFLICT, "extension_limit_reached")
        );
    }

    #[tokio::test]
    async fn extend_at_lifetime_cap_is_conflict() {
        let s = test_state().await;
        let now = Utc::now();
        // Created 23 hours ago with 1 hour left: the 24-hour cap is hit.
        seed_inbox(
            &s,
            "inbox-a",
            "tok-a",
            now - Duration::hours(23),
            now + Duration::hours(1),
            0,
        )
        .await;
        let err = super::extend(
            State(s.clone()),
            Path("inbox-a".to_string()),
            auth_headers("tok-a"),
        )
        .await
        .unwrap_err();
        assert_eq!(
            (err.0, err.1),
            (StatusCode::CONFLICT, "extension_limit_reached")
        );
    }

    #[tokio::test]
    async fn delete_happy_path_queues_purge_and_stops_access() {
        let s = test_state().await;
        let now = Utc::now();
        seed_inbox(&s, "inbox-a", "tok-a", now, now + Duration::hours(1), 2).await;
        let v = body_json(
            super::delete(
                State(s.clone()),
                Path("inbox-a".to_string()),
                auth_headers("tok-a"),
            )
            .await
            .unwrap(),
        )
        .await;
        assert_eq!(v["status"], "deleted");
        let status: String =
            sqlx::query_scalar("SELECT status FROM temporary_inboxes WHERE id='inbox-a'")
                .fetch_one(&s.pool)
                .await
                .unwrap();
        // `deletion_pending`: the retention job purges the mail through JMAP
        // and flips the row to `deleted`.
        assert_eq!(status, "deletion_pending");
        // The address is dead immediately: even the status endpoint 410s.
        let err = super::status(
            State(s.clone()),
            Path("inbox-a".to_string()),
            auth_headers("tok-a"),
        )
        .await
        .unwrap_err();
        assert_eq!((err.0, err.1), (StatusCode::GONE, "expired"));
    }

    #[tokio::test]
    async fn delete_wrong_token_is_not_found() {
        let s = test_state().await;
        let now = Utc::now();
        seed_inbox(&s, "inbox-a", "tok-a", now, now + Duration::hours(1), 0).await;
        let err = super::delete(
            State(s.clone()),
            Path("inbox-a".to_string()),
            auth_headers("tok-other"),
        )
        .await
        .unwrap_err();
        assert_eq!((err.0, err.1), (StatusCode::NOT_FOUND, "not_found"));
        let status: String =
            sqlx::query_scalar("SELECT status FROM temporary_inboxes WHERE id='inbox-a'")
                .fetch_one(&s.pool)
                .await
                .unwrap();
        assert_eq!(status, "active");
    }

    #[tokio::test]
    async fn delete_expired_inbox_is_gone() {
        let s = test_state().await;
        let now = Utc::now();
        seed_inbox(
            &s,
            "inbox-a",
            "tok-a",
            now - Duration::hours(2),
            now - Duration::minutes(1),
            0,
        )
        .await;
        let err = super::delete(
            State(s.clone()),
            Path("inbox-a".to_string()),
            auth_headers("tok-a"),
        )
        .await
        .unwrap_err();
        assert_eq!((err.0, err.1), (StatusCode::GONE, "expired"));
    }
}
