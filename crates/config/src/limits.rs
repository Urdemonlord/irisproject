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
/// Max messages destroyed per JMAP `destroy_to` / `destroy_before` call.
pub const MAX_DESTROY_PER_CALL: u32 = 500;

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

// --- Request caps (were `apps/api/src/main.rs`) ----------------------------

/// Default page size for message listing.
pub const DEFAULT_LIST_LIMIT: u32 = 50;
/// Max page size a client may request for message listing.
pub const MAX_LIST_LIMIT: u32 = 100;
/// Search terms are truncated to this many characters before going to JMAP.
pub const MAX_SEARCH_QUERY_CHARS: usize = 200;
/// Max request body for the Timeless password-reset endpoint.
pub const MAX_TIMELESS_RESET_BODY_BYTES: usize = 1_024;
/// Max request body for the transactional-email send endpoint.
pub const MAX_TRANSACTIONAL_EMAIL_BODY_BYTES: usize = 8_192;

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
        assert_eq!(MAX_RECIPIENTS, 50);
        assert_eq!(MAX_SUBJECT, 998);
        assert_eq!(MAX_BODY, 1_000_000);
        assert_eq!(MAX_HEADER_IDS, 20);
        assert_eq!(MAX_HEADER_ID_LEN, 250);
        assert_eq!(SENDS_PER_HOUR, 100);
        assert_eq!(IP_CREATES_PER_HOUR, 10);
        assert_eq!(ACTIVE_PER_CLIENT, 3);
        assert_eq!(TEMP_INBOX_MESSAGE_LIMIT, 50);
        assert_eq!(MAX_FAILED_LOGINS, 10);
        assert_eq!(MAX_FAILED_LOGINS_PER_IP, 50);
        assert_eq!(DEFAULT_LIST_LIMIT, 50);
        assert_eq!(MAX_LIST_LIMIT, 100);
        assert_eq!(MAX_SEARCH_QUERY_CHARS, 200);
        assert_eq!(MAX_TIMELESS_RESET_BODY_BYTES, 1_024);
        assert_eq!(MAX_TRANSACTIONAL_EMAIL_BODY_BYTES, 8_192);
    }
}
