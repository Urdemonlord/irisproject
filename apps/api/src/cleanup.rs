//! Background retention job (BSD §retention): expire temp inboxes, purge their mail through
//! JMAP (never by touching mail-server files), prune old sessions and rate-limit events.
use crate::AppState;
use chrono::{Duration, SecondsFormat, Utc};
use sqlx::SqlitePool;
use std::sync::Arc;

const INTERVAL_SECS: u64 = 60;
/// Grace after the longest possible inbox life before catch-all mail is swept regardless of To:
/// (covers Bcc and mail to never-issued addresses).
const SWEEP_GRACE_HOURS: i64 = 1;

pub async fn run(s: Arc<AppState>) {
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(INTERVAL_SECS));
    loop {
        tick.tick().await;
        if let Err(e) = once(&s).await {
            tracing::warn!("cleanup: {e}");
        }
    }
}

fn ts(t: chrono::DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Step 1 (DB only): active/expired inboxes past expiry become `deletion_pending`; prune
/// expired/revoked sessions after 7 days and rate-limit events after 2 days.
pub async fn mark(pool: &SqlitePool) -> Result<u64, sqlx::Error> {
    let now = Utc::now();
    let n = sqlx::query("UPDATE temporary_inboxes SET status='deletion_pending' WHERE status IN ('active','expired') AND expires_at<=?")
        .bind(ts(now)).execute(pool).await?.rows_affected();
    // auth_sessions timestamps are chrono to_rfc3339 (+00:00); compare in the same format.
    let week = (now - Duration::days(7)).to_rfc3339();
    sqlx::query("DELETE FROM auth_sessions WHERE expires_at<? OR revoked_at<?").bind(&week).bind(&week).execute(pool).await?;
    sqlx::query("DELETE FROM rate_limit_events WHERE occurred_at<?").bind(ts(now - Duration::days(2))).execute(pool).await?;
    Ok(n)
}

async fn once(s: &AppState) -> anyhow::Result<()> {
    let marked = mark(&s.pool).await?;
    let c = &s.config;
    if c.temp_inbox_mail_user.is_empty() {
        return Ok(());
    }
    let pending: Vec<(String, String)> = sqlx::query_as("SELECT id,address FROM temporary_inboxes WHERE status='deletion_pending' LIMIT 100")
        .fetch_all(&s.pool).await?;
    let (user, pass) = (&c.temp_inbox_mail_user, &c.temp_inbox_mail_password);
    let account = s.jmap.login(user, pass).await?;
    let mut purged = 0;
    for (id, address) in &pending {
        // Row only flips to `deleted` after the mail server confirmed; failures retry next tick.
        purged += s.jmap.destroy_to(user, pass, &account, address).await?;
        sqlx::query("UPDATE temporary_inboxes SET status='deleted', deleted_at=? WHERE id=?")
            .bind(ts(Utc::now())).bind(id).execute(&s.pool).await?;
    }
    let cutoff = Utc::now() - Duration::hours(c.temp_inbox_default_expiry_hours.max(1) as i64 + SWEEP_GRACE_HOURS);
    let swept = s.jmap.destroy_before(user, pass, &account, &ts(cutoff)).await?;
    if marked + pending.len() as u64 + purged as u64 + swept as u64 > 0 {
        tracing::info!(marked, inboxes = pending.len(), purged, swept, "cleanup");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mark_expires_only_past_inboxes() {
        let pool = iris_database::init_pool("sqlite::memory:").await.unwrap();
        let now = Utc::now();
        for (id, exp, status) in [("past", now - Duration::minutes(1), "active"), ("flipped", now - Duration::hours(2), "expired"),
                                  ("future", now + Duration::minutes(30), "active"), ("gone", now - Duration::hours(3), "deleted")] {
            sqlx::query("INSERT INTO temporary_inboxes (id,address,access_token_hash,status,created_at,expires_at) VALUES (?,?,?,?,?,?)")
                .bind(id).bind(format!("{id}@x")).bind("h").bind(status).bind(ts(now)).bind(ts(exp)).execute(&pool).await.unwrap();
        }
        assert_eq!(super::mark(&pool).await.unwrap(), 2);
        let rows: Vec<(String, String)> = sqlx::query_as("SELECT id,status FROM temporary_inboxes ORDER BY id").fetch_all(&pool).await.unwrap();
        assert_eq!(rows, [("flipped", "deletion_pending"), ("future", "active"), ("gone", "deleted"), ("past", "deletion_pending")]
            .map(|(a, b)| (a.to_string(), b.to_string())));
    }
}
