//! Compile-time limits shared by the API and the mail crate.
//!
//! Single home for every hard-coded cap in the workspace: mail size caps,
//! compose validation, rate limits, temp-inbox quotas, login throttling
//! and request caps. Values are unchanged from where each constant used
//! to live; only the location moved, so behaviour is identical.
//!
//! Constants that merely share a value keep separate names, because they
//! cap different things: `MAX_RAW_BYTES` and `MAX_PREVIEW_BYTES` are both
//! 5_000_000, `MAX_RECIPIENTS`, `MAX_FAILED_LOGINS_PER_IP`,
//! `DEFAULT_LIST_LIMIT` and `TEMP_INBOX_MESSAGE_LIMIT` are all 50, and
//! `MAX_FAILED_LOGINS` and `IP_CREATES_PER_HOUR` are both 10. Retuning one
//! must never silently retune the other.

// --- Mail size caps (were `crates/mail/src/lib.rs`) -----------------------

/// A raw message over this is refused with 413 rather than buffered.
pub const MAX_RAW_BYTES: u64 = 5_000_000;
/// An attachment over this is refused with 413 instead of downloaded.
pub const MAX_ATTACHMENT_BYTES: u64 = 25_000_000;
/// An attachment over this gets no text preview (413); download still works.
pub const MAX_PREVIEW_BYTES: u64 = 5_000_000;
/// Extracted preview text is truncated at this many bytes instead of refused,
/// so a big DMARC report still shows its first records.
pub const MAX_PREVIEW_TEXT: usize = 2_000_000;
/// Max messages returned for one thread. A runaway thread (a busy
/// mailing list) must not pull an unbounded `Email/get`; the newest
/// slice is still a useful conversation view.
pub const MAX_THREAD_MESSAGES: usize = 200;
/// Max messages destroyed per JMAP `destroy_to` / `destroy_before` call.
pub const MAX_DESTROY_PER_CALL: u32 = 500;
/// Max passes when clearing a label keyword: one pass handles at most
/// MAX_DESTROY_PER_CALL messages, so 100 passes clear 50,000. Past that
/// the clear fails instead of looping forever (see `clear_keyword`).
pub const MAX_CLEAR_KEYWORD_BATCHES: u32 = 100;

// --- Compose validation (were `apps/api/src/compose.rs`) ------------------

/// Max total recipients across to + cc + bcc.
pub const MAX_RECIPIENTS: usize = 50;
/// Max subject length in bytes.
pub const MAX_SUBJECT: usize = 998;
/// Max body length in bytes.
pub const MAX_BODY: usize = 1_000_000;
/// Max threading header ids kept from In-Reply-To / References.
pub const MAX_HEADER_IDS: usize = 20;
/// Max length in bytes of one threading header id.
pub const MAX_HEADER_ID_LEN: usize = 250;
/// Max sends per user per hour.
pub const SENDS_PER_HOUR: i64 = 100;
/// Max attachments on one composed message.
pub const MAX_COMPOSE_ATTACHMENTS: usize = 5;
/// Max size of one composed attachment, in bytes (decoded).
pub const MAX_COMPOSE_ATTACHMENT_BYTES: usize = 5_000_000;
/// Max total size of all attachments on one composed message, in bytes (decoded).
pub const MAX_COMPOSE_ATTACHMENTS_TOTAL_BYTES: usize = 10_000_000;
/// Max attachment filename length, in characters.
pub const MAX_ATTACHMENT_NAME_CHARS: usize = 255;
/// Max JSON body for the compose send/draft endpoints. Base64 inflates the
/// 10 MB attachment cap to ~13.4 MB, plus up to 1 MB of body text; 16 MB
/// leaves headroom without letting an unbounded body reach the handlers.
/// (Named separately from the attachment caps: retuning one must never
/// silently retune the other.)
pub const MAX_COMPOSE_JSON_BYTES: usize = 16_000_000;

// --- Temp inboxes (were `apps/api/src/temp.rs`) ----------------------------

/// Max temp-inbox creations per IP per hour.
pub const IP_CREATES_PER_HOUR: i64 = 10;
/// Max active temp inboxes per client cookie.
pub const ACTIVE_PER_CLIENT: i64 = 3;
/// Max messages listed for one temp inbox per request.
pub const TEMP_INBOX_MESSAGE_LIMIT: u32 = 50;

// --- Login throttling (were `apps/api/src/main.rs`) ------------------------

/// Max failed logins per email per 15 minutes.
pub const MAX_FAILED_LOGINS: i64 = 10;
/// Max failed logins per client IP per 15 minutes (tolerant of shared NAT).
pub const MAX_FAILED_LOGINS_PER_IP: i64 = 50;
/// Max failed Turnstile verifications per client IP per 15 minutes.
/// Separate from the login buckets: a failed verification never
/// reaches JMAP, so it never increments them, and without its own cap
/// an attacker could force one outbound Siteverify call per attempt,
/// forever.
pub const MAX_TURNSTILE_ATTEMPTS_PER_IP: i64 = 20;

// --- Authenticated message reads (debt round 2, slice 9) -------------------
// One shared bucket for every JMAP-backed read behind the session:
// list_messages, get_message, get_thread, get_message_raw,
// get_attachment, get_attachment_preview and compose::folders. The web polls list +
// folders every 30 s while visible (4 reads/min per tab) and debounces
// search to one list call per 300 ms pause, so 600 reads per 10 minutes
// leaves ~15x headroom for a single tab — search bursts, load-more paging
// and opening messages included — while a client hammering the API tops
// out at ~1 read/sec per user instead of unbounded JMAP round-trips.
// Keyed per user, not per IP: reads are authenticated, so one user's
// hammering must never block another user behind the same NAT.
//
// The live mailbox stream (`notify::MailWatch`, `MAIL_WATCH_POLL_SECS`)
// does NOT draw from this bucket: it is server-driven, one shared
// watcher per mailbox, one small JMAP poll per interval no matter how
// many tabs are open, and it stops when the last tab disconnects.
// That bound is the rate limit for the stream — charging the bucket
// per reconnect instead would let a flapping EventSource burn the
// budget the user's own list reads need.
/// Max authenticated message reads per user per window.
pub const MAX_READS_PER_WINDOW: i64 = 600;
/// Window for MAX_READS_PER_WINDOW, in minutes.
pub const READ_WINDOW_MINUTES: i64 = 10;

/// How often one shared mailbox watcher polls JMAP for new mail, in
/// seconds. One small `Email/query` per mailbox per interval, shared
/// by every open tab on that mailbox; the web falls back to its 30 s
/// list poll only while the stream is down.
pub const MAIL_WATCH_POLL_SECS: u64 = 15;
/// How often the SSE stream sends a keep-alive comment, in seconds:
/// inside the 60 s read timeout a reverse proxy typically applies to
/// an idle response.
pub const MAIL_WATCH_KEEPALIVE_SECS: u64 = 30;

// --- Request caps (were `apps/api/src/main.rs`) ----------------------------

/// Default page size for message listing.
pub const DEFAULT_LIST_LIMIT: u32 = 50;
/// Max page size a client may request for message listing.
pub const MAX_LIST_LIMIT: u32 = 100;
/// Search terms are truncated to this many characters before going to JMAP.
pub const MAX_SEARCH_QUERY_CHARS: usize = 200;
/// Max request body for the password-reset webhook endpoint.
pub const MAX_PASSWORD_RESET_BODY_BYTES: usize = 1_024;
/// Max request body for the transactional-email send endpoint.
pub const MAX_TRANSACTIONAL_EMAIL_BODY_BYTES: usize = 8_192;

// --- Separate mail-content origin (one-time HTML render URLs) -----------
// The iframe `src` path serves a stored HTML body under a random one-time
// token, so the body never crosses the wire to the browser as page data
// and the render origin never learns the session cookie (host-only).

/// Max HTML body stored for one one-time render URL (same value as
/// MAX_RAW_BYTES, separate name: retuning one must never retune the other).
pub const MAX_HTML_BODY_BYTES: u64 = 5_000_000;
/// How long a minted render URL stays valid, in seconds.
pub const HTML_BODY_TOKEN_TTL_SECS: u64 = 120;
/// Max render URLs pending at once; minting past this is refused with 429
/// so a runaway client cannot grow the in-memory store without bound.
pub const MAX_PENDING_HTML_BODIES: usize = 512;

// --- Contacts ---------------------------------------------------------------
// The address book is per user, not per mailbox: one list shared across
// every mailbox they own, so a single cap covers all of them.

/// Max contacts one user can store.
pub const MAX_CONTACTS_PER_USER: i64 = 500;
/// Max length of a contact name, in characters.
pub const MAX_CONTACT_NAME_CHARS: usize = 100;
/// Search terms are truncated to this many characters before going to SQL.
pub const MAX_CONTACT_SEARCH_CHARS: usize = 100;
/// Default page size for contact listing.
pub const DEFAULT_CONTACTS_LIMIT: u32 = 50;
/// Max page size a client may request for contact listing.
pub const MAX_CONTACTS_LIMIT: u32 = 100;

// --- Labels, custom folders and filters ------------------------------------

/// Max labels one mailbox can define.
pub const MAX_LABELS: i64 = 50;
/// Max custom folders one mailbox can have.
pub const MAX_CUSTOM_FOLDERS: usize = 50;
/// Max length of a custom folder name, in characters.
pub const MAX_FOLDER_NAME_CHARS: usize = 64;
/// Max filter rules per mailbox.
pub const MAX_FILTERS: i64 = 50;
/// Max length of a filter's match text, in characters.
pub const MAX_FILTER_VALUE_CHARS: usize = 200;

#[cfg(test)]
mod tests {
    use super::*;

    /// Values must never drift silently: a retune is a deliberate change
    /// and should update this test in the same commit.
    #[test]
    fn values_are_unchanged() {
        assert_eq!(MAX_RAW_BYTES, 5_000_000);
        assert_eq!(MAX_ATTACHMENT_BYTES, 25_000_000);
        assert_eq!(MAX_PREVIEW_BYTES, 5_000_000);
        assert_eq!(MAX_PREVIEW_TEXT, 2_000_000);
        assert_eq!(MAX_DESTROY_PER_CALL, 500);
        assert_eq!(MAX_THREAD_MESSAGES, 200);
        assert_eq!(MAX_CLEAR_KEYWORD_BATCHES, 100);
        assert_eq!(MAX_RECIPIENTS, 50);
        assert_eq!(MAX_SUBJECT, 998);
        assert_eq!(MAX_BODY, 1_000_000);
        assert_eq!(MAX_HEADER_IDS, 20);
        assert_eq!(MAX_HEADER_ID_LEN, 250);
        assert_eq!(SENDS_PER_HOUR, 100);
        assert_eq!(MAX_COMPOSE_ATTACHMENTS, 5);
        assert_eq!(MAX_COMPOSE_ATTACHMENT_BYTES, 5_000_000);
        assert_eq!(MAX_COMPOSE_ATTACHMENTS_TOTAL_BYTES, 10_000_000);
        assert_eq!(MAX_ATTACHMENT_NAME_CHARS, 255);
        assert_eq!(MAX_COMPOSE_JSON_BYTES, 16_000_000);
        assert_eq!(IP_CREATES_PER_HOUR, 10);
        assert_eq!(ACTIVE_PER_CLIENT, 3);
        assert_eq!(TEMP_INBOX_MESSAGE_LIMIT, 50);
        assert_eq!(MAX_FAILED_LOGINS, 10);
        assert_eq!(MAX_FAILED_LOGINS_PER_IP, 50);
        assert_eq!(MAX_TURNSTILE_ATTEMPTS_PER_IP, 20);
        assert_eq!(MAIL_WATCH_POLL_SECS, 15);
        assert_eq!(MAIL_WATCH_KEEPALIVE_SECS, 30);
        assert_eq!(MAX_READS_PER_WINDOW, 600);
        assert_eq!(READ_WINDOW_MINUTES, 10);
        assert_eq!(DEFAULT_LIST_LIMIT, 50);
        assert_eq!(MAX_LIST_LIMIT, 100);
        assert_eq!(MAX_SEARCH_QUERY_CHARS, 200);
        assert_eq!(MAX_PASSWORD_RESET_BODY_BYTES, 1_024);
        assert_eq!(MAX_TRANSACTIONAL_EMAIL_BODY_BYTES, 8_192);
        assert_eq!(MAX_HTML_BODY_BYTES, 5_000_000);
        assert_eq!(HTML_BODY_TOKEN_TTL_SECS, 120);
        assert_eq!(MAX_PENDING_HTML_BODIES, 512);
    }
}
