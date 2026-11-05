//! One-time render URLs for the separate mail-content origin.
//!
//! The web already fetches `html_body` through the authenticated message
//! endpoint, so it cannot hand the session cookie to another origin to
//! fetch the body again — the session cookie is host-only (no `Domain=`,
//! see `iris_auth::set_cookie`) and must stay that way. Instead the
//! authenticated mint endpoint stores the body here under a random token
//! and returns an absolute URL on the content origin; the public serve
//! endpoint claims it once and deletes it. No credential ever enters the
//! token: the token is only an index into this process-local store, and
//! the stored body is bound to one mailbox + one message + one
//! allow-remote-images flag, with a short expiry.
//!
//! The store is deliberately in-memory: entries live at most
//! [`HTML_BODY_TOKEN_TTL_SECS`] seconds, are capped at
//! [`MAX_PENDING_HTML_BODIES`], and a restart only invalidates pending
//! URLs (the web falls back to the srcdoc frame). Temp inboxes have no
//! mint endpoint yet, so they keep the srcdoc path.

use iris_config::limits::{HTML_BODY_TOKEN_TTL_SECS, MAX_PENDING_HTML_BODIES};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The CSP served with a stored HTML body. Same directives as the meta CSP
/// the web inlines into srcdoc frames today (`apps/web/src/main.ts`):
/// everything denied except inline styles (real HTML mail relies on them)
/// and images. `frame-ancestors` is intentionally NOT set here — the
/// content vhost adds it at deploy time (nginx), because the app origin
/// is deployment config, and a second CSP header is enforced alongside
/// this one. See `docs/separate-mail-origin.md`.
pub fn mail_body_csp(allow_remote: bool) -> String {
    format!(
        "default-src 'none'; script-src 'none'; object-src 'none'; media-src 'none'; font-src 'none'; connect-src 'none'; worker-src 'none'; manifest-src 'none'; frame-src 'none'; base-uri 'none'; form-action 'none'; style-src 'unsafe-inline'; img-src data: cid:{}",
        if allow_remote { " https:" } else { "" }
    )
}

/// Absolute render URL on the content origin for one stored body.
/// Callers must pass already-validated ids (mailbox from the DB lookup,
/// message through `valid_message_id`); this only interpolates them.
pub fn html_body_url(
    content_origin: &str,
    mailbox_id: &str,
    message_id: &str,
    token: &str,
) -> String {
    format!(
        "{content_origin}/api/v1/mailboxes/{mailbox_id}/messages/{message_id}/html-body?t={token}"
    )
}

struct PendingBody {
    mailbox_id: String,
    message_id: String,
    allow_remote: bool,
    html: String,
    expires: Instant,
}

/// Process-local store of HTML bodies awaiting a one-time render.
pub struct HtmlBodyStore {
    inner: Mutex<HashMap<String, PendingBody>>,
}

impl HtmlBodyStore {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }

    fn prune_locked(map: &mut HashMap<String, PendingBody>) {
        let now = Instant::now();
        map.retain(|_, b| b.expires > now);
    }

    /// Store `html` for one render and return its token. Returns `None`
    /// when the store is full (after pruning expired entries).
    pub fn mint(
        &self,
        mailbox_id: &str,
        message_id: &str,
        allow_remote: bool,
        html: String,
    ) -> Option<String> {
        let mut map = self.inner.lock().expect("html-body store lock");
        Self::prune_locked(&mut map);
        if map.len() >= MAX_PENDING_HTML_BODIES {
            return None;
        }
        let token = iris_auth::new_token();
        map.insert(
            token.clone(),
            PendingBody {
                mailbox_id: mailbox_id.to_string(),
                message_id: message_id.to_string(),
                allow_remote,
                html,
                expires: Instant::now() + Duration::from_secs(HTML_BODY_TOKEN_TTL_SECS),
            },
        );
        Some(token)
    }

    /// Claim a stored body once. The token is deleted whether or not the
    /// ids match, so a wrong guess cannot be retried against another
    /// message. Returns the HTML and its allow-remote flag.
    pub fn claim(&self, token: &str, mailbox_id: &str, message_id: &str) -> Option<(String, bool)> {
        let mut map = self.inner.lock().expect("html-body store lock");
        let body = map.remove(token)?;
        if body.expires <= Instant::now() {
            return None;
        }
        if body.mailbox_id != mailbox_id || body.message_id != message_id {
            return None;
        }
        Some((body.html, body.allow_remote))
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.inner.lock().expect("html-body store lock").len()
    }
}

impl Default for HtmlBodyStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mint_then_claim_once() {
        let store = HtmlBodyStore::new();
        let token = store
            .mint("mb1", "msg1", false, "<p>hi</p>".into())
            .unwrap();
        assert_eq!(
            store.claim(&token, "mb1", "msg1"),
            Some(("<p>hi</p>".to_string(), false))
        );
        assert_eq!(store.claim(&token, "mb1", "msg1"), None);
    }

    #[test]
    fn claim_rejects_wrong_message_and_wrong_mailbox() {
        let store = HtmlBodyStore::new();
        let token = store.mint("mb1", "msg1", true, "x".into()).unwrap();
        assert_eq!(store.claim(&token, "mb1", "msg2"), None);
        assert_eq!(store.len(), 0, "a wrong claim still burns the token");
        let token = store.mint("mb1", "msg1", true, "x".into()).unwrap();
        assert_eq!(store.claim(&token, "mb2", "msg1"), None);
    }

    #[test]
    fn unknown_token_claims_nothing() {
        let store = HtmlBodyStore::new();
        assert_eq!(store.claim("nope", "mb1", "msg1"), None);
    }

    #[test]
    fn csp_matches_srcdoc_policy() {
        assert_eq!(
            mail_body_csp(false),
            "default-src 'none'; script-src 'none'; object-src 'none'; media-src 'none'; font-src 'none'; connect-src 'none'; worker-src 'none'; manifest-src 'none'; frame-src 'none'; base-uri 'none'; form-action 'none'; style-src 'unsafe-inline'; img-src data: cid:"
        );
        assert!(mail_body_csp(true).ends_with("img-src data: cid: https:"));
    }

    #[test]
    fn render_url_is_absolute_on_the_content_origin() {
        assert_eq!(
            html_body_url("https://mail-content.meowlabs.id", "mb-1", "msg_2", "tok"),
            "https://mail-content.meowlabs.id/api/v1/mailboxes/mb-1/messages/msg_2/html-body?t=tok"
        );
    }
}
