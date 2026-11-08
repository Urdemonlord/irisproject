//! Address book: one contact list per user, shared across their mailboxes.
//!
//! Contacts are keyed by the signed-in user (`user_id` from the session),
//! never by mailbox: an address book belongs to the person, not to one
//! mailbox, and every query filters by `user_id`, so a user only ever
//! sees their own contacts. No JMAP is involved: the mail server never
//! learns about the address book, and reads skip the JMAP read limiter
//! (`check_read_limit` only guards reads about to hit the mail server).
use crate::{email_address::valid_email_address, ApiError, ApiResult, CurrentUser, S};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Extension, Json,
};
use iris_auth as auth;
use iris_config::limits::{
    DEFAULT_CONTACTS_LIMIT, MAX_CONTACTS_LIMIT, MAX_CONTACTS_PER_USER, MAX_CONTACT_NAME_CHARS,
    MAX_CONTACT_SEARCH_CHARS,
};
use serde::{Deserialize, Serialize};
use serde_json::json;

fn bad(code: &'static str) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, code)
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub(crate) struct Contact {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) email: String,
}

/// Trims and checks a contact name: 1-100 characters, no control
/// characters.
pub(crate) fn clean_name(name: &str) -> Result<String, ApiError> {
    let n = name.trim().to_string();
    let len = n.chars().count();
    if len == 0 || len > MAX_CONTACT_NAME_CHARS || n.chars().any(|c| c.is_control()) {
        return Err(bad("invalid_contact_name"));
    }
    Ok(n)
}

/// Trims, lowercases and checks a contact email with the strict address
/// check: a stored contact must be a deliverable address, not a sloppy
/// compose-time one. Lowercasing makes the per-user unique email match
/// case-insensitively, the way the compose form lowercases recipients.
pub(crate) fn clean_email(email: &str) -> Result<String, ApiError> {
    let e = email.trim().to_lowercase();
    if valid_email_address(&e) {
        Ok(e)
    } else {
        Err(bad("invalid_contact_email"))
    }
}

/// Escapes a LIKE pattern: `%`, `_` and the escape character itself are
/// literal in a search term, so a query can never turn into a wildcard.
fn like_pattern(q: &str) -> String {
    let mut out = String::with_capacity(q.len() + 2);
    out.push('%');
    for c in q.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('%');
    out
}

#[derive(Deserialize)]
pub(crate) struct ListQuery {
    q: Option<String>,
    limit: Option<u32>,
}

pub(crate) async fn list(
    State(s): S,
    Extension(u): Extension<CurrentUser>,
    Query(p): Query<ListQuery>,
) -> ApiResult {
    let limit = p
        .limit
        .unwrap_or(DEFAULT_CONTACTS_LIMIT)
        .clamp(1, MAX_CONTACTS_LIMIT);
    // Search terms are capped so a huge `q` is never forwarded verbatim to SQL.
    let q =
        p.q.as_deref()
            .map(str::trim)
            .filter(|q| !q.is_empty())
            .map(|q| q.chars().take(MAX_CONTACT_SEARCH_CHARS).collect::<String>());
    let contacts: Vec<Contact> = match q {
        Some(q) => {
            let pat = like_pattern(&q);
            sqlx::query_as::<_, Contact>(
                "SELECT id, name, email FROM contacts WHERE user_id=? \
                 AND (name LIKE ? ESCAPE '\\' OR email LIKE ? ESCAPE '\\') \
                 ORDER BY name COLLATE NOCASE, email LIMIT ?",
            )
            .bind(&u.user_id)
            .bind(&pat)
            .bind(&pat)
            .bind(limit)
            .fetch_all(&s.pool)
            .await?
        }
        None => {
            sqlx::query_as::<_, Contact>(
                "SELECT id, name, email FROM contacts WHERE user_id=? \
                 ORDER BY name COLLATE NOCASE, email LIMIT ?",
            )
            .bind(&u.user_id)
            .bind(limit)
            .fetch_all(&s.pool)
            .await?
        }
    };
    Ok(Json(json!({ "contacts": contacts })).into_response())
}

#[derive(Deserialize)]
pub(crate) struct ContactBody {
    name: String,
    email: String,
}

pub(crate) async fn create(
    State(s): S,
    Extension(u): Extension<CurrentUser>,
    Json(b): Json<ContactBody>,
) -> ApiResult {
    let name = clean_name(&b.name)?;
    let email = clean_email(&b.email)?;
    let id = uuid::Uuid::new_v4().to_string();
    let now = auth::now();
    // One statement, so the cap holds under concurrent creates. An
    // existing email is a 409, not a silent no-op: re-adding a contact
    // with a different name would otherwise look like it worked while
    // keeping the old name.
    let r = sqlx::query(
        "INSERT INTO contacts (id, user_id, name, email, created_at, updated_at) \
         SELECT ?, ?, ?, ?, ?, ? WHERE (SELECT COUNT(*) FROM contacts WHERE user_id = ?) < ? \
         AND NOT EXISTS (SELECT 1 FROM contacts WHERE user_id = ? AND email = ?)",
    )
    .bind(&id)
    .bind(&u.user_id)
    .bind(&name)
    .bind(&email)
    .bind(&now)
    .bind(&now)
    .bind(&u.user_id)
    .bind(MAX_CONTACTS_PER_USER)
    .bind(&u.user_id)
    .bind(&email)
    .execute(&s.pool)
    .await?;
    if r.rows_affected() == 0 {
        // Distinguish the two reasons the insert did not happen.
        let dup: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM contacts WHERE user_id=? AND email=?")
                .bind(&u.user_id)
                .bind(&email)
                .fetch_one(&s.pool)
                .await?;
        if dup > 0 {
            return Err(ApiError(StatusCode::CONFLICT, "contact_exists"));
        }
        return Err(ApiError(StatusCode::CONFLICT, "too_many_contacts"));
    }
    Ok((
        StatusCode::CREATED,
        Json(json!({ "contact": Contact { id, name, email } })),
    )
        .into_response())
}

#[derive(Deserialize)]
pub(crate) struct UpdateBody {
    name: Option<String>,
    email: Option<String>,
}

pub(crate) async fn update(
    State(s): S,
    Extension(u): Extension<CurrentUser>,
    Path(id): Path<String>,
    Json(b): Json<UpdateBody>,
) -> ApiResult {
    let name = b.name.as_deref().map(clean_name).transpose()?;
    let email = b.email.as_deref().map(clean_email).transpose()?;
    if name.is_none() && email.is_none() {
        return Err(bad("invalid_request"));
    }
    // 404 when the id is not this user's contact; 409 when the new email
    // already belongs to another of their contacts.
    let exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM contacts WHERE id=? AND user_id=?")
        .bind(&id)
        .bind(&u.user_id)
        .fetch_one(&s.pool)
        .await?;
    if exists == 0 {
        return Err(ApiError(StatusCode::NOT_FOUND, "not_found"));
    }
    if let Some(email) = &email {
        let dup: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM contacts WHERE user_id=? AND email=? AND id<>?",
        )
        .bind(&u.user_id)
        .bind(email)
        .bind(&id)
        .fetch_one(&s.pool)
        .await?;
        if dup > 0 {
            return Err(ApiError(StatusCode::CONFLICT, "contact_exists"));
        }
    }
    sqlx::query(
        "UPDATE contacts SET name=COALESCE(?, name), email=COALESCE(?, email), updated_at=? \
         WHERE id=? AND user_id=?",
    )
    .bind(name)
    .bind(email)
    .bind(auth::now())
    .bind(&id)
    .bind(&u.user_id)
    .execute(&s.pool)
    .await?;
    let contact: Contact = sqlx::query_as::<_, Contact>(
        "SELECT id, name, email FROM contacts WHERE id=? AND user_id=?",
    )
    .bind(&id)
    .bind(&u.user_id)
    .fetch_one(&s.pool)
    .await?;
    Ok(Json(json!({ "contact": contact })).into_response())
}

pub(crate) async fn delete(
    State(s): S,
    Extension(u): Extension<CurrentUser>,
    Path(id): Path<String>,
) -> ApiResult {
    let r = sqlx::query("DELETE FROM contacts WHERE id=? AND user_id=?")
        .bind(&id)
        .bind(&u.user_id)
        .execute(&s.pool)
        .await?;
    if r.rows_affected() == 0 {
        return Err(ApiError(StatusCode::NOT_FOUND, "not_found"));
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{html_body, AppState, Creds};
    use iris_config::Config;
    use iris_mail::JmapClient;
    use std::sync::Arc;

    async fn test_state() -> Arc<AppState> {
        let pool = iris_database::init_pool("sqlite::memory:").await.unwrap();
        // Contacts reference app_users, and foreign keys are enforced, so
        // the test users must exist before any contact can be stored.
        for id in ["alice", "bob"] {
            sqlx::query(
                "INSERT INTO app_users (id, email, display_name, created_at, updated_at) \
                 VALUES (?, ?, ?, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
            )
            .bind(id)
            .bind(format!("{id}@example.test"))
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        }
        Arc::new(AppState {
            pool,
            organize_lock: tokio::sync::Mutex::new(()),
            jmap: JmapClient::new("http://127.0.0.1:9".into()),
            http: reqwest::Client::new(),
            relay: None,
            html_bodies: html_body::HtmlBodyStore::new(),
            mail_watch: crate::notify::MailWatch::from_limits(),
            config: Config {
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

    fn user(id: &str) -> CurrentUser {
        CurrentUser {
            user_id: id.to_string(),
            email: format!("{id}@example.test"),
            creds: Creds {
                user: id.to_string(),
                pass: "secret".into(),
            },
        }
    }

    fn body(name: &str, email: &str) -> Json<ContactBody> {
        Json(serde_json::from_value(json!({ "name": name, "email": email })).unwrap())
    }

    async fn add(s: &Arc<AppState>, who: &str, name: &str, email: &str) {
        create(State(s.clone()), Extension(user(who)), body(name, email))
            .await
            .unwrap();
    }

    async fn listed(s: &Arc<AppState>, who: &str, q: Option<&str>) -> Vec<Contact> {
        let resp = list(
            State(s.clone()),
            Extension(user(who)),
            Query(ListQuery {
                q: q.map(str::to_string),
                limit: None,
            }),
        )
        .await
        .unwrap();
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice::<serde_json::Value>(&body).unwrap()["contacts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| Contact {
                id: c["id"].as_str().unwrap().to_string(),
                name: c["name"].as_str().unwrap().to_string(),
                email: c["email"].as_str().unwrap().to_string(),
            })
            .collect()
    }

    #[test]
    fn names_are_trimmed_and_bounded() {
        assert_eq!(clean_name("  Ada Lovelace ").unwrap(), "Ada Lovelace");
        for bad_name in ["", "   ", "a\nb", &"x".repeat(101)] {
            assert!(clean_name(bad_name).is_err(), "{bad_name:?}");
        }
    }

    #[test]
    fn emails_are_lowercased_and_strict() {
        assert_eq!(
            clean_email("  Ada@Example.COM ").unwrap(),
            "ada@example.com"
        );
        // The strict check applies: a stored contact must be deliverable.
        for bad_email in ["not-an-email", "a@b", ".a@x.id", "a@b@c.com"] {
            assert!(clean_email(bad_email).is_err(), "{bad_email:?}");
        }
    }

    #[test]
    fn like_wildcards_are_literal() {
        assert_eq!(like_pattern("a_b"), "%a\\_b%");
        assert_eq!(like_pattern("100%"), "%100\\%%");
        assert_eq!(like_pattern("a\\b"), "%a\\\\b%");
    }

    #[tokio::test]
    async fn create_and_list_round_trip() {
        let s = test_state().await;
        add(&s, "alice", "Bob", "bob@example.com").await;
        add(&s, "alice", "Ada", "ada@example.com").await;
        let all = listed(&s, "alice", None).await;
        // Ordered by name, not by insertion.
        assert_eq!(
            all.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            vec!["Ada", "Bob"]
        );
        // Search matches a name substring and an email substring.
        assert_eq!(listed(&s, "alice", Some("ada")).await.len(), 1);
        assert_eq!(listed(&s, "alice", Some("example")).await.len(), 2);
        assert!(listed(&s, "alice", Some("nobody")).await.is_empty());
    }

    #[tokio::test]
    async fn search_treats_wildcards_as_literal_text() {
        let s = test_state().await;
        add(&s, "alice", "axb", "axb@example.com").await;
        add(&s, "alice", "a_b", "a_b@example.com").await;
        // An unescaped `_` would match "axb" too; only the literal name may match.
        let found = listed(&s, "alice", Some("a_b")).await;
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "a_b");
    }

    #[tokio::test]
    async fn list_limit_is_clamped() {
        let s = test_state().await;
        add(&s, "alice", "A", "a@example.com").await;
        add(&s, "alice", "B", "b@example.com").await;
        add(&s, "alice", "C", "c@example.com").await;
        let one = list(
            State(s.clone()),
            Extension(user("alice")),
            Query(ListQuery {
                q: None,
                limit: Some(1),
            }),
        )
        .await
        .unwrap();
        let body = axum::body::to_bytes(one.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["contacts"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn create_rejects_bad_input_and_duplicates() {
        let s = test_state().await;
        for (name, email) in [
            ("", "a@example.com"),
            ("Ada", "not-an-email"),
            ("Ada", "a@b"),
            (&"x".repeat(101), "a@example.com"),
        ] {
            let e = create(
                State(s.clone()),
                Extension(user("alice")),
                body(name, email),
            )
            .await
            .unwrap_err();
            assert_eq!(e.0, StatusCode::BAD_REQUEST, "{name:?} {email:?}");
        }
        add(&s, "alice", "Ada", "ada@example.com").await;
        let e = create(
            State(s.clone()),
            Extension(user("alice")),
            body("Ada Again", "ADA@example.com"),
        )
        .await
        .unwrap_err();
        // The same email, different case, is the same contact.
        assert_eq!(e.0, StatusCode::CONFLICT);
        assert_eq!(e.1, "contact_exists");
    }

    #[tokio::test]
    async fn contacts_are_per_user() {
        let s = test_state().await;
        add(&s, "alice", "Ada", "ada@example.com").await;
        // Bob sees none of Alice's contacts, and may store the same email.
        assert!(listed(&s, "bob", None).await.is_empty());
        add(&s, "bob", "Ada", "ada@example.com").await;
        assert_eq!(listed(&s, "bob", None).await.len(), 1);
        assert_eq!(listed(&s, "alice", None).await.len(), 1);
        // Bob cannot update or delete Alice's contact.
        let id = listed(&s, "alice", None).await[0].id.clone();
        let upd = |name: Option<&str>| {
            Json(
                serde_json::from_value::<UpdateBody>(json!({ "name": name, "email": null }))
                    .unwrap(),
            )
        };
        let e = update(
            State(s.clone()),
            Extension(user("bob")),
            Path(id.clone()),
            upd(Some("Eve")),
        )
        .await
        .unwrap_err();
        assert_eq!(e.0, StatusCode::NOT_FOUND);
        let e = delete(State(s.clone()), Extension(user("bob")), Path(id.clone()))
            .await
            .unwrap_err();
        assert_eq!(e.0, StatusCode::NOT_FOUND);
        assert_eq!(listed(&s, "alice", None).await.len(), 1);
    }

    #[tokio::test]
    async fn update_and_delete() {
        let s = test_state().await;
        add(&s, "alice", "Ada", "ada@example.com").await;
        add(&s, "alice", "Bob", "bob@example.com").await;
        let id = listed(&s, "alice", Some("ada")).await[0].id.clone();
        // Name-only update keeps the email.
        let r = update(
            State(s.clone()),
            Extension(user("alice")),
            Path(id.clone()),
            Json(serde_json::from_value::<UpdateBody>(json!({ "name": "Ada L." })).unwrap()),
        )
        .await
        .unwrap();
        let body = axum::body::to_bytes(r.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["contact"]["name"], "Ada L.");
        assert_eq!(v["contact"]["email"], "ada@example.com");
        // Taking another contact's email is a conflict.
        let e = update(
            State(s.clone()),
            Extension(user("alice")),
            Path(id.clone()),
            Json(
                serde_json::from_value::<UpdateBody>(json!({ "email": "bob@example.com" }))
                    .unwrap(),
            ),
        )
        .await
        .unwrap_err();
        assert_eq!(e.0, StatusCode::CONFLICT);
        assert_eq!(e.1, "contact_exists");
        // An update with nothing to change is a bad request.
        let e = update(
            State(s.clone()),
            Extension(user("alice")),
            Path(id.clone()),
            Json(serde_json::from_value::<UpdateBody>(json!({})).unwrap()),
        )
        .await
        .unwrap_err();
        assert_eq!(e.0, StatusCode::BAD_REQUEST);
        // Delete removes it; a second delete is a 404.
        delete(State(s.clone()), Extension(user("alice")), Path(id.clone()))
            .await
            .unwrap();
        let e = delete(State(s.clone()), Extension(user("alice")), Path(id))
            .await
            .unwrap_err();
        assert_eq!(e.0, StatusCode::NOT_FOUND);
    }
}
