//! Anonymous temporary inboxes: random receive-only address, capability token, hard expiry.
//! Mail for *@TEMP_INBOX_DOMAIN lands in one catch-all mailbox; each inbox sees only
//! messages whose To: header is its address and that arrived after it was created.
use crate::{valid_message_id, ApiError, ApiResult, S};
use axum::{
    extract::{ConnectInfo, Path},
    http::{header, HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use iris_auth as auth;
use serde_json::json;
use std::net::SocketAddr;

const IP_CREATES_PER_HOUR: i64 = 10;
const ACTIVE_PER_CLIENT: i64 = 3;
const CLIENT_COOKIE: &str = "iris_temp_client";

fn ts(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn cookie<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(header::COOKIE)?.to_str().ok()?
        .split(';').map(str::trim).find_map(|c| c.strip_prefix(name)?.strip_prefix('='))
}

/// Peer IP; trust X-Real-IP only when the peer is a local reverse proxy.
fn client_ip(peer: SocketAddr, headers: &HeaderMap) -> String {
    let fwd = headers.get("x-real-ip").and_then(|h| h.to_str().ok()).map(str::trim);
    match fwd {
        Some(ip) if peer.ip().is_loopback() && ip.parse::<std::net::IpAddr>().is_ok() => ip.to_string(),
        _ => peer.ip().to_string(),
    }
}

/// 12 chars of [a-z0-9] (~62 bits) from two UUIDv4s.
fn random_local_part() -> String {
    const A: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let bytes = [uuid::Uuid::new_v4().into_bytes(), uuid::Uuid::new_v4().into_bytes()].concat();
    bytes.iter().filter(|b| **b < 252).take(12).map(|b| A[(*b % 36) as usize] as char).collect()
}

pub async fn create(axum::extract::State(s): S, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap) -> ApiResult {
    let now = Utc::now();
    let ip_subject = auth::hash_token(&format!("{}:temp-ip:{}", s.config.session_secret, client_ip(peer, &headers)));
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

    let secure = if s.config.env == "production" { "; Secure" } else { "" };
    let set = format!("{CLIENT_COOKIE}={client}; Path=/api/v1/temp-inboxes; HttpOnly; SameSite=Strict; Max-Age=86400{secure}");
    Ok((StatusCode::CREATED, [(header::SET_COOKIE, set)], Json(json!({
        "id": id, "address": address, "access_token": token,
        "created_at": ts(now), "expires_at": ts(expires_at),
        "expires_in_seconds": expiry.num_seconds(),
    }))).into_response())
}

struct Inbox { address: String, created_at: String, expires_at: String }

/// Capability check: bearer token must match, inbox must be active and unexpired.
/// Expired inboxes are flipped to `expired` and answer 410 Gone.
async fn authorize(s: &crate::AppState, id: &str, headers: &HeaderMap) -> Result<Inbox, ApiError> {
    let token = headers.get(header::AUTHORIZATION).and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer ")).ok_or(ApiError(StatusCode::UNAUTHORIZED, "unauthorized"))?;
    let row: Option<(String, String, String, String, String)> = sqlx::query_as(
        "SELECT address,access_token_hash,status,created_at,expires_at FROM temporary_inboxes WHERE id=?",
    ).bind(id).fetch_optional(&s.pool).await?;
    // Same 404 for unknown id and wrong token: no inbox enumeration.
    let (address, _, status, created_at, expires_at) = row
        .filter(|r| r.1 == auth::hash_token(token))
        .ok_or(ApiError(StatusCode::NOT_FOUND, "not_found"))?;
    let now = ts(Utc::now());
    if status != "active" || expires_at <= now {
        if status == "active" {
            sqlx::query("UPDATE temporary_inboxes SET status='expired' WHERE id=? AND status='active'").bind(id).execute(&s.pool).await?;
        }
        return Err(ApiError(StatusCode::GONE, "expired"));
    }
    sqlx::query("UPDATE temporary_inboxes SET last_accessed_at=? WHERE id=?").bind(&now).bind(id).execute(&s.pool).await?;
    Ok(Inbox { address, created_at, expires_at })
}

fn seconds_left(expires_at: &str) -> i64 {
    DateTime::parse_from_rfc3339(expires_at).map(|t| (t.with_timezone(&Utc) - Utc::now()).num_seconds().max(0)).unwrap_or(0)
}

pub async fn status(axum::extract::State(s): S, Path(id): Path<String>, headers: HeaderMap) -> ApiResult {
    let i = authorize(&s, &id, &headers).await?;
    Ok(Json(json!({
        "id": id, "address": i.address, "status": "active",
        "created_at": i.created_at, "expires_at": i.expires_at,
        "expires_in_seconds": seconds_left(&i.expires_at),
    })).into_response())
}

async fn mail_account(s: &crate::AppState) -> Result<String, ApiError> {
    // ponytail: one session lookup per request; cache the account id if this gets hot.
    Ok(s.jmap.login(&s.config.temp_inbox_mail_user, &s.config.temp_inbox_mail_password).await?)
}

pub async fn messages(axum::extract::State(s): S, Path(id): Path<String>, headers: HeaderMap) -> ApiResult {
    let i = authorize(&s, &id, &headers).await?;
    let c = &s.config;
    let account = mail_account(&s).await?;
    let list = s.jmap.list_to(&c.temp_inbox_mail_user, &c.temp_inbox_mail_password, &account, &i.address, &i.created_at, 50).await?;
    Ok(Json(json!({ "messages": list, "expires_in_seconds": seconds_left(&i.expires_at) })).into_response())
}

pub async fn message(axum::extract::State(s): S, Path((id, message_id)): Path<(String, String)>, headers: HeaderMap) -> ApiResult {
    let i = authorize(&s, &id, &headers).await?;
    if !valid_message_id(&message_id) {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_request"));
    }
    let c = &s.config;
    let account = mail_account(&s).await?;
    let m = s.jmap.get_message_to(&c.temp_inbox_mail_user, &c.temp_inbox_mail_password, &account, &message_id, &i.address).await?;
    if m.summary.received_at.as_str() < i.created_at.as_str() {
        return Err(ApiError(StatusCode::NOT_FOUND, "not_found"));
    }
    Ok(Json(json!({ "message": m })).into_response())
}

#[cfg(test)]
mod tests {
    #[test]
    fn local_part_shape() {
        let a = super::random_local_part();
        assert_eq!(a.len(), 12);
        assert!(a.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()));
        assert_ne!(a, super::random_local_part());
    }
}
