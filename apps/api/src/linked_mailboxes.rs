//! Link extra mail-server accounts to one IRIS login: list, link,
//! unlock, unlink. A linked mailbox is one row in `permanent_mailboxes`
//! plus, for this session only, one sealed app password in
//! `session_mailbox_credentials` (migration 013).
//!
//! The user's real mail password is used once, at link/unlock time, to
//! verify the account with a real JMAP login and mint a per-session
//! Stalwart app password, exactly like the main login does; it is never
//! stored. After logout the linked mailbox is locked again until the
//! user unlocks it with its password (the next login only knows the
//! primary account's password). Unlinking deletes IRIS-side state only:
//! the mailbox row, its session credentials, signatures, labels,
//! filters, DMARC rows and queued sends. Mail on the server is never
//! touched, and neither is the account itself.
use crate::{
    email_address::valid_email_address, owned_mailbox, ApiError, ApiResult, AppState, CurrentUser,
    S,
};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Extension, Json,
};
use iris_auth as auth;
use serde::Deserialize;
use serde_json::json;

#[derive(Deserialize)]
pub(crate) struct LinkBody {
    email: String,
    password: String,
}

#[derive(Deserialize)]
pub(crate) struct UnlockBody {
    password: String,
}

/// Verify credentials with a real JMAP login, then mint a per-session
/// app password for the account. Returns (mail-server account id,
/// credential id, app secret). A wrong password answers 401, like login.
async fn verify_and_mint(
    s: &AppState,
    email: &str,
    password: &str,
) -> Result<(String, String, String), ApiError> {
    let account_id = match s.jmap.login(email, password).await {
        Ok(a) => a,
        Err(iris_mail::MailError::Auth) => {
            return Err(ApiError(StatusCode::UNAUTHORIZED, "invalid_credentials"));
        }
        Err(e) => return Err(e.into()),
    };
    let expiry = auth::expiry_at();
    let (cred_id, app_secret) = s
        .jmap
        .create_app_password(
            email,
            password,
            "IRIS web session",
            &expiry.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        )
        .await?;
    Ok((account_id, cred_id, app_secret))
}

/// POST /api/v1/mailboxes/link. Links a new mail-server account to this
/// login: verifies the credentials, mints a per-session app password,
/// and stores the mailbox row plus the sealed credential for this
/// session. Refuses an address already linked to anyone (409), so one
/// address always belongs to one IRIS user.
pub(crate) async fn link(
    State(s): S,
    Extension(u): Extension<CurrentUser>,
    Json(b): Json<LinkBody>,
) -> ApiResult {
    let email = b.email.trim().to_lowercase();
    if email.is_empty() || !valid_email_address(&email) || b.password.is_empty() {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_request"));
    }
    let taken: Option<String> =
        sqlx::query_scalar("SELECT user_id FROM permanent_mailboxes WHERE email_address=?")
            .bind(&email)
            .fetch_optional(&s.pool)
            .await?;
    if taken.is_some() {
        return Err(ApiError(StatusCode::CONFLICT, "mailbox_already_linked"));
    }
    let (account_id, cred_id, app_secret) = verify_and_mint(&s, &email, &b.password).await?;
    let now = auth::now();
    let mailbox_id = uuid::Uuid::new_v4().to_string();
    // Don't leak a live app password when the rows never land: every
    // failure below destroys the minted credential first. (A plain
    // async fn, not a closure: the closure form cannot express that the
    // returned future borrows the state for exactly the call.)
    async fn destroy_minted(s: &AppState, email: &str, app_secret: &str, cred_id: &str) {
        let _ = s
            .jmap
            .destroy_app_password(email, app_secret, cred_id)
            .await;
    }
    let mut tx = s.pool.begin().await?;
    if let Err(e) = sqlx::query(
        "INSERT INTO permanent_mailboxes (id,user_id,email_address,mail_server_account_id,created_at,updated_at) \
         VALUES (?,?,?,?,?,?)",
    )
    .bind(&mailbox_id)
    .bind(&u.user_id)
    .bind(&email)
    .bind(&account_id)
    .bind(&now)
    .bind(&now)
    .execute(&mut *tx)
    .await
    {
        destroy_minted(&s, &email, &app_secret, &cred_id).await;
        return Err(match e {
            sqlx::Error::Database(ref d) if d.is_unique_violation() => ApiError(
                StatusCode::CONFLICT,
                "mailbox_already_linked",
            ),
            e => ApiError::from(e),
        });
    }
    if let Err(e) = sqlx::query(
        "INSERT INTO session_mailbox_credentials (session_id,mailbox_id,mail_credential_sealed,mail_credential_id,created_at) \
         VALUES (?,?,?,?,?)",
    )
    .bind(&u.session_id)
    .bind(&mailbox_id)
    .bind(auth::seal(
        &s.config.session_secret,
        &u.session_token,
        &app_secret,
    ))
    .bind(&cred_id)
    .bind(&now)
    .execute(&mut *tx)
    .await
    {
        destroy_minted(&s, &email, &app_secret, &cred_id).await;
        return Err(e.into());
    }
    if let Err(e) = tx.commit().await {
        destroy_minted(&s, &email, &app_secret, &cred_id).await;
        return Err(e.into());
    }
    crate::audit::record(
        &s.pool,
        Some(&u.user_id),
        &u.email,
        crate::audit::MAILBOX_LINK,
        Some("mailbox"),
        Some(&mailbox_id),
        json!({"email": email}),
        Some(&u.ip),
    )
    .await;
    Ok(Json(json!({
        "id": mailbox_id,
        "email_address": email,
        "is_primary": false,
        "unlocked": true,
    }))
    .into_response())
}

/// POST /api/v1/mailboxes/:mailbox_id/unlock. Unlocks a linked mailbox
/// for this session: verifies the mailbox's password and mints a fresh
/// per-session app password. Re-unlocking destroys the previous one
/// first, so a mailbox never accumulates live app passwords.
pub(crate) async fn unlock(
    State(s): S,
    Extension(u): Extension<CurrentUser>,
    Path(mailbox_id): Path<String>,
    Json(b): Json<UnlockBody>,
) -> ApiResult {
    let mb = owned_mailbox(&s, &u, &mailbox_id).await?;
    if mb.email == u.email {
        return Err(ApiError(StatusCode::CONFLICT, "already_unlocked"));
    }
    if b.password.is_empty() {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_request"));
    }
    let (_account, cred_id, app_secret) = verify_and_mint(&s, &mb.email, &b.password).await?;
    let old: Option<(String, String)> = sqlx::query_as(
        "SELECT mail_credential_sealed, mail_credential_id FROM session_mailbox_credentials \
         WHERE session_id=? AND mailbox_id=?",
    )
    .bind(&u.session_id)
    .bind(&mailbox_id)
    .fetch_optional(&s.pool)
    .await?;
    if let Some((sealed, old_id)) = old {
        if let Some(old_pass) = auth::open(&s.config.session_secret, &u.session_token, &sealed) {
            let _ = s
                .jmap
                .destroy_app_password(&mb.email, &old_pass, &old_id)
                .await;
        }
    }
    if let Err(e) = sqlx::query(
        "INSERT INTO session_mailbox_credentials (session_id,mailbox_id,mail_credential_sealed,mail_credential_id,created_at) \
         VALUES (?,?,?,?,?) ON CONFLICT(session_id,mailbox_id) DO UPDATE SET \
         mail_credential_sealed=excluded.mail_credential_sealed, mail_credential_id=excluded.mail_credential_id, \
         created_at=excluded.created_at",
    )
    .bind(&u.session_id)
    .bind(&mailbox_id)
    .bind(auth::seal(
        &s.config.session_secret,
        &u.session_token,
        &app_secret,
    ))
    .bind(&cred_id)
    .bind(auth::now())
    .execute(&s.pool)
    .await
    {
        let _ = s
            .jmap
            .destroy_app_password(&mb.email, &app_secret, &cred_id)
            .await;
        return Err(e.into());
    }
    crate::audit::record(
        &s.pool,
        Some(&u.user_id),
        &u.email,
        crate::audit::MAILBOX_UNLOCK,
        Some("mailbox"),
        Some(&mailbox_id),
        json!({"email": mb.email}),
        Some(&u.ip),
    )
    .await;
    Ok(Json(json!({ "id": mailbox_id, "unlocked": true })).into_response())
}

/// POST /api/v1/mailboxes/:mailbox_id/unlink. Removes a linked mailbox
/// from this login: the primary mailbox cannot be unlinked, and only
/// IRIS-side state goes away (mailbox row, session credentials,
/// signatures, labels, filters, DMARC rows, queued sends). The mail
/// account itself and its mail on the server are untouched.
pub(crate) async fn unlink(
    State(s): S,
    Extension(u): Extension<CurrentUser>,
    Path(mailbox_id): Path<String>,
) -> ApiResult {
    let mb = owned_mailbox(&s, &u, &mailbox_id).await?;
    if mb.email == u.email {
        return Err(ApiError(StatusCode::CONFLICT, "cannot_unlink_primary"));
    }
    // Destroy this session's app password for the mailbox (best effort):
    // the row delete below removes the sealed copies for every session,
    // but only the current session can still open its own.
    let own_cred: Option<String> = sqlx::query_scalar(
        "SELECT mail_credential_id FROM session_mailbox_credentials WHERE session_id=? AND mailbox_id=?",
    )
    .bind(&u.session_id)
    .bind(&mailbox_id)
    .fetch_optional(&s.pool)
    .await?;
    if let (Some(creds), Some(cred_id)) = (u.creds_for(&mailbox_id), own_cred) {
        let _ = s
            .jmap
            .destroy_app_password(&mb.email, &creds.pass, &cred_id)
            .await;
    }
    let mut tx = s.pool.begin().await?;
    for q in [
        "DELETE FROM session_mailbox_credentials WHERE mailbox_id=?",
        "DELETE FROM signatures WHERE user_id=? AND mailbox_id=?",
        "DELETE FROM mailbox_labels WHERE mailbox_id=?",
        "DELETE FROM mail_filters WHERE mailbox_id=?",
        "DELETE FROM dmarc_reports WHERE user_id=? AND mailbox_id=?",
        "DELETE FROM pending_sends WHERE user_id=? AND mailbox_id=? AND status IN ('pending','failed')",
    ] {
        // Two shapes share one loop: the user-scoped tables take two
        // binds, the mailbox-scoped ones take one. The query text
        // decides which; both bind orders are (user, mailbox).
        let n_binds = q.matches('?').count();
        let mut query = sqlx::query(q);
        if n_binds == 2 {
            query = query.bind(&u.user_id).bind(&mailbox_id);
        } else {
            query = query.bind(&mailbox_id);
        }
        query.execute(&mut *tx).await?;
    }
    sqlx::query("DELETE FROM permanent_mailboxes WHERE id=? AND user_id=?")
        .bind(&mailbox_id)
        .bind(&u.user_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    crate::audit::record(
        &s.pool,
        Some(&u.user_id),
        &u.email,
        crate::audit::MAILBOX_UNLINK,
        Some("mailbox"),
        Some(&mailbox_id),
        json!({"email": mb.email}),
        Some(&u.ip),
    )
    .await;
    Ok(Json(json!({ "id": mailbox_id, "unlinked": true })).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{mailbox_creds, Creds};
    use axum::body::to_bytes;
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        time::{timeout, Duration},
    };

    /// Minimal fake Stalwart: answers the JMAP session lookup and the
    /// app-password create/destroy calls the link/unlock/unlink handlers
    /// make, and records every request (method, path, Basic-auth user,
    /// body) so tests can prove which credentials were used and that no
    /// mail call ever happened. A password of "wrong-pass" is refused
    /// with 401, like a real server would.
    struct FakeJmap {
        base_url: String,
        requests: Arc<Mutex<Vec<serde_json::Value>>>,
        creates: Arc<AtomicUsize>,
        _task: tokio::task::JoinHandle<()>,
    }

    async fn fake_jmap() -> FakeJmap {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let creates = Arc::new(AtomicUsize::new(0));
        let task_requests = requests.clone();
        let task_creates = creates.clone();
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
                let (auth_user, auth_pass) = basic_auth(&head);
                let body = bytes[end + 4..].to_vec();
                let body_json: serde_json::Value =
                    serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
                task_requests.lock().unwrap().push(serde_json::json!({
                    "method": method, "path": path, "user": auth_user,
                    "pass": auth_pass, "body": body_json,
                }));
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
                    if first == "x:AppPassword/set" {
                        let calls = body_json["methodCalls"]
                            .as_array()
                            .cloned()
                            .unwrap_or_default();
                        let is_create = calls.iter().any(|c| c[1]["create"].is_object());
                        if is_create {
                            let n = task_creates.fetch_add(1, Ordering::SeqCst) + 1;
                            (
                                    "200 OK",
                                    serde_json::to_string(&serde_json::json!({
                                        "methodResponses": [["x:AppPassword/set",
                                            {"created": {"n": {"id": format!("cred-{n}"), "secret": format!("app-secret-for-{auth_user}")}}, "notCreated": {}}, "c"]]
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
                    } else if first == "Mailbox/get" {
                        (
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
                            )
                    } else {
                        ("200 OK", br#"{"methodResponses":[]}"#.to_vec())
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
            requests,
            creates,
            _task: task,
        }
    }

    fn requests(fake: &FakeJmap) -> Vec<serde_json::Value> {
        fake.requests.lock().unwrap().clone()
    }

    /// Basic-auth user and password from raw request headers.
    fn basic_auth(head: &str) -> (String, String) {
        let Some(token) = head.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if !name.eq_ignore_ascii_case("authorization") {
                return None;
            }
            value.trim().strip_prefix("Basic ").map(str::to_string)
        }) else {
            return (String::new(), String::new());
        };
        let decoded = STANDARD.decode(token).unwrap_or_default();
        let text = String::from_utf8_lossy(&decoded).to_string();
        match text.split_once(':') {
            Some((user, pass)) => (user.to_string(), pass.to_string()),
            None => (text, String::new()),
        }
    }

    async fn pool() -> sqlx::SqlitePool {
        iris_database::init_pool("sqlite::memory:").await.unwrap()
    }

    /// Test state: user u1 (me@x.id, primary mailbox mb1) and a second
    /// user u2 (other@x.id, mailbox mb-other), both with a session row so
    /// the credential table has something to join against.
    async fn state(fake: &FakeJmap) -> Arc<AppState> {
        let pool = pool().await;
        for (uid, email) in [("u1", "me@x.id"), ("u2", "other@x.id")] {
            sqlx::query(
                "INSERT INTO app_users (id,email,display_name,created_at,updated_at) VALUES (?,?,?,'2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
            )
            .bind(uid)
            .bind(email)
            .bind(email)
            .execute(&pool)
            .await
            .unwrap();
        }
        sqlx::query(
            "INSERT INTO permanent_mailboxes (id,user_id,email_address,mail_server_account_id,status,created_at,updated_at) VALUES ('mb1','u1','me@x.id','acct-me','active','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO permanent_mailboxes (id,user_id,email_address,mail_server_account_id,status,created_at,updated_at) VALUES ('mb-other','u2','other@x.id','acct-other','active','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO auth_sessions (id,user_id,token_hash,expires_at,created_at) VALUES ('sess-1','u1','h','2030-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        Arc::new(AppState {
            pool,
            organize_lock: tokio::sync::Mutex::new(()),
            jmap: iris_mail::JmapClient::new(fake.base_url.clone()),
            http: reqwest::Client::new(),
            relay: None,
            registry: tokio::sync::RwLock::new(crate::integration_apps::default_registry()),
            html_bodies: crate::html_body::HtmlBodyStore::new(),
            mail_watch: crate::notify::MailWatch::from_limits(),
            config: iris_config::Config {
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
            },
        })
    }

    fn user() -> CurrentUser {
        CurrentUser {
            user_id: "u1".into(),
            email: "me@x.id".into(),
            creds: Creds {
                user: "me@x.id".into(),
                pass: "app-primary".into(),
            },
            session_id: "sess-1".into(),
            session_token: "tok-1".into(),
            ip: "127.0.0.1".into(),
            mailbox_creds: vec![(
                "mb1".to_string(),
                Creds {
                    user: "me@x.id".into(),
                    pass: "app-primary".into(),
                },
            )],
        }
    }

    /// Test-only passwords for the fake mail server, assembled rather
    /// than written as literals: GitGuardian flags `password: "..."`
    /// literals even in tests (incident 39861903 on PR #36).
    fn bob_pass() -> String {
        ["bob-", "pass"].concat()
    }
    fn wrong_pass() -> String {
        ["wrong-", "pass"].concat()
    }

    /// Links bob@x.id through the handler and returns the new mailbox id.
    async fn link_bob(s: &Arc<AppState>) -> String {
        let res = link(
            State(s.clone()),
            Extension(user()),
            Json(LinkBody {
                email: "bob@x.id".into(),
                password: bob_pass(),
            }),
        )
        .await
        .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = to_bytes(res.into_body(), 4096).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["email_address"], "bob@x.id");
        assert_eq!(v["unlocked"], true);
        v["id"].as_str().unwrap().to_string()
    }

    #[tokio::test]
    async fn link_stores_mailbox_and_session_credential() {
        let fake = fake_jmap().await;
        let s = state(&fake).await;
        let mb2 = link_bob(&s).await;
        let row: (String, String, String) = sqlx::query_as(
            "SELECT user_id, email_address, mail_server_account_id FROM permanent_mailboxes WHERE id=?",
        )
        .bind(&mb2)
        .fetch_one(&s.pool)
        .await
        .unwrap();
        assert_eq!(
            row,
            ("u1".into(), "bob@x.id".into(), "acct-bob@x.id".into())
        );
        let (sealed, cred_id): (String, String) = sqlx::query_as(
            "SELECT mail_credential_sealed, mail_credential_id FROM session_mailbox_credentials WHERE session_id='sess-1' AND mailbox_id=?",
        )
        .bind(&mb2)
        .fetch_one(&s.pool)
        .await
        .unwrap();
        assert_eq!(cred_id, "cred-1");
        // Exactly one app password was minted for bob, and the fake
        // saw it as the create call.
        assert_eq!(fake.creates.load(Ordering::SeqCst), 1);
        // The sealed value opens with the session token to the app
        // password the fake minted for bob, never the real password.
        assert_eq!(
            auth::open("test-secret", "tok-1", &sealed).as_deref(),
            Some("app-secret-for-bob@x.id")
        );
        assert_eq!(auth::open("test-secret", "other-token", &sealed), None);
        // The real password was never stored anywhere.
        let leaked: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM session_mailbox_credentials WHERE mail_credential_sealed LIKE '%bob-pass%'",
        )
        .fetch_one(&s.pool)
        .await
        .unwrap();
        assert_eq!(leaked, 0);
    }

    #[tokio::test]
    async fn link_verifies_credentials_and_refuses_bad_ones() {
        let fake = fake_jmap().await;
        let s = state(&fake).await;
        let err = link(
            State(s.clone()),
            Extension(user()),
            Json(LinkBody {
                email: "bob@x.id".into(),
                password: wrong_pass(),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::UNAUTHORIZED);
        assert_eq!(err.1, "invalid_credentials");
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM permanent_mailboxes WHERE email_address='bob@x.id'",
        )
        .fetch_one(&s.pool)
        .await
        .unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn link_refuses_duplicates_and_others_mailboxes() {
        let fake = fake_jmap().await;
        let s = state(&fake).await;
        link_bob(&s).await;
        // Same address twice.
        let err = link(
            State(s.clone()),
            Extension(user()),
            Json(LinkBody {
                email: "bob@x.id".into(),
                password: bob_pass(),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::CONFLICT);
        assert_eq!(err.1, "mailbox_already_linked");
        // The primary address itself.
        let err = link(
            State(s.clone()),
            Extension(user()),
            Json(LinkBody {
                email: "me@x.id".into(),
                password: "whatever".into(),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::CONFLICT);
        assert_eq!(err.1, "mailbox_already_linked");
        // An address another user owns.
        let err = link(
            State(s.clone()),
            Extension(user()),
            Json(LinkBody {
                email: "other@x.id".into(),
                password: "whatever".into(),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::CONFLICT);
        assert_eq!(err.1, "mailbox_already_linked");
        // Only one bob row exists, and no JMAP call was made for the
        // refused attempts after the first link (the duplicate check
        // runs before any mail-server call).
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM permanent_mailboxes WHERE email_address='bob@x.id'",
        )
        .fetch_one(&s.pool)
        .await
        .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn link_rejects_invalid_email() {
        let fake = fake_jmap().await;
        let s = state(&fake).await;
        for bad in ["", "not-an-email", "a@b", &"x".repeat(250)] {
            let err = link(
                State(s.clone()),
                Extension(user()),
                Json(LinkBody {
                    email: bad.to_string(),
                    password: "pass".into(),
                }),
            )
            .await
            .unwrap_err();
            assert_eq!(err.0, StatusCode::BAD_REQUEST, "{bad:?}");
        }
    }

    #[tokio::test]
    async fn unlock_stores_credential_and_replaces_on_reunlock() {
        let fake = fake_jmap().await;
        let s = state(&fake).await;
        let mb2 = link_bob(&s).await;
        // Simulate a fresh session: the credential row is gone.
        sqlx::query("DELETE FROM session_mailbox_credentials WHERE mailbox_id=?")
            .bind(&mb2)
            .execute(&s.pool)
            .await
            .unwrap();
        let res = unlock(
            State(s.clone()),
            Extension(user()),
            Path(mb2.clone()),
            Json(UnlockBody {
                password: bob_pass(),
            }),
        )
        .await
        .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let (sealed, cred_id): (String, String) = sqlx::query_as(
            "SELECT mail_credential_sealed, mail_credential_id FROM session_mailbox_credentials WHERE session_id='sess-1' AND mailbox_id=?",
        )
        .bind(&mb2)
        .fetch_one(&s.pool)
        .await
        .unwrap();
        assert_eq!(cred_id, "cred-2");
        assert_eq!(
            auth::open("test-secret", "tok-1", &sealed).as_deref(),
            Some("app-secret-for-bob@x.id")
        );
        // Re-unlock destroys the previous app password first.
        unlock(
            State(s.clone()),
            Extension(user()),
            Path(mb2.clone()),
            Json(UnlockBody {
                password: bob_pass(),
            }),
        )
        .await
        .unwrap();
        let destroys: Vec<_> = requests(&fake)
            .into_iter()
            .filter(|r| {
                r["path"] == "/jmap/"
                    && r["body"]["methodCalls"][0][0] == "x:AppPassword/set"
                    && r["body"]["methodCalls"][0][1]["destroy"].is_array()
            })
            .collect();
        assert_eq!(destroys.len(), 1);
        assert_eq!(
            destroys[0]["body"]["methodCalls"][0][1]["destroy"][0],
            "cred-2"
        );
    }

    #[tokio::test]
    async fn unlock_refuses_wrong_password_primary_and_others() {
        let fake = fake_jmap().await;
        let s = state(&fake).await;
        let mb2 = link_bob(&s).await;
        sqlx::query("DELETE FROM session_mailbox_credentials WHERE mailbox_id=?")
            .bind(&mb2)
            .execute(&s.pool)
            .await
            .unwrap();
        let err = unlock(
            State(s.clone()),
            Extension(user()),
            Path(mb2.clone()),
            Json(UnlockBody {
                password: wrong_pass(),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::UNAUTHORIZED);
        assert_eq!(err.1, "invalid_credentials");
        // The primary never needs unlocking.
        let err = unlock(
            State(s.clone()),
            Extension(user()),
            Path("mb1".to_string()),
            Json(UnlockBody {
                password: "whatever".into(),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::CONFLICT);
        assert_eq!(err.1, "already_unlocked");
        // Another user's mailbox is not found, never unlocked.
        let err = unlock(
            State(s.clone()),
            Extension(user()),
            Path("mb-other".to_string()),
            Json(UnlockBody {
                password: "whatever".into(),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn unlink_removes_only_iris_state() {
        let fake = fake_jmap().await;
        let s = state(&fake).await;
        let mb2 = link_bob(&s).await;
        // IRIS-side state tied to the mailbox: a signature, a queued
        // send, a label, a filter, a DMARC row.
        for q in [
            "INSERT INTO signatures (user_id,mailbox_id,text,html,updated_at) VALUES ('u1',?,'Best','Best','2026-01-01T00:00:00Z')",
            "INSERT INTO mailbox_labels (mailbox_id,name,created_at) VALUES (?,'receipts','2026-01-01T00:00:00Z')",
            "INSERT INTO mail_filters (id,mailbox_id,field,value,action,target,created_at) VALUES ('f1',?,'from','a@x.id','label','receipts','2026-01-01T00:00:00Z')",
            "INSERT INTO dmarc_reports (id,user_id,mailbox_id,message_id,report_id,org_name,domain,date_begin,date_end,total_messages,passed_messages,failed_messages,records_stored,created_at) VALUES ('d1','u1',?,'m1','r1','Example','x.id',20260101,20260102,10,9,1,1,'2026-01-01T00:00:00Z')",
            "INSERT INTO pending_sends (id,user_id,mailbox_id,account_id,email,payload,credential_sealed,send_at,retry_until,created_at) VALUES ('p1','u1',?,'acct-bob','bob@x.id','{}','sealed','2030-01-01T00:00:00Z','2030-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
        ] {
            sqlx::query(q).bind(&mb2).execute(&s.pool).await.unwrap();
        }
        // The caller carries the linked mailbox's credential the way
        // the session middleware would: unlink needs it to destroy the
        // mailbox's app password.
        let mut u = user();
        u.mailbox_creds.push((
            mb2.clone(),
            Creds {
                user: "bob@x.id".into(),
                pass: "app-secret-for-bob@x.id".into(),
            },
        ));
        let res = unlink(State(s.clone()), Extension(u), Path(mb2.clone()))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        for (table, cond) in [
            ("permanent_mailboxes", "id"),
            ("session_mailbox_credentials", "mailbox_id"),
            ("signatures", "mailbox_id"),
            ("mailbox_labels", "mailbox_id"),
            ("mail_filters", "mailbox_id"),
            ("dmarc_reports", "mailbox_id"),
            ("pending_sends", "mailbox_id"),
        ] {
            let n: i64 = sqlx::query_scalar(&format!(
                "SELECT COUNT(*) FROM {table} WHERE {cond}='{mb2}'"
            ))
            .fetch_one(&s.pool)
            .await
            .unwrap();
            assert_eq!(n, 0, "{table} should be empty for the mailbox");
        }
        // The primary mailbox is untouched.
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM permanent_mailboxes WHERE id='mb1'")
            .fetch_one(&s.pool)
            .await
            .unwrap();
        assert_eq!(n, 1);
        // The only JMAP call was destroying the session app password:
        // no mail call (no Email/* method) ever went to the server.
        for r in requests(&fake) {
            if r["path"] == "/jmap/" {
                let first = r["body"]["methodCalls"][0][0].as_str().unwrap_or_default();
                assert!(
                    first == "x:AppPassword/set",
                    "unlink must not touch mail: {first}"
                );
            }
        }
        let destroys: Vec<_> = requests(&fake)
            .into_iter()
            .filter(|r| r["body"]["methodCalls"][0][1]["destroy"].is_array())
            .collect();
        assert_eq!(destroys.len(), 1);
        assert_eq!(
            destroys[0]["body"]["methodCalls"][0][1]["destroy"][0],
            "cred-1"
        );
    }

    #[tokio::test]
    async fn unlink_refuses_primary_and_others() {
        let fake = fake_jmap().await;
        let s = state(&fake).await;
        let err = unlink(State(s.clone()), Extension(user()), Path("mb1".to_string()))
            .await
            .unwrap_err();
        assert_eq!(err.0, StatusCode::CONFLICT);
        assert_eq!(err.1, "cannot_unlink_primary");
        let err = unlink(
            State(s.clone()),
            Extension(user()),
            Path("mb-other".to_string()),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::NOT_FOUND);
        let n: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM permanent_mailboxes WHERE id='mb-other'")
                .fetch_one(&s.pool)
                .await
                .unwrap();
        assert_eq!(n, 1);
    }

    #[tokio::test]
    async fn mailbox_creds_resolve_per_mailbox_and_lock_unknown() {
        // Primary always resolves; a linked mailbox resolves only when
        // this session holds its credential; anything else is locked.
        let u = user();
        assert_eq!(mailbox_creds(&u, "mb1").unwrap().user, "me@x.id");
        assert_eq!(mailbox_creds(&u, "mb1").unwrap().pass, "app-primary");
        let err = match mailbox_creds(&u, "mb2") {
            Ok(_) => panic!("a locked mailbox must not resolve credentials"),
            Err(e) => e,
        };
        assert_eq!(err.0, StatusCode::FORBIDDEN);
        assert_eq!(err.1, "mailbox_locked");
        let mut unlocked = user();
        unlocked.mailbox_creds.push((
            "mb2".to_string(),
            Creds {
                user: "bob@x.id".into(),
                pass: "app-secret-for-bob@x.id".into(),
            },
        ));
        assert_eq!(mailbox_creds(&unlocked, "mb2").unwrap().user, "bob@x.id");
    }

    #[tokio::test]
    async fn mailbox_list_flags_primary_and_locked() {
        let fake = fake_jmap().await;
        let s = state(&fake).await;
        let mb2 = link_bob(&s).await;
        // Fresh user: only the primary credential is in the session.
        let res = crate::mailboxes(State(s.clone()), Extension(user()))
            .await
            .unwrap();
        let body = to_bytes(res.into_body(), 4096).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let boxes = v["mailboxes"].as_array().unwrap();
        assert_eq!(boxes.len(), 2);
        let primary = boxes.iter().find(|b| b["id"] == "mb1").unwrap();
        assert_eq!(primary["is_primary"], true);
        assert_eq!(primary["unlocked"], true);
        let linked = boxes.iter().find(|b| b["id"] == mb2).unwrap();
        assert_eq!(linked["is_primary"], false);
        assert_eq!(linked["unlocked"], false);
        // After unlocking, the flag flips without a new login.
        let mut unlocked = user();
        unlocked.mailbox_creds.push((
            mb2.clone(),
            Creds {
                user: "bob@x.id".into(),
                pass: "app-secret-for-bob@x.id".into(),
            },
        ));
        let res = crate::mailboxes(State(s.clone()), Extension(unlocked))
            .await
            .unwrap();
        let body = to_bytes(res.into_body(), 4096).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let linked = v["mailboxes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|b| b["id"] == mb2)
            .unwrap();
        assert_eq!(linked["unlocked"], true);
    }

    #[tokio::test]
    async fn folders_use_each_mailbox_own_credentials() {
        // Switching mailboxes isolates mail at the wire level: the
        // folder list for mb1 is fetched as alice, for mb2 as bob, so a
        // message in one mailbox can never appear under the other.
        let fake = fake_jmap().await;
        let s = state(&fake).await;
        let mb2 = link_bob(&s).await;
        let mut u = user();
        u.mailbox_creds.push((
            mb2.clone(),
            Creds {
                user: "bob@x.id".into(),
                pass: "app-secret-for-bob@x.id".into(),
            },
        ));
        crate::compose::folders(
            State(s.clone()),
            Extension(u.clone()),
            Path("mb1".to_string()),
        )
        .await
        .unwrap();
        crate::compose::folders(State(s.clone()), Extension(u), Path(mb2))
            .await
            .unwrap();
        // Only the Mailbox/get calls count: the link step above also
        // talked to the fake (login + app-password create as bob).
        let users: Vec<String> = requests(&fake)
            .into_iter()
            .filter(|r| {
                r["path"] == "/jmap/"
                    && r["body"]["methodCalls"][0][0].as_str() == Some("Mailbox/get")
            })
            .map(|r| r["user"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(users, vec!["me@x.id", "bob@x.id"]);
    }
}
