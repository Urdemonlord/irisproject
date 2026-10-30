use uuid::Uuid;
use chrono::{DateTime, Utc, Duration};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub user_id: String,
    pub token_hash: String,
    pub expires_at: String,
    pub created_at: String,
}

impl Session {
    pub fn new(user_id: String) -> (Self, String) {
        let id = Uuid::new_v4().to_string();
        let token = Uuid::new_v4().to_string();
        let token_hash = format!("{:x}", md5::compute(token.as_bytes()));
        let now = Utc::now().to_rfc3339();
        let expires_at = (Utc::now() + Duration::days(30)).to_rfc3339();
        
        let session = Self {
            id,
            user_id,
            token_hash,
            expires_at,
            created_at: now,
        };
        
        (session, token)
    }

    pub fn is_expired(&self) -> bool {
        if let Ok(exp) = DateTime::parse_from_rfc3339(&self.expires_at) {
            exp < Utc::now().with_timezone(&chrono::FixedOffset::east_opt(0).unwrap())
        } else {
            true
        }
    }
}
