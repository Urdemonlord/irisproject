//! Read-only admin dashboard: users, quotas, and delivery status.
//!
//! The gate is an explicit allow-list, `IRIS_ADMIN_EMAILS`
//! (comma-separated, lowercased), read into `Config::admin_emails`.
//! There is no admin role in the login flow: any mail account that
//! passes JMAP auth gets an `app_users` row with role `user`, and
//! `app_users.role` is never read or written by any code, so the only
//! way to grant it would be hand-editing the production database —
//! invisible, unaudited, and typo-prone. An env list is visible in
//! the deployment config and fails closed when unset (empty list =
//! nobody is an admin). The check runs server-side at the top of the
//! handler, after `require_session` (so unauthenticated requests
//! answer 401 before this code is reached); the web only uses
//! `is_admin` from `/api/v1/session` to show or hide the nav link.
//!
//! The dashboard is read-only and reuses tables the API already
//! maintains: `app_users` / `permanent_mailboxes` / `auth_sessions`
//! for users, `integration_email_requests` / `integration_keys` for
//! transactional delivery, `rate_limit_events` for aggregate user
//! sends, and `temporary_inboxes` for temp-inbox counts. No new
//! table, no migration. Recipients of transactional mail are stored
//! only as hashes (`recipient_hash`), so the dashboard can never
//! show them; the recent-requests list carries no recipient field.

use crate::{ApiError, ApiResult, CurrentUser, S};
use axum::{extract::State, http::StatusCode, response::IntoResponse, Extension, Json};
use iris_auth as auth;
use iris_config::limits::{
    ACTIVE_PER_CLIENT, IP_CREATES_PER_HOUR, MAX_BODY, MAX_COMPOSE_ATTACHMENTS,
    MAX_COMPOSE_ATTACHMENTS_TOTAL_BYTES, MAX_COMPOSE_ATTACHMENT_BYTES, MAX_CUSTOM_FOLDERS,
    MAX_FILTERS, MAX_LABELS, MAX_READS_PER_WINDOW, MAX_RECIPIENTS, READ_WINDOW_MINUTES,
    SENDS_PER_HOUR, TEMP_INBOX_MESSAGE_LIMIT,
};
use serde_json::json;
use std::collections::HashMap;

/// One row of the recent transactional-requests list: id, app, template,
/// status, provider message id, created time.
type RecentRequestRow = (String, String, String, String, Option<String>, String);
/// One row of the integration-keys list: app, status, daily limit,
/// last used, expiry.
type IntegrationKeyRow = (String, String, i64, Option<String>, Option<String>);

/// Admin gate: the session user's email must be on the
/// `IRIS_ADMIN_EMAILS` allow-list. Non-admins get 403 `forbidden`;
/// an empty allow-list denies everyone.
pub(crate) fn require_admin(config: &iris_config::Config, email: &str) -> Result<(), ApiError> {
    if iris_config::is_admin_email(&config.admin_emails, email) {
        Ok(())
    } else {
        Err(ApiError(StatusCode::FORBIDDEN, "forbidden"))
    }
}

/// One page of the dashboard. Every count is a live query; the users
/// list is capped at 100 (newest first) and `truncated` says whether
/// more users exist beyond the cap.
pub(crate) async fn summary(State(s): S, Extension(u): Extension<CurrentUser>) -> ApiResult {
    require_admin(&s.config, &u.email)?;
    let now = auth::now();
    let day_ago = (chrono::Utc::now() - chrono::Duration::hours(24)).to_rfc3339();
    let week_ago = (chrono::Utc::now() - chrono::Duration::days(7)).to_rfc3339();

    // --- Users -------------------------------------------------------
    let (total_users, active_users, disabled_users): (i64, i64, i64) = sqlx::query_as(
        "SELECT COUNT(*), COUNT(CASE WHEN status='active' THEN 1 END), COUNT(CASE WHEN status='disabled' THEN 1 END) FROM app_users",
    )
    .fetch_one(&s.pool)
    .await?;
    let user_rows: Vec<(String, String, String, String, String, String)> = sqlx::query_as(
        "SELECT id,email,display_name,role,status,created_at FROM app_users ORDER BY created_at DESC LIMIT 100",
    )
    .fetch_all(&s.pool)
    .await?;
    let truncated = total_users > user_rows.len() as i64;
    let mailbox_rows: Vec<(String, String, i64, String)> = sqlx::query_as(
        "SELECT user_id,email_address,quota_bytes,status FROM permanent_mailboxes ORDER BY created_at",
    )
    .fetch_all(&s.pool)
    .await?;
    let session_rows: Vec<(String, i64, Option<String>)> = sqlx::query_as(
        "SELECT user_id, COUNT(*), MAX(last_seen_at) FROM auth_sessions WHERE revoked_at IS NULL AND expires_at>? GROUP BY user_id",
    )
    .bind(&now)
    .fetch_all(&s.pool)
    .await?;
    let mailboxes_by_user: HashMap<String, Vec<(String, i64, String)>> = {
        let mut m: HashMap<String, Vec<(String, i64, String)>> = HashMap::new();
        for (user_id, email, quota_bytes, status) in mailbox_rows {
            m.entry(user_id)
                .or_default()
                .push((email, quota_bytes, status));
        }
        m
    };
    let sessions_by_user: HashMap<String, (i64, Option<String>)> = session_rows
        .into_iter()
        .map(|(user_id, count, last_seen)| (user_id, (count, last_seen)))
        .collect();
    let users: Vec<_> = user_rows
        .into_iter()
        .map(|(id, email, display_name, role, status, created_at)| {
            let mailboxes = mailboxes_by_user.get(&id).cloned().unwrap_or_default();
            let (active_sessions, last_seen_at) =
                sessions_by_user.get(&id).cloned().unwrap_or((0, None));
            json!({
                "id": id,
                "email": email,
                "display_name": display_name,
                "role": role,
                "status": status,
                "created_at": created_at,
                "mailboxes": mailboxes.iter().map(|(email, quota_bytes, status)| json!({
                    "email_address": email,
                    "quota_bytes": quota_bytes,
                    "status": status,
                })).collect::<Vec<_>>(),
                "active_sessions": active_sessions,
                "last_seen_at": last_seen_at,
            })
        })
        .collect();

    // --- Transactional delivery --------------------------------------
    // Status counts for the last 24h and 7 days, per-app breakdown,
    // and the 20 most recent requests. Recipients are stored only as
    // hashes, so no recipient data exists to leak here.
    let delivery_counts = async |since: &str| -> Result<HashMap<String, i64>, sqlx::Error> {
        let rows: Vec<(String, i64)> = sqlx::query_as(
            "SELECT status, COUNT(*) FROM integration_email_requests WHERE created_at>=? GROUP BY status",
        )
        .bind(since)
        .fetch_all(&s.pool)
        .await?;
        Ok(rows.into_iter().collect())
    };
    let last_24h = delivery_counts(&day_ago).await?;
    let last_7d = delivery_counts(&week_ago).await?;
    let per_app_rows: Vec<(String, String, i64)> = sqlx::query_as(
        "SELECT k.app_id, r.status, COUNT(*) FROM integration_email_requests r JOIN integration_keys k ON k.key_id=r.key_id WHERE r.created_at>=? GROUP BY k.app_id, r.status ORDER BY k.app_id, r.status",
    )
    .bind(&week_ago)
    .fetch_all(&s.pool)
    .await?;
    let mut per_app: HashMap<String, HashMap<String, i64>> = HashMap::new();
    for (app_id, status, count) in per_app_rows {
        per_app.entry(app_id).or_default().insert(status, count);
    }
    let recent_rows: Vec<RecentRequestRow> =
        sqlx::query_as(
            "SELECT r.id, k.app_id, r.template_id, r.status, r.provider_message_id, r.created_at FROM integration_email_requests r JOIN integration_keys k ON k.key_id=r.key_id ORDER BY r.created_at DESC LIMIT 20",
        )
        .fetch_all(&s.pool)
        .await?;
    let recent: Vec<_> = recent_rows
        .into_iter()
        .map(
            |(id, app_id, template_id, status, provider_message_id, created_at)| {
                json!({
                    "id": id,
                    "app_id": app_id,
                    "template_id": template_id,
                    "status": status,
                    "provider_message_id": provider_message_id,
                    "created_at": created_at,
                })
            },
        )
        .collect();
    let key_rows: Vec<IntegrationKeyRow> = sqlx::query_as(
        "SELECT app_id, status, daily_send_limit, last_used_at, expires_at FROM integration_keys ORDER BY created_at DESC LIMIT 100",
    )
    .fetch_all(&s.pool)
    .await?;
    let keys: Vec<_> = key_rows
        .into_iter()
        .map(
            |(app_id, status, daily_send_limit, last_used_at, expires_at)| {
                json!({
                    "app_id": app_id,
                    "status": status,
                    "daily_send_limit": daily_send_limit,
                    "last_used_at": last_used_at,
                    "expires_at": expires_at,
                })
            },
        )
        .collect();

    // --- User sends (aggregate only) ---------------------------------
    // `rate_limit_events` records sends under a hashed subject, so
    // per-user attribution is impossible by design; the dashboard
    // shows the global total for the last 24h only.
    let user_sends_last_24h: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM rate_limit_events WHERE action='send' AND outcome='allowed' AND occurred_at>=?",
    )
    .bind(&day_ago)
    .fetch_one(&s.pool)
    .await?;

    // --- Temp inboxes --------------------------------------------------
    let temp_rows: Vec<(String, i64, i64, i64)> = sqlx::query_as(
        "SELECT status, COUNT(*), COALESCE(SUM(message_count),0), COALESCE(SUM(bytes_used),0) FROM temporary_inboxes GROUP BY status",
    )
    .fetch_all(&s.pool)
    .await?;
    let temp_created_last_24h: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM temporary_inboxes WHERE created_at>=?")
            .bind(&day_ago)
            .fetch_one(&s.pool)
            .await?;
    let temp_by_status: HashMap<String, serde_json::Value> = temp_rows
        .into_iter()
        .map(|(status, count, messages, bytes)| {
            (
                status,
                json!({ "count": count, "messages": messages, "bytes": bytes }),
            )
        })
        .collect();

    Ok(Json(json!({
        "users": {
            "total": total_users,
            "active": active_users,
            "disabled": disabled_users,
            "truncated": truncated,
            "list": users,
        },
        "quotas": {
            "sends_per_hour": SENDS_PER_HOUR,
            "reads": { "max": MAX_READS_PER_WINDOW, "window_minutes": READ_WINDOW_MINUTES },
            "temp_inbox": {
                "domain": s.config.temp_inbox_domain,
                "default_expiry_hours": s.config.temp_inbox_default_expiry_hours,
                "creates_per_ip_per_hour": IP_CREATES_PER_HOUR,
                "active_per_client": ACTIVE_PER_CLIENT,
                "message_limit": TEMP_INBOX_MESSAGE_LIMIT,
            },
            "compose": {
                "max_recipients": MAX_RECIPIENTS,
                "max_body_bytes": MAX_BODY,
                "max_attachments": MAX_COMPOSE_ATTACHMENTS,
                "max_attachment_bytes": MAX_COMPOSE_ATTACHMENT_BYTES,
                "max_total_attachment_bytes": MAX_COMPOSE_ATTACHMENTS_TOTAL_BYTES,
            },
            "organize": {
                "max_labels": MAX_LABELS,
                "max_custom_folders": MAX_CUSTOM_FOLDERS,
                "max_filters": MAX_FILTERS,
            },
            "integration_email_enabled": s.config.integration_email_enabled,
            "relay_configured": s.relay.is_some(),
        },
        "delivery": {
            "transactional_last_24h": last_24h,
            "transactional_last_7d": last_7d,
            "transactional_per_app_last_7d": per_app,
            "transactional_recent": recent,
            "integration_keys": keys,
            "user_sends_last_24h": user_sends_last_24h,
        },
        "temp_inboxes": {
            "by_status": temp_by_status,
            "created_last_24h": temp_created_last_24h,
        },
    }))
    .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AppState, Creds, CurrentUser};
    use axum::{body::to_bytes, response::Response};
    use iris_config::Config;
    use serde_json::Value;
    use std::sync::Arc;

    async fn test_state(admin_emails: Vec<String>) -> Arc<AppState> {
        let pool = iris_database::init_pool("sqlite::memory:").await.unwrap();
        Arc::new(AppState {
            pool,
            organize_lock: tokio::sync::Mutex::new(()),
            jmap: iris_mail::JmapClient::new("http://127.0.0.1:9".into()),
            http: reqwest::Client::new(),
            relay: None,
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
                admin_emails,
            },
        })
    }

    fn user(email: &str) -> CurrentUser {
        CurrentUser {
            user_id: format!("u-{email}"),
            email: email.to_string(),
            creds: Creds {
                user: email.to_string(),
                pass: "secret".into(),
            },
        }
    }

    async fn body_json(resp: Response) -> Value {
        let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn seed(s: &AppState) {
        let pool = &s.pool;
        let now = auth::now();
        let old = (chrono::Utc::now() - chrono::Duration::days(10)).to_rfc3339();
        // Users: one admin, one active, one disabled.
        for (id, email, status) in [
            ("u-admin", "admin@example.test", "active"),
            ("u-alice", "alice@example.test", "active"),
            ("u-bob", "bob@example.test", "disabled"),
        ] {
            sqlx::query("INSERT INTO app_users (id,email,display_name,created_at,updated_at,status) VALUES (?,?,?,?,?,?)")
                .bind(id).bind(email).bind(email).bind(&now).bind(&now).bind(status)
                .execute(pool).await.unwrap();
        }
        // Alice has a mailbox with a 1 GiB quota; bob has none.
        sqlx::query("INSERT INTO permanent_mailboxes (id,user_id,email_address,mail_server_account_id,quota_bytes,created_at,updated_at) VALUES ('m-alice','u-alice','alice@example.test','acct-alice',1073741824,?,?)")
            .bind(&now).bind(&now).execute(pool).await.unwrap();
        // Alice: one live session, one expired, one revoked. Only the
        // live one counts as active. The live session expires 30 days
        // out: an expiry of "now" would already be past by the time
        // the handler reads it.
        let future = (chrono::Utc::now() + chrono::Duration::days(30)).to_rfc3339();
        for (id, expires, revoked) in [
            ("s-live", future.clone(), None),
            ("s-expired", old.clone(), None),
            ("s-revoked", future.clone(), Some(now.clone())),
        ] {
            let revoked: Option<String> = revoked;
            sqlx::query("INSERT INTO auth_sessions (id,user_id,token_hash,expires_at,created_at,last_seen_at,revoked_at) VALUES (?,?,?,?,?,?,?)")
                .bind(id).bind("u-alice").bind(format!("hash-{id}")).bind(&expires).bind(&now).bind(&now).bind(&revoked)
                .execute(pool).await.unwrap();
        }
        // Integration keys: alpha active, beta revoked.
        for (key_id, app_id, status) in [
            ("key-alpha", "alpha", "active"),
            ("key-beta", "beta", "revoked"),
        ] {
            sqlx::query("INSERT INTO integration_keys (key_id,app_id,token_hash,scopes_json,status,created_at,daily_send_limit) VALUES (?,?,?,'[]',?,?,100)")
                .bind(key_id).bind(app_id).bind(format!("{key_id:0<64}")).bind(status).bind(&now)
                .execute(pool).await.unwrap();
        }
        // Transactional requests: two recent (accepted + failed), one
        // older than 7 days (counts in neither window).
        for (id, key_id, template, status, created) in [
            ("req-1", "key-alpha", "auth.verify", "accepted", now.clone()),
            ("req-2", "key-alpha", "auth.recovery", "failed", now.clone()),
            ("req-3", "key-alpha", "auth.verify", "accepted", old.clone()),
        ] {
            sqlx::query("INSERT INTO integration_email_requests (id,key_id,idempotency_key,template_id,recipient_hash,request_hash,status,created_at,updated_at) VALUES (?,?,?,?,?,?,?,?,?)")
                .bind(id).bind(key_id).bind(format!("idem-{id}")).bind(template)
                .bind("b".repeat(64)).bind("c".repeat(64)).bind(status).bind(&created).bind(&created)
                .execute(pool).await.unwrap();
        }
        // Two allowed user sends in the last 24h.
        for _ in 0..2 {
            sqlx::query("INSERT INTO rate_limit_events (id,subject_hash,action,occurred_at,outcome) VALUES (?,?,'send',?,'allowed')")
                .bind(uuid::Uuid::new_v4().to_string()).bind("hashed-subject").bind(&now)
                .execute(pool).await.unwrap();
        }
        // Temp inboxes: one active, one expired.
        for (id, address, status) in [
            ("t-1", "aaa@example.test", "active"),
            ("t-2", "bbb@example.test", "expired"),
        ] {
            sqlx::query("INSERT INTO temporary_inboxes (id,address,access_token_hash,status,created_at,expires_at,message_count,bytes_used) VALUES (?,?,'x',?,?,?,3,300)")
                .bind(id).bind(address).bind(status).bind(&now).bind(&now)
                .execute(pool).await.unwrap();
        }
    }

    #[tokio::test]
    async fn admin_sees_users_quotas_and_delivery() {
        let s = test_state(vec!["admin@example.test".into()]).await;
        seed(&s).await;
        let resp = summary(State(s.clone()), Extension(user("admin@example.test")))
            .await
            .unwrap();
        let v = body_json(resp).await;

        assert_eq!(v["users"]["total"], 3);
        assert_eq!(v["users"]["active"], 2);
        assert_eq!(v["users"]["disabled"], 1);
        assert_eq!(v["users"]["truncated"], false);
        let alice = v["users"]["list"]
            .as_array()
            .unwrap()
            .iter()
            .find(|u| u["email"] == "alice@example.test")
            .expect("alice must be listed");
        assert_eq!(alice["mailboxes"].as_array().unwrap().len(), 1);
        assert_eq!(alice["mailboxes"][0]["quota_bytes"], 1073741824);
        assert_eq!(alice["active_sessions"], 1);
        assert!(alice["last_seen_at"].is_string());

        assert_eq!(v["quotas"]["sends_per_hour"], 100);
        assert_eq!(v["quotas"]["reads"]["max"], 600);
        assert_eq!(v["quotas"]["temp_inbox"]["active_per_client"], 3);
        assert_eq!(v["quotas"]["compose"]["max_attachments"], 5);
        assert_eq!(v["quotas"]["organize"]["max_labels"], 50);
        assert_eq!(v["quotas"]["integration_email_enabled"], false);
        assert_eq!(v["quotas"]["relay_configured"], false);

        // Only the two recent requests count in the 24h window.
        assert_eq!(v["delivery"]["transactional_last_24h"]["accepted"], 1);
        assert_eq!(v["delivery"]["transactional_last_24h"]["failed"], 1);
        assert_eq!(v["delivery"]["transactional_last_7d"]["accepted"], 1);
        assert_eq!(
            v["delivery"]["transactional_per_app_last_7d"]["alpha"]["failed"],
            1
        );
        assert_eq!(
            v["delivery"]["transactional_recent"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
        // No recipient data anywhere in the response: recipients are
        // stored only as hashes and the recent list carries no such field.
        assert!(v["delivery"]["transactional_recent"][0]
            .get("recipient")
            .is_none());
        assert_eq!(v["delivery"]["user_sends_last_24h"], 2);
        assert_eq!(
            v["delivery"]["integration_keys"].as_array().unwrap().len(),
            2
        );
        assert!(v["delivery"]["integration_keys"][0].get("key_id").is_none());

        assert_eq!(v["temp_inboxes"]["by_status"]["active"]["count"], 1);
        assert_eq!(v["temp_inboxes"]["by_status"]["expired"]["count"], 1);
        assert_eq!(v["temp_inboxes"]["created_last_24h"], 2);
    }

    #[tokio::test]
    async fn non_admin_gets_403() {
        let s = test_state(vec!["admin@example.test".into()]).await;
        let err = summary(State(s), Extension(user("alice@example.test")))
            .await
            .expect_err("a non-admin must be refused");
        assert_eq!(err.0, StatusCode::FORBIDDEN);
        assert_eq!(err.1, "forbidden");
    }

    #[tokio::test]
    async fn empty_allow_list_denies_everyone() {
        let s = test_state(vec![]).await;
        let err = summary(State(s), Extension(user("admin@example.test")))
            .await
            .expect_err("an empty allow-list must deny everyone");
        assert_eq!(err.0, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn admin_check_is_case_insensitive() {
        let s = test_state(vec!["admin@example.test".into()]).await;
        // Session emails are lowercased at login, but the gate must not
        // depend on that: an upper-case email still matches.
        summary(State(s), Extension(user("ADMIN@Example.Test")))
            .await
            .expect("case must not matter for the admin check");
    }
}
