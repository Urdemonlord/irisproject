//! Admin dashboard: users, quotas, and delivery status, plus the
//! actions an admin needs: disable/enable users, revoke their
//! sessions, revoke integration keys, and delete temp inboxes.
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
//! The dashboard reads tables the API already maintains:
//! `app_users` / `permanent_mailboxes` / `auth_sessions`
//! for users, `integration_email_requests` / `integration_keys` for
//! transactional delivery, `rate_limit_events` for aggregate user
//! sends, and `temporary_inboxes` for temp-inbox counts. Migration
//! 008 added `integration_email_requests.error_message`, the relay
//! rejection reason, so a `failed` request shows its cause.
//! Recipients of transactional mail are stored
//! only as hashes (`recipient_hash`), so the dashboard can never
//! show them; the recent-requests list carries no recipient field.
//! Integration keys expose their `key_id` (the public identifier the
//! UI needs to revoke one); the token itself is never stored or
//! shown, only its hash.

use crate::{ApiError, ApiResult, CurrentUser, S};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Extension, Json,
};
use iris_auth as auth;
use iris_config::limits::{
    ACTIVE_PER_CLIENT, IP_CREATES_PER_HOUR, MAX_BODY, MAX_COMPOSE_ATTACHMENTS,
    MAX_COMPOSE_ATTACHMENTS_TOTAL_BYTES, MAX_COMPOSE_ATTACHMENT_BYTES, MAX_CUSTOM_FOLDERS,
    MAX_FILTERS, MAX_LABELS, MAX_READS_PER_WINDOW, MAX_RECIPIENTS, READ_WINDOW_MINUTES,
    SENDS_PER_HOUR, TEMP_INBOX_MESSAGE_LIMIT,
};
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;

/// One row of the recent transactional-requests list: id, app, template,
/// status, provider message id, rejection reason, created time.
type RecentRequestRow = (
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    String,
);
/// One row of the integration-keys list: id, app, status, daily limit,
/// last used, expiry.
type IntegrationKeyRow = (String, String, String, i64, Option<String>, Option<String>);

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
            "SELECT r.id, k.app_id, r.template_id, r.status, r.provider_message_id, r.error_message, r.created_at FROM integration_email_requests r JOIN integration_keys k ON k.key_id=r.key_id ORDER BY r.created_at DESC LIMIT 20",
        )
        .fetch_all(&s.pool)
        .await?;
    let recent: Vec<_> = recent_rows
        .into_iter()
        .map(
            |(id, app_id, template_id, status, provider_message_id, error_message, created_at)| {
                json!({
                    "id": id,
                    "app_id": app_id,
                    "template_id": template_id,
                    "status": status,
                    "provider_message_id": provider_message_id,
                    "error_message": error_message,
                    "created_at": created_at,
                })
            },
        )
        .collect();
    // The registered integration apps, so the dashboard can offer them
    // in the create-key form with the scopes each one may hold. The
    // snapshot in `AppState::registry` mirrors the database tables
    // and is refreshed after every app mutation.
    let registry_apps: Vec<_> = s
        .registry
        .read()
        .await
        .iter()
        .map(|app| {
            json!({
                "id": app.id,
                "host": app.host,
                "templates": app.templates.iter().map(|t| &t.id).collect::<Vec<_>>(),
            })
        })
        .collect();
    let key_rows: Vec<IntegrationKeyRow> = sqlx::query_as(
        "SELECT key_id, app_id, status, daily_send_limit, last_used_at, expires_at FROM integration_keys ORDER BY created_at DESC LIMIT 100",
    )
    .fetch_all(&s.pool)
    .await?;
    let keys: Vec<_> = key_rows
        .into_iter()
        .map(
            |(key_id, app_id, status, daily_send_limit, last_used_at, expires_at)| {
                json!({
                    "key_id": key_id,
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
    // Active temp inboxes the admin can delete early, newest first.
    let temp_active_rows: Vec<(String, String, String, String, i64)> = sqlx::query_as(
        "SELECT id, address, created_at, expires_at, message_count FROM temporary_inboxes WHERE status='active' ORDER BY created_at DESC LIMIT 100",
    )
    .fetch_all(&s.pool)
    .await?;
    let temp_active: Vec<_> = temp_active_rows
        .into_iter()
        .map(|(id, address, created_at, expires_at, message_count)| {
            json!({
                "id": id,
                "address": address,
                "created_at": created_at,
                "expires_at": expires_at,
                "message_count": message_count,
            })
        })
        .collect();
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
            "integration_apps": registry_apps,
            "user_sends_last_24h": user_sends_last_24h,
        },
        "temp_inboxes": {
            "by_status": temp_by_status,
            "created_last_24h": temp_created_last_24h,
            "active_list": temp_active,
        },
    }))
    .into_response())
}

/// Disable a user. A disabled user cannot log in (`persist_login`
/// refuses them with `account_disabled`) and their live sessions stop
/// validating (`require_session` requires `status='active'`), so the
/// account is locked out at once. The admin cannot disable their own
/// account: that would lock the only admin out with no way back in.
pub(crate) async fn disable_user(
    State(s): S,
    Extension(u): Extension<CurrentUser>,
    Path(id): Path<String>,
) -> ApiResult {
    require_admin(&s.config, &u.email)?;
    if u.user_id == id {
        return Err(ApiError(StatusCode::CONFLICT, "cannot_disable_self"));
    }
    set_user_status(&s.pool, &id, "active", "disabled").await
}

/// Re-enable a disabled user. Login works again from this write on.
pub(crate) async fn enable_user(
    State(s): S,
    Extension(u): Extension<CurrentUser>,
    Path(id): Path<String>,
) -> ApiResult {
    require_admin(&s.config, &u.email)?;
    set_user_status(&s.pool, &id, "disabled", "active").await
}

/// Shared disable/enable write: flip `status` from `from` to `to`.
/// Unknown id answers 404; a user in any other status answers 409.
/// The write is conditional on the old status, so a second click (or
/// a second admin) hears 409 instead of silently succeeding.
async fn set_user_status(pool: &sqlx::SqlitePool, id: &str, from: &str, to: &str) -> ApiResult {
    let code = if to == "disabled" {
        "user_not_active"
    } else {
        "user_not_disabled"
    };
    let row: Option<(String, String)> =
        sqlx::query_as("SELECT email, status FROM app_users WHERE id=?")
            .bind(id)
            .fetch_optional(pool)
            .await?;
    let Some((email, status)) = row else {
        return Err(ApiError(StatusCode::NOT_FOUND, "not_found"));
    };
    if status != from {
        return Err(ApiError(StatusCode::CONFLICT, code));
    }
    let now = auth::now();
    let updated =
        sqlx::query("UPDATE app_users SET status=?, updated_at=? WHERE id=? AND status=?")
            .bind(to)
            .bind(&now)
            .bind(id)
            .bind(from)
            .execute(pool)
            .await?;
    if updated.rows_affected() == 0 {
        return Err(ApiError(StatusCode::CONFLICT, code));
    }
    Ok(Json(json!({ "id": id, "email": email, "status": to })).into_response())
}

/// Revoke all of a user's live sessions, signing them out everywhere.
/// This only flips `revoked_at` on the session rows: it cannot destroy
/// the Stalwart app passwords behind them, because opening a sealed
/// mail credential needs that user's session token, which the admin
/// does not have. The passwords expire on their own (30 days) and the
/// sessions stop validating at once, so the sign-out is immediate.
pub(crate) async fn revoke_user_sessions(
    State(s): S,
    Extension(u): Extension<CurrentUser>,
    Path(id): Path<String>,
) -> ApiResult {
    require_admin(&s.config, &u.email)?;
    let exists: Option<String> = sqlx::query_scalar("SELECT id FROM app_users WHERE id=?")
        .bind(&id)
        .fetch_optional(&s.pool)
        .await?;
    if exists.is_none() {
        return Err(ApiError(StatusCode::NOT_FOUND, "not_found"));
    }
    let revoked = sqlx::query(
        "UPDATE auth_sessions SET revoked_at=? WHERE user_id=? AND revoked_at IS NULL AND expires_at>?",
    )
    .bind(auth::now())
    .bind(&id)
    .bind(auth::now())
    .execute(&s.pool)
    .await?;
    Ok(Json(json!({ "id": id, "revoked": revoked.rows_affected() })).into_response())
}

/// Revoke an integration key. The transactional endpoint authenticates
/// with `WHERE key_id=? AND status='active'`, so the key stops working
/// from this write on. Reuses the CLI revoke so both paths share one
/// implementation.
pub(crate) async fn revoke_integration_key(
    State(s): S,
    Extension(u): Extension<CurrentUser>,
    Path(key_id): Path<String>,
) -> ApiResult {
    require_admin(&s.config, &u.email)?;
    if !crate::integration_keys::valid_key_id(&key_id) {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_key_id"));
    }
    let row: Option<(String, String)> =
        sqlx::query_as("SELECT app_id, status FROM integration_keys WHERE key_id=?")
            .bind(&key_id)
            .fetch_optional(&s.pool)
            .await?;
    let Some((app_id, status)) = row else {
        return Err(ApiError(StatusCode::NOT_FOUND, "not_found"));
    };
    if status != "active" {
        return Err(ApiError(StatusCode::CONFLICT, "key_not_active"));
    }
    crate::integration_keys::revoke(&s.pool, &key_id)
        .await
        .map_err(|err| match err {
            "invalid_key_id" => ApiError(StatusCode::BAD_REQUEST, "invalid_key_id"),
            _ => ApiError(StatusCode::CONFLICT, "key_not_active"),
        })?;
    Ok(Json(json!({ "key_id": key_id, "app_id": app_id, "status": "revoked" })).into_response())
}

/// Body for creating an integration key from the dashboard. `scopes`
/// is optional: omitted means every scope the app may hold (its
/// template scopes plus `email:status`). `expires_days` and
/// `daily_limit` are required and share the CLI limits (1..=365 days,
/// 1..=10000 sends per day) because creation reuses the CLI minting
/// logic; the database also enforces the daily-limit cap.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateIntegrationKeyBody {
    app: String,
    expires_days: u32,
    daily_limit: u32,
    #[serde(default)]
    scopes: Option<Vec<String>>,
}

/// Mint an integration key for an app. Reuses the CLI minting logic
/// (`integration_keys::issue`), so dashboard keys and CLI keys are
/// identical: the plaintext secret is hashed before storage and is
/// returned only in this one response, never stored or shown again.
/// The CLI allows several active keys per app, so this does too; a
/// duplicate app is not an error. Revoke the key with the existing
/// revoke endpoint when it is no longer needed.
pub(crate) async fn create_integration_key(
    State(s): S,
    Extension(u): Extension<CurrentUser>,
    Json(body): Json<CreateIntegrationKeyBody>,
) -> ApiResult {
    require_admin(&s.config, &u.email)?;
    if !crate::integration_apps::valid_app_slug(&body.app) {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_app"));
    }
    if !(1..=365).contains(&body.expires_days) || !(1..=10_000).contains(&body.daily_limit) {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_limits"));
    }
    // One snapshot for the whole handler: the registry cannot change
    // under the checks below, so the gate and the mint agree.
    let apps = s.registry.read().await.clone();
    let Some(app) = crate::integration_apps::app(&apps, &body.app) else {
        return Err(ApiError(StatusCode::BAD_REQUEST, "unsupported_app"));
    };
    let scopes = match body.scopes {
        Some(scopes) => {
            if scopes.is_empty() {
                return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_scopes"));
            }
            scopes
        }
        None => app.all_scopes(),
    };
    // Validate before minting so a bad scope answers 400 with a clear
    // code; `issue` re-checks everything, so the rules live in one place.
    let mut seen = std::collections::HashSet::new();
    if scopes.len() > 4
        || scopes.iter().any(|scope| {
            !seen.insert(scope) || !crate::integration_apps::scope_allowed(&apps, &body.app, scope)
        })
        || !scopes
            .iter()
            .any(|scope| scope.starts_with("email:template:"))
    {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_scopes"));
    }
    let generated = crate::integration_keys::issue(
        &s.pool,
        &apps,
        &body.app,
        &scopes.join(","),
        body.expires_days,
        body.daily_limit,
    )
    .await
    .map_err(|err| match err {
        "unsupported_app" => ApiError(StatusCode::BAD_REQUEST, "unsupported_app"),
        "invalid_limits" => ApiError(StatusCode::BAD_REQUEST, "invalid_limits"),
        "invalid_scopes" => ApiError(StatusCode::BAD_REQUEST, "invalid_scopes"),
        _ => ApiError(StatusCode::INTERNAL_SERVER_ERROR, "internal_error"),
    })?;
    // Read the stored row back so the response carries exactly what was
    // stored (expiry and scopes), never a recomputed copy.
    let row: Option<(String, String)> =
        sqlx::query_as("SELECT expires_at, scopes_json FROM integration_keys WHERE key_id=?")
            .bind(&generated.key_id)
            .fetch_optional(&s.pool)
            .await?;
    let (expires_at, scopes_json) = row.ok_or(ApiError(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal_error",
    ))?;
    let stored_scopes: Vec<String> = serde_json::from_str(&scopes_json)
        .map_err(|_| ApiError(StatusCode::INTERNAL_SERVER_ERROR, "internal_error"))?;
    Ok(Json(json!({
        "key_id": generated.key_id,
        "app": body.app,
        "secret": generated.token,
        "expires_at": expires_at,
        "daily_limit": body.daily_limit,
        "scopes": stored_scopes,
    }))
    .into_response())
}

/// Refresh the in-memory registry from the database after an app
/// mutation, so the send path and key creation see the change at
/// once with no restart. Called after the database write commits;
/// a failure here is a 500, never a silent stale registry.
async fn refresh_registry(s: &crate::AppState) -> Result<(), ApiError> {
    let apps = crate::integration_apps::load_apps(&s.pool).await?;
    *s.registry.write().await = apps;
    Ok(())
}

fn integration_app_json(
    app: &crate::integration_apps::IntegrationApp,
    created_at: &str,
) -> serde_json::Value {
    json!({
        "id": app.id,
        "host": app.host,
        "created_at": created_at,
        "templates": app.templates.iter().map(|t| json!({
            "id": t.id,
            "subject": t.subject,
            "intro": t.intro,
            "closing": t.closing,
        })).collect::<Vec<_>>(),
    })
}

/// Body for one template in a create/update app request.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct IntegrationAppTemplateBody {
    id: String,
    subject: String,
    intro: String,
    closing: String,
}

/// Body for registering an app from the dashboard. The id is part of
/// the create body because the app is addressed by it from then on.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateIntegrationAppBody {
    id: String,
    host: String,
    templates: Vec<IntegrationAppTemplateBody>,
}

/// Body for updating an app. The id itself is immutable and comes
/// from the path, so only the host and templates are replaced.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UpdateIntegrationAppBody {
    host: String,
    templates: Vec<IntegrationAppTemplateBody>,
}

fn integration_app_from(
    id: String,
    host: String,
    templates: Vec<IntegrationAppTemplateBody>,
) -> crate::integration_apps::IntegrationApp {
    crate::integration_apps::IntegrationApp {
        id,
        host,
        templates: templates
            .into_iter()
            .map(|t| crate::integration_apps::Template {
                id: t.id,
                subject: t.subject,
                intro: t.intro,
                closing: t.closing,
            })
            .collect(),
    }
}

/// Map shared app validation to the admin error codes, so a bad id,
/// host, or template answers 400 with a code the UI can explain.
fn app_validation_error(err: crate::integration_apps::AppValidation) -> ApiError {
    use crate::integration_apps::AppValidation;
    match err {
        AppValidation::App(_) => ApiError(StatusCode::BAD_REQUEST, "invalid_app"),
        AppValidation::Host(_) => ApiError(StatusCode::BAD_REQUEST, "invalid_host"),
        AppValidation::Templates(_) => ApiError(StatusCode::BAD_REQUEST, "invalid_templates"),
    }
}

/// List every registered app with its templates, ordered by id.
/// The dashboard table reads this, not the summary, so it sees the
/// full template text (subjects) the summary's id-only list omits.
pub(crate) async fn list_integration_apps(
    State(s): S,
    Extension(u): Extension<CurrentUser>,
) -> ApiResult {
    require_admin(&s.config, &u.email)?;
    let stored = crate::integration_apps::load_stored_apps(&s.pool).await?;
    let apps: Vec<_> = stored
        .iter()
        .map(|(app, created_at)| integration_app_json(app, created_at))
        .collect();
    Ok(Json(json!({ "apps": apps })).into_response())
}

/// Register an app from the dashboard. The id and the host must both
/// be free (409 `duplicate_app` / `duplicate_host`); the templates
/// follow the same shared validation as the env seed, so an app
/// registered here and one seeded from config obey the same rules.
/// The in-memory registry is refreshed before answering, so the app
/// can send and take keys at once.
pub(crate) async fn create_integration_app(
    State(s): S,
    Extension(u): Extension<CurrentUser>,
    Json(body): Json<CreateIntegrationAppBody>,
) -> ApiResult {
    require_admin(&s.config, &u.email)?;
    let app = integration_app_from(body.id, body.host, body.templates);
    crate::integration_apps::validate_app(&app).map_err(app_validation_error)?;
    let now = auth::now();
    let mut tx = s.pool.begin_with("BEGIN IMMEDIATE").await?;
    let id_taken: Option<String> = sqlx::query_scalar("SELECT id FROM integration_apps WHERE id=?")
        .bind(&app.id)
        .fetch_optional(&mut *tx)
        .await?;
    if id_taken.is_some() {
        return Err(ApiError(StatusCode::CONFLICT, "duplicate_app"));
    }
    let host_taken: Option<String> =
        sqlx::query_scalar("SELECT id FROM integration_apps WHERE host=?")
            .bind(&app.host)
            .fetch_optional(&mut *tx)
            .await?;
    if host_taken.is_some() {
        return Err(ApiError(StatusCode::CONFLICT, "duplicate_host"));
    }
    sqlx::query("INSERT INTO integration_apps (id, host, created_at) VALUES (?,?,?)")
        .bind(&app.id)
        .bind(&app.host)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
    for template in &app.templates {
        sqlx::query(
            "INSERT INTO integration_app_templates (app_id, template_id, subject, intro, closing) VALUES (?,?,?,?,?)",
        )
        .bind(&app.id)
        .bind(&template.id)
        .bind(&template.subject)
        .bind(&template.intro)
        .bind(&template.closing)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    refresh_registry(&s).await?;
    Ok((StatusCode::CREATED, Json(integration_app_json(&app, &now))).into_response())
}

/// Replace an app's host and templates. The id in the path is the
/// app's identity and never changes; a template the new list omits
/// is gone, and a key scoped to it stops authenticating. Same
/// validation and duplicate-host check as create, except the app's
/// own host is not a conflict.
pub(crate) async fn update_integration_app(
    State(s): S,
    Extension(u): Extension<CurrentUser>,
    Path(id): Path<String>,
    Json(body): Json<UpdateIntegrationAppBody>,
) -> ApiResult {
    require_admin(&s.config, &u.email)?;
    let app = integration_app_from(id.clone(), body.host, body.templates);
    crate::integration_apps::validate_app(&app).map_err(app_validation_error)?;
    let mut tx = s.pool.begin_with("BEGIN IMMEDIATE").await?;
    let exists: Option<String> = sqlx::query_scalar("SELECT id FROM integration_apps WHERE id=?")
        .bind(&id)
        .fetch_optional(&mut *tx)
        .await?;
    if exists.is_none() {
        return Err(ApiError(StatusCode::NOT_FOUND, "unknown_app"));
    }
    let host_taken: Option<String> =
        sqlx::query_scalar("SELECT id FROM integration_apps WHERE host=? AND id<>?")
            .bind(&app.host)
            .bind(&id)
            .fetch_optional(&mut *tx)
            .await?;
    if host_taken.is_some() {
        return Err(ApiError(StatusCode::CONFLICT, "duplicate_host"));
    }
    sqlx::query("UPDATE integration_apps SET host=? WHERE id=?")
        .bind(&app.host)
        .bind(&id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM integration_app_templates WHERE app_id=?")
        .bind(&id)
        .execute(&mut *tx)
        .await?;
    for template in &app.templates {
        sqlx::query(
            "INSERT INTO integration_app_templates (app_id, template_id, subject, intro, closing) VALUES (?,?,?,?,?)",
        )
        .bind(&id)
        .bind(&template.id)
        .bind(&template.subject)
        .bind(&template.intro)
        .bind(&template.closing)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    refresh_registry(&s).await?;
    let created_at: String =
        sqlx::query_scalar("SELECT created_at FROM integration_apps WHERE id=?")
            .bind(&id)
            .fetch_one(&s.pool)
            .await?;
    Ok(Json(integration_app_json(&app, &created_at)).into_response())
}

/// Delete an app. Refused with 409 `app_has_active_keys` while any
/// key for the app is still active (not revoked and not expired):
/// deleting under a live key would leave a key that authenticates
/// against nothing. Revoke the keys first, then delete. Templates go
/// with the app (cascade); past delivery rows keep their `app_id`
/// string and are unaffected.
pub(crate) async fn delete_integration_app(
    State(s): S,
    Extension(u): Extension<CurrentUser>,
    Path(id): Path<String>,
) -> ApiResult {
    require_admin(&s.config, &u.email)?;
    let mut tx = s.pool.begin_with("BEGIN IMMEDIATE").await?;
    let exists: Option<String> = sqlx::query_scalar("SELECT id FROM integration_apps WHERE id=?")
        .bind(&id)
        .fetch_optional(&mut *tx)
        .await?;
    if exists.is_none() {
        return Err(ApiError(StatusCode::NOT_FOUND, "unknown_app"));
    }
    let active_keys: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM integration_keys WHERE app_id=? AND status='active' AND (expires_at IS NULL OR expires_at>?)",
    )
    .bind(&id)
    .bind(auth::now())
    .fetch_optional(&mut *tx)
    .await?
    .unwrap_or(0);
    if active_keys > 0 {
        return Err(ApiError(StatusCode::CONFLICT, "app_has_active_keys"));
    }
    sqlx::query("DELETE FROM integration_apps WHERE id=?")
        .bind(&id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    refresh_registry(&s).await?;
    Ok(Json(json!({ "id": id, "deleted": true })).into_response())
}

/// Delete an active temp inbox early. Same write as the user-facing
/// delete (`temp::mark_deletion_pending`): the address dies at once
/// and the retention job purges the mail. No Bearer token is involved:
/// the admin gate replaces the capability check.
pub(crate) async fn delete_temp_inbox(
    State(s): S,
    Extension(u): Extension<CurrentUser>,
    Path(id): Path<String>,
) -> ApiResult {
    require_admin(&s.config, &u.email)?;
    let row: Option<(String, String)> =
        sqlx::query_as("SELECT address, status FROM temporary_inboxes WHERE id=?")
            .bind(&id)
            .fetch_optional(&s.pool)
            .await?;
    let Some((address, status)) = row else {
        return Err(ApiError(StatusCode::NOT_FOUND, "not_found"));
    };
    if status != "active" {
        return Err(ApiError(StatusCode::CONFLICT, "temp_inbox_not_active"));
    }
    let Some(deleted_at) = crate::temp::mark_deletion_pending(&s.pool, &id).await? else {
        return Err(ApiError(StatusCode::CONFLICT, "temp_inbox_not_active"));
    };
    Ok(Json(json!({
        "id": id, "address": address, "status": "deleted", "deleted_at": deleted_at,
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
        // older than 7 days (counts in neither window). The failed one
        // carries a relay rejection reason.
        for (id, key_id, template, status, created) in [
            ("req-1", "key-alpha", "auth.verify", "accepted", now.clone()),
            ("req-2", "key-alpha", "auth.recovery", "failed", now.clone()),
            ("req-3", "key-alpha", "auth.verify", "accepted", old.clone()),
        ] {
            let error: Option<&str> = if id == "req-2" {
                Some("recipient address rejected")
            } else {
                None
            };
            sqlx::query("INSERT INTO integration_email_requests (id,key_id,idempotency_key,template_id,recipient_hash,request_hash,status,error_message,created_at,updated_at) VALUES (?,?,?,?,?,?,?,?,?,?)")
                .bind(id).bind(key_id).bind(format!("idem-{id}")).bind(template)
                .bind("b".repeat(64)).bind("c".repeat(64)).bind(status).bind(error).bind(&created).bind(&created)
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
        // The failed request carries its relay rejection reason; an
        // accepted one has no reason.
        let failed = v["delivery"]["transactional_recent"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["status"] == "failed")
            .expect("a failed request must be listed");
        assert_eq!(failed["error_message"], "recipient address rejected");
        let accepted = v["delivery"]["transactional_recent"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["status"] == "accepted")
            .expect("an accepted request must be listed");
        assert!(accepted["error_message"].is_null());
        // The registry is exposed so the dashboard can offer the apps
        // and their scopes in the create-key form.
        let apps = v["delivery"]["integration_apps"].as_array().unwrap();
        assert!(apps.iter().any(|a| a["id"] == "alpha"));
        let alpha = apps.iter().find(|a| a["id"] == "alpha").unwrap();
        assert_eq!(alpha["host"], "alpha.example.com");
        assert!(alpha["templates"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t == "auth.verify"));
        assert_eq!(v["delivery"]["user_sends_last_24h"], 2);
        assert_eq!(
            v["delivery"]["integration_keys"].as_array().unwrap().len(),
            2
        );
        // key_id is the public identifier the UI needs to revoke a key;
        // the token hash is never exposed.
        assert!(v["delivery"]["integration_keys"][0].get("key_id").is_some());
        assert!(v["delivery"]["integration_keys"][0]
            .get("token_hash")
            .is_none());

        assert_eq!(v["temp_inboxes"]["by_status"]["active"]["count"], 1);
        assert_eq!(v["temp_inboxes"]["by_status"]["expired"]["count"], 1);
        assert_eq!(v["temp_inboxes"]["created_last_24h"], 2);
        let active_list = v["temp_inboxes"]["active_list"].as_array().unwrap();
        assert_eq!(active_list.len(), 1);
        assert_eq!(active_list[0]["address"], "aaa@example.test");
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

    /// The seeded admin, with the real `u-admin` id the seed uses (the
    /// `user()` helper fabricates `u-{email}`, which matches no row).
    fn admin() -> CurrentUser {
        CurrentUser {
            user_id: "u-admin".into(),
            email: "admin@example.test".into(),
            creds: Creds {
                user: "admin@example.test".into(),
                pass: "secret".into(),
            },
        }
    }

    #[tokio::test]
    async fn disable_then_enable_user() {
        let s = test_state(vec!["admin@example.test".into()]).await;
        seed(&s).await;
        let resp = disable_user(State(s.clone()), Extension(admin()), Path("u-alice".into()))
            .await
            .unwrap();
        assert_eq!(body_json(resp).await["status"], "disabled");
        let status: String = sqlx::query_scalar("SELECT status FROM app_users WHERE id='u-alice'")
            .fetch_one(&s.pool)
            .await
            .unwrap();
        assert_eq!(status, "disabled");
        // A second disable hears 409 instead of silently succeeding.
        let err = disable_user(State(s.clone()), Extension(admin()), Path("u-alice".into()))
            .await
            .expect_err("disabling twice must fail");
        assert_eq!(err.0, StatusCode::CONFLICT);
        assert_eq!(err.1, "user_not_active");
        // Unknown user: 404.
        let err = disable_user(
            State(s.clone()),
            Extension(admin()),
            Path("u-nobody".into()),
        )
        .await
        .expect_err("unknown user must be 404");
        assert_eq!(err.0, StatusCode::NOT_FOUND);
        // The admin cannot disable their own account.
        let err = disable_user(State(s.clone()), Extension(admin()), Path("u-admin".into()))
            .await
            .expect_err("self-disable must fail");
        assert_eq!(err.0, StatusCode::CONFLICT);
        assert_eq!(err.1, "cannot_disable_self");
        // Re-enable restores the account, and a second enable is 409.
        let resp = enable_user(State(s.clone()), Extension(admin()), Path("u-alice".into()))
            .await
            .unwrap();
        assert_eq!(body_json(resp).await["status"], "active");
        let err = enable_user(State(s.clone()), Extension(admin()), Path("u-alice".into()))
            .await
            .expect_err("enabling twice must fail");
        assert_eq!(err.0, StatusCode::CONFLICT);
        assert_eq!(err.1, "user_not_disabled");
    }

    #[tokio::test]
    async fn admin_actions_refuse_non_admins() {
        let s = test_state(vec!["admin@example.test".into()]).await;
        seed(&s).await;
        for result in [
            disable_user(
                State(s.clone()),
                Extension(user("alice@example.test")),
                Path("u-bob".into()),
            )
            .await,
            enable_user(
                State(s.clone()),
                Extension(user("alice@example.test")),
                Path("u-bob".into()),
            )
            .await,
            revoke_user_sessions(
                State(s.clone()),
                Extension(user("alice@example.test")),
                Path("u-bob".into()),
            )
            .await,
            revoke_integration_key(
                State(s.clone()),
                Extension(user("alice@example.test")),
                Path("a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6".into()),
            )
            .await,
            delete_temp_inbox(
                State(s.clone()),
                Extension(user("alice@example.test")),
                Path("t-1".into()),
            )
            .await,
        ] {
            let err = result.expect_err("non-admins must be refused");
            assert_eq!(err.0, StatusCode::FORBIDDEN);
            assert_eq!(err.1, "forbidden");
        }
    }

    #[tokio::test]
    async fn revoke_sessions_signs_user_out_everywhere() {
        let s = test_state(vec!["admin@example.test".into()]).await;
        seed(&s).await;
        // Alice has exactly one live session (one expired, one revoked).
        let resp =
            revoke_user_sessions(State(s.clone()), Extension(admin()), Path("u-alice".into()))
                .await
                .unwrap();
        assert_eq!(body_json(resp).await["revoked"], 1);
        let live: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM auth_sessions WHERE user_id='u-alice' AND revoked_at IS NULL AND expires_at>?",
        )
        .bind(auth::now())
        .fetch_one(&s.pool)
        .await
        .unwrap();
        assert_eq!(live, 0);
        // Nothing left to revoke: still a success, with a zero count.
        let resp =
            revoke_user_sessions(State(s.clone()), Extension(admin()), Path("u-alice".into()))
                .await
                .unwrap();
        assert_eq!(body_json(resp).await["revoked"], 0);
        // Unknown user: 404.
        let err = revoke_user_sessions(
            State(s.clone()),
            Extension(admin()),
            Path("u-nobody".into()),
        )
        .await
        .expect_err("unknown user must be 404");
        assert_eq!(err.0, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn revoke_key_kills_authentication() {
        let s = test_state(vec!["admin@example.test".into()]).await;
        seed(&s).await;
        // A real key id (32 lowercase hex); the seed's "key-alpha" is
        // not a valid id shape for the revoke path.
        let key_id = "a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6";
        let now = auth::now();
        sqlx::query("INSERT INTO integration_keys (key_id,app_id,token_hash,scopes_json,status,created_at,daily_send_limit) VALUES (?,'alpha',?,'[]','active',?,100)")
            .bind(key_id).bind("a".repeat(64)).bind(&now)
            .execute(&s.pool).await.unwrap();
        let resp =
            revoke_integration_key(State(s.clone()), Extension(admin()), Path(key_id.into()))
                .await
                .unwrap();
        let v = body_json(resp).await;
        assert_eq!(v["status"], "revoked");
        assert_eq!(v["app_id"], "alpha");
        // The exact lookup the transactional endpoint authenticates
        // with no longer finds the key.
        let found: Option<String> = sqlx::query_scalar(
            "SELECT key_id FROM integration_keys WHERE key_id=? AND status='active'",
        )
        .bind(key_id)
        .fetch_optional(&s.pool)
        .await
        .unwrap();
        assert!(found.is_none());
        // A second revoke hears 409.
        let err = revoke_integration_key(State(s.clone()), Extension(admin()), Path(key_id.into()))
            .await
            .expect_err("revoking twice must fail");
        assert_eq!(err.0, StatusCode::CONFLICT);
        assert_eq!(err.1, "key_not_active");
        // Malformed id: 400. Well-formed but unknown: 404.
        let err = revoke_integration_key(
            State(s.clone()),
            Extension(admin()),
            Path("not-a-key".into()),
        )
        .await
        .expect_err("malformed key id must be 400");
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert_eq!(err.1, "invalid_key_id");
        let err = revoke_integration_key(
            State(s.clone()),
            Extension(admin()),
            Path("ffffffffffffffffffffffffffffffff".into()),
        )
        .await
        .expect_err("unknown key must be 404");
        assert_eq!(err.0, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn admin_temp_delete() {
        let s = test_state(vec!["admin@example.test".into()]).await;
        seed(&s).await;
        let resp = delete_temp_inbox(State(s.clone()), Extension(admin()), Path("t-1".into()))
            .await
            .unwrap();
        let v = body_json(resp).await;
        assert_eq!(v["status"], "deleted");
        assert_eq!(v["address"], "aaa@example.test");
        let status: String =
            sqlx::query_scalar("SELECT status FROM temporary_inboxes WHERE id='t-1'")
                .fetch_one(&s.pool)
                .await
                .unwrap();
        assert_eq!(status, "deletion_pending");
        // Already deleted: 409. Unknown: 404.
        let err = delete_temp_inbox(State(s.clone()), Extension(admin()), Path("t-1".into()))
            .await
            .expect_err("deleting twice must fail");
        assert_eq!(err.0, StatusCode::CONFLICT);
        assert_eq!(err.1, "temp_inbox_not_active");
        let err = delete_temp_inbox(
            State(s.clone()),
            Extension(admin()),
            Path("t-nobody".into()),
        )
        .await
        .expect_err("unknown inbox must be 404");
        assert_eq!(err.0, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn admin_routes_answer_401_without_a_session() {
        use axum::{
            body::Body,
            http::Request,
            middleware,
            routing::{delete, get, post, put},
            Router,
        };
        use tower::ServiceExt;
        let s = test_state(vec!["admin@example.test".into()]).await;
        // The real `require_session` middleware, on the same routes the
        // app serves: no cookie, no session, no handler runs.
        let app = Router::new()
            .route("/api/v1/admin/users/:id/disable", post(disable_user))
            .route("/api/v1/admin/users/:id/enable", post(enable_user))
            .route(
                "/api/v1/admin/users/:id/revoke-sessions",
                post(revoke_user_sessions),
            )
            .route(
                "/api/v1/admin/integration-keys/:key_id/revoke",
                post(revoke_integration_key),
            )
            .route(
                "/api/v1/admin/integration-keys",
                post(create_integration_key),
            )
            .route(
                "/api/v1/admin/integration-apps",
                get(list_integration_apps).post(create_integration_app),
            )
            .route(
                "/api/v1/admin/integration-apps/:id",
                put(update_integration_app).delete(delete_integration_app),
            )
            .route(
                "/api/v1/admin/temp-inboxes/:id/delete",
                post(delete_temp_inbox),
            )
            .route_layer(middleware::from_fn_with_state(
                s.clone(),
                crate::require_session,
            ))
            .with_state(s);
        for uri in [
            "/api/v1/admin/users/u-alice/disable",
            "/api/v1/admin/users/u-alice/enable",
            "/api/v1/admin/users/u-alice/revoke-sessions",
            "/api/v1/admin/integration-keys/a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6/revoke",
            "/api/v1/admin/temp-inboxes/t-1/delete",
            "/api/v1/admin/integration-apps/alpha",
        ] {
            let req = Request::builder()
                .method("POST")
                .uri(uri)
                .body(Body::empty())
                .unwrap();
            let resp = app.clone().oneshot(req).await.unwrap();
            assert_eq!(resp.status(), StatusCode::UNAUTHORIZED, "{uri} must be 401");
        }
        // The create-key route takes a JSON body, so it needs a body
        // on the request; without a session it must still be 401.
        let req = Request::builder()
            .method("POST")
            .uri("/api/v1/admin/integration-keys")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"app":"alpha","expires_days":30,"daily_limit":100}"#,
            ))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    fn key_body(
        app: &str,
        expires_days: u32,
        daily_limit: u32,
        scopes: Option<Vec<String>>,
    ) -> CreateIntegrationKeyBody {
        CreateIntegrationKeyBody {
            app: app.into(),
            expires_days,
            daily_limit,
            scopes,
        }
    }

    #[tokio::test]
    async fn admin_creates_a_key_and_the_secret_is_shown_once_only() {
        let s = test_state(vec!["admin@example.test".into()]).await;
        let resp = create_integration_key(
            State(s.clone()),
            Extension(admin()),
            Json(key_body("alpha", 30, 100, None)),
        )
        .await
        .expect("admin must be able to create a key");
        let v = body_json(resp).await;
        // Omitted scopes default to every scope the app may hold.
        assert_eq!(
            v["scopes"],
            json!([
                "email:template:auth.verify",
                "email:template:auth.recovery",
                "email:status"
            ])
        );
        assert_eq!(v["app"], "alpha");
        assert_eq!(v["daily_limit"], 100);
        assert!(v["expires_at"].is_string());
        let key_id = v["key_id"].as_str().unwrap().to_string();
        assert_eq!(key_id.len(), 32);
        let secret = v["secret"].as_str().unwrap().to_string();
        assert!(secret.starts_with("iris1_"));
        // Only the hash is stored, never the plaintext secret.
        let stored_hash: String =
            sqlx::query_scalar("SELECT token_hash FROM integration_keys WHERE key_id=?")
                .bind(&key_id)
                .fetch_one(&s.pool)
                .await
                .unwrap();
        assert_eq!(stored_hash, auth::hash_token(&secret));
        assert_ne!(stored_hash, secret);
        // The summary never exposes the secret afterwards.
        let summary_resp = summary(State(s.clone()), Extension(admin())).await.unwrap();
        let summary_json = body_json(summary_resp).await;
        let listed = summary_json["delivery"]["integration_keys"]
            .as_array()
            .unwrap()
            .iter()
            .find(|k| k["key_id"] == key_id)
            .expect("the new key must be listed");
        assert!(listed.get("secret").is_none());
        assert!(listed.get("token").is_none());
        assert!(listed.get("token_hash").is_none());
    }

    #[tokio::test]
    async fn non_admin_cannot_create_a_key() {
        let s = test_state(vec!["admin@example.test".into()]).await;
        let err = create_integration_key(
            State(s),
            Extension(user("alice@example.test")),
            Json(key_body("alpha", 30, 100, None)),
        )
        .await
        .expect_err("a non-admin must be refused");
        assert_eq!(err.0, StatusCode::FORBIDDEN);
        assert_eq!(err.1, "forbidden");
    }

    #[tokio::test]
    async fn create_key_validates_input() {
        let s = test_state(vec!["admin@example.test".into()]).await;
        let cases: Vec<(CreateIntegrationKeyBody, &str)> = vec![
            (key_body("", 30, 100, None), "invalid_app"),
            (key_body("Bad App!", 30, 100, None), "invalid_app"),
            (
                key_body("a".repeat(65).as_str(), 30, 100, None),
                "invalid_app",
            ),
            (key_body("unknownapp", 30, 100, None), "unsupported_app"),
            (key_body("alpha", 0, 100, None), "invalid_limits"),
            (key_body("alpha", 366, 100, None), "invalid_limits"),
            (key_body("alpha", 30, 0, None), "invalid_limits"),
            (key_body("alpha", 30, 10_001, None), "invalid_limits"),
            (
                key_body(
                    "alpha",
                    30,
                    100,
                    Some(vec!["email:template:auth.login_link".into()]),
                ),
                "invalid_scopes",
            ),
            (
                key_body("alpha", 30, 100, Some(vec!["email:status".into()])),
                "invalid_scopes",
            ),
            (key_body("alpha", 30, 100, Some(vec![])), "invalid_scopes"),
            (
                key_body(
                    "alpha",
                    30,
                    100,
                    Some(vec![
                        "email:template:auth.verify".into(),
                        "email:template:auth.verify".into(),
                    ]),
                ),
                "invalid_scopes",
            ),
        ];
        for (body, code) in cases {
            let err = create_integration_key(State(s.clone()), Extension(admin()), Json(body))
                .await
                .expect_err("bad input must be refused");
            assert_eq!(err.0, StatusCode::BAD_REQUEST, "expected 400 for {code}");
            assert_eq!(err.1, code);
        }
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM integration_keys")
            .fetch_one(&s.pool)
            .await
            .unwrap();
        assert_eq!(rows, 0, "no key may be minted for refused input");
    }

    fn app_body(id: &str, host: &str) -> CreateIntegrationAppBody {
        CreateIntegrationAppBody {
            id: id.into(),
            host: host.into(),
            templates: vec![IntegrationAppTemplateBody {
                id: "auth.verify".into(),
                subject: "Verify".into(),
                intro: "Verify.".into(),
                closing: "Ignore.".into(),
            }],
        }
    }

    fn update_body(host: &str, template_ids: &[&str]) -> UpdateIntegrationAppBody {
        UpdateIntegrationAppBody {
            host: host.into(),
            templates: template_ids
                .iter()
                .map(|id| IntegrationAppTemplateBody {
                    id: (*id).into(),
                    subject: format!("Subject for {id}"),
                    intro: "Intro.".into(),
                    closing: "Closing.".into(),
                })
                .collect(),
        }
    }

    #[tokio::test]
    async fn integration_app_crud_and_validation() {
        let s = test_state(vec!["admin@example.test".into()]).await;
        // Create: 201, stored in both tables, and visible to the
        // registry at once (a key can be minted for it right away).
        let resp = create_integration_app(
            State(s.clone()),
            Extension(admin()),
            Json(app_body("myapp", "myapp.example.com")),
        )
        .await
        .expect("admin must be able to register an app");
        assert_eq!(resp.status(), StatusCode::CREATED);
        let v = body_json(resp).await;
        assert_eq!(v["id"], "myapp");
        assert_eq!(v["host"], "myapp.example.com");
        assert_eq!(v["templates"][0]["id"], "auth.verify");
        assert!(v["created_at"].is_string());
        assert!(crate::integration_apps::app(&s.registry.read().await, "myapp").is_some());

        // The list endpoint shows the app with its full templates.
        let resp = list_integration_apps(State(s.clone()), Extension(admin()))
            .await
            .unwrap();
        let listed = body_json(resp).await["apps"].clone();
        assert_eq!(listed.as_array().unwrap().len(), 1);
        assert_eq!(listed[0]["templates"][0]["subject"], "Verify");

        // Duplicates: same id, or the same host under another id.
        for (body, code) in [
            (app_body("myapp", "other.example.com"), "duplicate_app"),
            (app_body("other", "myapp.example.com"), "duplicate_host"),
        ] {
            let err = create_integration_app(State(s.clone()), Extension(admin()), Json(body))
                .await
                .expect_err("duplicates must be refused");
            assert_eq!(err.0, StatusCode::CONFLICT);
            assert_eq!(err.1, code);
        }
        // Invalid input: bad id, bad host, no templates, five
        // templates, a bad template id.
        let mut bad_id = app_body("Bad App!", "bad.example.com");
        bad_id.id = "Bad App!".into();
        let mut bad_host = app_body("badhost", "https://bad.example.com");
        bad_host.host = "https://bad.example.com".into();
        let mut no_templates = app_body("notpl", "notpl.example.com");
        no_templates.templates = vec![];
        let mut five = app_body("five", "five.example.com");
        five.templates = (0..5)
            .map(|i| IntegrationAppTemplateBody {
                id: format!("t{i}"),
                subject: "S".into(),
                intro: "I".into(),
                closing: "C".into(),
            })
            .collect();
        let mut bad_template = app_body("badtpl", "badtpl.example.com");
        bad_template.templates[0].id = "Bad Template".into();
        for (body, code) in [
            (bad_id, "invalid_app"),
            (bad_host, "invalid_host"),
            (no_templates, "invalid_templates"),
            (five, "invalid_templates"),
            (bad_template, "invalid_templates"),
        ] {
            let err = create_integration_app(State(s.clone()), Extension(admin()), Json(body))
                .await
                .expect_err("invalid input must be refused");
            assert_eq!(err.0, StatusCode::BAD_REQUEST, "expected 400 for {code}");
            assert_eq!(err.1, code);
        }

        // Update: host and templates are replaced wholesale; the old
        // template is gone and the id stays put.
        let resp = update_integration_app(
            State(s.clone()),
            Extension(admin()),
            Path("myapp".into()),
            Json(update_body("renamed.example.com", &["auth.recovery"])),
        )
        .await
        .expect("admin must be able to update an app");
        let v = body_json(resp).await;
        assert_eq!(v["host"], "renamed.example.com");
        assert_eq!(v["templates"][0]["id"], "auth.recovery");
        let stored: Vec<String> = sqlx::query_scalar(
            "SELECT template_id FROM integration_app_templates WHERE app_id='myapp'",
        )
        .fetch_all(&s.pool)
        .await
        .unwrap();
        assert_eq!(stored, vec!["auth.recovery"]);
        // Updating an unknown app is 404; taking another app's host
        // is 409.
        let err = update_integration_app(
            State(s.clone()),
            Extension(admin()),
            Path("nosuchapp".into()),
            Json(update_body("x.example.com", &["auth.verify"])),
        )
        .await
        .expect_err("unknown app must be 404");
        assert_eq!(err.0, StatusCode::NOT_FOUND);
        assert_eq!(err.1, "unknown_app");
        create_integration_app(
            State(s.clone()),
            Extension(admin()),
            Json(app_body("second", "second.example.com")),
        )
        .await
        .unwrap();
        let err = update_integration_app(
            State(s.clone()),
            Extension(admin()),
            Path("myapp".into()),
            Json(update_body("second.example.com", &["auth.recovery"])),
        )
        .await
        .expect_err("taking another app's host must fail");
        assert_eq!(err.0, StatusCode::CONFLICT);
        assert_eq!(err.1, "duplicate_host");

        // Delete removes the app and its templates, and the registry
        // forgets it at once.
        let resp =
            delete_integration_app(State(s.clone()), Extension(admin()), Path("myapp".into()))
                .await
                .unwrap();
        assert_eq!(body_json(resp).await["deleted"], true);
        assert!(crate::integration_apps::app(&s.registry.read().await, "myapp").is_none());
        let remaining: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM integration_app_templates WHERE app_id='myapp'",
        )
        .fetch_one(&s.pool)
        .await
        .unwrap();
        assert_eq!(remaining, 0);
        let err =
            delete_integration_app(State(s.clone()), Extension(admin()), Path("myapp".into()))
                .await
                .expect_err("deleting twice must fail");
        assert_eq!(err.0, StatusCode::NOT_FOUND);
        assert_eq!(err.1, "unknown_app");
    }

    #[tokio::test]
    async fn delete_app_with_active_keys_is_refused_until_they_are_gone() {
        let s = test_state(vec!["admin@example.test".into()]).await;
        create_integration_app(
            State(s.clone()),
            Extension(admin()),
            Json(app_body("keyed", "keyed.example.com")),
        )
        .await
        .unwrap();
        let created = create_integration_key(
            State(s.clone()),
            Extension(admin()),
            Json(key_body("keyed", 30, 100, None)),
        )
        .await
        .expect("a key for the new app must be mintable");
        let key_id = body_json(created).await["key_id"]
            .as_str()
            .unwrap()
            .to_string();
        let err =
            delete_integration_app(State(s.clone()), Extension(admin()), Path("keyed".into()))
                .await
                .expect_err("an app with a live key must not be deletable");
        assert_eq!(err.0, StatusCode::CONFLICT);
        assert_eq!(err.1, "app_has_active_keys");
        // Revoking the key unblocks the delete.
        revoke_integration_key(State(s.clone()), Extension(admin()), Path(key_id))
            .await
            .unwrap();
        delete_integration_app(State(s.clone()), Extension(admin()), Path("keyed".into()))
            .await
            .expect("delete must succeed once the key is revoked");
        // An expired key does not block either: the key row stays
        // `active` in the table, but its expiry is past.
        create_integration_app(
            State(s.clone()),
            Extension(admin()),
            Json(app_body("stale", "stale.example.com")),
        )
        .await
        .unwrap();
        let past = (chrono::Utc::now() - chrono::Duration::days(1)).to_rfc3339();
        sqlx::query("INSERT INTO integration_keys (key_id,app_id,token_hash,scopes_json,status,created_at,expires_at,daily_send_limit) VALUES ('bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb','stale',?,'[]','active',?, ?,100)")
            .bind("b".repeat(64)).bind(auth::now()).bind(&past)
            .execute(&s.pool).await.unwrap();
        delete_integration_app(State(s.clone()), Extension(admin()), Path("stale".into()))
            .await
            .expect("an expired key must not block the delete");
    }

    #[tokio::test]
    async fn summary_exposes_the_database_registry() {
        let s = test_state(vec!["admin@example.test".into()]).await;
        // Seed a custom app straight into the tables, then refresh
        // the in-memory registry from the database, the way the app
        // does at startup and after a mutation.
        let now = auth::now();
        sqlx::query("INSERT INTO integration_apps (id, host, created_at) VALUES ('dbapp','dbapp.example.com',?)")
            .bind(&now).execute(&s.pool).await.unwrap();
        sqlx::query("INSERT INTO integration_app_templates (app_id, template_id, subject, intro, closing) VALUES ('dbapp','auth.verify','Verify','Verify.','Ignore.')")
            .execute(&s.pool).await.unwrap();
        *s.registry.write().await = crate::integration_apps::load_apps(&s.pool).await.unwrap();
        let resp = summary(State(s.clone()), Extension(admin())).await.unwrap();
        let apps = body_json(resp).await["delivery"]["integration_apps"]
            .as_array()
            .unwrap()
            .clone();
        // The snapshot mirrors the database: the custom app is there,
        // and the example registry is gone.
        assert!(apps.iter().any(|a| a["id"] == "dbapp"));
        assert!(apps.iter().all(|a| a["id"] != "alpha"));
        let dbapp = apps.iter().find(|a| a["id"] == "dbapp").unwrap();
        assert_eq!(dbapp["host"], "dbapp.example.com");
        assert!(dbapp["templates"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t == "auth.verify"));
    }

    #[tokio::test]
    async fn key_creation_works_for_a_database_only_app() {
        let s = test_state(vec!["admin@example.test".into()]).await;
        create_integration_app(
            State(s.clone()),
            Extension(admin()),
            Json(app_body("dbonly", "dbonly.example.com")),
        )
        .await
        .unwrap();
        let resp = create_integration_key(
            State(s.clone()),
            Extension(admin()),
            Json(key_body("dbonly", 30, 100, None)),
        )
        .await
        .expect("a key for a dashboard-registered app must be mintable");
        let v = body_json(resp).await;
        assert_eq!(v["app"], "dbonly");
        assert_eq!(
            v["scopes"],
            json!(["email:template:auth.verify", "email:status"])
        );
        assert!(v["secret"].as_str().unwrap().starts_with("iris1_"));
    }

    #[tokio::test]
    async fn non_admin_cannot_manage_apps() {
        let s = test_state(vec!["admin@example.test".into()]).await;
        for result in [
            list_integration_apps(State(s.clone()), Extension(user("alice@example.test"))).await,
            create_integration_app(
                State(s.clone()),
                Extension(user("alice@example.test")),
                Json(app_body("nope", "nope.example.com")),
            )
            .await,
            update_integration_app(
                State(s.clone()),
                Extension(user("alice@example.test")),
                Path("alpha".into()),
                Json(update_body("x.example.com", &["auth.verify"])),
            )
            .await,
            delete_integration_app(
                State(s.clone()),
                Extension(user("alice@example.test")),
                Path("alpha".into()),
            )
            .await,
        ] {
            let err = result.expect_err("non-admins must be refused");
            assert_eq!(err.0, StatusCode::FORBIDDEN);
            assert_eq!(err.1, "forbidden");
        }
    }

    #[tokio::test]
    async fn same_app_may_hold_several_active_keys() {
        let s = test_state(vec!["admin@example.test".into()]).await;
        let mut ids = Vec::new();
        for _ in 0..2 {
            let resp = create_integration_key(
                State(s.clone()),
                Extension(admin()),
                Json(key_body("alpha", 30, 100, None)),
            )
            .await
            .expect("a second key for the same app must be allowed, like the CLI");
            let v = body_json(resp).await;
            ids.push(v["key_id"].as_str().unwrap().to_string());
        }
        assert_ne!(ids[0], ids[1]);
        let active: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM integration_keys WHERE app_id='alpha' AND status='active'",
        )
        .fetch_one(&s.pool)
        .await
        .unwrap();
        assert_eq!(active, 2);
    }
}
