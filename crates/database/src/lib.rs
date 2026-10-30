use anyhow::Result;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool};
use std::str::FromStr;

pub async fn init_pool(database_url: &str) -> Result<SqlitePool> {
    let opts = SqliteConnectOptions::from_str(database_url)?
        .create_if_missing(true)
        .foreign_keys(true);
    let pool = SqlitePool::connect_with(opts).await?;
    sqlx::migrate!("../../migrations").run(&pool).await?;
    Ok(pool)
}

pub async fn health_check(pool: &SqlitePool) -> Result<()> {
    let _: i32 = sqlx::query_scalar("SELECT 1").fetch_one(pool).await?;
    Ok(())
}
