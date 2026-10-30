use chrono::{Duration, Utc};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub const SESSION_DAYS: i64 = 30;
pub const COOKIE_NAME: &str = "iris_session";

/// Random opaque token (UUIDv4 = 122 random bits). Only its hash is stored.
pub fn new_token() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

pub fn hash_token(token: &str) -> String {
    Sha256::digest(token.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

pub fn expiry() -> String {
    (Utc::now() + Duration::days(SESSION_DAYS)).to_rfc3339()
}

pub fn now() -> String {
    Utc::now().to_rfc3339()
}

/// Extract the session token from a raw Cookie header.
pub fn token_from_cookie(header: &str) -> Option<&str> {
    header.split(';').map(str::trim).find_map(|c| c.strip_prefix(COOKIE_NAME)?.strip_prefix('='))
}

pub fn set_cookie(token: &str, secure: bool) -> String {
    format!(
        "{COOKIE_NAME}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}{}",
        SESSION_DAYS * 86400,
        if secure { "; Secure" } else { "" }
    )
}

pub fn clear_cookie() -> String {
    format!("{COOKIE_NAME}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cookie_and_hash() {
        assert_eq!(token_from_cookie("a=1; iris_session=abc; b=2"), Some("abc"));
        assert_eq!(token_from_cookie("iris_sessionx=abc"), None);
        assert_eq!(hash_token("x").len(), 64);
        assert_ne!(new_token(), new_token());
    }
}
