use chrono::{Duration, Utc};
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, CHACHA20_POLY1305, NONCE_LEN};
use ring::rand::{SecureRandom, SystemRandom};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub const SESSION_DAYS: i64 = 30;
pub const COOKIE_NAME: &str = "iris_session";
pub const CSRF_COOKIE: &str = "iris_csrf";
pub const CSRF_HEADER: &str = "x-csrf-token";

/// Random opaque token (UUIDv4 = 122 random bits). Only its hash is stored.
pub fn new_token() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

pub fn hash_token(token: &str) -> String {
    Sha256::digest(token.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

pub fn expiry_at() -> chrono::DateTime<Utc> {
    Utc::now() + Duration::days(SESSION_DAYS)
}

pub fn expiry() -> String {
    expiry_at().to_rfc3339()
}

pub fn now() -> String {
    Utc::now().to_rfc3339()
}

/// Extract a named cookie from a raw Cookie header.
pub fn cookie<'a>(header: &'a str, name: &str) -> Option<&'a str> {
    header.split(';').map(str::trim).find_map(|c| c.strip_prefix(name)?.strip_prefix('='))
}

/// Extract the session token from a raw Cookie header.
pub fn token_from_cookie(header: &str) -> Option<&str> {
    cookie(header, COOKIE_NAME)
}

/// Constant-time equality for secrets of public length.
pub fn ct_eq(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Double-submit CSRF: unsafe requests must echo the `iris_csrf` cookie in `X-CSRF-Token`.
pub fn csrf_ok(cookie_header: Option<&str>, header_token: Option<&str>) -> bool {
    match (cookie_header.and_then(|c| cookie(c, CSRF_COOKIE)), header_token) {
        (Some(c), Some(h)) => c.len() == 64 && ct_eq(c, h),
        _ => false,
    }
}

/// Readable by JS (not HttpOnly) so the SPA can echo it; SameSite=Strict keeps it first-party.
pub fn set_csrf_cookie(token: &str, secure: bool) -> String {
    format!("{CSRF_COOKIE}={token}; Path=/; SameSite=Strict; Max-Age={}{}", SESSION_DAYS * 86400, if secure { "; Secure" } else { "" })
}

/// Mail credential sealed with a key derived from the session cookie token. The DB only holds
/// the token hash, so a DB dump alone cannot recover the mail credential.
fn session_key(server_secret: &str, token: &str) -> LessSafeKey {
    let k = Sha256::digest(format!("iris-cred-v1:{server_secret}:{token}").as_bytes());
    LessSafeKey::new(UnboundKey::new(&CHACHA20_POLY1305, &k).expect("32-byte key"))
}

pub fn seal(server_secret: &str, token: &str, plaintext: &str) -> String {
    let mut nonce = [0u8; NONCE_LEN];
    SystemRandom::new().fill(&mut nonce).expect("rng");
    let mut buf = plaintext.as_bytes().to_vec();
    session_key(server_secret, token)
        .seal_in_place_append_tag(Nonce::assume_unique_for_key(nonce), Aad::empty(), &mut buf)
        .expect("seal");
    nonce.iter().chain(&buf).map(|b| format!("{b:02x}")).collect()
}

pub fn open(server_secret: &str, token: &str, sealed: &str) -> Option<String> {
    let bytes: Vec<u8> = (0..sealed.len()).step_by(2)
        .map(|i| u8::from_str_radix(sealed.get(i..i + 2)?, 16).ok())
        .collect::<Option<_>>()?;
    if bytes.len() < NONCE_LEN + 16 {
        return None;
    }
    let (nonce, ct) = bytes.split_at(NONCE_LEN);
    let mut ct = ct.to_vec();
    let nonce = Nonce::try_assume_unique_for_key(nonce).ok()?;
    let pt = session_key(server_secret, token).open_in_place(nonce, Aad::empty(), &mut ct).ok()?;
    String::from_utf8(pt.to_vec()).ok()
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

    #[test]
    fn csrf_double_submit() {
        let t = new_token();
        let c = format!("x=1; {CSRF_COOKIE}={t}");
        assert!(csrf_ok(Some(&c), Some(&t)));
        assert!(!csrf_ok(Some(&c), Some(&new_token())));
        assert!(!csrf_ok(Some(&c), None));
        assert!(!csrf_ok(None, Some(&t)));
        assert!(!csrf_ok(Some("iris_csrf=short"), Some("short")));
    }

    #[test]
    fn seal_roundtrip_and_binding() {
        let tok = new_token();
        let s = seal("srv", &tok, "app_secret");
        assert_eq!(open("srv", &tok, &s).as_deref(), Some("app_secret"));
        assert_eq!(open("srv", &new_token(), &s), None); // other session token
        assert_eq!(open("other", &tok, &s), None); // other server secret
        assert_eq!(open("srv", &tok, &s[..s.len() - 2]), None); // tampered
        assert_ne!(s, seal("srv", &tok, "app_secret")); // random nonce
    }
}
