use axum::{
    extract::{Path, State},
    http::{header, HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use iris_auth as auth;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::SqlitePool;
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq)]
struct IntegrationPrincipal {
    key_id: String,
    app_id: String,
    scopes: Vec<String>,
}

async fn authenticate(
    pool: &SqlitePool,
    token: &str,
    required_scope: &str,
) -> Result<IntegrationPrincipal, StatusCode> {
    let parsed = auth::parse_api_key(token).ok_or(StatusCode::UNAUTHORIZED)?;
    let row: Option<(String, String, String, Option<String>)> = sqlx::query_as(
        "SELECT token_hash, app_id, scopes_json, expires_at FROM integration_keys WHERE key_id=? AND status='active'",
    )
    .bind(&parsed.key_id)
    .fetch_optional(pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let Some((stored_hash, app_id, scopes_json, expires_at)) = row else {
        return Err(StatusCode::UNAUTHORIZED);
    };
    if !auth::ct_eq(&parsed.token_hash, &stored_hash) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    if expires_at.as_deref().is_some_and(|value| {
        chrono::DateTime::parse_from_rfc3339(value)
            .map(|expiry| expiry <= chrono::Utc::now())
            .unwrap_or(true)
    }) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let scopes: Vec<String> = serde_json::from_str(&scopes_json)
        .map_err(|_| StatusCode::UNAUTHORIZED)?;
    if !scopes.iter().any(|scope| scope == required_scope) {
        return Err(StatusCode::FORBIDDEN);
    }
    sqlx::query("UPDATE integration_keys SET last_used_at=? WHERE key_id=?")
        .bind(auth::now())
        .bind(&parsed.key_id)
        .execute(pool)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(IntegrationPrincipal {
        key_id: parsed.key_id,
        app_id,
        scopes,
    })
}

fn action_url_allowed(app_id: &str, raw_url: &str) -> bool {
    let allowed_host = match app_id {
        "gencv" => "gencv.meowlabs.id",
        "academy" => "academy.meowlabs.id",
        "aksaria" => "aksaria.meowlabs.id",
        _ => return false,
    };

    let Ok(url) = reqwest::Url::parse(raw_url) else {
        return false;
    };

    url.scheme() == "https"
        && url.host_str() == Some(allowed_host)
        && url.port_or_known_default() == Some(443)
        && url.username().is_empty()
        && url.password().is_none()
}

fn valid_recipient(value: &str) -> bool {
    if value.len() > 254 || value.bytes().any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control() || matches!(byte, b'<' | b'>')) {
        return false;
    }
    let mut parts = value.split('@');
    let Some(local) = parts.next() else { return false; };
    let Some(domain) = parts.next() else { return false; };
    let valid_local = !local.starts_with('.')
        && !local.ends_with('.')
        && !local.contains("..")
        && local.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#' | b'$' | b'%' | b'&' | b'\'' | b'*' | b'+' | b'-' | b'/' | b'='
                        | b'?' | b'^' | b'_' | b'`' | b'{' | b'|' | b'}' | b'~' | b'.'
                )
        });
    if parts.next().is_some()
        || local.is_empty()
        || local.len() > 64
        || !valid_local
        || domain.is_empty()
    {
        return false;
    }
    let labels: Vec<_> = domain.split('.').collect();
    labels.len() >= 2 && labels.iter().all(|label| {
        !label.is_empty() && label.len() <= 63
            && label.as_bytes()[0].is_ascii_alphanumeric()
            && label.as_bytes()[label.len() - 1].is_ascii_alphanumeric()
            && label.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    })
}

fn render_message(
    app_id: &str,
    template_id: &str,
    recipient: &str,
    action_url: &str,
) -> Result<iris_mail::Compose, &'static str> {
    if !valid_recipient(recipient) || !action_url_allowed(app_id, action_url) {
        return Err("invalid request");
    }
    let (subject, intro, closing) = match (app_id, template_id) {
        ("gencv", "auth.verify") => (
            "Verify your GenCV email",
            "Verify your email address to finish setting up GenCV.",
            "If you did not create a GenCV account, ignore this message.",
        ),
        ("gencv", "auth.recovery") => (
            "Reset your GenCV password",
            "Use this link to reset your GenCV password.",
            "If you did not request a password reset, ignore this message.",
        ),
        ("academy", "auth.login_link") => (
            "Your MeowLabs Academy sign-in link",
            "Use this one-time link to sign in to MeowLabs Academy.",
            "If you did not request this link, ignore this message.",
        ),
        ("aksaria", "auth.recovery") => (
            "Reset kata sandi Aksaria",
            "Gunakan tautan ini untuk mengatur ulang kata sandi Aksaria.",
            "Jika kamu tidak meminta reset ini, abaikan pesan ini.",
        ),
        _ => return Err("unknown template"),
    };
    Ok(iris_mail::Compose {
        to: vec![recipient.to_string()],
        subject: subject.to_string(),
        text: format!("{intro}\n\n{action_url}\n\n{closing}"),
        ..Default::default()
    })
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SendEmailBody {
    pub(crate) template_id: String,
    pub(crate) recipient: String,
    pub(crate) action_url: String,
}

fn send_response(status: StatusCode, request_id: &str, delivery_status: &str) -> crate::ApiResult {
    Ok((status, Json(json!({ "request_id": request_id, "status": delivery_status }))).into_response())
}

fn auth_error(status: StatusCode) -> crate::ApiError {
    let code = if status == StatusCode::FORBIDDEN {
        "forbidden"
    } else if status == StatusCode::INTERNAL_SERVER_ERROR {
        "internal_error"
    } else {
        "unauthorized"
    };
    crate::ApiError(status, code)
}

pub async fn send(
    State(state): State<Arc<crate::AppState>>,
    headers: HeaderMap,
    Json(body): Json<SendEmailBody>,
) -> crate::ApiResult {
    let token = headers.get(header::AUTHORIZATION).and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .ok_or(crate::ApiError(StatusCode::UNAUTHORIZED, "unauthorized"))?;
    let required_scope = format!("email:template:{}", body.template_id);
    let principal = authenticate(&state.pool, token, &required_scope).await.map_err(auth_error)?;
    if !state.config.integration_email_enabled
        || state.relay.is_none()
        || !valid_recipient(&state.config.integration_email_from)
    {
        return Err(crate::ApiError(StatusCode::SERVICE_UNAVAILABLE, "delivery_disabled"));
    }
    let idempotency_key = headers.get("idempotency-key").and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty() && value.len() <= 128 && value.bytes().all(|byte| byte.is_ascii_graphic()))
        .ok_or(crate::ApiError(StatusCode::BAD_REQUEST, "invalid_idempotency_key"))?;
    let mut message = render_message(&principal.app_id, &body.template_id, &body.recipient, &body.action_url)
        .map_err(|_| crate::ApiError(StatusCode::BAD_REQUEST, "invalid_request"))?;
    message.from = state.config.integration_email_from.clone();

    let request_hash = auth::hash_token(&serde_json::to_string(&(principal.app_id.as_str(), &body))
        .map_err(|_| crate::ApiError(StatusCode::INTERNAL_SERVER_ERROR, "internal_error"))?);
    let recipient_hash = auth::hash_token(&body.recipient.trim().to_ascii_lowercase());
    let request_id = uuid::Uuid::new_v4().to_string();
    let now = auth::now();
    let mut tx = state.pool.begin().await.map_err(|_| crate::ApiError(StatusCode::INTERNAL_SERVER_ERROR, "internal_error"))?;
    let inserted = sqlx::query(
        "INSERT INTO integration_email_requests (id,key_id,idempotency_key,template_id,recipient_hash,request_hash,status,created_at,updated_at) VALUES (?,?,?,?,?,?, 'sending', ?,?) ON CONFLICT(key_id,idempotency_key) DO NOTHING",
    )
    .bind(&request_id).bind(&principal.key_id).bind(idempotency_key).bind(&body.template_id)
    .bind(&recipient_hash).bind(&request_hash).bind(&now).bind(&now)
    .execute(&mut *tx).await
    .map_err(|_| crate::ApiError(StatusCode::INTERNAL_SERVER_ERROR, "internal_error"))?;

    if inserted.rows_affected() == 0 {
        let existing: (String, String, String) = sqlx::query_as(
            "SELECT id,request_hash,status FROM integration_email_requests WHERE key_id=? AND idempotency_key=?",
        )
        .bind(&principal.key_id).bind(idempotency_key).fetch_one(&mut *tx).await
        .map_err(|_| crate::ApiError(StatusCode::INTERNAL_SERVER_ERROR, "internal_error"))?;
        tx.commit().await.map_err(|_| crate::ApiError(StatusCode::INTERNAL_SERVER_ERROR, "internal_error"))?;
        if !auth::ct_eq(&existing.1, &request_hash) {
            return Err(crate::ApiError(StatusCode::CONFLICT, "idempotency_conflict"));
        }
        return match existing.2.as_str() {
            "accepted" => send_response(StatusCode::ACCEPTED, &existing.0, "accepted"),
            "failed" => send_response(StatusCode::UNPROCESSABLE_ENTITY, &existing.0, "failed"),
            "unknown" | "sending" => {
                send_response(StatusCode::SERVICE_UNAVAILABLE, &existing.0, &existing.2)
            }
            _ => send_response(StatusCode::SERVICE_UNAVAILABLE, &existing.0, "unknown"),
        };
    }

    let utc_start = chrono::Utc::now().date_naive().and_hms_opt(0, 0, 0).unwrap().and_utc().to_rfc3339();
    let (sent_today, daily_limit): (i64, i64) = sqlx::query_as(
        "SELECT COUNT(r.id), k.daily_send_limit FROM integration_keys k LEFT JOIN integration_email_requests r ON r.key_id=k.key_id AND r.created_at>=? WHERE k.key_id=? GROUP BY k.key_id",
    )
    .bind(utc_start).bind(&principal.key_id).fetch_one(&mut *tx).await
    .map_err(|_| crate::ApiError(StatusCode::INTERNAL_SERVER_ERROR, "internal_error"))?;
    if sent_today > daily_limit {
        tx.rollback().await.map_err(|_| crate::ApiError(StatusCode::INTERNAL_SERVER_ERROR, "internal_error"))?;
        return Err(crate::ApiError(StatusCode::TOO_MANY_REQUESTS, "daily_limit_reached"));
    }
    tx.commit().await.map_err(|_| crate::ApiError(StatusCode::INTERNAL_SERVER_ERROR, "internal_error"))?;

    let Some(relay) = state.relay.as_ref() else {
        return Err(crate::ApiError(StatusCode::SERVICE_UNAVAILABLE, "delivery_disabled"));
    };
    match relay.send(&message).await {
        Ok(provider_id) if !provider_id.trim().is_empty() => {
            sqlx::query("UPDATE integration_email_requests SET status='accepted',provider_message_id=?,updated_at=? WHERE id=?")
                .bind(provider_id).bind(auth::now()).bind(&request_id).execute(&state.pool).await
                .map_err(|_| crate::ApiError(StatusCode::INTERNAL_SERVER_ERROR, "internal_error"))?;
            send_response(StatusCode::ACCEPTED, &request_id, "accepted")
        }
        Err(iris_mail::MailError::Rejected(_)) => {
            sqlx::query("UPDATE integration_email_requests SET status='failed',updated_at=? WHERE id=?")
                .bind(auth::now()).bind(&request_id).execute(&state.pool).await
                .map_err(|_| crate::ApiError(StatusCode::INTERNAL_SERVER_ERROR, "internal_error"))?;
            send_response(StatusCode::UNPROCESSABLE_ENTITY, &request_id, "failed")
        }
        _ => {
            sqlx::query("UPDATE integration_email_requests SET status='unknown',updated_at=? WHERE id=?")
                .bind(auth::now()).bind(&request_id).execute(&state.pool).await
                .map_err(|_| crate::ApiError(StatusCode::INTERNAL_SERVER_ERROR, "internal_error"))?;
            send_response(StatusCode::SERVICE_UNAVAILABLE, &request_id, "unknown")
        }
    }
}

pub async fn status(
    State(state): State<Arc<crate::AppState>>,
    headers: HeaderMap,
    Path(request_id): Path<String>,
) -> crate::ApiResult {
    let token = headers.get(header::AUTHORIZATION).and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .ok_or(crate::ApiError(StatusCode::UNAUTHORIZED, "unauthorized"))?;
    let principal = authenticate(&state.pool, token, "email:status").await.map_err(auth_error)?;
    let row: Option<String> = sqlx::query_scalar("SELECT status FROM integration_email_requests WHERE id=? AND key_id=?")
        .bind(&request_id).bind(&principal.key_id).fetch_optional(&state.pool).await
        .map_err(|_| crate::ApiError(StatusCode::INTERNAL_SERVER_ERROR, "internal_error"))?;
    match row {
        Some(delivery_status) => send_response(StatusCode::OK, &request_id, &delivery_status),
        None => Err(crate::ApiError(StatusCode::NOT_FOUND, "not_found")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iris_auth as auth;
    use serde_json::json;

    #[test]
    fn action_urls_are_restricted_to_the_exact_https_origin_for_each_app() {
        assert!(action_url_allowed("gencv", "https://gencv.meowlabs.id/verify?token=abc"));
        assert!(action_url_allowed("academy", "https://academy.meowlabs.id/masuk#token=abc"));
        assert!(!action_url_allowed("gencv", "https://academy.meowlabs.id/verify?token=abc"));
        assert!(!action_url_allowed("gencv", "http://gencv.meowlabs.id/verify?token=abc"));
        assert!(!action_url_allowed("gencv", "https://gencv.meowlabs.id.attacker.invalid/verify"));
        assert!(!action_url_allowed("gencv", "https://gencv.meowlabs.id@attacker.invalid/verify"));
        assert!(!action_url_allowed("gencv", "https://gencv.meowlabs.id:8443/verify"));
        assert!(!action_url_allowed("unknown", "https://gencv.meowlabs.id/verify"));
        assert!(action_url_allowed("aksaria", "https://aksaria.meowlabs.id/reset-sandi/?token=abc"));
        assert!(!action_url_allowed("aksaria", "https://gencv.meowlabs.id/reset-sandi/?token=abc"));
    }

    #[test]
    fn renders_only_registered_auth_templates_for_the_owning_app() {
        let message = render_message(
            "gencv",
            "auth.verify",
            "user@example.test",
            "https://gencv.meowlabs.id/verify?token=abc",
        )
        .expect("registered template should render");
        assert_eq!(message.to, ["user@example.test"]);
        assert_eq!(message.subject, "Verify your GenCV email");
        assert!(message.text.contains("https://gencv.meowlabs.id/verify?token=abc"));

        assert!(render_message(
            "gencv",
            "auth.login_link",
            "user@example.test",
            "https://gencv.meowlabs.id/verify?token=abc",
        )
        .is_err());
    }

    #[test]
    fn rejects_invalid_recipient_local_part_syntax() {
        for recipient in [
            "bad,other@example.test",
            "bad;local@example.test",
            "bad..local@example.test",
            ".leading@example.test",
            "trailing.@example.test",
        ] {
            assert!(!valid_recipient(recipient), "must reject {recipient}");
        }
        assert!(valid_recipient("first.last+auth@example.test"));
    }

    #[test]
    fn rejects_header_injection_in_recipient() {
        assert!(render_message(
            "gencv",
            "auth.verify",
            "user@example.test\r\nBcc: attacker@example.test",
            "https://gencv.meowlabs.id/verify?token=abc",
        )
        .is_err());
    }

    #[tokio::test]
    async fn accepts_an_active_key_with_the_required_scope() {
        let pool = iris_database::init_pool("sqlite::memory:").await.unwrap();
        let generated = auth::generate_api_key();
        let expiry = (chrono::Utc::now() + chrono::Duration::days(1)).to_rfc3339();
        sqlx::query("INSERT INTO integration_keys (key_id,app_id,token_hash,scopes_json,created_at,expires_at) VALUES (?,?,?,?,?,?)")
            .bind(&generated.key_id)
            .bind("gencv")
            .bind(&generated.token_hash)
            .bind(json!(["email:template:auth.verify"]).to_string())
            .bind(auth::now())
            .bind(expiry)
            .execute(&pool)
            .await
            .unwrap();

        let principal = authenticate(&pool, &generated.token, "email:template:auth.verify")
            .await
            .expect("valid scoped key must authenticate");
        assert_eq!(principal.key_id, generated.key_id);
        assert_eq!(principal.app_id, "gencv");
    }
}
