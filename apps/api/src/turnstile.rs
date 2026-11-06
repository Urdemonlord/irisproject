//! Cloudflare Turnstile verification for the login form.
//!
//! The widget runs in the browser and produces a single-use token; the
//! token proves nothing by itself — the server must check it against
//! Cloudflare's Siteverify API before the login is attempted. See
//! <https://developers.cloudflare.com/turnstile/get-started/server-side-validation/>.
//!
//! The check follows the canonical siteverify contract (see
//! <https://developers.cloudflare.com/turnstile/spin/prompt.md>): it
//! requires `success === true`, the expected `action` (`"login"`), and
//! a `hostname` inside the `TURNSTILE_HOSTNAMES` allowlist. An empty
//! allowlist fails closed — every login is rejected — so a half-wired
//! deployment cannot silently accept tokens minted anywhere.
//!
//! The check is off unless `TURNSTILE_SECRET_KEY` is set (see
//! `iris_config::Config`): an unconfigured deployment keeps logging in
//! exactly as before. When configured, a missing/invalid token rejects
//! the login, and a Siteverify outage fails closed (503) instead of
//! letting the login through unverified.

use reqwest::Client;
use serde::Deserialize;

/// Tokens are at most 2048 characters (Cloudflare docs); anything longer
/// cannot be a real token, so reject it without an outbound call.
const MAX_TOKEN_LEN: usize = 2048;

/// What went wrong verifying a token. The caller maps these to API
/// errors; only the variant reaches the client, never the detail.
#[derive(Debug, PartialEq, Eq)]
pub enum TurnstileError {
    /// No token was supplied while verification is on.
    MissingToken,
    /// Cloudflare said the token is not valid (expired, already used,
    /// wrong site key, ...). The exact error codes are logged server-side.
    Invalid,
    /// Siteverify could not be reached or answered something undecodable:
    /// fail closed, the login must not proceed unverified.
    Unavailable,
}

#[derive(Debug, Deserialize)]
struct SiteverifyResponse {
    success: bool,
    /// The `action` the token was minted for (the widget's `data-action`
    /// / render `action`; `"login"` here). Absent on old-style responses.
    #[serde(default)]
    action: Option<String>,
    /// The hostname the token was minted on. Absent on old-style
    /// responses; a missing hostname never matches the allowlist.
    #[serde(default)]
    hostname: Option<String>,
    #[serde(default, rename = "error-codes")]
    error_codes: Vec<String>,
}

/// The action the login widget mints tokens for. Must match the
/// widget's `action` (render option / `data-action="login"`).
pub const EXPECTED_ACTION: &str = "login";

fn outcome(
    resp: &SiteverifyResponse,
    expected_action: &str,
    expected_hostnames: &[String],
) -> Result<(), TurnstileError> {
    if !resp.success {
        // Server-side problems are not the user's fault: our secret is
        // wrong (`missing-input-secret`, `invalid-input-secret`) or
        // Cloudflare failed (`internal-error`). Report them as an
        // outage (503) instead of a bad token (400) so the user is not
        // told to retry something retrying cannot fix.
        const SERVER_ERRORS: [&str; 3] = [
            "missing-input-secret",
            "invalid-input-secret",
            "internal-error",
        ];
        if resp
            .error_codes
            .iter()
            .any(|c| SERVER_ERRORS.contains(&c.as_str()))
        {
            tracing::error!(
                "turnstile: siteverify server-side failure: {:?}",
                resp.error_codes
            );
            return Err(TurnstileError::Unavailable);
        }
        tracing::warn!(
            "turnstile: siteverify rejected token: {:?}",
            resp.error_codes
        );
        return Err(TurnstileError::Invalid);
    }
    if resp.action.as_deref() != Some(expected_action) {
        tracing::warn!(
            "turnstile: token action {:?} is not {:?}",
            resp.action,
            expected_action
        );
        return Err(TurnstileError::Invalid);
    }
    let hostname = resp.hostname.as_deref().unwrap_or_default().to_lowercase();
    if !expected_hostnames.iter().any(|h| h == &hostname) {
        tracing::warn!(
            "turnstile: token hostname {:?} is not in the allowlist",
            resp.hostname
        );
        return Err(TurnstileError::Invalid);
    }
    Ok(())
}

/// Verify `token` against Siteverify. `remoteip` is the login's client IP
/// (the same `client_ip` value the login handler already computes).
/// `siteverify_url` is [`iris_config::TURNSTILE_SITEVERIFY_URL`] in
/// production; tests point it at a local mock server.
pub async fn verify(
    client: &Client,
    secret: &str,
    token: Option<&str>,
    remoteip: &str,
    expected_action: &str,
    expected_hostnames: &[String],
    siteverify_url: &str,
) -> Result<(), TurnstileError> {
    let Some(token) = token.filter(|t| !t.is_empty()) else {
        return Err(TurnstileError::MissingToken);
    };
    if token.len() > MAX_TOKEN_LEN {
        return Err(TurnstileError::Invalid);
    }
    // No allowlist, no acceptance: a secret without hostnames would
    // otherwise accept tokens minted on any domain the widget allows.
    if expected_hostnames.is_empty() {
        tracing::error!(
            "turnstile: TURNSTILE_HOSTNAMES is empty while verification is on; rejecting login"
        );
        return Err(TurnstileError::Invalid);
    }
    let resp = client
        .post(siteverify_url)
        .json(&serde_json::json!({
            "secret": secret,
            "response": token,
            "remoteip": remoteip,
        }))
        .send()
        .await
        .map_err(|e| {
            tracing::warn!("turnstile: siteverify unreachable: {e}");
            TurnstileError::Unavailable
        })?;
    if !resp.status().is_success() {
        tracing::warn!("turnstile: siteverify HTTP {}", resp.status());
        return Err(TurnstileError::Unavailable);
    }
    let parsed: SiteverifyResponse = resp.json().await.map_err(|e| {
        tracing::warn!("turnstile: siteverify returned undecodable JSON: {e}");
        TurnstileError::Unavailable
    })?;
    outcome(&parsed, expected_action, expected_hostnames)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::{
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
        time::Duration,
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        sync::oneshot,
        task::JoinHandle,
        time::timeout,
    };

    /// A one-shot fake Siteverify: answers a single request with `body`
    /// at HTTP `status`, counts calls, and hands the request JSON back so
    /// the test can assert what was sent.
    struct FakeSiteverify {
        url: String,
        calls: Arc<AtomicUsize>,
        payload: oneshot::Receiver<Value>,
        task: JoinHandle<()>,
    }

    async fn fake_siteverify(
        status_line: &'static str,
        response_body: &'static str,
    ) -> FakeSiteverify {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let task_calls = calls.clone();
        let (tx, payload) = oneshot::channel();
        let response_body = response_body.to_string();
        let task = tokio::spawn(async move {
            let Ok(Ok((mut stream, _))) = timeout(Duration::from_secs(5), listener.accept()).await
            else {
                return;
            };
            task_calls.fetch_add(1, Ordering::SeqCst);
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
            let body_start = bytes.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
            let body: Value = serde_json::from_slice(&bytes[body_start..]).unwrap();
            let _ = tx.send(body);
            let response = format!(
                "HTTP/1.1 {status_line}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{response_body}",
                response_body.len(),
            );
            let _ = stream.write_all(response.as_bytes()).await;
        });
        FakeSiteverify {
            url: format!("http://{address}"),
            calls,
            payload,
            task,
        }
    }

    fn client() -> Client {
        Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap()
    }

    fn hostnames() -> Vec<String> {
        vec!["mail.example.com".to_string()]
    }

    #[tokio::test]
    async fn missing_token_is_rejected_without_a_call() {
        let fake = fake_siteverify("200 OK", r#"{"success":true}"#).await;
        let result = verify(
            &client(),
            "secret",
            None,
            "127.0.0.1",
            EXPECTED_ACTION,
            &hostnames(),
            &fake.url,
        )
        .await;
        assert_eq!(result, Err(TurnstileError::MissingToken));
        assert_eq!(fake.calls.load(Ordering::SeqCst), 0);
        fake.task.abort();
    }

    #[tokio::test]
    async fn empty_token_is_rejected_without_a_call() {
        let fake = fake_siteverify("200 OK", r#"{"success":true}"#).await;
        let result = verify(
            &client(),
            "secret",
            Some(""),
            "127.0.0.1",
            EXPECTED_ACTION,
            &hostnames(),
            &fake.url,
        )
        .await;
        assert_eq!(result, Err(TurnstileError::MissingToken));
        assert_eq!(fake.calls.load(Ordering::SeqCst), 0);
        fake.task.abort();
    }

    #[tokio::test]
    async fn overlong_token_is_rejected_without_a_call() {
        let fake = fake_siteverify("200 OK", r#"{"success":true}"#).await;
        let token = "x".repeat(2049);
        let result = verify(
            &client(),
            "secret",
            Some(&token),
            "127.0.0.1",
            EXPECTED_ACTION,
            &hostnames(),
            &fake.url,
        )
        .await;
        assert_eq!(result, Err(TurnstileError::Invalid));
        assert_eq!(fake.calls.load(Ordering::SeqCst), 0);
        fake.task.abort();
    }

    #[tokio::test]
    async fn empty_allowlist_rejects_without_a_call() {
        let fake = fake_siteverify(
            "200 OK",
            r#"{"success":true,"action":"login","hostname":"mail.example.com"}"#,
        )
        .await;
        let result = verify(
            &client(),
            "secret",
            Some("token"),
            "127.0.0.1",
            EXPECTED_ACTION,
            &[],
            &fake.url,
        )
        .await;
        assert_eq!(result, Err(TurnstileError::Invalid));
        assert_eq!(fake.calls.load(Ordering::SeqCst), 0);
        fake.task.abort();
    }

    #[tokio::test]
    async fn success_response_verifies_and_sends_secret_token_and_ip() {
        let fake = fake_siteverify(
            "200 OK",
            r#"{"success":true,"action":"login","challenge_ts":"2026-11-06T00:00:00Z","hostname":"mail.example.com"}"#,
        )
        .await;
        let result = verify(
            &client(),
            "test-secret",
            Some("test-token"),
            "203.0.113.7",
            EXPECTED_ACTION,
            &hostnames(),
            &fake.url,
        )
        .await;
        assert_eq!(result, Ok(()));
        let sent = fake.payload.await.unwrap();
        assert_eq!(sent["secret"], "test-secret");
        assert_eq!(sent["response"], "test-token");
        assert_eq!(sent["remoteip"], "203.0.113.7");
        fake.task.await.unwrap();
    }

    #[tokio::test]
    async fn hostname_match_is_case_insensitive() {
        let fake = fake_siteverify(
            "200 OK",
            r#"{"success":true,"action":"login","hostname":"MAIL.EXAMPLE.COM"}"#,
        )
        .await;
        let result = verify(
            &client(),
            "secret",
            Some("token"),
            "127.0.0.1",
            EXPECTED_ACTION,
            &hostnames(),
            &fake.url,
        )
        .await;
        assert_eq!(result, Ok(()));
        fake.task.await.unwrap();
    }

    #[tokio::test]
    async fn wrong_action_is_invalid() {
        let fake = fake_siteverify(
            "200 OK",
            r#"{"success":true,"action":"signup","hostname":"mail.example.com"}"#,
        )
        .await;
        let result = verify(
            &client(),
            "secret",
            Some("token"),
            "127.0.0.1",
            EXPECTED_ACTION,
            &hostnames(),
            &fake.url,
        )
        .await;
        assert_eq!(result, Err(TurnstileError::Invalid));
        fake.task.await.unwrap();
    }

    #[tokio::test]
    async fn hostname_outside_allowlist_is_invalid() {
        let fake = fake_siteverify(
            "200 OK",
            r#"{"success":true,"action":"login","hostname":"evil.example.com"}"#,
        )
        .await;
        let result = verify(
            &client(),
            "secret",
            Some("token"),
            "127.0.0.1",
            EXPECTED_ACTION,
            &hostnames(),
            &fake.url,
        )
        .await;
        assert_eq!(result, Err(TurnstileError::Invalid));
        fake.task.await.unwrap();
    }

    #[tokio::test]
    async fn hostname_missing_from_response_is_invalid() {
        let fake = fake_siteverify("200 OK", r#"{"success":true,"action":"login"}"#).await;
        let result = verify(
            &client(),
            "secret",
            Some("token"),
            "127.0.0.1",
            EXPECTED_ACTION,
            &hostnames(),
            &fake.url,
        )
        .await;
        assert_eq!(result, Err(TurnstileError::Invalid));
        fake.task.await.unwrap();
    }

    #[tokio::test]
    async fn failure_response_is_invalid() {
        let fake = fake_siteverify(
            "200 OK",
            r#"{"success":false,"error-codes":["timeout-or-duplicate"]}"#,
        )
        .await;
        let result = verify(
            &client(),
            "secret",
            Some("stale-token"),
            "127.0.0.1",
            EXPECTED_ACTION,
            &hostnames(),
            &fake.url,
        )
        .await;
        assert_eq!(result, Err(TurnstileError::Invalid));
        fake.task.await.unwrap();
    }

    #[tokio::test]
    async fn server_side_error_codes_are_unavailable() {
        // Our secret is wrong, or Cloudflare failed: an outage (503),
        // not a bad token (400). Each code gets its own fake server.
        for (code, body) in [
            (
                "missing-input-secret",
                r#"{"success":false,"error-codes":["missing-input-secret"]}"#,
            ),
            (
                "invalid-input-secret",
                r#"{"success":false,"error-codes":["invalid-input-secret"]}"#,
            ),
            (
                "internal-error",
                r#"{"success":false,"error-codes":["internal-error"]}"#,
            ),
        ] {
            let fake = fake_siteverify("200 OK", body).await;
            let result = verify(
                &client(),
                "secret",
                Some("token"),
                "127.0.0.1",
                EXPECTED_ACTION,
                &hostnames(),
                &fake.url,
            )
            .await;
            assert_eq!(result, Err(TurnstileError::Unavailable), "{code}");
            fake.task.await.unwrap();
        }
    }

    #[tokio::test]
    async fn http_error_is_unavailable() {
        let fake = fake_siteverify("500 Internal Server Error", r#"{"success":false}"#).await;
        let result = verify(
            &client(),
            "secret",
            Some("token"),
            "127.0.0.1",
            EXPECTED_ACTION,
            &hostnames(),
            &fake.url,
        )
        .await;
        assert_eq!(result, Err(TurnstileError::Unavailable));
        fake.task.await.unwrap();
    }

    #[tokio::test]
    async fn undecodable_response_is_unavailable() {
        let fake = fake_siteverify("200 OK", "this is not json").await;
        let result = verify(
            &client(),
            "secret",
            Some("token"),
            "127.0.0.1",
            EXPECTED_ACTION,
            &hostnames(),
            &fake.url,
        )
        .await;
        assert_eq!(result, Err(TurnstileError::Unavailable));
        fake.task.await.unwrap();
    }

    #[tokio::test]
    async fn unreachable_siteverify_is_unavailable() {
        // A just-closed ephemeral port: nothing can be listening there,
        // without assuming any particular port is closed.
        let addr = TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap()
            .local_addr()
            .unwrap();
        let result = verify(
            &client(),
            "secret",
            Some("token"),
            "127.0.0.1",
            EXPECTED_ACTION,
            &hostnames(),
            &format!("http://{addr}"),
        )
        .await;
        assert_eq!(result, Err(TurnstileError::Unavailable));
    }
}
