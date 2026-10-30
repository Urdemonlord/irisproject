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
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct MessageDetail {
    #[serde(flatten)]
    pub summary: MessageSummary,
    pub text_body: Option<String>,
    pub html_body: Option<String>,
}

#[derive(Clone)]
pub struct JmapClient {
    base_url: String,
    http: reqwest::Client,
}

const USING: [&str; 2] = ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail"];
const SUMMARY_PROPS: [&str; 7] = ["from", "to", "subject", "preview", "receivedAt", "keywords", "size"];

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
        // ponytail: posts to {base}/jmap/ instead of session.apiUrl (Stalwart advertises
        // the public hostname); switch to apiUrl if API and mail server get split.
        let resp = self
            .http
            .post(format!("{}/jmap/", self.base_url))
            .basic_auth(user, Some(pass))
            .json(&json!({ "using": USING, "methodCalls": calls }))
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

    pub async fn list_inbox(
        &self, user: &str, pass: &str, account: &str, limit: u32, position: u32,
    ) -> Result<(Vec<MessageSummary>, u64), MailError> {
        let mb = self
            .call(user, pass, json!([["Mailbox/query", {"accountId": account, "filter": {"role": "inbox"}}, "m"]]))
            .await?;
        let inbox = mb[0]["ids"][0].as_str().ok_or(MailError::NotFound)?.to_string();
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

    pub async fn get_message(
        &self, user: &str, pass: &str, account: &str, id: &str,
    ) -> Result<MessageDetail, MailError> {
        let mut props: Vec<&str> = SUMMARY_PROPS.to_vec();
        props.extend(["textBody", "htmlBody", "bodyValues"]);
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
        Ok(MessageDetail { summary: summary(e), text_body: body("textBody"), html_body: html })
    }
}

async fn check(resp: reqwest::Response) -> Result<Value, MailError> {
    match resp.status().as_u16() {
        401 | 403 => Err(MailError::Auth),
        s if s >= 400 => Err(MailError::Unavailable(format!("HTTP {s}"))),
        _ => resp.json().await.map_err(|e| MailError::Protocol(e.to_string())),
    }
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
        size_bytes: e["size"].as_u64().unwrap_or(0),
    }
}
