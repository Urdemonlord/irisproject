use sqlx::SqlitePool;
use anyhow::Result;

pub async fn init_pool(database_url: &str) -> Result<SqlitePool> {
    let pool = SqlitePool::connect(database_url).await?;
    sqlx::migrate!("./migrations").run(&pool).await?;
    Ok(pool)
}

pub async fn health_check(pool: &SqlitePool) -> Result<()> {
    let _: i32 = sqlx::query_scalar("SELECT 1").fetch_one(pool).await?;
    Ok(())
}
