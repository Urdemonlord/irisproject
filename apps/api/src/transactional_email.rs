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
) -> Result<IntegrationPrincipal, crate::ApiError> {
    // Auth failures stay 401/403 with generic codes. A database failure
    // surfaces through `ApiError::from` instead, so a pool outage
    // answers 503 `database_unavailable` rather than masquerading as a
    // 500, and the detail is logged server-side rather than dropped.
    let unauthorized = || crate::ApiError(StatusCode::UNAUTHORIZED, "unauthorized");
    let parsed = auth::parse_api_key(token).ok_or_else(unauthorized)?;
    let row: Option<(String, String, String, Option<String>)> = sqlx::query_as(
        "SELECT token_hash, app_id, scopes_json, expires_at FROM integration_keys WHERE key_id=? AND status='active'",
    )
    .bind(&parsed.key_id)
    .fetch_optional(pool)
    .await?;
    let Some((stored_hash, app_id, scopes_json, expires_at)) = row else {
        return Err(unauthorized());
    };
    if !auth::ct_eq(&parsed.token_hash, &stored_hash) {
        return Err(unauthorized());
    }
    if expires_at.as_deref().is_some_and(|value| {
        chrono::DateTime::parse_from_rfc3339(value)
            .map(|expiry| expiry <= chrono::Utc::now())
            .unwrap_or(true)
    }) {
        return Err(unauthorized());
    }
    let scopes: Vec<String> = serde_json::from_str(&scopes_json).map_err(|_| unauthorized())?;
    if !scopes.iter().any(|scope| scope == required_scope) {
        return Err(crate::ApiError(StatusCode::FORBIDDEN, "forbidden"));
    }
    sqlx::query("UPDATE integration_keys SET last_used_at=? WHERE key_id=?")
        .bind(auth::now())
        .bind(&parsed.key_id)
        .execute(pool)
        .await?;
    Ok(IntegrationPrincipal {
        key_id: parsed.key_id,
        app_id,
        scopes,
    })
}

fn action_url_allowed(app: &crate::integration_apps::IntegrationApp, raw_url: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(raw_url) else {
        return false;
    };

    url.scheme() == "https"
        && url.host_str() == Some(app.host.as_str())
        && url.port_or_known_default() == Some(443)
        && url.username().is_empty()
        && url.password().is_none()
}

fn render_message(
    app: &crate::integration_apps::IntegrationApp,
    template_id: &str,
    recipient: &str,
    action_url: &str,
) -> Result<iris_mail::Compose, &'static str> {
    if !crate::email_address::valid_email_address(recipient) || !action_url_allowed(app, action_url)
    {
        return Err("invalid request");
    }
    let Some(template) = app.template(template_id) else {
        return Err("unknown template");
    };
    Ok(iris_mail::Compose {
        to: vec![recipient.to_string()],
        subject: template.subject.to_string(),
        text: format!("{}\n\n{action_url}\n\n{}", template.intro, template.closing),
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
    Ok((
        status,
        Json(json!({ "request_id": request_id, "status": delivery_status })),
    )
        .into_response())
}

/// Cap a stored relay reason so a hostile or chatty relay cannot bloat
/// the requests table. 500 chars is far more than any Resend message.
fn cap_reason(reason: &str) -> String {
    if reason.chars().count() > 500 {
        reason.chars().take(500).collect()
    } else {
        reason.to_string()
    }
}

pub async fn send(
    State(state): State<Arc<crate::AppState>>,
    headers: HeaderMap,
    Json(body): Json<SendEmailBody>,
) -> crate::ApiResult {
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .ok_or(crate::ApiError(StatusCode::UNAUTHORIZED, "unauthorized"))?;
    let required_scope = format!("email:template:{}", body.template_id);
    let principal = authenticate(&state.pool, token, &required_scope).await?;
    if !state.config.integration_email_enabled
        || state.relay.is_none()
        || !crate::email_address::valid_email_address(&state.config.integration_email_from)
    {
        return Err(crate::ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "delivery_disabled",
        ));
    }
    let idempotency_key = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 128
                && value.bytes().all(|byte| byte.is_ascii_graphic())
        })
        .ok_or(crate::ApiError(
            StatusCode::BAD_REQUEST,
            "invalid_idempotency_key",
        ))?;
    let mut message = {
        // The app behind the key, from the live registry: an app
        // deleted from the dashboard after the key was issued can no
        // longer send, instead of falling back to a stale registry.
        let apps = state.registry.read().await;
        let Some(app) = crate::integration_apps::app(&apps, &principal.app_id) else {
            return Err(crate::ApiError(StatusCode::BAD_REQUEST, "invalid_request"));
        };
        render_message(&app, &body.template_id, &body.recipient, &body.action_url)
    }
    .map_err(|_| crate::ApiError(StatusCode::BAD_REQUEST, "invalid_request"))?;
    message.from = state.config.integration_email_from.clone();

    let request_hash = auth::hash_token(
        &serde_json::to_string(&(principal.app_id.as_str(), &body))
            .map_err(|_| crate::ApiError(StatusCode::INTERNAL_SERVER_ERROR, "internal_error"))?,
    );
    let recipient_hash = auth::hash_token(&body.recipient.trim().to_ascii_lowercase());
    let request_id = uuid::Uuid::new_v4().to_string();
    let now = auth::now();
    let mut tx = state.pool.begin_with("BEGIN IMMEDIATE").await?;
    let inserted = sqlx::query(
        "INSERT INTO integration_email_requests (id,key_id,idempotency_key,template_id,recipient_hash,request_hash,status,created_at,updated_at) VALUES (?,?,?,?,?,?, 'sending', ?,?) ON CONFLICT(key_id,idempotency_key) DO NOTHING",
    )
    .bind(&request_id).bind(&principal.key_id).bind(idempotency_key).bind(&body.template_id)
    .bind(&recipient_hash).bind(&request_hash).bind(&now).bind(&now)
    .execute(&mut *tx).await
    ?;

    if inserted.rows_affected() == 0 {
        let existing: (String, String, String) = sqlx::query_as(
            "SELECT id,request_hash,status FROM integration_email_requests WHERE key_id=? AND idempotency_key=?",
        )
        .bind(&principal.key_id).bind(idempotency_key).fetch_one(&mut *tx).await
        ?;
        tx.commit().await?;
        if !auth::ct_eq(&existing.1, &request_hash) {
            return Err(crate::ApiError(
                StatusCode::CONFLICT,
                "idempotency_conflict",
            ));
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

    let utc_start = chrono::Utc::now()
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc()
        .to_rfc3339();
    // Daily quota counts accepted sends plus in-flight (`sending`) rows of
    // OTHER requests for the same key. Counting only `accepted` allowed two
    // concurrent sends to both pass when the count sat at limit-1 (TOCTOU);
    // counting `sending` rows (excluding this request's own row, which is
    // still `sending` here) closes the window. BEGIN IMMEDIATE serializes
    // the transactions so a concurrent commit is visible before this
    // count runs.
    let (accepted_today, daily_limit): (i64, i64) = sqlx::query_as(
        "SELECT COUNT(r.id), k.daily_send_limit FROM integration_keys k LEFT JOIN integration_email_requests r ON r.key_id=k.key_id AND r.created_at>=? AND r.status IN ('sending','accepted') AND r.id<>? WHERE k.key_id=? GROUP BY k.key_id",
    )
    .bind(utc_start).bind(&request_id).bind(&principal.key_id).fetch_one(&mut *tx).await
    ?;
    if accepted_today >= daily_limit {
        tx.rollback().await?;
        return Err(crate::ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "daily_limit_reached",
        ));
    }
    tx.commit().await?;

    let Some(relay) = state.relay.as_ref() else {
        return Err(crate::ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "delivery_disabled",
        ));
    };
    match relay.send(&message).await {
        Ok(provider_id) if !provider_id.trim().is_empty() => {
            sqlx::query("UPDATE integration_email_requests SET status='accepted',provider_message_id=?,updated_at=? WHERE id=?")
                .bind(provider_id).bind(auth::now()).bind(&request_id).execute(&state.pool).await
                ?;
            send_response(StatusCode::ACCEPTED, &request_id, "accepted")
        }
        // The relay refused the message: store its reason so the failure
        // is diagnosable from the admin dashboard instead of a bare
        // `failed` status with no cause.
        Err(iris_mail::MailError::Rejected(detail)) => {
            let reason = cap_reason(&detail);
            tracing::warn!(
                request_id = %request_id,
                app_id = %principal.app_id,
                template_id = %body.template_id,
                reason = %reason,
                "transactional email rejected by relay"
            );
            sqlx::query(
                "UPDATE integration_email_requests SET status='failed',error_message=?,updated_at=? WHERE id=?",
            )
            .bind(&reason)
            .bind(auth::now())
            .bind(&request_id)
            .execute(&state.pool)
            .await?;
            send_response(StatusCode::UNPROCESSABLE_ENTITY, &request_id, "failed")
        }
        Ok(_) => {
            // The relay answered 2xx with no provider id: nothing was
            // proven delivered, so the request stays `unknown` and the
            // reason is stored the same way.
            let reason = "relay returned an empty provider id";
            tracing::warn!(
                request_id = %request_id,
                app_id = %principal.app_id,
                template_id = %body.template_id,
                reason = reason,
                "transactional email relay returned no provider id"
            );
            sqlx::query(
                "UPDATE integration_email_requests SET status='unknown',error_message=?,updated_at=? WHERE id=?",
            )
            .bind(reason)
            .bind(auth::now())
            .bind(&request_id)
            .execute(&state.pool)
            .await?;
            send_response(StatusCode::SERVICE_UNAVAILABLE, &request_id, "unknown")
        }
        Err(err) => {
            let reason = cap_reason(&err.to_string());
            tracing::warn!(
                request_id = %request_id,
                app_id = %principal.app_id,
                template_id = %body.template_id,
                reason = %reason,
                "transactional email relay error"
            );
            sqlx::query(
                "UPDATE integration_email_requests SET status='unknown',error_message=?,updated_at=? WHERE id=?",
            )
            .bind(&reason)
            .bind(auth::now())
            .bind(&request_id)
            .execute(&state.pool)
            .await?;
            send_response(StatusCode::SERVICE_UNAVAILABLE, &request_id, "unknown")
        }
    }
}

pub async fn status(
    State(state): State<Arc<crate::AppState>>,
    headers: HeaderMap,
    Path(request_id): Path<String>,
) -> crate::ApiResult {
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .ok_or(crate::ApiError(StatusCode::UNAUTHORIZED, "unauthorized"))?;
    let principal = authenticate(&state.pool, token, "email:status").await?;
    let row: Option<String> =
        sqlx::query_scalar("SELECT status FROM integration_email_requests WHERE id=? AND key_id=?")
            .bind(&request_id)
            .bind(&principal.key_id)
            .fetch_optional(&state.pool)
            .await?;
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
        let apps = crate::integration_apps::default_registry();
        let alpha = crate::integration_apps::app(&apps, "alpha").unwrap();
        let beta = crate::integration_apps::app(&apps, "beta").unwrap();
        let gamma = crate::integration_apps::app(&apps, "gamma").unwrap();
        assert!(action_url_allowed(
            &alpha,
            "https://alpha.example.com/verify?token=abc"
        ));
        assert!(action_url_allowed(
            &beta,
            "https://beta.example.com/masuk#token=abc"
        ));
        assert!(!action_url_allowed(
            &alpha,
            "https://beta.example.com/verify?token=abc"
        ));
        assert!(!action_url_allowed(
            &alpha,
            "http://alpha.example.com/verify?token=abc"
        ));
        assert!(!action_url_allowed(
            &alpha,
            "https://alpha.example.com.attacker.invalid/verify"
        ));
        assert!(!action_url_allowed(
            &alpha,
            "https://alpha.example.com@attacker.invalid/verify"
        ));
        assert!(!action_url_allowed(
            &alpha,
            "https://alpha.example.com:8443/verify"
        ));
        assert!(action_url_allowed(
            &gamma,
            "https://gamma.example.com/reset-sandi/?token=abc"
        ));
        assert!(!action_url_allowed(
            &gamma,
            "https://alpha.example.com/reset-sandi/?token=abc"
        ));
    }

    #[test]
    fn renders_only_registered_auth_templates_for_the_owning_app() {
        let apps = crate::integration_apps::default_registry();
        let alpha = crate::integration_apps::app(&apps, "alpha").unwrap();
        let message = render_message(
            &alpha,
            "auth.verify",
            "user@example.test",
            "https://alpha.example.com/verify?token=abc",
        )
        .expect("registered template should render");
        assert_eq!(message.to, ["user@example.test"]);
        assert_eq!(message.subject, "Verify your email");
        assert!(message
            .text
            .contains("https://alpha.example.com/verify?token=abc"));

        assert!(render_message(
            &alpha,
            "auth.login_link",
            "user@example.test",
            "https://alpha.example.com/verify?token=abc",
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
            assert!(
                !crate::email_address::valid_email_address(recipient),
                "must reject {recipient}"
            );
        }
        assert!(crate::email_address::valid_email_address(
            "first.last+auth@example.test"
        ));
    }

    #[test]
    fn rejects_header_injection_in_recipient() {
        let apps = crate::integration_apps::default_registry();
        let alpha = crate::integration_apps::app(&apps, "alpha").unwrap();
        assert!(render_message(
            &alpha,
            "auth.verify",
            "user@example.test\r\nBcc: attacker@example.test",
            "https://alpha.example.com/verify?token=abc",
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
            .bind("alpha")
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
        assert_eq!(principal.app_id, "alpha");
    }

    /// Minimal local stand-in for the Resend relay: answers one request
    /// with the given status line and JSON body, then closes.
    struct FakeRelay {
        endpoint: String,
        task: tokio::task::JoinHandle<()>,
    }

    /// Like `fake_relay`, but keeps the raw request bytes the relay
    /// received, so a test can prove which subject and body the send
    /// path actually rendered.
    struct CapturingRelay {
        endpoint: String,
        task: tokio::task::JoinHandle<()>,
        captured: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    }

    async fn fake_relay_capturing(
        status_line: &'static str,
        response_body: &'static str,
    ) -> CapturingRelay {
        use tokio::{
            io::{AsyncReadExt, AsyncWriteExt},
            net::TcpListener,
            time::{timeout, Duration},
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured_in_task = captured.clone();
        let task = tokio::spawn(async move {
            let Ok(Ok((mut stream, _))) = timeout(Duration::from_secs(5), listener.accept()).await
            else {
                return;
            };
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
            *captured_in_task.lock().unwrap() = bytes;
            let response = format!(
                "HTTP/1.1 {status_line}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{response_body}",
                response_body.len(),
            );
            let _ = stream.write_all(response.as_bytes()).await;
        });
        CapturingRelay {
            endpoint: format!("http://{address}"),
            task,
            captured,
        }
    }

    async fn fake_relay(status_line: &'static str, response_body: &'static str) -> FakeRelay {
        use tokio::{
            io::{AsyncReadExt, AsyncWriteExt},
            net::TcpListener,
            time::{timeout, Duration},
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let Ok(Ok((mut stream, _))) = timeout(Duration::from_secs(5), listener.accept()).await
            else {
                return;
            };
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
            let response = format!(
                "HTTP/1.1 {status_line}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{response_body}",
                response_body.len(),
            );
            let _ = stream.write_all(response.as_bytes()).await;
        });
        FakeRelay {
            endpoint: format!("http://{address}"),
            task,
        }
    }

    async fn relay_state(relay: iris_mail::ResendClient) -> (Arc<crate::AppState>, String) {
        use iris_config::Config;
        let pool = iris_database::init_pool("sqlite::memory:").await.unwrap();
        let generated = auth::generate_api_key();
        let expiry = (chrono::Utc::now() + chrono::Duration::days(1)).to_rfc3339();
        sqlx::query("INSERT INTO integration_keys (key_id,app_id,token_hash,scopes_json,created_at,expires_at,daily_send_limit) VALUES (?,?,?,?,?,?,1000)")
            .bind(&generated.key_id)
            .bind("alpha")
            .bind(&generated.token_hash)
            .bind(json!(["email:template:auth.verify"]).to_string())
            .bind(auth::now())
            .bind(expiry)
            .execute(&pool)
            .await
            .unwrap();
        let state = Arc::new(crate::AppState {
            pool,
            organize_lock: tokio::sync::Mutex::new(()),
            jmap: iris_mail::JmapClient::new("http://127.0.0.1:9".into()),
            http: reqwest::Client::new(),
            relay: Some(relay),
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
                resend_api_key: "test-resend-key".into(),
                integration_email_from: "no-reply@example.com".into(),
                integration_email_enabled: true,
                password_reset_service_token: String::new(),
                password_reset_email_from: String::new(),
                password_reset_email_to: String::new(),
                password_reset_public_origin: String::new(),
                resend_api_url: String::new(),
                mail_content_origin: String::new(),
                admin_emails: Vec::new(),
            },
        });
        (state, generated.token)
    }

    fn send_headers(token: &str, idempotency_key: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            format!("Bearer {token}").parse().unwrap(),
        );
        headers.insert("idempotency-key", idempotency_key.parse().unwrap());
        headers
    }

    fn send_body() -> SendEmailBody {
        SendEmailBody {
            template_id: "auth.verify".into(),
            recipient: "user@example.test".into(),
            action_url: "https://alpha.example.com/verify?token=abc".into(),
        }
    }

    #[tokio::test]
    async fn relay_rejection_stores_the_reason_and_stays_failed() {
        let relay = fake_relay(
            "400 Bad Request",
            r#"{"message":"recipient address rejected"}"#,
        )
        .await;
        let (state, token) = relay_state(iris_mail::ResendClient::with_endpoint(
            "test-key".into(),
            relay.endpoint.clone(),
        ))
        .await;
        let resp = send(
            State(state.clone()),
            send_headers(&token, "rej-001"),
            Json(send_body()),
        )
        .await
        .expect("a relay rejection answers, it does not raise");
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
        relay.task.abort();
        let row: (String, Option<String>) = sqlx::query_as(
            "SELECT status, error_message FROM integration_email_requests WHERE idempotency_key='rej-001'",
        )
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(row.0, "failed");
        assert_eq!(row.1.as_deref(), Some("recipient address rejected"));
    }

    #[tokio::test]
    async fn relay_error_stores_the_reason_and_stays_unknown() {
        let relay = fake_relay(
            "500 Internal Server Error",
            r#"{"message":"relay exploded"}"#,
        )
        .await;
        let (state, token) = relay_state(iris_mail::ResendClient::with_endpoint(
            "test-key".into(),
            relay.endpoint.clone(),
        ))
        .await;
        let resp = send(
            State(state.clone()),
            send_headers(&token, "unk-001"),
            Json(send_body()),
        )
        .await
        .expect("a relay error answers, it does not raise");
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        relay.task.abort();
        let row: (String, Option<String>) = sqlx::query_as(
            "SELECT status, error_message FROM integration_email_requests WHERE idempotency_key='unk-001'",
        )
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(row.0, "unknown");
        // The relay formats the Resend message value as JSON, so the
        // stored reason keeps its quotes.
        assert_eq!(
            row.1.as_deref(),
            Some("mail server unavailable: relay HTTP 500: \"relay exploded\"")
        );
    }

    #[tokio::test]
    async fn send_uses_a_database_registered_app_host_and_template() {
        // The app exists only in the database (registered from the
        // dashboard), never in the env seed or the example registry.
        let relay = fake_relay_capturing("200 OK", r#"{"id":"prov-1"}"#).await;
        let (state, _) = relay_state(iris_mail::ResendClient::with_endpoint(
            "test-key".into(),
            relay.endpoint.clone(),
        ))
        .await;
        let now = auth::now();
        sqlx::query("INSERT INTO integration_apps (id, host, created_at) VALUES ('dbapp','dbapp.example.com',?)")
            .bind(&now).execute(&state.pool).await.unwrap();
        sqlx::query("INSERT INTO integration_app_templates (app_id, template_id, subject, intro, closing) VALUES ('dbapp','auth.verify','Verify via DB App','Open this link to verify.','Ignore this if it was not you.')")
            .execute(&state.pool).await.unwrap();
        *state.registry.write().await = crate::integration_apps::load_apps(&state.pool)
            .await
            .unwrap();
        let apps = state.registry.read().await.clone();
        let generated = crate::integration_keys::issue(
            &state.pool,
            &apps,
            "dbapp",
            "email:template:auth.verify,email:status",
            30,
            1000,
        )
        .await
        .expect("a key for the database app must be mintable");
        let headers = send_headers(&generated.token, "db-001");
        let body = SendEmailBody {
            template_id: "auth.verify".into(),
            recipient: "user@example.test".into(),
            action_url: "https://dbapp.example.com/verify?token=abc".into(),
        };
        let resp = send(State(state.clone()), headers, Json(body))
            .await
            .expect("the send answers, it does not raise");
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
        relay.task.abort();
        // The relay received the database app's template, not the
        // example registry's wording: its host rule accepted the
        // dbapp host, and the rendered subject and intro are the
        // ones stored in the database.
        let raw = relay.captured.lock().unwrap().clone();
        let text = String::from_utf8_lossy(&raw);
        assert!(text.contains("Verify via DB App"));
        assert!(text.contains("Open this link to verify."));
        assert!(text.contains("https://dbapp.example.com/verify?token=abc"));
        let row: (String, Option<String>) = sqlx::query_as(
            "SELECT status, provider_message_id FROM integration_email_requests WHERE idempotency_key='db-001'",
        )
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(row.0, "accepted");
        assert_eq!(row.1.as_deref(), Some("prov-1"));
        // And the same send to the example host is refused for this
        // app: the host rule follows the database row.
        let body = SendEmailBody {
            template_id: "auth.verify".into(),
            recipient: "user@example.test".into(),
            action_url: "https://alpha.example.com/verify?token=abc".into(),
        };
        let err = send(
            State(state.clone()),
            send_headers(&generated.token, "db-002"),
            Json(body),
        )
        .await
        .expect_err("a send to another app's host must be refused");
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert_eq!(err.1, "invalid_request");
    }

    #[tokio::test]
    async fn empty_provider_id_stores_a_reason_and_stays_unknown() {
        let relay = fake_relay("200 OK", r#"{"id":""}"#).await;
        let (state, token) = relay_state(iris_mail::ResendClient::with_endpoint(
            "test-key".into(),
            relay.endpoint.clone(),
        ))
        .await;
        let resp = send(
            State(state.clone()),
            send_headers(&token, "empty-001"),
            Json(send_body()),
        )
        .await
        .expect("an empty provider id answers, it does not raise");
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
        relay.task.abort();
        let row: (String, Option<String>) = sqlx::query_as(
            "SELECT status, error_message FROM integration_email_requests WHERE idempotency_key='empty-001'",
        )
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(row.0, "unknown");
        assert_eq!(
            row.1.as_deref(),
            Some("relay returned an empty provider id")
        );
    }
}
