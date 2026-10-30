//! Minimal JMAP client. Every call uses the end user's own credentials
//! (never an admin credential), per TRD §5.
use serde::Serialize;
use serde_json::{json, Value};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum MailError {
    #[error("mail server unavailable: {0}")]
    Unavailable(String),
    #[error("authentication failed")]
    Auth,
    #[error("not found")]
    NotFound,
    #[error("unexpected response: {0}")]
    Protocol(String),
    /// The mail server refused to send (e.g. recipient rejected).
    #[error("rejected: {0}")]
    Rejected(String),
}

#[derive(Debug, Clone, Serialize)]
pub struct MessageSummary {
    pub id: String,
    pub from: String,
    pub to: Vec<String>,
    pub subject: String,
    pub preview: String,
    pub received_at: String,
    pub is_read: bool,
    pub is_draft: bool,
    pub has_attachment: bool,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct MessageDetail {
    #[serde(flatten)]
    pub summary: MessageSummary,
    pub text_body: Option<String>,
    pub html_body: Option<String>,
    pub cc: Vec<String>,
    /// Message-ID header (without angle brackets), for threading replies.
    pub message_id: Option<String>,
    pub references: Vec<String>,
}

/// A folder (JMAP mailbox) with its standard role.
#[derive(Debug, Clone, Serialize)]
pub struct Folder {
    pub id: String,
    pub role: String,
    pub name: String,
    pub total: u64,
    pub unread: u64,
}

/// Standard folders IRIS shows, in display order, with the name used when one is missing.
pub const FOLDER_ROLES: [(&str, &str); 5] = [
    ("inbox", "Inbox"), ("sent", "Sent"), ("drafts", "Drafts"), ("archive", "Archive"), ("trash", "Trash"),
];

/// An outgoing message or draft. `from` is always the signed-in user.
#[derive(Debug, Clone, Default)]
pub struct Compose {
    pub from: String,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub subject: String,
    pub text: String,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
}

#[derive(Clone)]
pub struct JmapClient {
    base_url: String,
    http: reqwest::Client,
}

const USING: [&str; 2] = ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail"];
const USING_STALWART: [&str; 2] = ["urn:ietf:params:jmap:core", "urn:stalwart:jmap"];
const USING_SUBMIT: [&str; 3] = ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail", "urn:ietf:params:jmap:submission"];
const SUMMARY_PROPS: [&str; 8] = ["from", "to", "subject", "preview", "receivedAt", "keywords", "size", "hasAttachment"];

impl JmapClient {
    pub fn new(base_url: String) -> Self {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .expect("reqwest client");
        Self { base_url: base_url.trim_end_matches('/').to_string(), http }
    }

    /// Validates credentials; returns the primary mail account id.
    pub async fn login(&self, user: &str, pass: &str) -> Result<String, MailError> {
        let resp = self
            .http
            .get(format!("{}/jmap/session", self.base_url))
            .basic_auth(user, Some(pass))
            .send()
            .await
            .map_err(|e| MailError::Unavailable(e.to_string()))?;
        let v = check(resp).await?;
        v["primaryAccounts"]["urn:ietf:params:jmap:mail"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| MailError::Protocol("no mail account".into()))
    }

    async fn call(&self, user: &str, pass: &str, calls: Value) -> Result<Vec<Value>, MailError> {
        self.call_using(user, pass, &USING, calls).await
    }

    async fn call_using(&self, user: &str, pass: &str, using: &[&str], calls: Value) -> Result<Vec<Value>, MailError> {
        // ponytail: posts to {base}/jmap/ instead of session.apiUrl (Stalwart advertises
        // the public hostname); switch to apiUrl if API and mail server get split.
        let resp = self
            .http
            .post(format!("{}/jmap/", self.base_url))
            .basic_auth(user, Some(pass))
            .json(&json!({ "using": using, "methodCalls": calls }))
            .send()
            .await
            .map_err(|e| MailError::Unavailable(e.to_string()))?;
        let v = check(resp).await?;
        let out: Vec<Value> = v["methodResponses"]
            .as_array()
            .ok_or_else(|| MailError::Protocol("no methodResponses".into()))?
            .iter()
            .map(|r| r[1].clone())
            .collect();
        for (i, r) in v["methodResponses"].as_array().unwrap().iter().enumerate() {
            if r[0] == "error" {
                return Err(MailError::Protocol(format!("call {i}: {}", r[1])));
            }
        }
        Ok(out)
    }

    /// Creates a Stalwart app password for the authenticated user; returns (id, secret).
    /// The secret is only ever returned by this call.
    pub async fn create_app_password(
        &self, user: &str, pass: &str, description: &str, expires_at: &str,
    ) -> Result<(String, String), MailError> {
        let r = self
            .call_using(user, pass, &USING_STALWART, json!([["x:AppPassword/set", {"create": {"n": {
                "description": description, "permissions": {"@type": "Inherit"}, "expiresAt": expires_at}}}, "c"]]))
            .await?;
        let c = &r[0]["created"]["n"];
        match (c["id"].as_str(), c["secret"].as_str()) {
            (Some(id), Some(secret)) => Ok((id.to_string(), secret.to_string())),
            _ => Err(MailError::Protocol(format!("app password not created: {}", r[0]["notCreated"]))),
        }
    }

    /// Revokes an app password (an app password may revoke itself).
    pub async fn destroy_app_password(&self, user: &str, pass: &str, id: &str) -> Result<(), MailError> {
        self.call_using(user, pass, &USING_STALWART, json!([["x:AppPassword/set", {"destroy": [id]}, "c"]])).await?;
        Ok(())
    }

    /// Destroys messages whose To: contains exactly `to` (temp inbox purge). Returns count destroyed.
    pub async fn destroy_to(&self, user: &str, pass: &str, account: &str, to: &str) -> Result<usize, MailError> {
        let r = self
            .call(user, pass, json!([
                ["Email/query", {"accountId": account, "filter": {"to": to}, "limit": 500}, "q"],
                ["Email/get", {"accountId": account,
                    "#ids": {"resultOf": "q", "name": "Email/query", "path": "/ids"}, "properties": ["to"]}, "g"]
            ]))
            .await?;
        // Full-text `to` filter can over-match; only destroy exact recipient hits.
        let ids: Vec<&str> = r[1]["list"].as_array().into_iter().flatten()
            .filter(|e| has_rcpt(e, to)).filter_map(|e| e["id"].as_str()).collect();
        self.destroy(user, pass, account, &ids).await
    }

    /// Destroys messages received before `before` (UTC RFC3339), up to 500 per call.
    pub async fn destroy_before(&self, user: &str, pass: &str, account: &str, before: &str) -> Result<usize, MailError> {
        let r = self
            .call(user, pass, json!([["Email/query", {"accountId": account, "filter": {"before": before}, "limit": 500}, "q"]]))
            .await?;
        let ids: Vec<&str> = r[0]["ids"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
        self.destroy(user, pass, account, &ids).await
    }

    async fn destroy(&self, user: &str, pass: &str, account: &str, ids: &[&str]) -> Result<usize, MailError> {
        if ids.is_empty() {
            return Ok(0);
        }
        let r = self.call(user, pass, json!([["Email/set", {"accountId": account, "destroy": ids}, "d"]])).await?;
        Ok(r[0]["destroyed"].as_array().map_or(0, Vec::len))
    }

    pub async fn list_inbox(
        &self, user: &str, pass: &str, account: &str, limit: u32, position: u32,
    ) -> Result<(Vec<MessageSummary>, u64), MailError> {
        self.list_folder(user, pass, account, "inbox", limit, position).await
    }

    /// Lists a standard folder by role (inbox, sent, drafts, archive, trash), newest first.
    pub async fn list_folder(
        &self, user: &str, pass: &str, account: &str, role: &str, limit: u32, position: u32,
    ) -> Result<(Vec<MessageSummary>, u64), MailError> {
        let folders = self.folders(user, pass, account).await?;
        let inbox = folders.iter().find(|f| f.role == role).ok_or(MailError::NotFound)?.id.clone();
        let r = self
            .call(user, pass, json!([
                ["Email/query", {"accountId": account, "filter": {"inMailbox": inbox},
                    "sort": [{"property": "receivedAt", "isAscending": false}],
                    "position": position, "limit": limit, "calculateTotal": true}, "q"],
                ["Email/get", {"accountId": account,
                    "#ids": {"resultOf": "q", "name": "Email/query", "path": "/ids"},
                    "properties": SUMMARY_PROPS}, "g"]
            ]))
            .await?;
        let total = r[0]["total"].as_u64().unwrap_or(0);
        let list = r[1]["list"].as_array().cloned().unwrap_or_default();
        Ok((list.iter().map(summary).collect(), total))
    }

    /// Messages in `account` whose To: header contains exactly `to`, received after `after` (UTC RFC3339).
    // ponytail: matches the To: header only, so Bcc'd mail is invisible; switch to the
    // envelope recipient (Delivered-To / sieve tagging) if that matters.
    pub async fn list_to(
        &self, user: &str, pass: &str, account: &str, to: &str, after: &str, limit: u32,
    ) -> Result<Vec<MessageSummary>, MailError> {
        let r = self
            .call(user, pass, json!([
                ["Email/query", {"accountId": account,
                    "filter": {"operator": "AND", "conditions": [{"to": to}, {"after": after}]},
                    "sort": [{"property": "receivedAt", "isAscending": false}], "limit": limit}, "q"],
                ["Email/get", {"accountId": account,
                    "#ids": {"resultOf": "q", "name": "Email/query", "path": "/ids"},
                    "properties": SUMMARY_PROPS}, "g"]
            ]))
            .await?;
        let list = r[1]["list"].as_array().cloned().unwrap_or_default();
        // Full-text `to` filter can over-match; keep only exact address hits.
        Ok(list.iter().filter(|e| has_rcpt(e, to)).map(summary).collect())
    }

    /// Standard folders with counts. Missing ones (e.g. Archive) are created on first use.
    pub async fn folders(&self, user: &str, pass: &str, account: &str) -> Result<Vec<Folder>, MailError> {
        let get = json!([["Mailbox/get", {"accountId": account, "ids": null,
            "properties": ["name", "role", "totalEmails", "unreadEmails"]}, "m"]]);
        let mut list = self.call(user, pass, get.clone()).await?[0]["list"].as_array().cloned().unwrap_or_default();
        let has = |list: &[Value], role: &str| list.iter().any(|m| m["role"] == role);
        let mut create = serde_json::Map::new();
        let mut update = serde_json::Map::new();
        for (role, name) in FOLDER_ROLES {
            if has(&list, role) { continue }
            // Reuse a same-named folder without a role instead of creating a duplicate.
            match list.iter().find(|m| m["role"].is_null() && m["name"].as_str().is_some_and(|n| n.eq_ignore_ascii_case(name))) {
                Some(m) => { update.insert(m["id"].as_str().unwrap_or_default().to_string(), json!({"role": role})); }
                None => { create.insert(role.to_string(), json!({"name": name, "role": role})); }
            }
        }
        if !create.is_empty() || !update.is_empty() {
            let r = self.call(user, pass, json!([["Mailbox/set", {"accountId": account, "create": create, "update": update}, "s"]])).await?;
            if let Some(err) = r[0]["notCreated"].as_object().filter(|o| !o.is_empty()) {
                return Err(MailError::Protocol(format!("folders not created: {}", Value::Object(err.clone()))));
            }
            list = self.call(user, pass, get).await?[0]["list"].as_array().cloned().unwrap_or_default();
        }
        Ok(FOLDER_ROLES.iter().filter_map(|(role, _)| {
            let m = list.iter().find(|m| m["role"] == *role)?;
            Some(Folder {
                id: m["id"].as_str()?.to_string(),
                role: role.to_string(),
                name: m["name"].as_str().unwrap_or(role).to_string(),
                total: m["totalEmails"].as_u64().unwrap_or(0),
                unread: m["unreadEmails"].as_u64().unwrap_or(0),
            })
        }).collect())
    }

    async fn folder_id(&self, user: &str, pass: &str, account: &str, role: &str) -> Result<String, MailError> {
        Ok(self.folders(user, pass, account).await?.into_iter().find(|f| f.role == role).ok_or(MailError::NotFound)?.id)
    }

    /// Roles of the folders a message is currently in.
    pub async fn message_roles(&self, user: &str, pass: &str, account: &str, id: &str) -> Result<Vec<String>, MailError> {
        let folders = self.folders(user, pass, account).await?;
        let r = self.call(user, pass, json!([["Email/get", {"accountId": account, "ids": [id], "properties": ["mailboxIds"]}, "g"]])).await?;
        let e = r[0]["list"].get(0).ok_or(MailError::NotFound)?;
        let ids = e["mailboxIds"].as_object().cloned().unwrap_or_default();
        Ok(folders.into_iter().filter(|f| ids.contains_key(&f.id)).map(|f| f.role).collect())
    }

    /// Moves a message into exactly one standard folder.
    pub async fn move_to(&self, user: &str, pass: &str, account: &str, id: &str, role: &str) -> Result<(), MailError> {
        let target = self.folder_id(user, pass, account, role).await?;
        let r = self.call(user, pass, json!([["Email/set", {"accountId": account,
            "update": {id: {"mailboxIds": {target: true}}}}, "u"]])).await?;
        updated(&r[0], id)
    }

    pub async fn set_seen(&self, user: &str, pass: &str, account: &str, id: &str, seen: bool) -> Result<(), MailError> {
        let patch = if seen { json!(true) } else { Value::Null };
        let r = self.call(user, pass, json!([["Email/set", {"accountId": account,
            "update": {id: {"keywords/$seen": patch}}}, "u"]])).await?;
        updated(&r[0], id)
    }

    /// Permanently deletes one message.
    pub async fn destroy_one(&self, user: &str, pass: &str, account: &str, id: &str) -> Result<(), MailError> {
        match self.destroy(user, pass, account, &[id]).await? {
            0 => Err(MailError::NotFound),
            _ => Ok(()),
        }
    }

    /// Saves a draft (replacing `replace` if given) and returns the new draft id.
    pub async fn save_draft(&self, user: &str, pass: &str, account: &str, c: &Compose, replace: Option<&str>) -> Result<String, MailError> {
        let drafts = self.folder_id(user, pass, account, "drafts").await?;
        let mut set = json!({"accountId": account, "create": {"n": email_json(c, &drafts)}});
        if let Some(old) = replace { set["destroy"] = json!([old]); }
        let r = self.call(user, pass, json!([["Email/set", set, "s"]])).await?;
        r[0]["created"]["n"]["id"].as_str().map(str::to_string)
            .ok_or_else(|| MailError::Protocol(format!("draft not saved: {}", r[0]["notCreated"])))
    }

    /// Sends a message as the signed-in user; it lands in Sent. Deletes `draft` if given.
    pub async fn send(&self, user: &str, pass: &str, account: &str, c: &Compose, draft: Option<&str>) -> Result<(), MailError> {
        let folders = self.folders(user, pass, account).await?;
        let id_of = |role: &str| folders.iter().find(|f| f.role == role).map(|f| f.id.clone()).ok_or(MailError::NotFound);
        let (drafts, sent) = (id_of("drafts")?, id_of("sent")?);
        let identity = self.identity(user, pass, account, &c.from).await?;
        let mut email_set = json!({"accountId": account, "create": {"n": email_json(c, &drafts)}});
        if let Some(old) = draft { email_set["destroy"] = json!([old]); }
        let r = self.call_using(user, pass, &USING_SUBMIT, json!([
            ["Email/set", email_set, "e"],
            ["EmailSubmission/set", {"accountId": account,
                "create": {"s": {"identityId": identity, "emailId": "#n"}},
                "onSuccessUpdateEmail": {"#s": {
                    format!("mailboxIds/{drafts}"): null, format!("mailboxIds/{sent}"): true,
                    "keywords/$draft": null, "keywords/$seen": true}}}, "x"]
        ])).await?;
        if r[0]["created"]["n"].is_null() {
            return Err(MailError::Protocol(format!("message not created: {}", r[0]["notCreated"])));
        }
        if r[1]["created"]["s"].is_null() {
            return Err(MailError::Rejected(r[1]["notCreated"]["s"]["description"].as_str()
                .or(r[1]["notCreated"]["s"]["type"].as_str()).unwrap_or("submission refused").to_string()));
        }
        Ok(())
    }

    /// The sending identity for `email`, created if the account has none.
    async fn identity(&self, user: &str, pass: &str, account: &str, email: &str) -> Result<String, MailError> {
        let r = self.call_using(user, pass, &USING_SUBMIT, json!([["Identity/get", {"accountId": account, "ids": null}, "i"]])).await?;
        if let Some(id) = r[0]["list"].as_array().into_iter().flatten()
            .find(|i| i["email"].as_str().is_some_and(|e| e.eq_ignore_ascii_case(email)))
            .and_then(|i| i["id"].as_str()) {
            return Ok(id.to_string());
        }
        let r = self.call_using(user, pass, &USING_SUBMIT, json!([["Identity/set", {"accountId": account,
            "create": {"i": {"email": email, "name": ""}}}, "s"]])).await?;
        r[0]["created"]["i"]["id"].as_str().map(str::to_string)
            .ok_or_else(|| MailError::Protocol(format!("identity not created: {}", r[0]["notCreated"])))
    }

    /// Like `get_message`, but NotFound unless the To: header contains `to` exactly.
    pub async fn get_message_to(
        &self, user: &str, pass: &str, account: &str, id: &str, to: &str,
    ) -> Result<MessageDetail, MailError> {
        let r = self
            .call(user, pass, json!([["Email/get", {"accountId": account, "ids": [id], "properties": ["to"]}, "g"]]))
            .await?;
        let e = r[0]["list"].get(0).ok_or(MailError::NotFound)?;
        if !has_rcpt(e, to) {
            return Err(MailError::NotFound);
        }
        self.get_message(user, pass, account, id).await
    }

    pub async fn get_message(
        &self, user: &str, pass: &str, account: &str, id: &str,
    ) -> Result<MessageDetail, MailError> {
        let mut props: Vec<&str> = SUMMARY_PROPS.to_vec();
        props.extend(["textBody", "htmlBody", "bodyValues", "cc", "messageId", "references"]);
        let r = self
            .call(user, pass, json!([["Email/get", {"accountId": account, "ids": [id],
                "properties": props, "fetchTextBodyValues": true, "fetchHTMLBodyValues": true,
                "maxBodyValueBytes": 1_000_000}, "g"]]))
            .await?;
        let e = r[0]["list"].get(0).ok_or(MailError::NotFound)?;
        let body = |kind: &str| -> Option<String> {
            let parts = e[kind].as_array()?;
            let s: String = parts
                .iter()
                .filter_map(|p| e["bodyValues"][p["partId"].as_str()?]["value"].as_str())
                .collect::<Vec<_>>()
                .join("\n");
            (!s.is_empty()).then_some(s)
        };
        let html = e["htmlBody"]
            .as_array()
            .is_some_and(|p| p.iter().any(|p| p["type"] == "text/html"))
            .then(|| body("htmlBody"))
            .flatten();
        let strings = |k: &str| e[k].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect::<Vec<_>>()).unwrap_or_default();
        Ok(MessageDetail {
            summary: summary(e), text_body: body("textBody"), html_body: html,
            cc: e["cc"].as_array().map(|a| a.iter().map(addr).collect()).unwrap_or_default(),
            message_id: strings("messageId").into_iter().next(),
            references: strings("references"),
        })
    }
}

async fn check(resp: reqwest::Response) -> Result<Value, MailError> {
    match resp.status().as_u16() {
        401 | 403 => Err(MailError::Auth),
        s if s >= 400 => Err(MailError::Unavailable(format!("HTTP {s}"))),
        _ => resp.json().await.map_err(|e| MailError::Protocol(e.to_string())),
    }
}

fn has_rcpt(e: &Value, to: &str) -> bool {
    e["to"].as_array().is_some_and(|a| a.iter().any(|t| t["email"].as_str().is_some_and(|x| x.eq_ignore_ascii_case(to))))
}

fn addr(v: &Value) -> String {
    match (v["name"].as_str(), v["email"].as_str()) {
        (Some(n), Some(e)) if !n.is_empty() => format!("{n} <{e}>"),
        (_, Some(e)) => e.to_string(),
        _ => String::new(),
    }
}

fn summary(e: &Value) -> MessageSummary {
    let addrs = |k: &str| e[k].as_array().map(|a| a.iter().map(addr).collect::<Vec<String>>()).unwrap_or_default();
    MessageSummary {
        id: e["id"].as_str().unwrap_or_default().to_string(),
        from: addrs("from").into_iter().next().unwrap_or_default(),
        to: addrs("to"),
        subject: e["subject"].as_str().unwrap_or_default().to_string(),
        preview: e["preview"].as_str().unwrap_or_default().to_string(),
        received_at: e["receivedAt"].as_str().unwrap_or_default().to_string(),
        is_read: e["keywords"]["$seen"] == true,
        is_draft: e["keywords"]["$draft"] == true,
        has_attachment: e["hasAttachment"] == true,
        size_bytes: e["size"].as_u64().unwrap_or(0),
    }
}

fn updated(r: &Value, id: &str) -> Result<(), MailError> {
    if r["updated"].as_object().is_some_and(|o| o.contains_key(id)) {
        return Ok(());
    }
    match r["notUpdated"][id]["type"].as_str() {
        Some("notFound") => Err(MailError::NotFound),
        _ => Err(MailError::Protocol(format!("not updated: {}", r["notUpdated"]))),
    }
}

/// JMAP Email/create object for a plain-text message in `mailbox`, flagged as a draft.
fn email_json(c: &Compose, mailbox: &str) -> Value {
    let addrs = |v: &[String]| v.iter().map(|e| json!({"email": e})).collect::<Vec<_>>();
    let mut e = json!({
        "mailboxIds": {mailbox: true},
        "keywords": {"$draft": true, "$seen": true},
        "from": [{"email": c.from}],
        "to": addrs(&c.to),
        "subject": c.subject,
        "bodyValues": {"b": {"value": c.text}},
        "textBody": [{"partId": "b", "type": "text/plain"}],
    });
    if !c.cc.is_empty() { e["cc"] = json!(addrs(&c.cc)); }
    if !c.bcc.is_empty() { e["bcc"] = json!(addrs(&c.bcc)); }
    if let Some(r) = &c.in_reply_to { e["inReplyTo"] = json!([r]); }
    if !c.references.is_empty() { e["references"] = json!(c.references); }
    e
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_json_shapes_a_plain_text_draft() {
        let c = Compose { from: "a@x.id".into(), to: vec!["b@x.id".into()], subject: "Hi".into(), text: "Body".into(),
            in_reply_to: Some("m1@x".into()), references: vec!["m0@x".into(), "m1@x".into()], ..Default::default() };
        let e = email_json(&c, "D");
        assert_eq!(e["mailboxIds"]["D"], true);
        assert_eq!(e["keywords"]["$draft"], true);
        assert_eq!(e["to"][0]["email"], "b@x.id");
        assert!(e.get("cc").is_none() && e.get("bcc").is_none());
        assert_eq!(e["bodyValues"]["b"]["value"], "Body");
        assert_eq!(e["inReplyTo"][0], "m1@x");
        assert_eq!(e["references"][1], "m1@x");
    }

    #[test]
    fn updated_maps_results() {
        assert!(updated(&json!({"updated": {"x": null}}), "x").is_ok());
        assert!(matches!(updated(&json!({"notUpdated": {"x": {"type": "notFound"}}}), "x"), Err(MailError::NotFound)));
    }
}
