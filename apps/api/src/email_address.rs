//! Shared email-address validation: the single source of truth for what
//! counts as an acceptable address in this API.
//!
//! Two strictness levels exist, deliberately, because the call sites have
//! different stakes:
//!
//! * [`valid_email_address`] — strict. Transactional / integration recipients
//!   and the configured integration `From` address, where an invalid address
//!   is a misconfiguration or a bad integration call and must fail fast:
//!   exactly one `@`, an RFC 5321 `atext` local part (no leading or trailing
//!   dot, no `..`, max 64 chars), a domain of at least two LDH labels (each
//!   1-63 chars, alphanumerics or `-`, never starting or ending with `-`),
//!   at most 254 chars total, no whitespace, control characters, or angle
//!   brackets.
//!
//! * [`valid_compose_address`] — loose. User-typed compose recipients, where
//!   the mail server remains the final arbiter and rejecting an address the
//!   server would accept is worse than passing a sloppy one through. It
//!   shares the strict check's length and domain-shape guards (at most 254
//!   chars, non-empty local part of at most 64 chars, domain of at least two
//!   non-empty dot-separated labels) but does not enforce the `atext`
//!   local-part charset, dot placement in the local part, the single-`@`
//!   rule, or the LDH label rules; it only requires printable ASCII without
//!   angle brackets, parens, square brackets, backslash, comma, semicolon,
//!   colon, or double quote.
//!
//! Concretely, the loose check accepts addresses the strict check rejects,
//! e.g. `.a@x.id`, `a..b@x.id`, `a@b@x.id`, `a@x_y.id`, `a@-x.id`, and a
//! domain label longer than 63 chars. Every address the strict check accepts
//! is also accepted by the loose check. Neither check accepts display names
//! or quoted local parts.

/// Splits a domain into labels; `None` when it has fewer than two labels
/// or any label is empty (i.e. a leading or trailing dot, or `..`).
/// Shared by both address checks: it is exactly the loose check's old
/// domain-shape rule, and the strict check adds its LDH rules on top.
fn domain_labels(domain: &str) -> Option<Vec<&str>> {
    let labels: Vec<&str> = domain.split('.').collect();
    if labels.len() >= 2 && labels.iter().all(|label| !label.is_empty()) {
        Some(labels)
    } else {
        None
    }
}

/// Strict address check for transactional / integration mail: the recipient
/// and the configured integration `From` address must be deliverable, so an
/// invalid address fails fast here instead of at the relay.
pub(crate) fn valid_email_address(value: &str) -> bool {
    if value.len() > 254
        || value.bytes().any(|byte| {
            byte.is_ascii_whitespace() || byte.is_ascii_control() || matches!(byte, b'<' | b'>')
        })
    {
        return false;
    }
    let mut parts = value.split('@');
    let Some(local) = parts.next() else {
        return false;
    };
    let Some(domain) = parts.next() else {
        return false;
    };
    let valid_local = !local.starts_with('.')
        && !local.ends_with('.')
        && !local.contains("..")
        && local.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(
                    byte,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'/'
                        | b'='
                        | b'?'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'{'
                        | b'|'
                        | b'}'
                        | b'~'
                        | b'.'
                )
        });
    if parts.next().is_some()
        || local.is_empty()
        || local.len() > 64
        || !valid_local
        || domain.is_empty()
    {
        return false;
    }
    let Some(labels) = domain_labels(domain) else {
        return false;
    };
    labels.iter().all(|label| {
        label.len() <= 63
            && label.as_bytes()[0].is_ascii_alphanumeric()
            && label.as_bytes()[label.len() - 1].is_ascii_alphanumeric()
            && label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    })
}

/// Loose address check for user-typed compose recipients: same length and
/// domain-shape guards as the strict check, but the mail server remains the
/// final arbiter, so local-part charset, dot placement, the single-`@`
/// rule, and the LDH label rules are not enforced here.
pub(crate) fn valid_compose_address(value: &str) -> bool {
    let Some((local, domain)) = value.rsplit_once('@') else {
        return false;
    };
    value.len() <= 254
        && !local.is_empty()
        && local.len() <= 64
        && domain_labels(domain).is_some()
        && value
            .chars()
            .all(|c| c.is_ascii_graphic() && !"<>()[]\\,;:\"".contains(c))
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strict_addresses_are_also_loose_addresses() {
        for addr in ["a@meowlabs.id", "first.last+tag@sub.example.co"] {
            assert!(valid_email_address(addr), "{addr}");
            assert!(valid_compose_address(addr), "{addr}");
        }
    }

    #[test]
    fn loose_accepts_addresses_strict_rejects() {
        // Documented divergence (see module docs): the compose check is
        // deliberately looser than the transactional check.
        for addr in [".a@x.id", "a..b@x.id", "a@b@x.id", "a@x_y.id", "a@-x.id"] {
            assert!(!valid_email_address(addr), "strict must reject {addr}");
            assert!(valid_compose_address(addr), "loose must accept {addr}");
        }
        let long_label = format!("a@{}.id", "x".repeat(64));
        assert!(!valid_email_address(&long_label));
        assert!(valid_compose_address(&long_label));
    }
}
