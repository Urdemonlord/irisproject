//! Per-mailbox signatures: the sign-off the composer appends to new
//! messages. One signature per user and mailbox, stored in both forms
//! (plain text and HTML): the editor saves whichever form the user
//! edited, and the server derives the other, so the composer can append
//! the right form for its mode without converting at send time.
use crate::{owned_account, ApiError, ApiResult, AppState, CurrentUser, S};
use axum::{
    extract::{Path, State},
    response::IntoResponse,
    Extension, Json,
};
use iris_auth as auth;
use iris_config::limits::MAX_SIGNATURE_CHARS;
use iris_mail::html as mail_html;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// A signature as the web reads and writes it: both forms, either of
/// which may be empty. Sending both blank clears the signature.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub(crate) struct SignatureBody {
    #[serde(default)]
    pub(crate) text: String,
    #[serde(default)]
    pub(crate) html: String,
}

async fn load(s: &AppState, user_id: &str, mailbox_id: &str) -> Result<SignatureBody, sqlx::Error> {
    let row: Option<(String, String)> =
        sqlx::query_as("SELECT text, html FROM signatures WHERE user_id=? AND mailbox_id=?")
            .bind(user_id)
            .bind(mailbox_id)
            .fetch_optional(&s.pool)
            .await?;
    Ok(
        row.map_or_else(SignatureBody::default, |(text, html)| SignatureBody {
            text,
            html,
        }),
    )
}

/// GET /api/v1/mailboxes/:mailbox_id/signature. An unset signature is
/// an empty one, not a 404: the composer treats "no signature" and
/// "empty signature" the same way.
pub(crate) async fn get(
    State(s): S,
    Extension(u): Extension<CurrentUser>,
    Path(mailbox_id): Path<String>,
) -> ApiResult {
    let _account = owned_account(&s, &u, &mailbox_id).await?;
    Ok(Json(json!(load(&s, &u.user_id, &mailbox_id).await?)).into_response())
}

/// PUT /api/v1/mailboxes/:mailbox_id/signature. The saved HTML is
/// sanitized before it is stored; the missing form is derived from the
/// saved one. Both blank clears the signature (the row is deleted).
pub(crate) async fn set(
    State(s): S,
    Extension(u): Extension<CurrentUser>,
    Path(mailbox_id): Path<String>,
    Json(b): Json<SignatureBody>,
) -> ApiResult {
    let _account = owned_account(&s, &u, &mailbox_id).await?;
    if b.text.chars().count() > MAX_SIGNATURE_CHARS || b.html.chars().count() > MAX_SIGNATURE_CHARS
    {
        return Err(ApiError(
            axum::http::StatusCode::BAD_REQUEST,
            "invalid_signature",
        ));
    }
    let text = b.text.trim().to_string();
    let html = mail_html::sanitize_html(b.html.trim());
    // The two forms must agree: derive whichever the user did not type.
    // A signature that sanitizes down to nothing is empty in both forms.
    let (text, html) = if text.is_empty() && html.trim().is_empty() {
        (String::new(), String::new())
    } else if text.is_empty() {
        (mail_html::html_to_text(&html), html)
    } else if html.trim().is_empty() {
        (text.clone(), mail_html::text_to_html(&text))
    } else {
        (text, html)
    };
    let cleared = text.is_empty() && html.trim().is_empty();
    if cleared {
        sqlx::query("DELETE FROM signatures WHERE user_id=? AND mailbox_id=?")
            .bind(&u.user_id)
            .bind(&mailbox_id)
            .execute(&s.pool)
            .await?;
    } else {
        sqlx::query(
            "INSERT INTO signatures (user_id, mailbox_id, text, html, updated_at) VALUES (?,?,?,?,?) \
             ON CONFLICT(user_id, mailbox_id) DO UPDATE SET text=excluded.text, html=excluded.html, updated_at=excluded.updated_at",
        )
        .bind(&u.user_id)
        .bind(&mailbox_id)
        .bind(&text)
        .bind(&html)
        .bind(auth::now())
        .execute(&s.pool)
        .await?;
    }
    crate::audit::record(
        &s.pool,
        Some(&u.user_id),
        &u.email,
        crate::audit::SIGNATURE_SAVE,
        Some("mailbox"),
        Some(&mailbox_id),
        json!({"mailbox_id": mailbox_id, "cleared": cleared}),
        Some(&u.ip),
    )
    .await;
    Ok(Json(json!(SignatureBody { text, html })).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn pool() -> sqlx::SqlitePool {
        iris_database::init_pool("sqlite::memory:").await.unwrap()
    }

    fn user() -> CurrentUser {
        CurrentUser {
            user_id: "u1".into(),
            email: "me@x.id".into(),
            creds: crate::Creds {
                user: "me@x.id".into(),
                pass: "secret".into(),
            },
            session_id: "sess-1".into(),
            session_token: "tok-1".into(),
            ip: "127.0.0.1".into(),
            mailbox_creds: vec![],
        }
    }

    /// A state with the mailbox `mb1` owned by `u1`, for the
    /// `owned_account` check the handlers run first.
    async fn state() -> std::sync::Arc<AppState> {
        let pool = pool().await;
        sqlx::query(
            "INSERT INTO app_users (id,email,display_name,created_at,updated_at) VALUES ('u1','me@x.id','Me','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO permanent_mailboxes (id,user_id,email_address,mail_server_account_id,status,created_at,updated_at) VALUES ('mb1','u1','me@x.id','acc1','active','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let config = iris_config::Config {
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
            admin_emails: Vec::new(),
        };
        std::sync::Arc::new(AppState {
            pool,
            organize_lock: tokio::sync::Mutex::new(()),
            jmap: iris_mail::JmapClient::new("http://127.0.0.1:9".into()),
            http: reqwest::Client::new(),
            relay: None,
            registry: tokio::sync::RwLock::new(crate::integration_apps::default_registry()),
            html_bodies: crate::html_body::HtmlBodyStore::new(),
            mail_watch: crate::notify::MailWatch::from_limits(),
            config,
        })
    }

    #[tokio::test]
    async fn get_returns_empty_when_unset() {
        let s = state().await;
        let sig = load(&s, "u1", "mb1").await.unwrap();
        assert_eq!(sig, SignatureBody::default());
    }

    #[tokio::test]
    async fn set_text_derives_html_and_round_trips() {
        let s = state().await;
        let res = set(
            State(s.clone()),
            Extension(user()),
            Path("mb1".to_string()),
            Json(SignatureBody {
                text: "Best,\nAda".into(),
                html: String::new(),
            }),
        )
        .await;
        assert!(res.is_ok());
        let sig = load(&s, "u1", "mb1").await.unwrap();
        assert_eq!(sig.text, "Best,\nAda");
        assert_eq!(sig.html, "Best,<br>Ada");
    }

    #[tokio::test]
    async fn set_html_sanitizes_and_derives_text() {
        let s = state().await;
        let res = set(
            State(s.clone()),
            Extension(user()),
            Path("mb1".to_string()),
            Json(SignatureBody {
                text: String::new(),
                html: "<p>Best</p><script>alert(1)</script>".into(),
            }),
        )
        .await;
        assert!(res.is_ok());
        let sig = load(&s, "u1", "mb1").await.unwrap();
        assert_eq!(sig.html, "<p>Best</p>");
        assert_eq!(sig.text, "Best");
    }

    #[tokio::test]
    async fn signatures_are_per_mailbox() {
        let s = state().await;
        sqlx::query(
            "INSERT INTO permanent_mailboxes (id,user_id,email_address,mail_server_account_id,status,created_at,updated_at) VALUES ('mb2','u1','me2@x.id','acc2','active','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
        )
        .execute(&s.pool)
        .await
        .unwrap();
        for (mb, text) in [("mb1", "One"), ("mb2", "Two")] {
            set(
                State(s.clone()),
                Extension(user()),
                Path(mb.to_string()),
                Json(SignatureBody {
                    text: text.into(),
                    html: String::new(),
                }),
            )
            .await
            .unwrap();
        }
        assert_eq!(load(&s, "u1", "mb1").await.unwrap().text, "One");
        assert_eq!(load(&s, "u1", "mb2").await.unwrap().text, "Two");
    }

    #[tokio::test]
    async fn clearing_both_forms_deletes_the_row() {
        let s = state().await;
        set(
            State(s.clone()),
            Extension(user()),
            Path("mb1".to_string()),
            Json(SignatureBody {
                text: "Best".into(),
                html: String::new(),
            }),
        )
        .await
        .unwrap();
        set(
            State(s.clone()),
            Extension(user()),
            Path("mb1".to_string()),
            Json(SignatureBody::default()),
        )
        .await
        .unwrap();
        assert_eq!(
            load(&s, "u1", "mb1").await.unwrap(),
            SignatureBody::default()
        );
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM signatures WHERE user_id='u1'")
            .fetch_one(&s.pool)
            .await
            .unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn oversize_signature_is_refused() {
        let s = state().await;
        let err = set(
            State(s.clone()),
            Extension(user()),
            Path("mb1".to_string()),
            Json(SignatureBody {
                text: "x".repeat(MAX_SIGNATURE_CHARS + 1),
                html: String::new(),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(err.1, "invalid_signature");
    }

    #[tokio::test]
    async fn unknown_mailbox_is_not_found() {
        let s = state().await;
        let err = get(
            State(s.clone()),
            Extension(user()),
            Path("nope".to_string()),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, axum::http::StatusCode::NOT_FOUND);
    }
}
