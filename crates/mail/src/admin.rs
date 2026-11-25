//! Stalwart management API client: account aliases and domain
//! catch-all settings.
//!
//! Unlike `JmapClient`, which always uses the end user's own credentials
//! (TRD §5), this client authenticates with a Stalwart admin credential
//! and talks to the separate management listener (loopback 8080 in the
//! installer; see `packaging/src/usr/lib/iris/iris-setup.py`). It is
//! only constructed when the admin credential is configured — the API
//! layer fails closed with 503 `aliases_unavailable` while it is not
//! (see `iris_config::Config::stalwart_admin_configured`).
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

use crate::{check, MailError, USING_STALWART};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

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
fn parse_alias_list(v: &Value) -> Vec<EmailAlias> {
    match v {
        Value::Array(items) => items
            .iter()
            .filter_map(|i| serde_json::from_value(i.clone()).ok())
            .collect(),
        Value::Object(map) => {
            let mut indexed: Vec<(usize, EmailAlias)> = map
                .iter()
                .filter_map(|(k, i)| {
                    let pos = k.parse::<usize>().ok()?;
                    let alias = serde_json::from_value(i.clone()).ok()?;
                    Some((pos, alias))
                })
                .collect();
            indexed.sort_by_key(|(pos, _)| *pos);
            indexed.into_iter().map(|(_, a)| a).collect()
        }
        _ => Vec::new(),
    }
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
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .expect("reqwest client");
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            user,
            pass,
            http,
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
        let v = check(resp).await?;
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
        Ok(parse_alias_list(&account["aliases"]))
    }

    /// Replaces an account's whole alias list (index-keyed map: lists
    /// are objects, never arrays, in `x:Account/set`). Callers hold the
    /// API's `organize_lock` across the read and this write, so two
    /// IRIS requests cannot interleave a read-modify-write; only an
    /// out-of-band admin change (WebUI/CLI) could still race it, and
    /// that change would be overwritten. A refused update — the address
    /// belongs to another account — is `MailError::Conflict`.
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
            return Err(MailError::Conflict(format!(
                "aliases not updated: {}",
                r[0]["notUpdated"]
            )));
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

    #[test]
    fn alias_list_parses_array_and_indexed_map() {
        let from_array = parse_alias_list(&json!([
            {"name": "a", "domainId": "d1", "enabled": true},
            {"name": "b", "domainId": "d1"},
        ]));
        assert_eq!(from_array.len(), 2);
        assert_eq!(from_array[0].name, "a");
        // A missing `enabled` flag means enabled.
        assert!(from_array[1].enabled);
        let from_map = parse_alias_list(&json!({
            "1": {"name": "b", "domainId": "d1", "enabled": false},
            "0": {"name": "a", "domainId": "d1", "enabled": true},
        }));
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
        // Non-list values parse as empty, never panic.
        assert!(parse_alias_list(&json!(null)).is_empty());
        assert!(parse_alias_list(&json!("x")).is_empty());
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
