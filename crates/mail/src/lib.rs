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
    /// Every attachment JMAP reports, plus any `disposition: attachment` leaf
    /// found by walking `bodyStructure`. Download uses `part_id`, never a
    /// caller-supplied blob id, so a blob can only be fetched for a message
    /// the caller already opened.
    pub attachments: Vec<Attachment>,
    /// Flat walk of `bodyStructure` (multipart containers omitted), in MIME
    /// order. This is what lets the UI say *why* a message looks empty
    /// instead of rendering a blank pane.
    pub mime_parts: Vec<MimePart>,
    /// Where the displayed body came from: `jmap` (textBody/htmlBody),
    /// `structure` (a text part JMAP did not classify as a body part), or
    /// `none` (no displayable text part at all, e.g. attachment-only mail).
    pub body_source: String,
    /// JMAP blob id of the whole message, for View Original / Raw Source.
    pub raw_blob_id: Option<String>,
}

/// One attachment (or inline file part) of a message.
#[derive(Debug, Clone, Serialize)]
pub struct Attachment {
    pub part_id: String,
    pub blob_id: String,
    pub name: String,
    pub content_type: String,
    pub size: u64,
    pub disposition: Option<String>,
}

/// One leaf of the MIME tree, in order.
#[derive(Debug, Clone, Serialize)]
pub struct MimePart {
    pub part_id: String,
    pub content_type: String,
    pub name: Option<String>,
    pub disposition: Option<String>,
    pub size: u64,
}

/// Caps for the raw / attachment endpoints. A raw message over this is
/// refused with 413 rather than buffered; previews are truncated at
/// `MAX_PREVIEW_TEXT` instead of refused, so a big DMARC report still
/// shows its first records.
pub const MAX_RAW_BYTES: u64 = 5_000_000;
pub const MAX_ATTACHMENT_BYTES: u64 = 25_000_000;
pub const MAX_PREVIEW_BYTES: u64 = 5_000_000;
pub const MAX_PREVIEW_TEXT: usize = 2_000_000;

/// A decompressed / extracted text preview of one attachment.
#[derive(Debug, Clone)]
pub struct ExtractedText {
    pub inner_name: String,
    pub text: String,
    pub truncated: bool,
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
    ("inbox", "Inbox"),
    ("sent", "Sent"),
    ("drafts", "Drafts"),
    ("archive", "Archive"),
    ("trash", "Trash"),
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
const USING_SUBMIT: [&str; 3] = [
    "urn:ietf:params:jmap:core",
    "urn:ietf:params:jmap:mail",
    "urn:ietf:params:jmap:submission",
];
const SUMMARY_PROPS: [&str; 8] = [
    "from",
    "to",
    "subject",
    "preview",
    "receivedAt",
    "keywords",
    "size",
    "hasAttachment",
];

/// JMAP `Email/query` filter for one folder, optionally narrowed by a
/// full-text `text` condition.
///
/// Stalwart answers the `text` condition from its full-text search index
/// (default indexed email fields: attachment, bcc, body, cc, from, subject,
/// to). Indexing runs in the background, so a message received seconds ago
/// may not match yet; the web UI keeps its local `matches()` filter as a
/// fallback for the list it already loaded.
fn folder_filter(mailbox_id: &str, query: Option<&str>) -> Value {
    let in_mailbox = json!({ "inMailbox": mailbox_id });
    match query.map(str::trim).filter(|q| !q.is_empty()) {
        Some(q) => json!({
            "operator": "AND",
            "conditions": [in_mailbox, { "text": q }],
        }),
        None => in_mailbox,
    }
}

/// Paging and search options for listing one folder.
#[derive(Debug, Clone, Copy, Default)]
pub struct ListPage<'a> {
    pub limit: u32,
    pub position: u32,
    /// Full-text search terms; `None` lists the whole folder.
    pub query: Option<&'a str>,
}

impl JmapClient {
    pub fn new(base_url: String) -> Self {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .expect("reqwest client");
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            http,
        }
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

    async fn call_using(
        &self,
        user: &str,
        pass: &str,
        using: &[&str],
        calls: Value,
    ) -> Result<Vec<Value>, MailError> {
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

    /// Creates a Stalwart app password for the authenticated user; returns (id, secret).
    /// The secret is only ever returned by this call.
    pub async fn create_app_password(
        &self,
        user: &str,
        pass: &str,
        description: &str,
        expires_at: &str,
    ) -> Result<(String, String), MailError> {
        let r = self
            .call_using(user, pass, &USING_STALWART, json!([["x:AppPassword/set", {"create": {"n": {
                "description": description, "permissions": {"@type": "Inherit"}, "expiresAt": expires_at}}}, "c"]]))
            .await?;
        let c = &r[0]["created"]["n"];
        match (c["id"].as_str(), c["secret"].as_str()) {
            (Some(id), Some(secret)) => Ok((id.to_string(), secret.to_string())),
            _ => Err(MailError::Protocol(format!(
                "app password not created: {}",
                r[0]["notCreated"]
            ))),
        }
    }

    /// Revokes an app password (an app password may revoke itself).
    pub async fn destroy_app_password(
        &self,
        user: &str,
        pass: &str,
        id: &str,
    ) -> Result<(), MailError> {
        self.call_using(
            user,
            pass,
            &USING_STALWART,
            json!([["x:AppPassword/set", {"destroy": [id]}, "c"]]),
        )
        .await?;
        Ok(())
    }

    /// Destroys messages whose To: contains exactly `to` (temp inbox purge). Returns count destroyed.
    pub async fn destroy_to(
        &self,
        user: &str,
        pass: &str,
        account: &str,
        to: &str,
    ) -> Result<usize, MailError> {
        let r = self
            .call(user, pass, json!([
                ["Email/query", {"accountId": account, "filter": {"to": to}, "limit": 500}, "q"],
                ["Email/get", {"accountId": account,
                    "#ids": {"resultOf": "q", "name": "Email/query", "path": "/ids"}, "properties": ["to"]}, "g"]
            ]))
            .await?;
        // Full-text `to` filter can over-match; only destroy exact recipient hits.
        let ids: Vec<&str> = r[1]["list"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|e| has_rcpt(e, to))
            .filter_map(|e| e["id"].as_str())
            .collect();
        self.destroy(user, pass, account, &ids).await
    }

    /// Destroys messages received before `before` (UTC RFC3339), up to 500 per call.
    pub async fn destroy_before(
        &self,
        user: &str,
        pass: &str,
        account: &str,
        before: &str,
    ) -> Result<usize, MailError> {
        let r = self
            .call(user, pass, json!([["Email/query", {"accountId": account, "filter": {"before": before}, "limit": 500}, "q"]]))
            .await?;
        let ids: Vec<&str> = r[0]["ids"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        self.destroy(user, pass, account, &ids).await
    }

    async fn destroy(
        &self,
        user: &str,
        pass: &str,
        account: &str,
        ids: &[&str],
    ) -> Result<usize, MailError> {
        if ids.is_empty() {
            return Ok(0);
        }
        let r = self
            .call(
                user,
                pass,
                json!([["Email/set", {"accountId": account, "destroy": ids}, "d"]]),
            )
            .await?;
        Ok(r[0]["destroyed"].as_array().map_or(0, Vec::len))
    }

    pub async fn list_inbox(
        &self,
        user: &str,
        pass: &str,
        account: &str,
        limit: u32,
        position: u32,
    ) -> Result<(Vec<MessageSummary>, u64), MailError> {
        self.list_folder(
            user,
            pass,
            account,
            "inbox",
            ListPage {
                limit,
                position,
                query: None,
            },
        )
        .await
    }

    /// Lists a standard folder by role (inbox, sent, drafts, archive, trash), newest first.
    /// With `page.query`, only messages matching the full-text search
    /// (subject, sender, recipients, body, attachments) are returned;
    /// `total` is then the number of matches, so pagination works over the
    /// search results.
    pub async fn list_folder(
        &self,
        user: &str,
        pass: &str,
        account: &str,
        role: &str,
        page: ListPage<'_>,
    ) -> Result<(Vec<MessageSummary>, u64), MailError> {
        let folders = self.folders(user, pass, account).await?;
        let inbox = folders
            .iter()
            .find(|f| f.role == role)
            .ok_or(MailError::NotFound)?
            .id
            .clone();
        let r = self
            .call(
                user,
                pass,
                json!([
                    ["Email/query", {"accountId": account, "filter": folder_filter(&inbox, page.query),
                        "sort": [{"property": "receivedAt", "isAscending": false}],
                        "position": page.position, "limit": page.limit, "calculateTotal": true}, "q"],
                    ["Email/get", {"accountId": account,
                        "#ids": {"resultOf": "q", "name": "Email/query", "path": "/ids"},
                        "properties": SUMMARY_PROPS}, "g"]
                ]),
            )
            .await?;
        let total = r[0]["total"].as_u64().unwrap_or(0);
        let list = r[1]["list"].as_array().cloned().unwrap_or_default();
        Ok((list.iter().map(summary).collect(), total))
    }

    /// Messages in `account` whose To: header contains exactly `to`, received after `after` (UTC RFC3339).
    // ponytail: matches the To: header only, so Bcc'd mail is invisible; switch to the
    // envelope recipient (Delivered-To / sieve tagging) if that matters.
    pub async fn list_to(
        &self,
        user: &str,
        pass: &str,
        account: &str,
        to: &str,
        after: &str,
        limit: u32,
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
        Ok(list
            .iter()
            .filter(|e| has_rcpt(e, to))
            .map(summary)
            .collect())
    }

    /// Standard folders with counts. Missing ones (e.g. Archive) are created on first use.
    pub async fn folders(
        &self,
        user: &str,
        pass: &str,
        account: &str,
    ) -> Result<Vec<Folder>, MailError> {
        let get = json!([["Mailbox/get", {"accountId": account, "ids": null,
            "properties": ["name", "role", "totalEmails", "unreadEmails"]}, "m"]]);
        let mut list = self.call(user, pass, get.clone()).await?[0]["list"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let has = |list: &[Value], role: &str| list.iter().any(|m| m["role"] == role);
        let mut create = serde_json::Map::new();
        let mut update = serde_json::Map::new();
        for (role, name) in FOLDER_ROLES {
            if has(&list, role) {
                continue;
            }
            // Reuse a same-named folder without a role instead of creating a duplicate.
            match list.iter().find(|m| {
                m["role"].is_null()
                    && m["name"]
                        .as_str()
                        .is_some_and(|n| n.eq_ignore_ascii_case(name))
            }) {
                Some(m) => {
                    update.insert(
                        m["id"].as_str().unwrap_or_default().to_string(),
                        json!({"role": role}),
                    );
                }
                None => {
                    create.insert(role.to_string(), json!({"name": name, "role": role}));
                }
            }
        }
        if !create.is_empty() || !update.is_empty() {
            let r = self.call(user, pass, json!([["Mailbox/set", {"accountId": account, "create": create, "update": update}, "s"]])).await?;
            if let Some(err) = r[0]["notCreated"].as_object().filter(|o| !o.is_empty()) {
                return Err(MailError::Protocol(format!(
                    "folders not created: {}",
                    Value::Object(err.clone())
                )));
            }
            list = self.call(user, pass, get).await?[0]["list"]
                .as_array()
                .cloned()
                .unwrap_or_default();
        }
        Ok(FOLDER_ROLES
            .iter()
            .filter_map(|(role, _)| {
                let m = list.iter().find(|m| m["role"] == *role)?;
                Some(Folder {
                    id: m["id"].as_str()?.to_string(),
                    role: role.to_string(),
                    name: m["name"].as_str().unwrap_or(role).to_string(),
                    total: m["totalEmails"].as_u64().unwrap_or(0),
                    unread: m["unreadEmails"].as_u64().unwrap_or(0),
                })
            })
            .collect())
    }

    async fn folder_id(
        &self,
        user: &str,
        pass: &str,
        account: &str,
        role: &str,
    ) -> Result<String, MailError> {
        Ok(self
            .folders(user, pass, account)
            .await?
            .into_iter()
            .find(|f| f.role == role)
            .ok_or(MailError::NotFound)?
            .id)
    }

    /// Roles of the folders a message is currently in.
    pub async fn message_roles(
        &self,
        user: &str,
        pass: &str,
        account: &str,
        id: &str,
    ) -> Result<Vec<String>, MailError> {
        let folders = self.folders(user, pass, account).await?;
        let r = self.call(user, pass, json!([["Email/get", {"accountId": account, "ids": [id], "properties": ["mailboxIds"]}, "g"]])).await?;
        let e = r[0]["list"].get(0).ok_or(MailError::NotFound)?;
        let ids = e["mailboxIds"].as_object().cloned().unwrap_or_default();
        Ok(folders
            .into_iter()
            .filter(|f| ids.contains_key(&f.id))
            .map(|f| f.role)
            .collect())
    }

    /// Moves a message into exactly one standard folder.
    pub async fn move_to(
        &self,
        user: &str,
        pass: &str,
        account: &str,
        id: &str,
        role: &str,
    ) -> Result<(), MailError> {
        let target = self.folder_id(user, pass, account, role).await?;
        let r = self
            .call(
                user,
                pass,
                json!([["Email/set", {"accountId": account,
            "update": {id: {"mailboxIds": {target: true}}}}, "u"]]),
            )
            .await?;
        updated(&r[0], id)
    }

    pub async fn set_seen(
        &self,
        user: &str,
        pass: &str,
        account: &str,
        id: &str,
        seen: bool,
    ) -> Result<(), MailError> {
        let patch = if seen { json!(true) } else { Value::Null };
        let r = self
            .call(
                user,
                pass,
                json!([["Email/set", {"accountId": account,
            "update": {id: {"keywords/$seen": patch}}}, "u"]]),
            )
            .await?;
        updated(&r[0], id)
    }

    /// Permanently deletes one message.
    pub async fn destroy_one(
        &self,
        user: &str,
        pass: &str,
        account: &str,
        id: &str,
    ) -> Result<(), MailError> {
        match self.destroy(user, pass, account, &[id]).await? {
            0 => Err(MailError::NotFound),
            _ => Ok(()),
        }
    }

    /// Saves a draft (replacing `replace` if given) and returns the new draft id.
    pub async fn save_draft(
        &self,
        user: &str,
        pass: &str,
        account: &str,
        c: &Compose,
        replace: Option<&str>,
    ) -> Result<String, MailError> {
        let drafts = self.folder_id(user, pass, account, "drafts").await?;
        let mut set = json!({"accountId": account, "create": {"n": email_json(c, &drafts)}});
        if let Some(old) = replace {
            set["destroy"] = json!([old]);
        }
        let r = self
            .call(user, pass, json!([["Email/set", set, "s"]]))
            .await?;
        r[0]["created"]["n"]["id"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| MailError::Protocol(format!("draft not saved: {}", r[0]["notCreated"])))
    }

    /// Sends a message as the signed-in user; it lands in Sent. Deletes `draft` if given.
    pub async fn send(
        &self,
        user: &str,
        pass: &str,
        account: &str,
        c: &Compose,
        draft: Option<&str>,
    ) -> Result<(), MailError> {
        let folders = self.folders(user, pass, account).await?;
        let id_of = |role: &str| {
            folders
                .iter()
                .find(|f| f.role == role)
                .map(|f| f.id.clone())
                .ok_or(MailError::NotFound)
        };
        let (drafts, sent) = (id_of("drafts")?, id_of("sent")?);
        let identity = self.identity(user, pass, account, &c.from).await?;
        let mut email_set = json!({"accountId": account, "create": {"n": email_json(c, &drafts)}});
        if let Some(old) = draft {
            email_set["destroy"] = json!([old]);
        }
        let r = self.call_using(user, pass, &USING_SUBMIT, json!([
            ["Email/set", email_set, "e"],
            ["EmailSubmission/set", {"accountId": account,
                "create": {"s": {"identityId": identity, "emailId": "#n"}},
                "onSuccessUpdateEmail": {"#s": {
                    format!("mailboxIds/{drafts}"): null, format!("mailboxIds/{sent}"): true,
                    "keywords/$draft": null, "keywords/$seen": true}}}, "x"]
        ])).await?;
        if r[0]["created"]["n"].is_null() {
            return Err(MailError::Protocol(format!(
                "message not created: {}",
                r[0]["notCreated"]
            )));
        }
        if r[1]["created"]["s"].is_null() {
            return Err(MailError::Rejected(
                r[1]["notCreated"]["s"]["description"]
                    .as_str()
                    .or(r[1]["notCreated"]["s"]["type"].as_str())
                    .unwrap_or("submission refused")
                    .to_string(),
            ));
        }
        Ok(())
    }

    /// Files a copy of a message sent elsewhere (e.g. via the HTTP relay) into Sent, removing `draft`.
    pub async fn store_sent(
        &self,
        user: &str,
        pass: &str,
        account: &str,
        c: &Compose,
        draft: Option<&str>,
    ) -> Result<(), MailError> {
        let sent = self.folder_id(user, pass, account, "sent").await?;
        let mut e = email_json(c, &sent);
        e["keywords"] = json!({"$seen": true});
        let mut set = json!({"accountId": account, "create": {"n": e}});
        if let Some(old) = draft {
            set["destroy"] = json!([old]);
        }
        let r = self
            .call(user, pass, json!([["Email/set", set, "s"]]))
            .await?;
        if r[0]["created"]["n"].is_null() {
            return Err(MailError::Protocol(format!(
                "sent copy not stored: {}",
                r[0]["notCreated"]
            )));
        }
        Ok(())
    }

    /// The sending identity for `email`, created if the account has none.
    async fn identity(
        &self,
        user: &str,
        pass: &str,
        account: &str,
        email: &str,
    ) -> Result<String, MailError> {
        let r = self
            .call_using(
                user,
                pass,
                &USING_SUBMIT,
                json!([["Identity/get", {"accountId": account, "ids": null}, "i"]]),
            )
            .await?;
        if let Some(id) = r[0]["list"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|i| {
                i["email"]
                    .as_str()
                    .is_some_and(|e| e.eq_ignore_ascii_case(email))
            })
            .and_then(|i| i["id"].as_str())
        {
            return Ok(id.to_string());
        }
        let r = self
            .call_using(
                user,
                pass,
                &USING_SUBMIT,
                json!([["Identity/set", {"accountId": account,
            "create": {"i": {"email": email, "name": ""}}}, "s"]]),
            )
            .await?;
        r[0]["created"]["i"]["id"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| {
                MailError::Protocol(format!("identity not created: {}", r[0]["notCreated"]))
            })
    }

    /// Like `get_message`, but NotFound unless the To: header contains `to` exactly.
    pub async fn get_message_to(
        &self,
        user: &str,
        pass: &str,
        account: &str,
        id: &str,
        to: &str,
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
        &self,
        user: &str,
        pass: &str,
        account: &str,
        id: &str,
    ) -> Result<MessageDetail, MailError> {
        let mut props: Vec<&str> = SUMMARY_PROPS.to_vec();
        props.extend([
            "textBody",
            "htmlBody",
            "bodyValues",
            "bodyStructure",
            "attachments",
            "blobId",
            "cc",
            "messageId",
            "references",
        ]);
        let r = self
            .call(
                user,
                pass,
                json!([["Email/get", {"accountId": account, "ids": [id],
                "properties": props, "fetchTextBodyValues": true, "fetchHTMLBodyValues": true,
                // Classified body parts alone are not enough: a text part nested
                // under multipart/mixed or multipart/report (the usual DMARC
                // shape) may not appear in textBody/htmlBody at all, so also
                // fetch values for every part and walk bodyStructure below.
                "fetchAllBodyValues": true,
                "maxBodyValueBytes": 1_000_000}, "g"]]),
            )
            .await?;
        let e = r[0]["list"].get(0).ok_or(MailError::NotFound)?;
        Ok(detail_from_email(e))
    }

    /// Streams one JMAP blob (a whole message, or one attachment part) with
    /// the caller's own credentials. Callers must have resolved `blob_id`
    /// from a message they already opened; the id is never taken from an
    /// untrusted request body on its own (see the API layer, which looks
    /// the attachment up in `MessageDetail` first).
    pub async fn download_blob(
        &self,
        user: &str,
        pass: &str,
        account: &str,
        blob_id: &str,
        name: &str,
    ) -> Result<reqwest::Response, MailError> {
        // JMAP download URL, mirroring the {base}/jmap/ API prefix used by
        // `call_using` (Stalwart advertises a public hostname in its session
        // downloadUrl, so the loopback prefix is used here instead).
        let url = format!(
            "{}/jmap/download/{}/{}/{}",
            self.base_url, account, blob_id, name
        );
        let resp = self
            .http
            .get(url)
            .basic_auth(user, Some(pass))
            .send()
            .await
            .map_err(|e| MailError::Unavailable(e.to_string()))?;
        match resp.status().as_u16() {
            401 | 403 => Err(MailError::Auth),
            404 => Err(MailError::NotFound),
            s if s >= 400 => Err(MailError::Unavailable(format!("HTTP {s}"))),
            _ => Ok(resp),
        }
    }

    /// Whole blob as bytes, capped by the caller (the API layer checks the
    /// known message/attachment size before calling this).
    pub async fn download_blob_bytes(
        &self,
        user: &str,
        pass: &str,
        account: &str,
        blob_id: &str,
        name: &str,
    ) -> Result<Vec<u8>, MailError> {
        let resp = self
            .download_blob(user, pass, account, blob_id, name)
            .await?;
        resp.bytes()
            .await
            .map(|b| b.to_vec())
            .map_err(|e| MailError::Unavailable(e.to_string()))
    }
}

/// Builds a `MessageDetail` from one JMAP Email object. Pure, so the DMARC
/// shape can be tested from a fixture without a mail server.
fn detail_from_email(e: &Value) -> MessageDetail {
    let body_value = |part_id: &str| -> Option<String> {
        let v = e["bodyValues"][part_id]["value"].as_str()?;
        (!v.is_empty()).then_some(v.to_string())
    };
    let classified = |kind: &str| -> Option<String> {
        let parts = e[kind].as_array()?;
        let s: String = parts
            .iter()
            .filter_map(|p| body_value(p["partId"].as_str()?))
            .collect::<Vec<_>>()
            .join("\n");
        (!s.is_empty()).then_some(s)
    };
    let mut text_body = classified("textBody");
    let mut html_body = {
        let has_html = e["htmlBody"]
            .as_array()
            .is_some_and(|p| p.iter().any(|p| p["type"] == "text/html"));
        if has_html {
            classified("htmlBody")
        } else {
            None
        }
    };
    let mime_parts = walk_structure(&e["bodyStructure"]);
    let mut body_source = if html_body.is_some() || text_body.is_some() {
        "jmap".to_string()
    } else {
        "none".to_string()
    };
    // Fallback: walk the MIME tree in order for a displayable text part
    // that JMAP did not classify as a body part. Attachment parts are
    // skipped on purpose: a .xml.gz attachment must not become the body.
    if text_body.is_none() && html_body.is_none() {
        for part in &mime_parts {
            if part.disposition.as_deref() == Some("attachment") {
                continue;
            }
            if part.content_type == "text/html" {
                if let Some(v) = body_value(&part.part_id) {
                    html_body = Some(v);
                    body_source = "structure".to_string();
                    break;
                }
            }
        }
    }
    if text_body.is_none() {
        for part in &mime_parts {
            if part.disposition.as_deref() == Some("attachment") {
                continue;
            }
            if part.content_type == "text/plain" {
                if let Some(v) = body_value(&part.part_id) {
                    text_body = Some(v);
                    body_source = "structure".to_string();
                    break;
                }
            }
        }
    }
    if text_body.is_none() && html_body.is_none() {
        for part in &mime_parts {
            if part.disposition.as_deref() == Some("attachment") {
                continue;
            }
            if part.content_type.starts_with("text/") {
                if let Some(v) = body_value(&part.part_id) {
                    text_body = Some(v);
                    body_source = "structure".to_string();
                    break;
                }
            }
        }
    }
    let strings = |k: &str| {
        e[k].as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    MessageDetail {
        summary: summary(e),
        text_body,
        html_body,
        cc: e["cc"]
            .as_array()
            .map(|a| a.iter().map(addr).collect())
            .unwrap_or_default(),
        message_id: strings("messageId").into_iter().next(),
        references: strings("references"),
        attachments: attachments_from(e, &mime_parts),
        mime_parts,
        body_source,
        raw_blob_id: e["blobId"].as_str().map(str::to_string),
    }
}

/// Flattens a JMAP `bodyStructure` tree into leaves, in MIME order.
/// Multipart containers have `subParts` and no usable body of their own.
fn walk_structure(v: &Value) -> Vec<MimePart> {
    let mut out = Vec::new();
    walk_structure_into(v, &mut out);
    out
}

fn walk_structure_into(v: &Value, out: &mut Vec<MimePart>) {
    if let Some(subs) = v["subParts"].as_array() {
        for s in subs {
            walk_structure_into(s, out);
        }
        return;
    }
    if v["partId"].as_str().is_none() && v["type"].as_str().is_none() {
        return;
    }
    out.push(MimePart {
        part_id: v["partId"].as_str().unwrap_or_default().to_string(),
        content_type: v["type"]
            .as_str()
            .unwrap_or("application/octet-stream")
            .to_string(),
        name: v["name"].as_str().map(str::to_string),
        disposition: v["disposition"].as_str().map(str::to_string),
        size: v["size"].as_u64().unwrap_or(0),
    });
}

/// Attachment list: JMAP's `attachments` first, then any leaf with
/// `disposition: attachment` that JMAP did not list (defensive, same shape).
fn attachments_from(e: &Value, parts: &[MimePart]) -> Vec<Attachment> {
    let mut out: Vec<Attachment> = Vec::new();
    if let Some(list) = e["attachments"].as_array() {
        for a in list {
            out.push(Attachment {
                part_id: a["partId"].as_str().unwrap_or_default().to_string(),
                blob_id: a["blobId"].as_str().unwrap_or_default().to_string(),
                name: a["name"].as_str().unwrap_or("attachment").to_string(),
                content_type: a["type"]
                    .as_str()
                    .unwrap_or("application/octet-stream")
                    .to_string(),
                size: a["size"].as_u64().unwrap_or(0),
                disposition: a["disposition"].as_str().map(str::to_string),
            });
        }
    }
    for p in parts {
        if p.disposition.as_deref() != Some("attachment") {
            continue;
        }
        if out.iter().any(|a| a.part_id == p.part_id) {
            continue;
        }
        out.push(Attachment {
            part_id: p.part_id.clone(),
            blob_id: String::new(),
            name: p.name.clone().unwrap_or_else(|| "attachment".to_string()),
            content_type: p.content_type.clone(),
            size: p.size,
            disposition: p.disposition.clone(),
        });
    }
    out.retain(|a| !a.part_id.is_empty() && !a.blob_id.is_empty());
    out
}

fn read_limited<R: std::io::Read>(r: R) -> std::io::Result<(Vec<u8>, bool)> {
    let mut buf = Vec::new();
    let mut limited = r.take((MAX_PREVIEW_TEXT + 1) as u64);
    std::io::Read::read_to_end(&mut limited, &mut buf)?;
    let truncated = buf.len() > MAX_PREVIEW_TEXT;
    if truncated {
        buf.truncate(MAX_PREVIEW_TEXT);
    }
    Ok((buf, truncated))
}

/// Extracts previewable text from an attachment: plain XML, a gzip stream
/// (`.xml.gz`, the usual DMARC shape), or a zip archive (Google also ships
/// `.zip`; the first `.xml` entry wins). Anything else is an error, so the
/// UI can fall back to "download" instead of showing binary garbage.
/// Decompression is capped at `MAX_PREVIEW_TEXT`; oversized output is
/// truncated, never buffered without bound.
pub fn extract_text_attachment(
    name: &str,
    content_type: &str,
    bytes: &[u8],
) -> Result<ExtractedText, MailError> {
    let lower_name = name.to_lowercase();
    let lower_type = content_type.to_lowercase();
    let is_gzip =
        lower_name.ends_with(".gz") || lower_type.contains("gzip") || lower_type.contains("x-gzip");
    let is_zip = lower_name.ends_with(".zip") || lower_type.contains("zip");
    let is_xml = lower_name.ends_with(".xml") || lower_type.contains("xml");
    if is_gzip {
        use flate2::read::GzDecoder;
        let (raw, truncated) = read_limited(GzDecoder::new(bytes))
            .map_err(|e| MailError::Protocol(format!("gzip decode failed: {e}")))?;
        let inner = lower_name
            .strip_suffix(".gz")
            .unwrap_or(&lower_name)
            .to_string();
        return Ok(ExtractedText {
            inner_name: inner,
            text: String::from_utf8_lossy(&raw).into_owned(),
            truncated,
        });
    }
    if is_zip {
        use std::io::Cursor;
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
            .map_err(|e| MailError::Protocol(format!("zip open failed: {e}")))?;
        let mut pick: Option<usize> = None;
        for i in 0..archive.len() {
            let file = archive
                .by_index(i)
                .map_err(|e| MailError::Protocol(e.to_string()))?;
            if file.is_dir() {
                continue;
            }
            let fname = file.name().to_string();
            if fname.to_lowercase().ends_with(".xml") {
                pick = Some(i);
                break;
            }
            if pick.is_none() {
                pick = Some(i);
            }
        }
        let i = pick.ok_or_else(|| MailError::Protocol("empty archive".into()))?;
        let mut file = archive
            .by_index(i)
            .map_err(|e| MailError::Protocol(e.to_string()))?;
        let inner_name = file.name().to_string();
        let (raw, truncated) = read_limited(&mut file)
            .map_err(|e| MailError::Protocol(format!("zip entry read failed: {e}")))?;
        return Ok(ExtractedText {
            inner_name,
            text: String::from_utf8_lossy(&raw).into_owned(),
            truncated,
        });
    }
    if is_xml {
        let (raw, truncated) = if bytes.len() > MAX_PREVIEW_TEXT {
            (bytes[..MAX_PREVIEW_TEXT].to_vec(), true)
        } else {
            (bytes.to_vec(), false)
        };
        return Ok(ExtractedText {
            inner_name: name.to_string(),
            text: String::from_utf8_lossy(&raw).into_owned(),
            truncated,
        });
    }
    Err(MailError::Protocol("not_previewable".into()))
}

async fn check(resp: reqwest::Response) -> Result<Value, MailError> {
    match resp.status().as_u16() {
        401 | 403 => Err(MailError::Auth),
        s if s >= 400 => Err(MailError::Unavailable(format!("HTTP {s}"))),
        _ => resp
            .json()
            .await
            .map_err(|e| MailError::Protocol(e.to_string())),
    }
}

fn has_rcpt(e: &Value, to: &str) -> bool {
    e["to"].as_array().is_some_and(|a| {
        a.iter().any(|t| {
            t["email"]
                .as_str()
                .is_some_and(|x| x.eq_ignore_ascii_case(to))
        })
    })
}

fn addr(v: &Value) -> String {
    match (v["name"].as_str(), v["email"].as_str()) {
        (Some(n), Some(e)) if !n.is_empty() => format!("{n} <{e}>"),
        (_, Some(e)) => e.to_string(),
        _ => String::new(),
    }
}

fn summary(e: &Value) -> MessageSummary {
    let addrs = |k: &str| {
        e[k].as_array()
            .map(|a| a.iter().map(addr).collect::<Vec<String>>())
            .unwrap_or_default()
    };
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
        _ => Err(MailError::Protocol(format!(
            "not updated: {}",
            r["notUpdated"]
        ))),
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
    if !c.cc.is_empty() {
        e["cc"] = json!(addrs(&c.cc));
    }
    if !c.bcc.is_empty() {
        e["bcc"] = json!(addrs(&c.bcc));
    }
    if let Some(r) = &c.in_reply_to {
        e["inReplyTo"] = json!([r]);
    }
    if !c.references.is_empty() {
        e["references"] = json!(c.references);
    }
    e
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_json_shapes_a_plain_text_draft() {
        let c = Compose {
            from: "a@x.id".into(),
            to: vec!["b@x.id".into()],
            subject: "Hi".into(),
            text: "Body".into(),
            in_reply_to: Some("m1@x".into()),
            references: vec!["m0@x".into(), "m1@x".into()],
            ..Default::default()
        };
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
        assert!(matches!(
            updated(&json!({"notUpdated": {"x": {"type": "notFound"}}}), "x"),
            Err(MailError::NotFound)
        ));
    }

    /// Google DMARC shape: multipart/mixed, no classified body, one
    /// .xml.gz attachment. The old code rendered this as an empty pane;
    /// the detail must now surface the attachment and say body_source=none.
    fn dmarc_email() -> Value {
        json!({
            "id": "dmarc1",
            "from": [{"email": "noreply-dmarc-support@google.com"}],
            "to": [{"email": "admin@meowlabs.id"}],
            "subject": "Report domain: meowlabs.id Submitter: google.com Report-ID: 1",
            "preview": "", "receivedAt": "2026-11-04T00:00:00Z",
            "keywords": {"$seen": true}, "size": 12345, "hasAttachment": true,
            "blobId": "msg-blob-1",
            "textBody": [], "htmlBody": [], "bodyValues": {},
            "bodyStructure": {"type": "multipart/mixed", "subParts": [
                {"type": "text/plain", "partId": "1", "blobId": "b1", "size": 0},
                {"type": "application/gzip", "partId": "2", "blobId": "b2", "size": 1234,
                 "name": "google.com!meowlabs.id!1700000000!1700086400.xml.gz", "disposition": "attachment"}
            ]},
            "attachments": [{"type": "application/gzip", "partId": "2", "blobId": "b2", "size": 1234,
                "name": "google.com!meowlabs.id!1700000000!1700086400.xml.gz", "disposition": "attachment"}]
        })
    }

    #[test]
    fn dmarc_attachment_only_is_not_an_empty_message() {
        let d = detail_from_email(&dmarc_email());
        assert!(d.text_body.is_none() && d.html_body.is_none());
        assert_eq!(d.body_source, "none");
        assert_eq!(d.attachments.len(), 1);
        assert_eq!(d.attachments[0].part_id, "2");
        assert_eq!(d.attachments[0].content_type, "application/gzip");
        assert_eq!(d.mime_parts.len(), 2);
        assert_eq!(d.raw_blob_id.as_deref(), Some("msg-blob-1"));
    }

    #[test]
    fn nested_text_part_is_found_by_structure_walk() {
        let mut e = dmarc_email();
        e["bodyStructure"] = json!({"type": "multipart/mixed", "subParts": [
            {"type": "text/plain", "partId": "t1", "blobId": "bt", "size": 12}
        ]});
        e["bodyValues"] = json!({"t1": {"value": "Nested hello"}});
        e["attachments"] = json!([]);
        e["hasAttachment"] = json!(false);
        let d = detail_from_email(&e);
        assert_eq!(d.text_body.as_deref(), Some("Nested hello"));
        assert_eq!(d.body_source, "structure");
    }

    #[test]
    fn extract_gzip_and_zip_dmarc_reports() {
        use flate2::{write::GzEncoder, Compression};
        use std::io::Write;
        let xml = "<feedback><report_metadata><org_name>google.com</org_name></report_metadata></feedback>";
        let mut enc = GzEncoder::new(Vec::new(), Compression::default());
        enc.write_all(xml.as_bytes()).unwrap();
        let gz = enc.finish().unwrap();
        let out =
            extract_text_attachment("google.com!meowlabs.id!1!2.xml.gz", "application/gzip", &gz)
                .unwrap();
        assert_eq!(out.inner_name, "google.com!meowlabs.id!1!2.xml");
        assert!(out.text.contains("google.com"));
        assert!(!out.truncated);

        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut buf);
            zip.start_file(
                "google.com!meowlabs.id!1!2.xml",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
            zip.write_all(xml.as_bytes()).unwrap();
            zip.finish().unwrap();
        }
        let out = extract_text_attachment(
            "google.com!meowlabs.id!1!2.zip",
            "application/zip",
            buf.get_ref(),
        )
        .unwrap();
        assert!(out.inner_name.ends_with(".xml"));
        assert!(out.text.contains("<feedback>"));

        assert!(extract_text_attachment("photo.png", "image/png", b"not-text").is_err());
    }

    #[test]
    fn folder_filter_combines_mailbox_and_text() {
        let plain = folder_filter("m1", None);
        assert_eq!(plain["inMailbox"], "m1");
        assert!(plain.get("operator").is_none());
        let searched = folder_filter("m1", Some("  invoice  "));
        assert_eq!(searched["operator"], "AND");
        assert_eq!(searched["conditions"][0]["inMailbox"], "m1");
        assert_eq!(searched["conditions"][1]["text"], "invoice");
        // A blank search term is the same as no search.
        assert_eq!(folder_filter("m1", Some("   "))["inMailbox"], "m1");
    }
}

/// Outbound relay over Resend's HTTP API, used for recipients outside our own domains
/// (the host can't send on port 25).
#[derive(Clone)]
pub struct ResendClient {
    api_key: String,
    endpoint: String,
    http: reqwest::Client,
}

impl ResendClient {
    pub fn new(api_key: String) -> Self {
        Self::with_endpoint(api_key, "https://api.resend.com/emails".to_string())
    }

    /// Creates a client for a configured non-production relay endpoint.
    pub fn with_endpoint(api_key: String, endpoint: String) -> Self {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .expect("reqwest client");
        Self {
            api_key,
            endpoint,
            http,
        }
    }

    pub async fn send(&self, c: &Compose) -> Result<String, MailError> {
        let resp = self
            .http
            .post(&self.endpoint)
            .bearer_auth(&self.api_key)
            .json(&resend_json(c))
            .send()
            .await
            .map_err(|e| MailError::Unavailable(e.to_string()))?;
        let status = resp.status().as_u16();
        let v: Value = resp.json().await.unwrap_or(Value::Null);
        match status {
            200..=299 => Ok(v["id"].as_str().unwrap_or_default().to_string()),
            401 | 403 => Err(MailError::Unavailable(format!(
                "relay auth failed: {}",
                v["message"]
            ))),
            429 | 500.. => Err(MailError::Unavailable(format!(
                "relay HTTP {status}: {}",
                v["message"]
            ))),
            _ => Err(MailError::Rejected(
                v["message"]
                    .as_str()
                    .unwrap_or("relay refused the message")
                    .to_string(),
            )),
        }
    }
}

fn resend_json(c: &Compose) -> Value {
    let mut v = json!({"from": c.from, "to": c.to, "subject": c.subject, "text": c.text});
    if !c.cc.is_empty() {
        v["cc"] = json!(c.cc);
    }
    if !c.bcc.is_empty() {
        v["bcc"] = json!(c.bcc);
    }
    let mut headers = serde_json::Map::new();
    if let Some(r) = &c.in_reply_to {
        headers.insert("In-Reply-To".into(), json!(format!("<{r}>")));
    }
    if !c.references.is_empty() {
        headers.insert(
            "References".into(),
            json!(c
                .references
                .iter()
                .map(|r| format!("<{r}>"))
                .collect::<Vec<_>>()
                .join(" ")),
        );
    }
    if !headers.is_empty() {
        v["headers"] = Value::Object(headers);
    }
    v
}

#[cfg(test)]
mod resend_tests {
    use super::*;

    #[test]
    fn resend_payload() {
        let c = Compose {
            from: "a@x.id".into(),
            to: vec!["b@gmail.com".into()],
            bcc: vec!["c@y.id".into()],
            subject: "Re: hi".into(),
            text: "t".into(),
            in_reply_to: Some("m1@x".into()),
            references: vec!["m0@x".into(), "m1@x".into()],
            ..Default::default()
        };
        let v = resend_json(&c);
        assert_eq!(v["from"], "a@x.id");
        assert_eq!(v["to"][0], "b@gmail.com");
        assert!(v.get("cc").is_none());
        assert_eq!(v["bcc"][0], "c@y.id");
        assert_eq!(v["headers"]["In-Reply-To"], "<m1@x>");
        assert_eq!(v["headers"]["References"], "<m0@x> <m1@x>");
    }
}
