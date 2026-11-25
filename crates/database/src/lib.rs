use anyhow::Result;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool};
use std::str::FromStr;
use std::time::Duration;

pub async fn init_pool(database_url: &str) -> Result<SqlitePool> {
    // WAL + busy timeout, applied to every pooled connection:
    // - journal_mode=WAL: readers no longer block the single writer (and
    //   vice versa), so concurrent API requests stop failing with
    //   "database is locked" under the previous DELETE journal mode.
    //   WAL is persistent per database file; for `sqlite::memory:` test
    //   databases SQLite keeps the MEMORY journal mode instead, so the
    //   in-memory tests elsewhere are unaffected.
    // - busy_timeout=5s: a contended writer waits up to 5 seconds for the
    //   lock instead of failing immediately. 5s is also sqlx's default;
    //   it is pinned explicitly here so a sqlx upgrade cannot silently
    //   change it, and 5s bounds how long a request can stall on a lock.
    // No pool sizing is set on purpose: SqlitePool::connect_with keeps
    // sqlx's default (max 10 connections). SQLite serialises writers no
    // matter how large the pool is, so a larger pool would only add lock
    // contention on this single-instance, low-traffic API; WAL already
    // lets readers run concurrently with the writer.
    let opts = SqliteConnectOptions::from_str(database_url)?
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(Duration::from_secs(5));
    let pool = SqlitePool::connect_with(opts).await?;
    sqlx::migrate!("../../migrations").run(&pool).await?;
    Ok(pool)
}

pub async fn health_check(pool: &SqlitePool) -> Result<()> {
    let _: i32 = sqlx::query_scalar("SELECT 1").fetch_one(pool).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Proves the connection config above actually lands on a file-backed
    // database (in-memory databases always report journal_mode=memory,
    // so this test needs a real temp file).
    #[tokio::test]
    async fn pool_uses_wal_and_busy_timeout() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("iris-wal-test-{nanos}.db"));
        let url = format!("sqlite://{}?mode=rwc", path.display());
        let pool = init_pool(&url).await.unwrap();
        let journal_mode: String = sqlx::query_scalar("PRAGMA journal_mode")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(journal_mode, "wal");
        let busy_timeout: i64 = sqlx::query_scalar("PRAGMA busy_timeout")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(busy_timeout, 5000);
        pool.close().await;
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("db-wal"));
        let _ = std::fs::remove_file(path.with_extension("db-shm"));
    }

    // Migration 016: a mailbox delete must fail while alias rows
    // point at it (ON DELETE RESTRICT), instead of silently deleting
    // the alias rows the way 015's CASCADE did. The unlink handler
    // refuses such a mailbox with 409 first; this is the schema
    // backstop behind it, and it also proves the 015 -> 016 rebuild
    // kept the rows and the indexes.
    #[tokio::test]
    async fn aliases_mailbox_delete_is_restricted() {
        let pool = init_pool("sqlite::memory:").await.unwrap();
        sqlx::query(
            "INSERT INTO app_users (id,email,display_name,created_at,updated_at) \
             VALUES ('u1','me@x.id','me','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO permanent_mailboxes (id,user_id,email_address,mail_server_account_id,status,created_at,updated_at) \
             VALUES ('mb1','u1','me@x.id','acct-me','active','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO mail_aliases (id,user_id,mailbox_id,address,stalwart_account_id,enabled,created_at) \
             VALUES ('al1','u1','mb1','sales@x.id','acct-me',1,'2026-01-01T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        // The mailbox delete fails while the alias row exists, and
        // the alias row survives the refused delete.
        let err = sqlx::query("DELETE FROM permanent_mailboxes WHERE id='mb1'")
            .execute(&pool)
            .await
            .unwrap_err();
        assert!(
            matches!(&err, sqlx::Error::Database(d) if d.message().contains("FOREIGN KEY")),
            "expected a foreign key failure, got {err}"
        );
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM mail_aliases WHERE id='al1'")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(n, 1);
        // Once the alias row is gone the mailbox deletes, and the
        // rebuilt table really carries RESTRICT, not CASCADE.
        sqlx::query("DELETE FROM mail_aliases WHERE id='al1'")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM permanent_mailboxes WHERE id='mb1'")
            .execute(&pool)
            .await
            .unwrap();
        let sql: String = sqlx::query_scalar(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='mail_aliases'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(sql.contains("ON DELETE RESTRICT"), "{sql}");
    }
}
