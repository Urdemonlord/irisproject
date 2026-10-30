use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum MailError {
    #[error("Mail server error: {0}")]
    ServerError(String),
    #[error("Authentication failed")]
    AuthError,
    #[error("Not found: {0}")]
    NotFound(String),
    #[error("Invalid request: {0}")]
    InvalidRequest(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub from: String,
    pub to: Vec<String>,
    pub subject: String,
    pub received_at: String,
    pub is_read: bool,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JmapSession {
    pub primary_account: String,
    pub accounts: std::collections::HashMap<String, serde_json::Value>,
    pub capabilities: std::collections::HashMap<String, serde_json::Value>,
}

pub struct JmapClient {
    base_url: String,
    http: reqwest::Client,
    username: String,
    password: String,
}

impl JmapClient {
    pub fn new(base_url: String, username: String, password: String) -> Self {
        Self {
            base_url,
            http: reqwest::Client::new(),
            username,
            password,
        }
    }

    pub async fn session(&self) -> Result<JmapSession, MailError> {
        let url = format!("{}/jmap/session", self.base_url);
        let resp = self
            .http
            .get(&url)
            .basic_auth(&self.username, Some(&self.password))
            .send()
            .await
            .map_err(|e| MailError::ServerError(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(MailError::AuthError);
        }

        resp.json().await
            .map_err(|e| MailError::ServerError(e.to_string()))
    }

    pub async fn query_inbox_messages(
        &self,
        account_id: &str,
        limit: u32,
    ) -> Result<Vec<Message>, MailError> {
        let url = format!("{}/jmap", self.base_url);
        let payload = serde_json::json!({
            "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail"],
            "methodCalls": [
                [
                    "Email/query",
                    {
                        "accountId": account_id,
                        "filter": { "inMailbox": account_id },
                        "sort": [{ "property": "receivedAt", "isAscending": false }],
                        "limit": limit
                    },
                    "0"
                ]
            ]
        });

        let resp = self
            .http
            .post(&url)
            .basic_auth(&self.username, Some(&self.password))
            .json(&payload)
            .send()
            .await
            .map_err(|e| MailError::ServerError(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(MailError::ServerError("Query failed".to_string()));
        }

        // Simplified: return empty for now pending full JMAP integration
        Ok(vec![])
    }
}
