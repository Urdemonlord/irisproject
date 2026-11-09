//! Audit trail: who did what, when. One row in `audit_events` per
//! recorded action (logins, sends, admin actions, key and mailbox
//! changes). Message reads, lists and searches are never recorded, and
//! neither is any message content, password, token or secret: a send
//! records only the mailbox id and the recipient count, never the
//! addresses. Key use is not double-logged here; the transactional
//! endpoint already records every key use in
//! `integration_email_requests`.
//!
//! Recording is best effort by design: `record` swallows its own
//! errors (logging them) so a failed audit write can never fail the
//! user action it records. Rows are kept forever; no retention prune
//! touches this table. The table itself dates from migration 001,
//! which created it but nothing ever wrote to; migration 014
//! reshapes it (renamed columns, actor email and client IP) into
//! the trail recorded here.
//!
//! The list endpoint is admin-only (`require_admin`), paginated with
//! a `before` cursor over `(created_at, id)`, and filtered by exact
//! `action` and by `user` (actor email or user id).

use crate::{admin, ApiError, ApiResult, CurrentUser, S};
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    Extension, Json,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde::Deserialize;
use serde_json::json;

/// Login succeeded (JMAP auth passed and the session was created).
pub(crate) const LOGIN_SUCCESS: &str = "login.success";
/// Login failed (wrong credentials, or the account is disabled).
pub(crate) const LOGIN_FAILURE: &str = "login.failure";
/// The user signed out, revoking the session.
pub(crate) const LOGOUT: &str = "logout";
/// A compose send was queued for the undo window.
pub(crate) const MAIL_SEND: &str = "mail.send";
/// An integration key was created from the dashboard.
pub(crate) const KEY_CREATE: &str = "integration_key.create";
/// An integration key was revoked.
pub(crate) const KEY_REVOKE: &str = "integration_key.revoke";
/// An admin disabled a user.
pub(crate) const USER_DISABLE: &str = "admin.user_disable";
/// An admin re-enabled a user.
pub(crate) const USER_ENABLE: &str = "admin.user_enable";
/// An admin revoked all of a user's sessions.
pub(crate) const SESSIONS_REVOKE: &str = "admin.sessions_revoke";
/// An admin deleted a temp inbox.
pub(crate) const TEMP_DELETE: &str = "admin.temp_inbox_delete";
/// A mail-server account was linked to a login.
pub(crate) const MAILBOX_LINK: &str = "mailbox.link";
/// A linked mailbox was unlocked for a session.
pub(crate) const MAILBOX_UNLOCK: &str = "mailbox.unlock";
/// A linked mailbox was unlinked from a login.
pub(crate) const MAILBOX_UNLINK: &str = "mailbox.unlink";
/// A per-mailbox signature was saved (or cleared).
pub(crate) const SIGNATURE_SAVE: &str = "signature.save";

/// Record one audit event. Best effort: a failed write is logged and
/// swallowed, never returned, so the recorded action is unaffected.
/// `detail` is stored as JSON; callers must keep secrets, passwords,
/// tokens and recipient addresses out of it. `ip` is stored as-is;
/// pass `None` (or an empty string) when the caller has no client IP.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn record(
    pool: &sqlx::SqlitePool,
    user_id: Option<&str>,
    actor_email: &str,
    action: &str,
    target_type: Option<&str>,
    target_id: Option<&str>,
    detail: serde_json::Value,
    ip: Option<&str>,
) {
    let detail = serde_json::to_string(&detail).unwrap_or_else(|_| "{}".to_string());
    let ip = ip.filter(|s| !s.is_empty());
    if let Err(e) = sqlx::query(
        "INSERT INTO audit_events (id,actor_user_id,actor_email,action,target_type,target_id,detail,ip,created_at) \
         VALUES (?,?,?,?,?,?,?,?,?)",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(user_id)
    .bind(actor_email)
    .bind(action)
    .bind(target_type)
    .bind(target_id)
    .bind(detail)
    .bind(ip)
    .bind(iris_auth::now())
    .execute(pool)
    .await
    {
        tracing::warn!("audit record failed for {action}: {e}");
    }
}

#[derive(Deserialize)]
pub(crate) struct AuditQuery {
    limit: Option<u32>,
    before: Option<String>,
    action: Option<String>,
    user: Option<String>,
}

type AuditRow = (
    String,
    Option<String>,
    String,
    String,
    Option<String>,
    Option<String>,
    String,
    Option<String>,
    String,
);

/// `before` cursors are base64url of `created_at|id`: the last row of
/// the previous page, so the next page starts strictly after it in
/// `(created_at DESC, id DESC)` order.
fn encode_cursor(created_at: &str, id: &str) -> String {
    URL_SAFE_NO_PAD.encode(format!("{created_at}|{id}"))
}

fn decode_cursor(cursor: &str) -> Result<(String, String), ApiError> {
    let decoded = URL_SAFE_NO_PAD
        .decode(cursor)
        .map_err(|_| ApiError(StatusCode::BAD_REQUEST, "invalid_request"))?;
    let text = String::from_utf8(decoded)
        .map_err(|_| ApiError(StatusCode::BAD_REQUEST, "invalid_request"))?;
    text.split_once('|')
        .map(|(c, id)| (c.to_string(), id.to_string()))
        .ok_or(ApiError(StatusCode::BAD_REQUEST, "invalid_request"))
}

/// List audit events, newest first. Admin-only. `limit` defaults to
/// 50 and caps at 200; `before` pages from a previous response's
/// `next_before`; `action` matches exactly; `user` matches the actor
/// email or the user id. The response carries the stored fields only.
pub(crate) async fn list(
    State(s): S,
    Extension(u): Extension<CurrentUser>,
    Query(q): Query<AuditQuery>,
) -> ApiResult {
    admin::require_admin(&s.config, &u.email)?;
    let limit = q.limit.unwrap_or(50).clamp(1, 200) as i64;
    let before = match &q.before {
        Some(cursor) => Some(decode_cursor(cursor)?),
        None => None,
    };
    let mut sql = String::from(
        "SELECT id,actor_user_id,actor_email,action,target_type,target_id,detail,ip,created_at \
         FROM audit_events",
    );
    let mut conds: Vec<&str> = Vec::new();
    if q.action.is_some() {
        conds.push("action=?");
    }
    if q.user.is_some() {
        conds.push("(actor_email=? OR actor_user_id=?)");
    }
    if before.is_some() {
        conds.push("(created_at<? OR (created_at=? AND id<?))");
    }
    if !conds.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&conds.join(" AND "));
    }
    sql.push_str(" ORDER BY created_at DESC, id DESC LIMIT ?");
    let mut query = sqlx::query_as::<_, AuditRow>(&sql);
    if let Some(action) = &q.action {
        query = query.bind(action);
    }
    if let Some(user) = &q.user {
        query = query.bind(user).bind(user);
    }
    if let Some((created_at, id)) = &before {
        query = query.bind(created_at).bind(created_at).bind(id);
    }
    let mut rows = query.bind(limit + 1).fetch_all(&s.pool).await?;
    let next_before = if rows.len() as i64 > limit {
        rows.truncate(limit as usize);
        rows.last().map(|r| encode_cursor(&r.8, &r.0))
    } else {
        None
    };
    let events: Vec<_> = rows
        .into_iter()
        .map(
            |(id, user_id, actor_email, action, target_type, target_id, detail, ip, created_at)| {
                json!({
                    "id": id,
                    "user_id": user_id,
                    "actor_email": actor_email,
                    "action": action,
                    "target_type": target_type,
                    "target_id": target_id,
                    "detail": serde_json::from_str::<serde_json::Value>(&detail).unwrap_or(json!({})),
                    "ip": ip,
                    "created_at": created_at,
                })
            },
        )
        .collect();
    Ok(Json(json!({ "events": events, "next_before": next_before })).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AppState, Creds};
    use axum::body::to_bytes;
    use axum::response::Response;
    use base64::engine::general_purpose::STANDARD;
    use iris_config::Config;
    use serde_json::Value;
    use std::sync::Arc;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        time::{timeout, Duration},
    };

    async fn pool() -> sqlx::SqlitePool {
        iris_database::init_pool("sqlite::memory:").await.unwrap()
    }

    fn user(email: &str) -> CurrentUser {
        CurrentUser {
            user_id: format!("u-{email}"),
            email: email.to_string(),
            creds: Creds {
                user: email.to_string(),
                pass: "secret".into(),
            },
            session_id: "sess-1".into(),
            session_token: "tok-1".into(),
            mailbox_creds: vec![],
            ip: "127.0.0.1".into(),
        }
    }

    /// Minimal fake Stalwart: answers the JMAP session lookup, the
    /// app-password create the login flow mints, and the mailbox/message
    /// reads the read handlers use. A password of "wrong-pass" is
    /// refused with 401, like a real server would. Everything else
    /// answers with empty results, so reads succeed with no messages.
    struct FakeJmap {
        base_url: String,
        _task: tokio::task::JoinHandle<()>,
    }

    async fn fake_jmap() -> FakeJmap {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            loop {
                let Ok(Ok((mut stream, _))) =
                    timeout(Duration::from_secs(5), listener.accept()).await
                else {
                    return;
                };
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
                let (auth_user, auth_pass) = head
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        if !name.eq_ignore_ascii_case("authorization") {
                            return None;
                        }
                        let token = value.trim().strip_prefix("Basic ")?;
                        let decoded = STANDARD.decode(token).unwrap_or_default();
                        let text = String::from_utf8_lossy(&decoded).to_string();
                        Some(match text.split_once(':') {
                            Some((u, p)) => (u.to_string(), p.to_string()),
                            None => (text, String::new()),
                        })
                    })
                    .unwrap_or_default();
                let body = bytes[end + 4..].to_vec();
                let body_json: serde_json::Value =
                    serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
                let (status, payload): (&str, Vec<u8>) = if method == "GET"
                    && path == "/jmap/session"
                {
                    if auth_pass == "wrong-pass" {
                        (
                            "401 Unauthorized",
                            br#"{"error":"bad credentials"}"#.to_vec(),
                        )
                    } else {
                        (
                            "200 OK",
                            serde_json::to_string(&serde_json::json!({
                                "primaryAccounts": {"urn:ietf:params:jmap:mail": format!("acct-{auth_user}")}
                            }))
                            .unwrap()
                            .into_bytes(),
                        )
                    }
                } else if method == "POST" && path == "/jmap/" {
                    let first = body_json["methodCalls"][0][0].as_str().unwrap_or_default();
                    match first {
                        "x:AppPassword/set" => {
                            let is_create = body_json["methodCalls"]
                                .as_array()
                                .map(|calls| {
                                    calls.iter().any(|c| c[1]["create"].is_object())
                                })
                                .unwrap_or(false);
                            if is_create {
                                (
                                    "200 OK",
                                    serde_json::to_string(&serde_json::json!({
                                        "methodResponses": [["x:AppPassword/set",
                                            {"created": {"n": {"id": "cred-1", "secret": "app-secret"}}, "notCreated": {}}, "c"]]
                                    }))
                                    .unwrap()
                                    .into_bytes(),
                                )
                            } else {
                                (
                                    "200 OK",
                                    serde_json::to_string(&serde_json::json!({
                                        "methodResponses": [["x:AppPassword/set",
                                            {"created": {}, "notCreated": {}}, "c"]]
                                    }))
                                    .unwrap()
                                    .into_bytes(),
                                )
                            }
                        }
                        "Mailbox/get" => (
                            "200 OK",
                            serde_json::to_string(&serde_json::json!({
                                "methodResponses": [["Mailbox/get",
                                    {"list": [
                                        {"id": "f-inbox", "name": "Inbox", "role": "inbox", "totalEmails": 0, "unreadEmails": 0},
                                        {"id": "f-sent", "name": "Sent", "role": "sent", "totalEmails": 0, "unreadEmails": 0},
                                        {"id": "f-drafts", "name": "Drafts", "role": "drafts", "totalEmails": 0, "unreadEmails": 0},
                                        {"id": "f-archive", "name": "Archive", "role": "archive", "totalEmails": 0, "unreadEmails": 0},
                                        {"id": "f-trash", "name": "Trash", "role": "trash", "totalEmails": 0, "unreadEmails": 0}
                                    ], "notFound": []}, "m"]]
                            }))
                            .unwrap()
                            .into_bytes(),
                        ),
                        // The client always pairs Email/query with
                        // Email/get in one call, so the fake answers
                        // both: an empty result set.
                        "Email/query" => (
                            "200 OK",
                            serde_json::to_string(&serde_json::json!({
                                "methodResponses": [
                                    ["Email/query", {"ids": [], "queryState": "q1", "total": 0}, "c"],
                                    ["Email/get", {"list": [], "notFound": []}, "g"],
                                ]
                            }))
                            .unwrap()
                            .into_bytes(),
                        ),
                        "Email/get" => (
                            "200 OK",
                            serde_json::to_string(&serde_json::json!({
                                "methodResponses": [["Email/get",
                                    {"list": [], "notFound": []}, "c"]]
                            }))
                            .unwrap()
                            .into_bytes(),
                        ),
                        _ => ("200 OK", br#"{"methodResponses":[]}"#.to_vec()),
                    }
                } else {
                    ("404 Not Found", br#"{"error":"no such route"}"#.to_vec())
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    payload.len()
                );
                let mut out = response.into_bytes();
                out.extend_from_slice(&payload);
                let _ = stream.write_all(&out).await;
                let _ = stream.shutdown().await;
            }
        });
        FakeJmap {
            base_url: format!("http://{address}"),
            _task: task,
        }
    }

    async fn state(fake: &FakeJmap, admin_emails: Vec<String>) -> Arc<AppState> {
        app_state(pool().await, fake.base_url.clone(), admin_emails)
    }

    fn app_state(
        pool: sqlx::SqlitePool,
        jmap_url: String,
        admin_emails: Vec<String>,
    ) -> Arc<AppState> {
        Arc::new(AppState {
            pool,
            organize_lock: tokio::sync::Mutex::new(()),
            jmap: iris_mail::JmapClient::new(jmap_url.clone()),
            http: reqwest::Client::new(),
            relay: None,
            registry: tokio::sync::RwLock::new(crate::integration_apps::default_registry()),
            html_bodies: crate::html_body::HtmlBodyStore::new(),
            mail_watch: crate::notify::MailWatch::from_limits(),
            config: Config {
                env: "test".into(),
                host: "127.0.0.1".into(),
                port: 8000,
                database_url: "sqlite::memory:".into(),
                session_secret: "test-secret".into(),
                turnstile_secret_key: String::new(),
                turnstile_hostnames: Vec::new(),
                turnstile_siteverify_url: String::new(),
                stalwart_api_url: jmap_url,
                temp_inbox_domain: "example.test".into(),
                temp_inbox_default_expiry_hours: 1,
                temp_inbox_mail_user: String::new(),
                temp_inbox_mail_password: String::new(),
                send_local_domains: vec![],
                send_external: false,
                resend_api_key: String::new(),
                brevo_api_key: String::new(),
                integration_email_from: "no-reply@example.com".into(),
                integration_email_enabled: false,
                password_reset_service_token: String::new(),
                password_reset_email_from: String::new(),
                password_reset_email_to: String::new(),
                password_reset_public_origin: String::new(),
                resend_api_url: String::new(),
                brevo_api_url: String::new(),
                mail_content_origin: String::new(),
                admin_emails,
            },
        })
    }

    async fn audit_count(pool: &sqlx::SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM audit_events")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn body_json(resp: Response) -> Value {
        let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn record_stores_the_row_and_nothing_secret() {
        let pool = pool().await;
        sqlx::query(
            "INSERT INTO app_users (id,email,display_name,created_at,updated_at) VALUES ('u1','me@x.id','me@x.id','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        record(
            &pool,
            Some("u1"),
            "me@x.id",
            LOGIN_SUCCESS,
            Some("user"),
            Some("u1"),
            json!({"mailbox_id": "mb1"}),
            Some("127.0.0.1"),
        )
        .await;
        let row: (String, Option<String>, String, String, String) =
            sqlx::query_as("SELECT actor_email, ip, action, target_type, detail FROM audit_events")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            row,
            (
                "me@x.id".into(),
                Some("127.0.0.1".into()),
                "login.success".into(),
                "user".into(),
                r#"{"mailbox_id":"mb1"}"#.into()
            )
        );
    }

    #[tokio::test]
    async fn failed_write_never_fails_the_action() {
        // A pool with no audit_events table (no migrations ran): the
        // write fails, and record still returns normally.
        let bare = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        record(
            &bare,
            Some("u1"),
            "me@x.id",
            LOGIN_SUCCESS,
            None,
            None,
            json!({}),
            None,
        )
        .await;
        // And a closed pool: same, no panic, no error to propagate.
        bare.close().await;
        record(
            &bare,
            Some("u1"),
            "me@x.id",
            LOGIN_SUCCESS,
            None,
            None,
            json!({}),
            None,
        )
        .await;
    }

    #[tokio::test]
    async fn login_success_leaves_exactly_one_row() {
        let fake = fake_jmap().await;
        let s = state(&fake, vec![]).await;
        let resp = crate::login(
            State(s.clone()),
            axum::extract::ConnectInfo(std::net::SocketAddr::from(([127, 0, 0, 1], 12345))),
            axum::http::HeaderMap::new(),
            Json(crate::LoginBody {
                email: "alice@x.id".into(),
                password: "alice-pass".into(),
                turnstile_token: None,
            }),
        )
        .await
        .unwrap();
        assert_eq!(resp.status(), axum::http::StatusCode::OK);
        assert_eq!(audit_count(&s.pool).await, 1);
        let row: (Option<String>, String, String, Option<String>, String) = sqlx::query_as(
            "SELECT actor_user_id, actor_email, action, ip, detail FROM audit_events",
        )
        .fetch_one(&s.pool)
        .await
        .unwrap();
        assert!(row.0.is_some());
        assert_eq!(row.1, "alice@x.id");
        assert_eq!(row.2, "login.success");
        assert_eq!(row.3.as_deref(), Some("127.0.0.1"));
        // The password never lands in the trail.
        let all: String = sqlx::query_scalar(
            "SELECT group_concat(actor_email || '|' || action || '|' || detail || '|' || ifnull(ip, '')) FROM audit_events",
        )
        .fetch_one(&s.pool)
        .await
        .unwrap();
        assert!(!all.contains("alice-pass"));
    }

    #[tokio::test]
    async fn login_failure_leaves_exactly_one_row_with_no_user() {
        let fake = fake_jmap().await;
        let s = state(&fake, vec![]).await;
        let err = crate::login(
            State(s.clone()),
            axum::extract::ConnectInfo(std::net::SocketAddr::from(([127, 0, 0, 1], 12345))),
            axum::http::HeaderMap::new(),
            Json(crate::LoginBody {
                email: "bob@x.id".into(),
                password: "wrong-pass".into(),
                turnstile_token: None,
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, axum::http::StatusCode::UNAUTHORIZED);
        assert_eq!(audit_count(&s.pool).await, 1);
        let row: (Option<String>, String, String, String) =
            sqlx::query_as("SELECT actor_user_id, actor_email, action, detail FROM audit_events")
                .fetch_one(&s.pool)
                .await
                .unwrap();
        assert_eq!(row.0, None);
        assert_eq!(row.1, "bob@x.id");
        assert_eq!(row.2, "login.failure");
        assert!(!row.3.contains("wrong-pass"));
    }

    #[tokio::test]
    async fn admin_disable_is_listed_for_admins_only() {
        let fake = fake_jmap().await;
        let s = state(&fake, vec!["admin@x.id".into()]).await;
        let pool = &s.pool;
        for (id, email) in [("u-admin@x.id", "admin@x.id"), ("u-alice", "alice@x.id")] {
            sqlx::query(
                "INSERT INTO app_users (id,email,display_name,created_at,updated_at) VALUES (?,?,?,'2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
            )
            .bind(id)
            .bind(email)
            .bind(email)
            .execute(pool)
            .await
            .unwrap();
        }
        crate::admin::disable_user(
            State(s.clone()),
            Extension(user("admin@x.id")),
            axum::extract::Path("u-alice".to_string()),
        )
        .await
        .unwrap();
        assert_eq!(audit_count(pool).await, 1);
        let listed = list(
            State(s.clone()),
            Extension(user("admin@x.id")),
            Query(AuditQuery {
                limit: None,
                before: None,
                action: None,
                user: None,
            }),
        )
        .await
        .unwrap();
        let v = body_json(listed).await;
        assert_eq!(v["events"].as_array().unwrap().len(), 1);
        assert_eq!(v["events"][0]["action"], "admin.user_disable");
        assert_eq!(v["events"][0]["actor_email"], "admin@x.id");
        assert_eq!(v["events"][0]["target_id"], "u-alice");
        // A non-admin cannot list at all.
        let err = list(
            State(s.clone()),
            Extension(user("alice@x.id")),
            Query(AuditQuery {
                limit: None,
                before: None,
                action: None,
                user: None,
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, axum::http::StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn reads_leave_no_rows() {
        let fake = fake_jmap().await;
        let s = state(&fake, vec![]).await;
        sqlx::query(
            "INSERT INTO app_users (id,email,display_name,created_at,updated_at) VALUES ('u-me@x.id','me@x.id','me@x.id','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
        )
        .execute(&s.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO permanent_mailboxes (id,user_id,email_address,mail_server_account_id,status,created_at,updated_at) VALUES ('mb1','u-me@x.id','me@x.id','acct-me','active','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
        )
        .execute(&s.pool)
        .await
        .unwrap();
        let mut u = user("me@x.id");
        u.mailbox_creds.push((
            "mb1".to_string(),
            Creds {
                user: "me@x.id".into(),
                pass: "app-secret".into(),
            },
        ));
        crate::compose::folders(
            State(s.clone()),
            Extension(u.clone()),
            axum::extract::Path("mb1".to_string()),
        )
        .await
        .unwrap();
        crate::list_messages(
            State(s.clone()),
            Extension(u),
            axum::extract::Path("mb1".to_string()),
            axum::extract::Query(crate::Page {
                limit: None,
                position: None,
                folder: None,
                folder_id: None,
                view: None,
                label: None,
                q: None,
            }),
        )
        .await
        .unwrap();
        assert_eq!(audit_count(&s.pool).await, 0);
    }

    #[tokio::test]
    async fn list_pages_and_filters() {
        let pool = pool().await;
        sqlx::query(
            "INSERT INTO app_users (id,email,display_name,created_at,updated_at) VALUES ('u1','me@x.id','me@x.id','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        for (i, action) in [
            "login.success",
            "mail.send",
            "login.success",
            "mail.send",
            "login.failure",
        ]
        .iter()
        .enumerate()
        {
            record(
                &pool,
                Some("u1"),
                if i % 2 == 0 { "me@x.id" } else { "other@x.id" },
                action,
                None,
                None,
                json!({"n": i}),
                None,
            )
            .await;
        }
        let s = app_state(
            pool.clone(),
            "http://127.0.0.1:9".into(),
            vec!["admin@x.id".into()],
        );
        let admin = user("admin@x.id");
        let page1 = list(
            State(s.clone()),
            Extension(admin.clone()),
            Query(AuditQuery {
                limit: Some(2),
                before: None,
                action: None,
                user: None,
            }),
        )
        .await
        .unwrap();
        let v1 = body_json(page1).await;
        assert_eq!(v1["events"].as_array().unwrap().len(), 2);
        let next = v1["next_before"].as_str().unwrap().to_string();
        let page2 = list(
            State(s.clone()),
            Extension(admin.clone()),
            Query(AuditQuery {
                limit: Some(2),
                before: Some(next),
                action: None,
                user: None,
            }),
        )
        .await
        .unwrap();
        let v2 = body_json(page2).await;
        assert_eq!(v2["events"].as_array().unwrap().len(), 2);
        let page3 = list(
            State(s.clone()),
            Extension(admin),
            Query(AuditQuery {
                limit: Some(2),
                before: Some(v2["next_before"].as_str().unwrap().to_string()),
                action: None,
                user: None,
            }),
        )
        .await
        .unwrap();
        let v3 = body_json(page3).await;
        assert_eq!(v3["events"].as_array().unwrap().len(), 1);
        assert!(v3["next_before"].is_null());
        // No row appears twice across the pages.
        let mut ids: Vec<String> = v1["events"]
            .as_array()
            .unwrap()
            .iter()
            .chain(v2["events"].as_array().unwrap())
            .chain(v3["events"].as_array().unwrap())
            .map(|e| e["id"].as_str().unwrap().to_string())
            .collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 5);
        // Action filter and user filter.
        let filtered = list(
            State(s.clone()),
            Extension(user("admin@x.id")),
            Query(AuditQuery {
                limit: None,
                before: None,
                action: Some("mail.send".into()),
                user: None,
            }),
        )
        .await
        .unwrap();
        let vf = body_json(filtered).await;
        assert_eq!(vf["events"].as_array().unwrap().len(), 2);
        assert!(vf["events"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["action"] == "mail.send"));
        let by_user = list(
            State(s.clone()),
            Extension(user("admin@x.id")),
            Query(AuditQuery {
                limit: None,
                before: None,
                action: None,
                user: Some("other@x.id".into()),
            }),
        )
        .await
        .unwrap();
        let vu = body_json(by_user).await;
        assert_eq!(vu["events"].as_array().unwrap().len(), 2);
        assert!(vu["events"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["actor_email"] == "other@x.id"));
        // A bad cursor is a 400, not a 500.
        let err = list(
            State(s.clone()),
            Extension(user("admin@x.id")),
            Query(AuditQuery {
                limit: None,
                before: Some("!!!not-base64".into()),
                action: None,
                user: None,
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, axum::http::StatusCode::BAD_REQUEST);
    }
}
