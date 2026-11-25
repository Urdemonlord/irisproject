//! Stalwart management API client: account aliases and domain
//! catch-all settings.
//!
//! Unlike `JmapClient`, which always uses the end user's own credentials
//! (TRD §5), this client authenticates with a Stalwart admin credential.
//! Management and user JMAP share the single loopback HTTP listener on
//! 127.0.0.1:8080 (the installer binds one HTTP listener and deletes the
//! others; see `packaging/src/usr/lib/iris/iris-setup.py`). There is no
//! separate management listener: the admin credential, not a separate
//! port, is what isolates management calls. It is only constructed when
//! the admin credential is configured — the API layer fails closed with
//! 503 `aliases_unavailable` while it is not (see
//! `iris_config::Config::stalwart_admin_configured`).
//!
//! Wire shapes (verified against the Stalwart docs, "Managing
//! accounts" and the Account object reference):
//! * An alias is an `EmailAlias` entry of an account's `aliases` list:
//!   `{ "name": <local part>, "domainId": <domain id>, "enabled": bool }`.
//! * Lists are index-keyed JSON objects in `x:Account/set` arguments,
//!   never arrays (`{"0": {...}, "1": {...}}`); a `get` answer may
//!   return either shape, so reads accept both.
//! * Catch-all is the domain's `catchAllAddress` field (`null`
//!   disables it); set with `x:Domain/set`.
//! * The caller needs `sysAccountGet`/`sysAccountUpdate` for aliases
//!   and `sysDomainGet`/`sysDomainUpdate` for catch-all, and must hold
//!   every permission of the account it changes.

use crate::{MailError, USING_STALWART};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::OnceLock;

/// One `reqwest::Client` shared by every `AdminClient` (connection
/// reuse across alias/catch-all requests; `reqwest::Client` is cheap
/// to clone).
static SHARED_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

fn shared_client() -> reqwest::Client {
    SHARED_CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(15))
                .build()
                .expect("reqwest client")
        })
        .clone()
}

/// Response check for the admin client only. Unlike the crate-root
/// `check` (used by the user JMAP client), a 401/403 here must NOT
/// become `MailError::Auth`: the API turns `Auth` into 401
/// `unauthorized` and the web client signs the user out, but a bad
/// admin credential is a server-side misconfiguration, not a bad user
/// session. It becomes `Unavailable` instead, which the API maps to
/// 502 `mail_server_unavailable`.
async fn check_admin(resp: reqwest::Response) -> Result<Value, MailError> {
    match resp.status().as_u16() {
        401 => Err(MailError::Unavailable(
            "management API auth failed: HTTP 401".into(),
        )),
        403 => Err(MailError::Unavailable(
            "management API auth failed: HTTP 403".into(),
        )),
        s if s >= 400 => Err(MailError::Unavailable(format!("HTTP {s}"))),
        _ => resp
            .json()
            .await
            .map_err(|e| MailError::Protocol(e.to_string())),
    }
}

/// One entry of an account's server-side alias list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailAlias {
    /// Local part of the alias address (before the `@`).
    pub name: String,
    /// Id of the alias address's domain.
    #[serde(rename = "domainId")]
    pub domain_id: String,
    /// Disabled aliases stay listed but receive no mail. Stalwart
    /// always reports the flag; a missing flag means enabled.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_enabled() -> bool {
    true
}

impl EmailAlias {
    /// The full address this alias delivers to (`name@domain`).
    pub fn address(&self, domain_name: &str) -> String {
        format!("{}@{domain_name}", self.name)
    }
}

/// A mail domain with its catch-all target, if one is set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainInfo {
    pub id: String,
    pub name: String,
    /// Where otherwise-unmatched addresses in this domain go; `None`
    /// means unknown recipients are rejected.
    pub catch_all: Option<String>,
}

/// Parses one `EmailAlias` from a `get` answer. Accepts the list in
/// either shape: a JSON array, or an index-keyed object.
///
/// Strict: every entry must deserialize (object with `name` and
/// `domainId` strings; a missing `enabled` still defaults to enabled)
/// and every object key must parse as `usize`. Any failure is
/// `MailError::Protocol` — silently dropping a malformed entry would
/// be dangerous, because `set_account_aliases` replaces the whole
/// server list and a truncated parse could delete real aliases.
/// EXCEPTION: a missing or null `aliases` field parses as empty (a
/// fresh account legitimately has no aliases field). A present-but-
/// wrong-typed value (string/number/bool) is an error.
fn parse_alias_list(v: &Value) -> Result<Vec<EmailAlias>, MailError> {
    match v {
        Value::Null => Ok(Vec::new()),
        Value::Array(items) => items
            .iter()
            .map(|i| {
                serde_json::from_value(i.clone())
                    .map_err(|e| MailError::Protocol(format!("malformed alias entry: {e}")))
            })
            .collect(),
        Value::Object(map) => {
            let mut indexed: Vec<(usize, EmailAlias)> = Vec::with_capacity(map.len());
            for (k, i) in map {
                let pos = k.parse::<usize>().map_err(|_| {
                    MailError::Protocol(format!("malformed alias index key: {k:?}"))
                })?;
                let alias = serde_json::from_value(i.clone())
                    .map_err(|e| MailError::Protocol(format!("malformed alias entry: {e}")))?;
                indexed.push((pos, alias));
            }
            indexed.sort_by_key(|(pos, _)| *pos);
            Ok(indexed.into_iter().map(|(_, a)| a).collect())
        }
        other => Err(MailError::Protocol(format!(
            "aliases is not a list: {other}"
        ))),
    }
}

/// True when a `notUpdated` refusal says the address is already in
/// use: some entry's `description` (or `type`) mentions it being
/// taken, case-insensitively ("taken" or "already"). Anything else
/// (permission, not found, invalid patch) is not a conflict.
fn alias_refusal_is_taken(not_updated: &Value) -> bool {
    let Some(entries) = not_updated.as_object() else {
        return false;
    };
    entries.values().any(|entry| {
        let texts: Vec<&str> = match entry {
            Value::Object(_) => ["description", "type"]
                .into_iter()
                .filter_map(|k| entry[k].as_str())
                .collect(),
            Value::String(s) => vec![s.as_str()],
            _ => Vec::new(),
        };
        texts.iter().any(|t| {
            let lower = t.to_lowercase();
            lower.contains("taken") || lower.contains("already")
        })
    })
}

fn domain_from_value(v: &Value) -> Option<DomainInfo> {
    Some(DomainInfo {
        id: v["id"].as_str()?.to_string(),
        name: v["name"].as_str()?.to_string(),
        catch_all: v["catchAllAddress"].as_str().map(str::to_string),
    })
}

/// JMAP management client, authenticated as a Stalwart admin.
#[derive(Clone)]
pub struct AdminClient {
    base_url: String,
    user: String,
    pass: String,
    http: reqwest::Client,
}

impl AdminClient {
    pub fn new(base_url: String, user: String, pass: String) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            user,
            pass,
            http: shared_client(),
        }
    }

    async fn call(&self, calls: Value) -> Result<Vec<Value>, MailError> {
        let resp = self
            .http
            .post(format!("{}/jmap/", self.base_url))
            .basic_auth(&self.user, Some(&self.pass))
            .json(&json!({ "using": USING_STALWART, "methodCalls": calls }))
            .send()
            .await
            .map_err(|e| MailError::Unavailable(e.to_string()))?;
        let v = check_admin(resp).await?;
        let responses = v["methodResponses"]
            .as_array()
            .ok_or_else(|| MailError::Protocol("no methodResponses".into()))?;
        let out: Vec<Value> = responses.iter().map(|r| r[1].clone()).collect();
        for (i, r) in responses.iter().enumerate() {
            if r[0] == "error" {
                return Err(MailError::Protocol(format!("call {i}: {}", r[1])));
            }
        }
        Ok(out)
    }

    /// Aliases of one mail-server account, in server order. `NotFound`
    /// when the account does not exist.
    pub async fn get_account_aliases(
        &self,
        account_id: &str,
    ) -> Result<Vec<EmailAlias>, MailError> {
        let r = self
            .call(json!([["x:Account/get", {"ids": [account_id], "properties": ["aliases"]}, "g"]]))
            .await?;
        let account = r[0]["list"].get(0).ok_or(MailError::NotFound)?;
        parse_alias_list(&account["aliases"])
    }

    /// Replaces an account's whole alias list (index-keyed map: lists
    /// are objects, never arrays, in `x:Account/set`). Callers hold the
    /// API's `organize_lock` across the read and this write, so two
    /// IRIS requests cannot interleave a read-modify-write; only an
    /// out-of-band admin change (WebUI/CLI) could still race it, and
    /// that change would be overwritten. A refused update is
    /// `MailError::Conflict` only when the address is already in use
    /// by another account; any other refusal (permission, not found,
    /// invalid patch) is `MailError::Protocol`.
    pub async fn set_account_aliases(
        &self,
        account_id: &str,
        aliases: &[EmailAlias],
    ) -> Result<(), MailError> {
        let mut map = serde_json::Map::new();
        for (i, a) in aliases.iter().enumerate() {
            map.insert(
                i.to_string(),
                json!({"name": a.name, "domainId": a.domain_id, "enabled": a.enabled}),
            );
        }
        let r = self
            .call(json!([["x:Account/set", {"update": {account_id: {"aliases": map}}}, "s"]]))
            .await?;
        if r[0]["notUpdated"]
            .as_object()
            .is_some_and(|o| !o.is_empty())
        {
            let detail = format!("aliases not updated: {}", r[0]["notUpdated"]);
            // Stalwart's exact refusal shape is not fully documented,
            // so this is a best-effort distinction: only a refusal
            // whose description (or type) says the address is taken
            // counts as a conflict.
            if alias_refusal_is_taken(&r[0]["notUpdated"]) {
                return Err(MailError::Conflict(detail));
            }
            return Err(MailError::Protocol(detail));
        }
        Ok(())
    }

    /// Every domain the server knows, with its catch-all target.
    pub async fn list_domains(&self) -> Result<Vec<DomainInfo>, MailError> {
        let r = self
            .call(json!([["x:Domain/get", {"ids": null, "properties": ["name", "catchAllAddress"]}, "g"]]))
            .await?;
        let list = r[0]["list"].as_array().cloned().unwrap_or_default();
        Ok(list.iter().filter_map(domain_from_value).collect())
    }

    /// One domain by name (case-insensitive), or `None` when the server
    /// has no such domain.
    pub async fn resolve_domain(&self, name: &str) -> Result<Option<DomainInfo>, MailError> {
        Ok(self
            .list_domains()
            .await?
            .into_iter()
            .find(|d| d.name.eq_ignore_ascii_case(name)))
    }

    /// Sets a domain's catch-all target (`None` disables it, so unknown
    /// recipients are rejected again).
    pub async fn set_catch_all(
        &self,
        domain_id: &str,
        target: Option<&str>,
    ) -> Result<(), MailError> {
        let r = self
            .call(json!([["x:Domain/set", {"update": {domain_id: {"catchAllAddress": target}}}, "s"]]))
            .await?;
        if r[0]["notUpdated"]
            .as_object()
            .is_some_and(|o| !o.is_empty())
        {
            return Err(MailError::Protocol(format!(
                "catch-all not updated: {}",
                r[0]["notUpdated"]
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::time::timeout;

    /// Minimal fake admin server: answers every request with the same
    /// fixed status and body, then closes the connection. The accept
    /// loop gives up after 5s idle, so the task ends on its own.
    async fn fake_admin_server(status: u16, body: String) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            loop {
                let Ok(Ok((mut stream, _))) =
                    timeout(Duration::from_secs(5), listener.accept()).await
                else {
                    return;
                };
                let mut bytes = Vec::new();
                let mut chunk = [0u8; 4096];
                let mut header_end: Option<usize> = None;
                let mut expected: Option<usize> = None;
                let mut complete = false;
                while !complete {
                    let Ok(n) = stream.read(&mut chunk).await else {
                        break;
                    };
                    if n == 0 {
                        break;
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
                            complete = true;
                        }
                    }
                }
                let reason = match status {
                    401 => "Unauthorized",
                    403 => "Forbidden",
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
        });
        base_url
    }

    fn set_response(not_updated: Value) -> String {
        serde_json::to_string(&json!({"methodResponses": [[
            "x:Account/set",
            {"accountId": "acct", "updated": {}, "notUpdated": not_updated},
            "s"
        ]]}))
        .unwrap()
    }

    #[tokio::test]
    async fn admin_401_and_403_are_unavailable_not_auth() {
        for (status, want) in [(401u16, "HTTP 401"), (403u16, "HTTP 403")] {
            let base = fake_admin_server(status, String::new()).await;
            let client = AdminClient::new(base, "admin".into(), "secret".into());
            let err = client.get_account_aliases("acct").await.unwrap_err();
            assert!(
                matches!(err, MailError::Unavailable(ref m) if m.contains("management API auth failed") && m.contains(want)),
                "status {status} gave {err:?}, want Unavailable mentioning {want}"
            );
        }
    }

    #[tokio::test]
    async fn set_aliases_taken_is_conflict_other_refusal_is_protocol() {
        let alias = EmailAlias {
            name: "sales".into(),
            domain_id: "d1".into(),
            enabled: true,
        };
        let taken = fake_admin_server(
            200,
            set_response(json!({"acct": {"type": "invalidPatch", "description": "address taken"}})),
        )
        .await;
        let client = AdminClient::new(taken, "admin".into(), "secret".into());
        assert!(matches!(
            client
                .set_account_aliases("acct", std::slice::from_ref(&alias))
                .await,
            Err(MailError::Conflict(_))
        ));
        let forbidden = fake_admin_server(
            200,
            set_response(json!({"acct": {"type": "forbidden", "description": "not permitted"}})),
        )
        .await;
        let client = AdminClient::new(forbidden, "admin".into(), "secret".into());
        assert!(matches!(
            client.set_account_aliases("acct", &[alias]).await,
            Err(MailError::Protocol(_))
        ));
    }

    #[test]
    fn alias_list_parses_array_and_indexed_map() {
        let from_array = parse_alias_list(&json!([
            {"name": "a", "domainId": "d1", "enabled": true},
            {"name": "b", "domainId": "d1"},
        ]))
        .unwrap();
        assert_eq!(from_array.len(), 2);
        assert_eq!(from_array[0].name, "a");
        // A missing `enabled` flag means enabled.
        assert!(from_array[1].enabled);
        let from_map = parse_alias_list(&json!({
            "1": {"name": "b", "domainId": "d1", "enabled": false},
            "0": {"name": "a", "domainId": "d1", "enabled": true},
        }))
        .unwrap();
        assert_eq!(
            from_map,
            vec![
                EmailAlias {
                    name: "a".into(),
                    domain_id: "d1".into(),
                    enabled: true
                },
                EmailAlias {
                    name: "b".into(),
                    domain_id: "d1".into(),
                    enabled: false
                },
            ]
        );
        // A missing or null `aliases` field parses as empty (a fresh
        // account legitimately has no aliases field).
        assert!(parse_alias_list(&json!(null)).unwrap().is_empty());
        // A present-but-wrong-typed value is an error, never empty.
        assert!(matches!(
            parse_alias_list(&json!("x")),
            Err(MailError::Protocol(_))
        ));
        assert!(matches!(
            parse_alias_list(&json!(42)),
            Err(MailError::Protocol(_))
        ));
        assert!(matches!(
            parse_alias_list(&json!(true)),
            Err(MailError::Protocol(_))
        ));
        // One malformed entry among good ones errors instead of
        // silently dropping it (a truncated parse could delete real
        // aliases on the next whole-list write).
        assert!(matches!(
            parse_alias_list(&json!([
                {"name": "a", "domainId": "d1"},
                {"name": "b"},
            ])),
            Err(MailError::Protocol(_))
        ));
        assert!(matches!(
            parse_alias_list(&json!({
                "0": {"name": "a", "domainId": "d1"},
                "x": {"name": "b", "domainId": "d1"},
            })),
            Err(MailError::Protocol(_))
        ));
    }

    #[test]
    fn alias_refusal_taken_only_on_taken_or_already() {
        // The shape the API fake returns for a taken address.
        assert!(alias_refusal_is_taken(&json!({
            "acct-me": {"type": "invalidPatch", "description": "address taken"}
        })));
        assert!(alias_refusal_is_taken(&json!({
            "acct-me": {"type": "invalidPatch", "description": "Address Already In Use"}
        })));
        // Other refusals are not conflicts.
        assert!(!alias_refusal_is_taken(&json!({
            "acct-me": {"type": "forbidden", "description": "not permitted"}
        })));
        assert!(!alias_refusal_is_taken(&json!({
            "acct-me": {"type": "notFound", "description": "no such account"}
        })));
        assert!(!alias_refusal_is_taken(&json!({})));
    }

    #[test]
    fn domain_parses_catch_all_and_missing_target() {
        let with = domain_from_value(
            &json!({"id": "d1", "name": "x.id", "catchAllAddress": "postmaster@x.id"}),
        )
        .unwrap();
        assert_eq!(with.catch_all.as_deref(), Some("postmaster@x.id"));
        let without =
            domain_from_value(&json!({"id": "d1", "name": "x.id", "catchAllAddress": null}))
                .unwrap();
        assert_eq!(without.catch_all, None);
        assert!(domain_from_value(&json!({"name": "x.id"})).is_none());
    }

    #[test]
    fn alias_address_is_local_at_domain() {
        let a = EmailAlias {
            name: "sales".into(),
            domain_id: "d1".into(),
            enabled: true,
        };
        assert_eq!(a.address("x.id"), "sales@x.id");
    }
}
