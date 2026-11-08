//! Live mailbox stream: new mail without polling.
//!
//! One shared watcher per mailbox polls JMAP for the inbox snapshot
//! (size + newest message id, one small `Email/query`) and broadcasts
//! it to every open tab on that mailbox over server-sent events. The
//! watcher is the only thing that touches JMAP on a timer: it starts
//! on the first subscribe, ticks once per `poll_interval`, and stops
//! when the last tab disconnects, so an idle deployment polls nothing
//! and ten tabs on one mailbox still cost one JMAP call per interval.
//!
//! The stream itself never draws from the read limiter: the watcher
//! is server-driven and self-bounded (see `MAIL_WATCH_POLL_SECS`),
//! which is the rate limit for the stream. What it does inherit from
//! the rest of the API is session auth (the route lives behind
//! `require_session`, and `EventSource` sends the session cookie) and
//! ownership (a mailbox that is not yours answers 404 before any
//! watcher starts). CSRF needs nothing extra: the endpoint is a GET,
//! and GETs are safe methods under `require_csrf`.
//!
//! Change detection is `iris_mail::new_mail_count` over
//! `InboxSnapshot`s — total growth, or a replaced newest message at
//! a flat total — never the JMAP `queryState`, which also moves when
//! a message is merely marked read.

use axum::response::sse::Event;
use iris_config::limits::MAIL_WATCH_POLL_SECS;
use iris_mail::{new_mail_count, InboxSnapshot, JmapClient, MailError};
use serde_json::json;
use std::{
    collections::HashMap,
    convert::Infallible,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::watch;
use tokio_stream::{wrappers::WatchStream, Stream, StreamExt};

use crate::Creds;

/// (user_id, mailbox_id): a watcher is per mailbox the API knows,
/// not per JMAP account, so two mailboxes never share one.
type WatchKey = (String, String);

struct WatchHandle {
    tx: watch::Sender<Option<InboxSnapshot>>,
}

/// Shared mailbox watchers, one per subscribed mailbox. The registry
/// is an `Arc` so a watcher task can remove its own entry on exit
/// without borrowing the `MailWatch` that spawned it.
pub(crate) struct MailWatch {
    watchers: Arc<Mutex<HashMap<WatchKey, Arc<WatchHandle>>>>,
    poll_interval: Duration,
}

impl MailWatch {
    pub(crate) fn new(poll_interval: Duration) -> Self {
        Self {
            watchers: Arc::new(Mutex::new(HashMap::new())),
            poll_interval,
        }
    }

    /// Production constructor: the interval from limits.
    pub(crate) fn from_limits() -> Self {
        Self::new(Duration::from_secs(MAIL_WATCH_POLL_SECS))
    }

    /// How many watcher tasks are currently registered. Only used by
    /// tests to prove a watcher stops when its last tab disconnects.
    #[cfg(test)]
    pub(crate) fn active_watchers(&self) -> usize {
        self.watchers.lock().map(|m| m.len()).unwrap_or(0)
    }

    /// Subscribe to the inbox snapshots of one mailbox, starting its
    /// watcher on the first subscriber. Resolves the inbox folder id
    /// once (the watcher reuses it for every tick); a mailbox the
    /// user does not own, or a mail server that cannot be reached,
    /// fails here, before any stream starts.
    pub(crate) async fn subscribe(
        &self,
        jmap: &JmapClient,
        creds: &Creds,
        account: &str,
        user_id: &str,
        mailbox_id: &str,
    ) -> Result<watch::Receiver<Option<InboxSnapshot>>, MailError> {
        if let Ok(map) = self.watchers.lock() {
            if let Some(h) = map.get(&(user_id.to_string(), mailbox_id.to_string())) {
                return Ok(h.tx.subscribe());
            }
        }
        let inbox_id = jmap
            .folders(&creds.user, &creds.pass, account)
            .await?
            .into_iter()
            .find(|f| f.role == "inbox")
            .map(|f| f.id)
            .ok_or(MailError::NotFound)?;
        let key = (user_id.to_string(), mailbox_id.to_string());
        let mut map = self
            .watchers
            .lock()
            .map_err(|_| MailError::Protocol("mail watch registry poisoned".into()))?;
        if let Some(h) = map.get(&key) {
            return Ok(h.tx.subscribe());
        }
        let (tx, rx) = watch::channel(None);
        let handle = Arc::new(WatchHandle { tx });
        map.insert(key.clone(), handle.clone());
        let watchers = self.watchers.clone();
        let jmap = jmap.clone();
        let creds = creds.clone();
        let account = account.to_string();
        let interval = self.poll_interval;
        tokio::spawn(run_watcher(
            key, watchers, handle, jmap, creds, account, inbox_id, interval,
        ));
        Ok(rx)
    }
}

/// One watcher task: poll the inbox snapshot, broadcast it when it
/// moves, stop when nobody is listening or the credentials die.
/// The registry entry is removed on exit so the next subscriber
/// spawns a fresh watcher.
#[allow(clippy::too_many_arguments)]
async fn run_watcher(
    key: WatchKey,
    watchers: Arc<Mutex<HashMap<WatchKey, Arc<WatchHandle>>>>,
    handle: Arc<WatchHandle>,
    jmap: JmapClient,
    creds: Creds,
    account: String,
    inbox_id: String,
    poll_interval: Duration,
) {
    let tx = &handle.tx;
    let mut interval = tokio::time::interval(poll_interval);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last: Option<InboxSnapshot> = None;
    let mut failed = false;
    loop {
        interval.tick().await;
        if tx.receiver_count() == 0 {
            break;
        }
        match jmap
            .inbox_snapshot(&creds.user, &creds.pass, &account, &inbox_id)
            .await
        {
            Ok(snap) => {
                if failed {
                    tracing::info!("mail watch: snapshot recovered");
                    failed = false;
                }
                if last.as_ref() != Some(&snap) {
                    let _ = tx.send(Some(snap.clone()));
                    last = Some(snap);
                }
            }
            Err(MailError::Auth) => {
                // Session revoked or logged out: the credentials are
                // dead, and the tab will be gone with them.
                tracing::warn!("mail watch: auth failed, stopping watcher");
                break;
            }
            Err(e) => {
                if !failed {
                    tracing::warn!("mail watch: snapshot failed: {e}");
                    failed = true;
                }
            }
        }
    }
    if let Ok(mut map) = watchers.lock() {
        if let Some(h) = map.get(&key) {
            if Arc::ptr_eq(h, &handle) {
                map.remove(&key);
            }
        }
    }
}

/// One SSE event for a new snapshot, or `None` when the client needs
/// nothing: the first snapshot is a `hello` (the stream is live),
/// later ones are `new-mail` only when messages actually arrived.
pub(crate) fn event_for(prev: Option<&InboxSnapshot>, snap: &InboxSnapshot) -> Option<Event> {
    match prev {
        None => Event::default()
            .event("hello")
            .json_data(json!({ "total": snap.total }))
            .ok(),
        Some(p) => {
            let n = new_mail_count(p, snap);
            if n == 0 {
                None
            } else {
                Event::default()
                    .event("new-mail")
                    .json_data(json!({ "new": n, "total": snap.total }))
                    .ok()
            }
        }
    }
}

/// Turns a snapshot channel into the SSE event stream: first snapshot
/// is `hello`, arrivals become `new-mail`, anything else is silent.
/// A closed channel ends the stream, and the browser's `EventSource`
/// reconnects on its own.
pub(crate) fn events_stream(
    rx: watch::Receiver<Option<InboxSnapshot>>,
) -> impl Stream<Item = Result<Event, Infallible>> {
    let mut prev: Option<InboxSnapshot> = None;
    WatchStream::new(rx).filter_map(move |snap| {
        snap.and_then(|snap| {
            let e = event_for(prev.as_ref(), &snap);
            prev = Some(snap);
            e
        })
        .map(Ok::<_, Infallible>)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(total: u64, newest: Option<&str>) -> InboxSnapshot {
        InboxSnapshot {
            total,
            newest_id: newest.map(str::to_string),
        }
    }

    #[test]
    fn first_snapshot_is_hello() {
        let e = event_for(None, &snap(13, Some("a"))).expect("hello event");
        // Debug shows the wire bytes with JSON quotes escaped.
        let body = format!("{e:?}");
        assert!(
            body.contains("event: hello"),
            "event name must be hello: {body}"
        );
        assert!(
            body.contains("\\\"total\\\":13"),
            "hello carries total: {body}"
        );
    }

    #[test]
    fn arrival_is_new_mail_with_count() {
        let e =
            event_for(Some(&snap(13, Some("a"))), &snap(15, Some("c"))).expect("new-mail event");
        let body = format!("{e:?}");
        assert!(body.contains("event: new-mail"), "event name: {body}");
        assert!(body.contains("\\\"new\\\":2"), "count: {body}");
        assert!(body.contains("\\\"total\\\":15"), "total: {body}");
    }

    #[test]
    fn no_arrival_is_silent() {
        // A read, a move out, or nothing at all: no event.
        assert!(event_for(Some(&snap(13, Some("a"))), &snap(13, Some("a"))).is_none());
        assert!(event_for(Some(&snap(13, Some("a"))), &snap(12, Some("a"))).is_none());
        // Arrival plus deletion in one interval still counts as one.
        assert!(event_for(Some(&snap(13, Some("a"))), &snap(13, Some("b"))).is_some());
    }
}
