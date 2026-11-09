//! Undo send: a compose send waits out a 10 second window in SQLite
//! before the background sweeper dispatches it, so the web can offer Undo.
//!
//! The request handler only validates and queues; delivery happens in
//! `sweep_once`, which runs every second in-process. The pending row
//! carries the validated compose request as JSON plus the user's mail
//! credential sealed with the server secret alone (the sweeper has no
//! session token to open a session-sealed credential with). The row is
//! deleted on dispatch, on undo, or when its retry deadline passes.
//!
//! At-least-once, by design: a row is claimed atomically (a conditional
//! UPDATE, so two sweeps cannot both send it), but a crash between a
//! successful delivery and the row delete would re-dispatch after a
//! restart. The relay and JMAP have no idempotency key for user compose
//! sends, so that window cannot be closed here; it is one DELETE
//! statement wide.
use crate::{
    compose::{self, ComposeBody},
    ApiError, ApiResult, AppState, Creds, CurrentUser, S,
};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Extension,
};
use chrono::{Duration, SecondsFormat, Utc};
use iris_auth as auth;
use iris_mail::MailError;
use std::sync::Arc;

/// How long a queued send waits before the sweeper may dispatch it.
pub(crate) const UNDO_WINDOW_SECS: i64 = 10;
const SWEEP_INTERVAL_SECS: u64 = 1;
/// How long a failed dispatch keeps retrying before it is marked failed.
const RETRY_WINDOW_SECS: i64 = 3600;
/// Longest pause between retries (backoff is 5s per attempt, capped here).
const MAX_BACKOFF_SECS: i64 = 300;
/// Stored failure reasons are capped like the integration error column.
const MAX_ERROR_CHARS: usize = 500;
const SWEEP_BATCH: i64 = 100;

fn ts(t: chrono::DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

pub(crate) struct Queued {
    pub(crate) id: String,
    pub(crate) send_at: String,
}

/// Persist a validated compose request for the undo window. The caller
/// (the send handler) already validated the request and checked the
/// external-sending gate; the hourly cap is enforced at dispatch time.
pub(crate) async fn queue(
    s: &AppState,
    u: &CurrentUser,
    mailbox_id: &str,
    account: &str,
    body: ComposeBody,
) -> Result<Queued, ApiError> {
    let now = Utc::now();
    let send_at = now + Duration::seconds(UNDO_WINDOW_SECS);
    let id = uuid::Uuid::new_v4().to_string();
    let payload = serde_json::to_string(&body)
        .map_err(|_| ApiError(StatusCode::INTERNAL_SERVER_ERROR, "database_unavailable"))?;
    sqlx::query(
        "INSERT INTO pending_sends (id,user_id,mailbox_id,account_id,email,payload,credential_sealed,send_at,retry_until,created_at) \
         VALUES (?,?,?,?,?,?,?,?,?,?)",
    )
    .bind(&id)
    .bind(&u.user_id)
    .bind(mailbox_id)
    .bind(account)
    .bind(&u.email)
    .bind(&payload)
    .bind(auth::seal_server(&s.config.session_secret, &u.creds.pass))
    .bind(ts(send_at))
    .bind(ts(send_at + Duration::seconds(RETRY_WINDOW_SECS)))
    .bind(ts(now))
    .execute(&s.pool)
    .await?;
    Ok(Queued {
        id,
        send_at: ts(send_at),
    })
}

enum UndoOutcome {
    Cancelled,
    AlreadySent,
    Unknown,
}

async fn cancel_row(
    pool: &sqlx::SqlitePool,
    user_id: &str,
    mailbox_id: &str,
    id: &str,
) -> Result<UndoOutcome, sqlx::Error> {
    // A failed row never sent anything, so deleting it still honors the undo.
    let deleted = sqlx::query(
        "DELETE FROM pending_sends WHERE id=? AND user_id=? AND mailbox_id=? AND status IN ('pending','failed')",
    )
    .bind(id)
    .bind(user_id)
    .bind(mailbox_id)
    .execute(pool)
    .await?;
    if deleted.rows_affected() > 0 {
        return Ok(UndoOutcome::Cancelled);
    }
    // Still here: either gone (already dispatched) or claimed for sending
    // right now, in which case it is too late to stop it.
    let status: Option<String> = sqlx::query_scalar(
        "SELECT status FROM pending_sends WHERE id=? AND user_id=? AND mailbox_id=?",
    )
    .bind(id)
    .bind(user_id)
    .bind(mailbox_id)
    .fetch_optional(pool)
    .await?;
    match status.as_deref() {
        Some("sending") => Ok(UndoOutcome::AlreadySent),
        _ => Ok(UndoOutcome::Unknown),
    }
}

/// DELETE /api/v1/mailboxes/:mailbox_id/pending-sends/:id, undo a send
/// inside the window. 204 when the send is cancelled, 409 when it is
/// already being dispatched, 404 when it already went out (or never
/// existed). Only the owning user and mailbox can cancel a row.
pub(crate) async fn undo(
    State(s): S,
    Extension(u): Extension<CurrentUser>,
    Path((mailbox_id, id)): Path<(String, String)>,
) -> ApiResult {
    match cancel_row(&s.pool, &u.user_id, &mailbox_id, &id).await? {
        UndoOutcome::Cancelled => Ok(StatusCode::NO_CONTENT.into_response()),
        UndoOutcome::AlreadySent => Err(ApiError(StatusCode::CONFLICT, "already_sent")),
        UndoOutcome::Unknown => Err(ApiError(StatusCode::NOT_FOUND, "unknown_pending_send")),
    }
}

/// Rows left `sending` by a previous process (crash between delivery and
/// row delete) go back to the queue: a stuck row would silently drop a
/// send, while a re-dispatch is the same at-least-once window the claim
/// already has. Runs once at startup; one process serves this API, so no
/// other sweeper can hold a live claim while this reset runs.
pub(crate) async fn reset_stuck_claims(pool: &sqlx::SqlitePool) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE pending_sends SET status='pending', next_try_at=NULL WHERE status='sending'",
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub(crate) async fn run(s: Arc<AppState>) {
    if let Err(e) = reset_stuck_claims(&s.pool).await {
        tracing::warn!("pending sends reset: {e}");
    }
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(SWEEP_INTERVAL_SECS));
    loop {
        tick.tick().await;
        if let Err(e) = sweep_once(&s).await {
            tracing::warn!("pending sends sweep: {e}");
        }
    }
}

/// One sweep: fail rows past their retry deadline, then dispatch every
/// due row. Past-due rows dispatch on the next sweep after a restart,
/// which is what makes the queue survive one.
pub(crate) async fn sweep_once(s: &AppState) -> anyhow::Result<()> {
    let now = Utc::now();
    sqlx::query(
        "UPDATE pending_sends SET status='failed', error='retry_expired' WHERE status='pending' AND retry_until<=?",
    )
    .bind(ts(now))
    .execute(&s.pool)
    .await?;
    let due: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM pending_sends WHERE status='pending' AND send_at<=? AND (next_try_at IS NULL OR next_try_at<=?) ORDER BY send_at LIMIT ?",
    )
    .bind(ts(now))
    .bind(ts(now))
    .bind(SWEEP_BATCH)
    .fetch_all(&s.pool)
    .await?;
    for id in due {
        dispatch_one(s, &id).await;
    }
    Ok(())
}

async fn mark_failed(pool: &sqlx::SqlitePool, id: &str, error: &str) {
    let capped: String = error.chars().take(MAX_ERROR_CHARS).collect();
    let _ = sqlx::query("UPDATE pending_sends SET status='failed', error=? WHERE id=?")
        .bind(capped)
        .bind(id)
        .execute(pool)
        .await;
}

async fn defer(pool: &sqlx::SqlitePool, id: &str, attempts: i64) {
    let delay = (5 * attempts).min(MAX_BACKOFF_SECS);
    let _ = sqlx::query("UPDATE pending_sends SET status='pending', next_try_at=? WHERE id=?")
        .bind(ts(Utc::now() + Duration::seconds(delay)))
        .bind(id)
        .execute(pool)
        .await;
}

type PendingRow = (
    String, // id
    String, // user_id
    String, // mailbox_id
    String, // account_id
    String, // email
    String, // payload
    String, // credential_sealed
    i64,    // attempts
);

/// Claim one row and dispatch it. The claim is a conditional UPDATE, so
/// two sweeps racing the same row cannot both send it.
async fn dispatch_one(s: &AppState, id: &str) {
    let claimed = sqlx::query(
        "UPDATE pending_sends SET status='sending', attempts=attempts+1, next_try_at=NULL WHERE id=? AND status='pending'",
    )
    .bind(id)
    .execute(&s.pool)
    .await;
    let Ok(claimed) = claimed else { return };
    if claimed.rows_affected() == 0 {
        return;
    }
    let row: Option<PendingRow> = sqlx::query_as(
        "SELECT id,user_id,mailbox_id,account_id,email,payload,credential_sealed,attempts FROM pending_sends WHERE id=?",
    )
    .bind(id)
    .fetch_optional(&s.pool)
    .await
    .ok()
    .flatten();
    let Some(row) = row else { return };
    if let Err(reason) = deliver(s, &row).await {
        tracing::warn!("pending send {} failed: {reason}", row.0);
    }
}

/// Deliver one claimed row through the same code path an immediate send
/// used to take: hourly cap first, then the relay for other domains
/// (with the Sent copy filed afterwards) or JMAP submission for local
/// domains. Returns the failure reason for the log; the row state was
/// already set. Only a permanent refusal marks the row failed; anything
/// else goes back to pending for another try until the retry deadline.
async fn deliver(s: &AppState, row: &PendingRow) -> Result<(), String> {
    let (id, user_id, _mailbox_id, account_id, email, payload, sealed, attempts) = row;
    let Some(pass) = auth::open_server(&s.config.session_secret, sealed) else {
        mark_failed(&s.pool, id, "credential_unsealed").await;
        return Err("credential_unsealed".into());
    };
    let Ok(body) = serde_json::from_str::<ComposeBody>(payload) else {
        mark_failed(&s.pool, id, "payload_unreadable").await;
        return Err("payload_unreadable".into());
    };
    let user = CurrentUser {
        user_id: user_id.clone(),
        email: email.clone(),
        creds: Creds {
            user: email.clone(),
            pass,
        },
    };
    let Ok((c, draft)) = compose::build(&user, body) else {
        mark_failed(&s.pool, id, "payload_invalid").await;
        return Err("payload_invalid".into());
    };
    match compose::reserve_send_slot(&s.pool, user_id).await {
        Ok(true) => {}
        Ok(false) => {
            defer(&s.pool, id, *attempts).await;
            return Err("too_many_sends".into());
        }
        Err(e) => {
            defer(&s.pool, id, *attempts).await;
            return Err(e.to_string());
        }
    }
    let external = compose::first_external(s, &c).is_some();
    let sent = match (&s.relay, external) {
        // Other domains go out through the relay (it also delivers our
        // local recipients via MX); the copy for Sent is filed afterwards.
        // A failed copy doesn't undo a sent message.
        (Some(relay), true) => match relay.send(&c).await {
            Ok(_) => {
                if let Err(e) = s
                    .jmap
                    .store_sent(
                        &user.creds.user,
                        &user.creds.pass,
                        account_id,
                        &c,
                        draft.as_deref(),
                    )
                    .await
                {
                    tracing::error!(
                        "pending send {id} delivered via relay but Sent copy failed: {e}"
                    );
                }
                Ok(())
            }
            Err(e) => Err(e),
        },
        _ => {
            s.jmap
                .send(
                    &user.creds.user,
                    &user.creds.pass,
                    account_id,
                    &c,
                    draft.as_deref(),
                )
                .await
        }
    };
    match sent {
        Ok(()) => {
            let _ = sqlx::query("DELETE FROM pending_sends WHERE id=?")
                .bind(id)
                .execute(&s.pool)
                .await;
            Ok(())
        }
        Err(MailError::Rejected(detail)) => {
            mark_failed(&s.pool, id, &detail).await;
            Err(detail)
        }
        Err(e) => {
            defer(&s.pool, id, *attempts).await;
            Err(e.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iris_mail::{Relay, RelayChain, ResendClient};

    fn test_user() -> CurrentUser {
        CurrentUser {
            user_id: "u1".into(),
            email: "me@x.id".into(),
            creds: Creds {
                user: "me@x.id".into(),
                pass: "secret".into(),
            },
        }
    }

    fn body() -> ComposeBody {
        ComposeBody {
            to: vec!["b@example.com".into()],
            subject: "hi".into(),
            text: "hello".into(),
            ..Default::default()
        }
    }

    async fn state_with_relay(relay: Option<RelayChain>) -> Arc<AppState> {
        // Full Config literal, like the other test states: adding a field
        // to Config breaks this on purpose, so the test state never
        // silently runs with a zero value.
        Arc::new(AppState {
            pool: iris_database::init_pool("sqlite::memory:").await.unwrap(),
            organize_lock: tokio::sync::Mutex::new(()),
            jmap: iris_mail::JmapClient::new("http://127.0.0.1:9".into()),
            http: reqwest::Client::new(),
            relay,
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
                resend_api_key: "test-resend-key".into(),
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

    /// Minimal local stand-in for the Resend relay: answers every request
    /// with the given status line and JSON body, and counts calls plus the
    /// raw request bytes, so a test can prove whether the relay was hit
    /// and what the send path actually rendered.
    struct FakeRelay {
        endpoint: String,
        calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        captured: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
        _task: tokio::task::JoinHandle<()>,
    }

    async fn fake_relay(status_line: &'static str, response_body: &'static str) -> FakeRelay {
        use tokio::{
            io::{AsyncReadExt, AsyncWriteExt},
            net::TcpListener,
            time::{timeout, Duration},
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let calls_in_task = calls.clone();
        let captured_in_task = captured.clone();
        let status_line = status_line.to_string();
        let response_body = response_body.to_string();
        let task = tokio::spawn(async move {
            loop {
                let Ok(Ok((mut stream, _))) =
                    timeout(Duration::from_secs(5), listener.accept()).await
                else {
                    return;
                };
                calls_in_task.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let mut bytes = Vec::new();
                let mut chunk = [0u8; 1024];
                let mut expected = None;
                while let Ok(n) = stream.read(&mut chunk).await {
                    if n == 0 {
                        break;
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
            }
        });
        FakeRelay {
            endpoint: format!("http://{address}"),
            calls,
            captured,
            _task: task,
        }
    }

    fn relay_chain(fake: &FakeRelay) -> RelayChain {
        RelayChain::new(vec![Relay::Resend(ResendClient::with_endpoint(
            "test-resend-key".into(),
            format!("{}/emails", fake.endpoint),
        ))])
    }

    async fn queued(state: &Arc<AppState>) -> Queued {
        queue(state, &test_user(), "mb1", "acc1", body())
            .await
            .unwrap()
    }

    async fn set_due(pool: &sqlx::SqlitePool, id: &str) {
        sqlx::query("UPDATE pending_sends SET send_at=? WHERE id=?")
            .bind(ts(Utc::now() - Duration::seconds(1)))
            .bind(id)
            .execute(pool)
            .await
            .unwrap();
    }

    async fn row_status(pool: &sqlx::SqlitePool, id: &str) -> Option<String> {
        sqlx::query_scalar("SELECT status FROM pending_sends WHERE id=?")
            .bind(id)
            .fetch_optional(pool)
            .await
            .unwrap()
    }

    fn calls(fake: &FakeRelay) -> usize {
        fake.calls.load(std::sync::atomic::Ordering::SeqCst)
    }

    #[tokio::test]
    async fn queue_stores_row_for_the_window() {
        let state = state_with_relay(None).await;
        let q = queued(&state).await;
        let row: (String, String, String, i64) =
            sqlx::query_as("SELECT status, send_at, email, attempts FROM pending_sends WHERE id=?")
                .bind(&q.id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(row.0, "pending");
        assert_eq!(row.2, "me@x.id");
        assert_eq!(row.3, 0);
        // send_at is ~10s out: not due yet, so a sweep must not send it.
        let sent_at: chrono::DateTime<Utc> =
            chrono::DateTime::parse_from_rfc3339(&row.1).unwrap().into();
        assert!(sent_at > Utc::now() + Duration::seconds(8));
        sweep_once(&state).await.unwrap();
        assert_eq!(
            row_status(&state.pool, &q.id).await.as_deref(),
            Some("pending")
        );
    }

    #[tokio::test]
    async fn undo_within_window_cancels_and_relay_is_never_called() {
        let fake = fake_relay("200 OK", r#"{"id":"resend-1"}"#).await;
        let state = state_with_relay(Some(relay_chain(&fake))).await;
        let q = queued(&state).await;
        let res = undo(
            State(state.clone()),
            Extension(test_user()),
            Path(("mb1".to_string(), q.id.clone())),
        )
        .await;
        assert!(res.is_ok());
        assert_eq!(row_status(&state.pool, &q.id).await, None);
        // Even a due sweep afterwards has nothing to send.
        set_due(&state.pool, &q.id).await;
        sweep_once(&state).await.unwrap();
        assert_eq!(calls(&fake), 0);
    }

    #[tokio::test]
    async fn undo_of_another_users_row_is_not_found() {
        let state = state_with_relay(None).await;
        let q = queued(&state).await;
        let mut other = test_user();
        other.user_id = "u2".into();
        let res = undo(
            State(state.clone()),
            Extension(other),
            Path(("mb1".to_string(), q.id.clone())),
        )
        .await;
        let err = res.unwrap_err();
        assert_eq!(err.0, StatusCode::NOT_FOUND);
        assert_eq!(err.1, "unknown_pending_send");
        assert_eq!(
            row_status(&state.pool, &q.id).await.as_deref(),
            Some("pending")
        );
    }

    #[tokio::test]
    async fn past_due_row_dispatches_via_relay() {
        let fake = fake_relay("200 OK", r#"{"id":"resend-1"}"#).await;
        let state = state_with_relay(Some(relay_chain(&fake))).await;
        let q = queued(&state).await;
        set_due(&state.pool, &q.id).await;
        sweep_once(&state).await.unwrap();
        assert_eq!(calls(&fake), 1);
        let body = String::from_utf8(fake.captured.lock().unwrap().clone()).unwrap();
        assert!(body.contains("b@example.com"), "relay got: {body}");
        assert!(body.contains("\"subject\":\"hi\""), "relay got: {body}");
        assert_eq!(row_status(&state.pool, &q.id).await, None);
    }

    #[tokio::test]
    async fn concurrent_sweeps_send_once() {
        let fake = fake_relay("200 OK", r#"{"id":"resend-1"}"#).await;
        let state = state_with_relay(Some(relay_chain(&fake))).await;
        let q = queued(&state).await;
        set_due(&state.pool, &q.id).await;
        let (a, b) = tokio::join!(sweep_once(&state), sweep_once(&state));
        a.unwrap();
        b.unwrap();
        assert_eq!(calls(&fake), 1);
        assert_eq!(row_status(&state.pool, &q.id).await, None);
    }

    #[tokio::test]
    async fn stuck_claim_from_a_restart_dispatches() {
        let fake = fake_relay("200 OK", r#"{"id":"resend-1"}"#).await;
        let state = state_with_relay(Some(relay_chain(&fake))).await;
        let q = queued(&state).await;
        set_due(&state.pool, &q.id).await;
        // Simulate a crash between claim and dispatch: the row is left `sending`.
        sqlx::query("UPDATE pending_sends SET status='sending' WHERE id=?")
            .bind(&q.id)
            .execute(&state.pool)
            .await
            .unwrap();
        sweep_once(&state).await.unwrap();
        assert_eq!(
            calls(&fake),
            0,
            "stuck claim is not retried by a plain sweep"
        );
        reset_stuck_claims(&state.pool).await.unwrap();
        sweep_once(&state).await.unwrap();
        assert_eq!(calls(&fake), 1);
        assert_eq!(row_status(&state.pool, &q.id).await, None);
    }

    #[tokio::test]
    async fn rejected_send_marks_row_failed() {
        let fake = fake_relay("400 Bad Request", r#"{"message":"bad recipient"}"#).await;
        let state = state_with_relay(Some(relay_chain(&fake))).await;
        let q = queued(&state).await;
        set_due(&state.pool, &q.id).await;
        sweep_once(&state).await.unwrap();
        assert_eq!(calls(&fake), 1);
        assert_eq!(
            row_status(&state.pool, &q.id).await.as_deref(),
            Some("failed")
        );
        let error: Option<String> =
            sqlx::query_scalar("SELECT error FROM pending_sends WHERE id=?")
                .bind(&q.id)
                .fetch_optional(&state.pool)
                .await
                .unwrap();
        assert!(error.unwrap().contains("bad recipient"));
    }

    #[tokio::test]
    async fn rate_limit_at_dispatch_keeps_row_pending() {
        let fake = fake_relay("200 OK", r#"{"id":"resend-1"}"#).await;
        let state = state_with_relay(Some(relay_chain(&fake))).await;
        // Fill the user's hourly cap: 100 allowed sends in the last hour.
        let subject = iris_auth::hash_token("send:u1");
        let since = Utc::now() - Duration::minutes(30);
        for i in 0..100 {
            sqlx::query(
                "INSERT INTO rate_limit_events (id, subject_hash, action, occurred_at, outcome) VALUES (?,?,?,?, 'allowed')",
            )
            .bind(format!("r{i}"))
            .bind(&subject)
            .bind("send")
            .bind(ts(since))
            .execute(&state.pool)
            .await
            .unwrap();
        }
        let q = queued(&state).await;
        set_due(&state.pool, &q.id).await;
        sweep_once(&state).await.unwrap();
        assert_eq!(calls(&fake), 0);
        assert_eq!(
            row_status(&state.pool, &q.id).await.as_deref(),
            Some("pending")
        );
    }

    #[tokio::test]
    async fn undo_after_dispatch_is_not_found() {
        let fake = fake_relay("200 OK", r#"{"id":"resend-1"}"#).await;
        let state = state_with_relay(Some(relay_chain(&fake))).await;
        let q = queued(&state).await;
        set_due(&state.pool, &q.id).await;
        sweep_once(&state).await.unwrap();
        let err = undo(
            State(state.clone()),
            Extension(test_user()),
            Path(("mb1".to_string(), q.id.clone())),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::NOT_FOUND);
        assert_eq!(err.1, "unknown_pending_send");
    }

    #[tokio::test]
    async fn undo_while_sending_is_conflict() {
        let state = state_with_relay(None).await;
        let q = queued(&state).await;
        sqlx::query("UPDATE pending_sends SET status='sending' WHERE id=?")
            .bind(&q.id)
            .execute(&state.pool)
            .await
            .unwrap();
        let err = undo(
            State(state.clone()),
            Extension(test_user()),
            Path(("mb1".to_string(), q.id.clone())),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::CONFLICT);
        assert_eq!(err.1, "already_sent");
    }

    #[tokio::test]
    async fn queued_credential_opens_with_server_secret_only() {
        let state = state_with_relay(None).await;
        let q = queued(&state).await;
        let sealed: String =
            sqlx::query_scalar("SELECT credential_sealed FROM pending_sends WHERE id=?")
                .bind(&q.id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(
            auth::open_server("test-secret", &sealed).as_deref(),
            Some("secret")
        );
        assert_eq!(auth::open_server("other-secret", &sealed), None);
    }
}
