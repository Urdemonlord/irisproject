//! `GET /heartbeat`: a real-path health probe for external monitors
//! (Uptime Kuma, healthchecks.io, cron + curl).
//!
//! Unlike `/health` (a static `"OK"`) and `/ready` (SQLite only), this
//! endpoint proves the mail path is alive: it logs in to Stalwart's
//! JMAP session with the admin credential and performs a real
//! management read (`x:Domain/get`). A monitor alerts on the status
//! code alone: 200 when every required check passes, 503 when any one
//! fails. The body carries only component names, statuses, and
//! latencies, never credentials, addresses, or error detail.
//!
//! Checks:
//! * `database`: `iris_database::health_check` on the SQLite pool.
//! * `stalwart_jmap`: `JmapClient::login` against
//!   `STALWART_MANAGEMENT_URL` with the admin credential, then
//!   `AdminClient::list_domains`. This login is a plain
//!   `GET /jmap/session` with Basic auth: it mints no app password
//!   (only `/auth/login` does, via `create_app_password`), so probing
//!   does not consume Stalwart's 5-app-passwords-per-account quota.
//!   Fail-closed: while the admin credential is unset the check
//!   reports `unavailable` and the endpoint answers 503, matching the
//!   alias endpoints' behaviour.
//! * `mail_roundtrip`: only when the probe asks for it
//!   (`?roundtrip=true`) and the dedicated mailbox is configured
//!   (`IRIS_HEARTBEAT_MAILBOX_USER` /
//!   `IRIS_HEARTBEAT_MAILBOX_PASSWORD`). Sends a test message from the
//!   heartbeat mailbox to itself, polls the inbox until it arrives,
//!   then deletes it. The send also files a copy in Sent (the same
//!   message object the submission moved there), and the delivery
//!   files a second, distinct copy in the inbox, so the probe
//!   deletes both: the Sent copy right after the send returns, and
//!   the inbox copy once it arrives. A probe that deleted only the
//!   inbox copy would leave one Sent message behind per probe.
//!   Unconfigured it reports `disabled` and never
//!   fails the endpoint; configured but not requested it reports
//!   `skipped`. The slow cadence (every 15-30 minutes) belongs to the
//!   external monitor, which calls the plain endpoint frequently and
//!   the `?roundtrip=true` variant on its own schedule. Never point
//!   the roundtrip at the demo mailbox: the probe deletes what it
//!   sends.
//!
//! Alerting v1 is the HTTP status code plus one structured log line
//! per failed check. There is no built-in push notification
//! (email/Slack/Telegram); the external monitor owns alerting and
//! consecutive-failure counting.

use crate::{AppState, S};
use axum::{
    extract::{Query, State},
    http::StatusCode,
    Json,
};
use iris_mail::{admin::AdminClient, Compose, JmapClient, ListPage, MailError};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::time::timeout;

/// Per-check budget. The clients underneath carry longer (15s)
/// timeouts of their own; this bound is what the probe promises to a
/// monitor, so one hung upstream cannot stall the endpoint.
pub(crate) const CHECK_TIMEOUT: Duration = Duration::from_secs(5);
/// Whole-roundtrip budget (send + delivery poll + delete). Only
/// applies to the `?roundtrip=true` variant, which the monitor calls
/// on a slow cadence.
pub(crate) const ROUNDTRIP_TIMEOUT: Duration = Duration::from_secs(60);
/// Gap between inbox polls while waiting for the test message.
const ROUNDTRIP_POLL_INTERVAL: Duration = Duration::from_secs(2);
/// Polls before the roundtrip gives up: ~30s of polling after the send.
const ROUNDTRIP_MAX_POLLS: usize = 15;
/// Attempts to find this probe's Sent copy before giving up. The copy
/// is filed by the submission itself, so it is there as soon as
/// `send` returns; the retries only cover a slow mailbox update.
const SENT_DELETE_ATTEMPTS: usize = 3;
/// Gap between Sent-copy lookups.
const SENT_DELETE_RETRY_INTERVAL: Duration = Duration::from_secs(1);
/// How many of the newest Sent messages one lookup scans. The
/// heartbeat mailbox holds nothing but probe messages, so the
/// probe's own copy is always the newest; the bound only keeps a
/// leaked backlog from turning the scan into a full-folder read.
const SENT_SCAN_LIMIT: u32 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum CheckStatus {
    Ok,
    Failed,
    Unavailable,
    Disabled,
    Skipped,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub(crate) struct CheckResult {
    status: CheckStatus,
    /// Milliseconds the check took (0 when it never ran).
    latency_ms: u64,
}

impl CheckResult {
    fn ok(latency: Duration) -> Self {
        Self {
            status: CheckStatus::Ok,
            latency_ms: latency.as_millis() as u64,
        }
    }

    fn failed(latency: Duration) -> Self {
        Self {
            status: CheckStatus::Failed,
            latency_ms: latency.as_millis() as u64,
        }
    }

    fn unavailable() -> Self {
        Self {
            status: CheckStatus::Unavailable,
            latency_ms: 0,
        }
    }

    fn disabled() -> Self {
        Self {
            status: CheckStatus::Disabled,
            latency_ms: 0,
        }
    }

    fn skipped() -> Self {
        Self {
            status: CheckStatus::Skipped,
            latency_ms: 0,
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct HeartbeatChecks {
    database: CheckResult,
    stalwart_jmap: CheckResult,
    mail_roundtrip: CheckResult,
}

#[derive(Debug, Serialize)]
pub(crate) struct HeartbeatReport {
    /// `"ok"` when every required check passed, `"degraded"` otherwise.
    status: &'static str,
    checks: HeartbeatChecks,
}

#[derive(Debug, Deserialize)]
pub(crate) struct HeartbeatQuery {
    /// Run the slow mail roundtrip on this probe. Defaults to false:
    /// the frequent monitor probe checks database + JMAP only.
    #[serde(default)]
    roundtrip: bool,
}

/// `GET /heartbeat`, public like `/health` (no auth, no CSRF token
/// needed for a GET): external monitors must be able to call it
/// unauthenticated.
pub(crate) async fn heartbeat(
    State(s): S,
    Query(query): Query<HeartbeatQuery>,
) -> (StatusCode, Json<HeartbeatReport>) {
    let database = check_database(&s.pool).await;
    if database.status != CheckStatus::Ok {
        // Error detail is deliberately not logged: mail errors can
        // carry upstream text, and a probe log must never leak
        // credentials or server internals.
        tracing::warn!("heartbeat: database check failed");
    }
    let stalwart_jmap = check_stalwart_jmap(&s.config).await;
    if stalwart_jmap.status != CheckStatus::Ok {
        tracing::warn!("heartbeat: stalwart_jmap check failed");
    }
    let mail_roundtrip = check_mail_roundtrip(&s, query.roundtrip).await;
    if mail_roundtrip.status == CheckStatus::Failed {
        tracing::warn!("heartbeat: mail_roundtrip check failed");
    }

    // `disabled` (mailbox unconfigured) and `skipped` (roundtrip not
    // requested on this probe) never fail the endpoint; `unavailable`
    // (admin credential unset) does, fail-closed like the aliases.
    let healthy = database.status == CheckStatus::Ok
        && stalwart_jmap.status == CheckStatus::Ok
        && matches!(
            mail_roundtrip.status,
            CheckStatus::Ok | CheckStatus::Disabled | CheckStatus::Skipped
        );
    let status = if healthy { "ok" } else { "degraded" };
    let code = if healthy {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (
        code,
        Json(HeartbeatReport {
            status,
            checks: HeartbeatChecks {
                database,
                stalwart_jmap,
                mail_roundtrip,
            },
        }),
    )
}

async fn check_database(pool: &sqlx::SqlitePool) -> CheckResult {
    let start = Instant::now();
    let ok = matches!(
        timeout(CHECK_TIMEOUT, iris_database::health_check(pool)).await,
        Ok(Ok(()))
    );
    if ok {
        CheckResult::ok(start.elapsed())
    } else {
        CheckResult::failed(start.elapsed())
    }
}

async fn check_stalwart_jmap(config: &iris_config::Config) -> CheckResult {
    if !config.stalwart_admin_configured() {
        return CheckResult::unavailable();
    }
    let start = Instant::now();
    let probe = async {
        // A plain session login: proves the credential works and the
        // server answers JMAP, without minting an app password.
        let jmap = JmapClient::new(config.stalwart_management_url.clone());
        let _account = jmap
            .login(&config.stalwart_admin_user, &config.stalwart_admin_password)
            .await?;
        // A real management read on the same credential: proves the
        // management path (not just the session endpoint) works.
        let admin = AdminClient::new(
            config.stalwart_management_url.clone(),
            config.stalwart_admin_user.clone(),
            config.stalwart_admin_password.clone(),
        );
        admin.list_domains().await?;
        Ok::<(), MailError>(())
    };
    let ok = matches!(timeout(CHECK_TIMEOUT, probe).await, Ok(Ok(())));
    if ok {
        CheckResult::ok(start.elapsed())
    } else {
        CheckResult::failed(start.elapsed())
    }
}

async fn check_mail_roundtrip(s: &Arc<AppState>, want_roundtrip: bool) -> CheckResult {
    if !s.config.heartbeat_mailbox_configured() {
        return CheckResult::disabled();
    }
    if !want_roundtrip {
        return CheckResult::skipped();
    }
    let start = Instant::now();
    let ok = matches!(
        timeout(ROUNDTRIP_TIMEOUT, run_roundtrip(s)).await,
        Ok(Ok(()))
    );
    if ok {
        CheckResult::ok(start.elapsed())
    } else {
        CheckResult::failed(start.elapsed())
    }
}

/// Sends one test message from the heartbeat mailbox to itself,
/// deletes the Sent copy the submission filed, waits until the
/// delivery copy shows up in the inbox, then deletes that too. Any
/// error (login, send, Sent delete, delivery, inbox delete) fails
/// the check: a half-proven roundtrip is not a healthy mail path,
/// and a half-cleaned one would leave residue behind.
async fn run_roundtrip(s: &Arc<AppState>) -> Result<(), MailError> {
    let user = &s.config.heartbeat_mailbox_user;
    let pass = &s.config.heartbeat_mailbox_password;
    let client = JmapClient::new(s.config.stalwart_api_url.clone());
    let account = client.login(user, pass).await?;
    // Unique per probe, so the folder searches cannot match an older
    // probe's leftover message (or another probe running concurrently).
    let token = format!("iris-heartbeat-{}", uuid::Uuid::new_v4());
    let compose = Compose {
        from: user.clone(),
        to: vec![user.clone()],
        subject: format!("IRIS heartbeat {token}"),
        text: format!("IRIS heartbeat probe {token}.\nDelete after receipt; this message only proves delivery."),
        ..Compose::default()
    };
    client.send(user, pass, &account, &compose, None).await?;
    // Delete the Sent copy first, before waiting on delivery: if
    // delivery never arrives the check fails, but the Sent folder is
    // already clean instead of leaking one message per failed probe.
    // The Sent copy is the message object the submission moved there;
    // the inbox delivery is a second, distinct object, so destroying
    // one cannot remove the other.
    let sent_ids = delete_sent_copy(&client, user, pass, &account, &token).await?;
    for _ in 0..ROUNDTRIP_MAX_POLLS {
        let (found, _) = client
            .list_folder(
                user,
                pass,
                &account,
                "inbox",
                ListPage {
                    limit: 1,
                    position: 0,
                    query: Some(&token),
                },
            )
            .await?;
        if let Some(message) = found.into_iter().next() {
            // The search is full-text; confirm the hit really is this
            // probe's message before deleting anything.
            if message.subject.contains(&token) {
                // Defensive: never destroy an id already destroyed
                // as the Sent copy (the two copies are distinct
                // objects on Stalwart, but a server that merged them
                // must not turn the second destroy into an error).
                if !sent_ids.iter().any(|id| id == &message.id) {
                    client
                        .destroy_one(user, pass, &account, &message.id)
                        .await?;
                }
                return Ok(());
            }
        }
        tokio::time::sleep(ROUNDTRIP_POLL_INTERVAL).await;
    }
    Err(MailError::Unavailable(
        "heartbeat test message was not delivered".into(),
    ))
}

/// Finds this probe's Sent copy and destroys it, returning the ids
/// destroyed. The lookup lists the newest Sent messages and matches
/// the subject client-side instead of using a full-text search: the
/// server's search index lags behind a message filed seconds ago,
/// while a plain newest-first listing sees it immediately. Only
/// messages whose subject carries this probe's token are destroyed,
/// so no other mail is ever touched.
async fn delete_sent_copy(
    client: &JmapClient,
    user: &str,
    pass: &str,
    account: &str,
    token: &str,
) -> Result<Vec<String>, MailError> {
    for attempt in 0..SENT_DELETE_ATTEMPTS {
        let (found, _) = client
            .list_folder(
                user,
                pass,
                account,
                "sent",
                ListPage {
                    limit: SENT_SCAN_LIMIT,
                    position: 0,
                    query: None,
                },
            )
            .await?;
        let targets: Vec<String> = found
            .into_iter()
            .filter(|m| m.subject.contains(token))
            .map(|m| m.id)
            .collect();
        if !targets.is_empty() {
            for id in &targets {
                client.destroy_one(user, pass, account, id).await?;
            }
            return Ok(targets);
        }
        if attempt + 1 < SENT_DELETE_ATTEMPTS {
            tokio::time::sleep(SENT_DELETE_RETRY_INTERVAL).await;
        }
    }
    Err(MailError::Unavailable(
        "heartbeat test message was not stored in Sent".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{html_body, integration_apps, notify};
    use iris_config::Config;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::Mutex;
    use tokio::time::timeout as test_timeout;

    /// Test `AppState` with the heartbeat-relevant config dials
    /// exposed. Everything else mirrors the `state()` helper in
    /// `main.rs` tests (dead upstream URLs, in-memory SQLite).
    async fn state(
        mgmt_url: String,
        api_url: String,
        admin_user: &str,
        admin_pass: &str,
        hb_user: &str,
        hb_pass: &str,
    ) -> Arc<AppState> {
        let pool = iris_database::init_pool("sqlite::memory:").await.unwrap();
        Arc::new(AppState {
            pool,
            organize_lock: tokio::sync::Mutex::new(()),
            jmap: JmapClient::new("http://127.0.0.1:9".into()),
            http: reqwest::Client::new(),
            relay: None,
            registry: tokio::sync::RwLock::new(integration_apps::default_registry()),
            html_bodies: html_body::HtmlBodyStore::new(),
            mail_watch: notify::MailWatch::from_limits(),
            config: Config {
                env: "test".into(),
                host: "127.0.0.1".into(),
                port: 8000,
                database_url: "sqlite::memory:".into(),
                session_secret: "test-secret-change-in-prod-0123456789".into(),
                turnstile_secret_key: String::new(),
                turnstile_hostnames: Vec::new(),
                turnstile_siteverify_url: String::new(),
                stalwart_api_url: api_url,
                stalwart_management_url: mgmt_url,
                stalwart_admin_user: admin_user.to_string(),
                stalwart_admin_password: admin_pass.to_string(),
                heartbeat_mailbox_user: hb_user.to_string(),
                heartbeat_mailbox_password: hb_pass.to_string(),
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

    /// Reads one HTTP request (headers + `Content-Length` body) off the
    /// stream and returns (method, path, headers, body). `None` when the
    /// peer went away before a full request arrived.
    async fn read_request(
        stream: &mut tokio::net::TcpStream,
    ) -> Option<(String, String, String, Vec<u8>)> {
        let mut bytes = Vec::new();
        let mut chunk = [0u8; 4096];
        let mut header_end: Option<usize> = None;
        let mut expected: Option<usize> = None;
        loop {
            let Ok(n) = stream.read(&mut chunk).await else {
                return None;
            };
            if n == 0 {
                return None;
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
                    let head = String::from_utf8_lossy(&bytes[..end]).to_string();
                    let mut lines = head.lines();
                    let request_line = lines.next().unwrap_or_default().to_string();
                    let mut parts = request_line.split_whitespace();
                    let method = parts.next().unwrap_or_default().to_string();
                    let path = parts.next().unwrap_or_default().to_string();
                    return Some((method, path, head, bytes));
                }
            }
        }
    }

    async fn write_json(stream: &mut tokio::net::TcpStream, status: u16, body: &str) {
        let reason = match status {
            401 => "Unauthorized",
            _ => "OK",
        };
        let response = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = stream.write_all(response.as_bytes()).await;
        let _ = stream.flush().await;
    }

    /// Fake management server: answers `GET /jmap/session` and the
    /// `x:Domain/get` call. `unauthorized` answers 401 to everything,
    /// the way Stalwart answers a wrong admin credential.
    async fn fake_mgmt(unauthorized: bool) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            loop {
                let Ok(Ok((mut stream, _))) =
                    test_timeout(Duration::from_secs(5), listener.accept()).await
                else {
                    return;
                };
                let Some((method, path, _, _)) = read_request(&mut stream).await else {
                    return;
                };
                if unauthorized {
                    write_json(&mut stream, 401, "").await;
                    continue;
                }
                if method == "GET" && path == "/jmap/session" {
                    write_json(
                        &mut stream,
                        200,
                        r#"{"primaryAccounts":{"urn:ietf:params:jmap:mail":"acct-admin"}}"#,
                    )
                    .await;
                } else {
                    write_json(
                        &mut stream,
                        200,
                        r#"{"methodResponses":[["x:Domain/get",{"accountId":"acct-admin","list":[{"id":"d1","name":"example.test","catchAllAddress":null}],"notUpdated":{}},"g"]]}"#,
                    )
                    .await;
                }
            }
        });
        base_url
    }

    /// State shared by the fake user-JMAP server for the roundtrip
    /// happy path: it only reports the test message as arrived after
    /// an `Email/set` create landed, and records whether the probe
    /// deleted it again. The send files one copy in Sent (id
    /// `msg-sent`) and the delivery files a second, distinct copy in
    /// the inbox (id `msg-1`), the way Stalwart does: the probe must
    /// destroy both.
    struct FakeJmapState {
        delivered: bool,
        /// Subject text of the last `Email/query` search, so the
        /// `Email/get` answer can carry this probe's token.
        last_query_text: Option<String>,
        /// Subject of the created message, so the `Email/get` answer
        /// carries this probe's token even for a query without a
        /// full-text search (the Sent listing).
        created_subject: Option<String>,
        /// Mailbox id of the last `Email/query`, so the `Email/get`
        /// answer returns that folder's copy.
        last_query_mailbox: Option<String>,
        destroyed: Vec<String>,
    }

    /// Fake user-JMAP server: session, folders, identity create,
    /// send, inbox search, and delete. Enough of JMAP to run a real
    /// `run_roundtrip` against it, nothing more.
    async fn fake_jmap() -> (String, Arc<Mutex<FakeJmapState>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let shared = Arc::new(Mutex::new(FakeJmapState {
            delivered: false,
            last_query_text: None,
            created_subject: None,
            last_query_mailbox: None,
            destroyed: Vec::new(),
        }));
        let task_shared = shared.clone();
        tokio::spawn(async move {
            loop {
                let Ok(Ok((mut stream, _))) =
                    test_timeout(Duration::from_secs(5), listener.accept()).await
                else {
                    return;
                };
                let Some((method, path, _, raw)) = read_request(&mut stream).await else {
                    return;
                };
                if method == "GET" && path == "/jmap/session" {
                    write_json(
                        &mut stream,
                        200,
                        r#"{"primaryAccounts":{"urn:ietf:params:jmap:mail":"acct-hb"}}"#,
                    )
                    .await;
                    continue;
                }
                // POST /jmap/: answer every methodCall in the batch.
                let body_start = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
                let body: serde_json::Value =
                    serde_json::from_slice(&raw[body_start..]).unwrap_or_default();
                let mut responses: Vec<serde_json::Value> = Vec::new();
                let mut guard = task_shared.lock().await;
                for call in body["methodCalls"].as_array().cloned().unwrap_or_default() {
                    let name = call[0].as_str().unwrap_or_default().to_string();
                    let args = call[1].clone();
                    let call_id = call[2].clone();
                    let result = match name.as_str() {
                        "Mailbox/get" => serde_json::json!({
                            "accountId": "acct-hb",
                            "list": [
                                {"id": "m-inbox", "name": "Inbox", "role": "inbox", "totalEmails": 0, "unreadEmails": 0},
                                {"id": "m-sent", "name": "Sent", "role": "sent", "totalEmails": 0, "unreadEmails": 0},
                                {"id": "m-drafts", "name": "Drafts", "role": "drafts", "totalEmails": 0, "unreadEmails": 0},
                                {"id": "m-archive", "name": "Archive", "role": "archive", "totalEmails": 0, "unreadEmails": 0},
                                {"id": "m-trash", "name": "Trash", "role": "trash", "totalEmails": 0, "unreadEmails": 0}
                            ],
                            "notUpdated": {}
                        }),
                        // No identity matches: the client falls back
                        // to creating one, which the fake grants.
                        "Identity/get" => serde_json::json!({
                            "accountId": "acct-hb", "list": [], "notUpdated": {}
                        }),
                        "Identity/set" => serde_json::json!({
                            "accountId": "acct-hb",
                            "created": {"i": {"id": "id-1"}},
                            "notCreated": {}
                        }),
                        "Email/set" => {
                            if args["create"].is_object() {
                                guard.delivered = true;
                                guard.created_subject =
                                    args["create"]["n"]["subject"].as_str().map(str::to_string);
                                serde_json::json!({
                                    "accountId": "acct-hb",
                                    "created": {"n": {"id": "msg-sent"}},
                                    "notCreated": {}
                                })
                            } else {
                                let ids: Vec<String> = args["destroy"]
                                    .as_array()
                                    .cloned()
                                    .unwrap_or_default()
                                    .into_iter()
                                    .filter_map(|v| v.as_str().map(str::to_string))
                                    .collect();
                                guard.destroyed.extend(ids.clone());
                                serde_json::json!({
                                    "accountId": "acct-hb",
                                    "destroyed": ids,
                                    "notDestroyed": {}
                                })
                            }
                        }
                        "EmailSubmission/set" => serde_json::json!({
                            "accountId": "acct-hb",
                            "created": {"s": {}},
                            "notCreated": {}
                        }),
                        "Email/query" => {
                            // The client's full-text search carries the
                            // probe token as a `text` condition inside
                            // `filter.conditions` (see `with_text` in
                            // the mail crate); remember it for the
                            // `Email/get` answer below. Also remember
                            // which folder was queried: a plain folder
                            // filter carries `inMailbox` directly,
                            // a searched one inside `conditions`.
                            guard.last_query_text = args["filter"]["conditions"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .find_map(|c| c["text"].as_str().map(str::to_string));
                            guard.last_query_mailbox = args["filter"]["inMailbox"]
                                .as_str()
                                .map(str::to_string)
                                .or_else(|| {
                                    args["filter"]["conditions"]
                                        .as_array()
                                        .into_iter()
                                        .flatten()
                                        .find_map(|c| c["inMailbox"].as_str().map(str::to_string))
                                });
                            let sent = guard.last_query_mailbox.as_deref() == Some("m-sent");
                            let ids: &[&str] = if guard.delivered && sent {
                                &["msg-sent"]
                            } else if guard.delivered {
                                &["msg-1"]
                            } else {
                                &[]
                            };
                            serde_json::json!({
                                "accountId": "acct-hb",
                                "ids": ids, "total": ids.len(),
                                "queryState": "q1", "canCalculateChanges": false
                            })
                        }
                        "Email/get" => {
                            let subject = guard.created_subject.clone().unwrap_or_else(|| {
                                guard
                                    .last_query_text
                                    .clone()
                                    .map(|t| format!("IRIS heartbeat {t}"))
                                    .unwrap_or_default()
                            });
                            let id = if guard.last_query_mailbox.as_deref() == Some("m-sent") {
                                "msg-sent"
                            } else {
                                "msg-1"
                            };
                            serde_json::json!({
                                "accountId": "acct-hb",
                                "list": [{
                                    "id": id,
                                    "subject": subject,
                                    "from": [{"email": "hb@example.test"}],
                                    "to": [{"email": "hb@example.test"}],
                                    "preview": "",
                                    "receivedAt": "2026-11-26T00:00:00Z",
                                    "keywords": {"$seen": true},
                                    "hasAttachment": false,
                                    "size": 100
                                }],
                                "notUpdated": {}
                            })
                        }
                        _ => serde_json::json!({"accountId": "acct-hb"}),
                    };
                    responses.push(serde_json::json!([name, result, call_id]));
                }
                drop(guard);
                let body = serde_json::to_string(&serde_json::json!({
                    "methodResponses": responses,
                }))
                .unwrap();
                write_json(&mut stream, 200, &body).await;
            }
        });
        (base_url, shared)
    }

    async fn probe(s: Arc<AppState>, roundtrip: bool) -> (StatusCode, HeartbeatReport) {
        // Call the handler directly: no HTTP server, no CSRF layer.
        let (code, Json(report)) = heartbeat(State(s), Query(HeartbeatQuery { roundtrip })).await;
        (code, report)
    }

    #[tokio::test]
    async fn heartbeat_200_when_healthy_and_roundtrip_disabled() {
        let mgmt = fake_mgmt(false).await;
        let s = state(mgmt, "http://127.0.0.1:9".into(), "admin", "secret", "", "").await;
        let (code, report) = probe(s, false).await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(report.status, "ok");
        assert_eq!(report.checks.database.status, CheckStatus::Ok);
        assert_eq!(report.checks.stalwart_jmap.status, CheckStatus::Ok);
        // Mailbox unconfigured: disabled, and it must not fail a
        // healthy endpoint.
        assert_eq!(report.checks.mail_roundtrip.status, CheckStatus::Disabled);
    }

    #[tokio::test]
    async fn heartbeat_503_when_admin_credential_unset() {
        let mgmt = fake_mgmt(false).await;
        let s = state(mgmt, "http://127.0.0.1:9".into(), "", "", "", "").await;
        let (code, report) = probe(s, false).await;
        assert_eq!(code, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(report.status, "degraded");
        assert_eq!(report.checks.stalwart_jmap.status, CheckStatus::Unavailable);
        assert_eq!(report.checks.database.status, CheckStatus::Ok);
    }

    #[tokio::test]
    async fn heartbeat_503_when_stalwart_rejects_admin_credential() {
        let mgmt = fake_mgmt(true).await;
        let s = state(mgmt, "http://127.0.0.1:9".into(), "admin", "wrong", "", "").await;
        let (code, report) = probe(s, false).await;
        assert_eq!(code, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(report.checks.stalwart_jmap.status, CheckStatus::Failed);
    }

    #[tokio::test]
    async fn heartbeat_503_when_stalwart_unreachable() {
        let s = state(
            "http://127.0.0.1:9".into(),
            "http://127.0.0.1:9".into(),
            "admin",
            "secret",
            "",
            "",
        )
        .await;
        let (code, report) = probe(s, false).await;
        assert_eq!(code, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(report.checks.stalwart_jmap.status, CheckStatus::Failed);
    }

    #[tokio::test]
    async fn heartbeat_503_when_database_down() {
        let mgmt = fake_mgmt(false).await;
        let s = state(mgmt, "http://127.0.0.1:9".into(), "admin", "secret", "", "").await;
        s.pool.close().await;
        let (code, report) = probe(s, false).await;
        assert_eq!(code, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(report.checks.database.status, CheckStatus::Failed);
        // Stalwart is still healthy: only the database fails.
        assert_eq!(report.checks.stalwart_jmap.status, CheckStatus::Ok);
    }

    #[tokio::test]
    async fn roundtrip_skipped_when_configured_but_not_requested() {
        let mgmt = fake_mgmt(false).await;
        let s = state(
            mgmt,
            "http://127.0.0.1:9".into(),
            "admin",
            "secret",
            "hb@example.test",
            "secret",
        )
        .await;
        let (code, report) = probe(s, false).await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(report.checks.mail_roundtrip.status, CheckStatus::Skipped);
    }

    #[tokio::test]
    async fn roundtrip_happy_path_delivers_and_deletes() {
        let mgmt = fake_mgmt(false).await;
        let (api, shared) = fake_jmap().await;
        let s = state(mgmt, api, "admin", "secret", "hb@example.test", "secret").await;
        let (code, report) = probe(s, true).await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(report.checks.mail_roundtrip.status, CheckStatus::Ok);
        // The probe deleted exactly the two copies it sent: the
        // Sent copy first, then the inbox delivery copy.
        let guard = shared.lock().await;
        assert!(guard.delivered);
        assert_eq!(
            guard.destroyed,
            vec!["msg-sent".to_string(), "msg-1".to_string()]
        );
    }

    #[tokio::test]
    async fn roundtrip_503_when_mail_server_unreachable() {
        let mgmt = fake_mgmt(false).await;
        let s = state(
            mgmt,
            "http://127.0.0.1:9".into(),
            "admin",
            "secret",
            "hb@example.test",
            "secret",
        )
        .await;
        let (code, report) = probe(s, true).await;
        assert_eq!(code, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(report.checks.mail_roundtrip.status, CheckStatus::Failed);
        // The other checks still pass: only the roundtrip fails.
        assert_eq!(report.checks.database.status, CheckStatus::Ok);
        assert_eq!(report.checks.stalwart_jmap.status, CheckStatus::Ok);
    }
}
