use iris_auth as auth;
use sqlx::SqlitePool;

fn scope_allowed(app_id: &str, scope: &str) -> bool {
    matches!(
        (app_id, scope),
        ("gencv", "email:template:auth.verify")
            | ("gencv", "email:template:auth.recovery")
            | ("aksaria", "email:template:auth.recovery")
            | ("aksaria", "email:template:auth.verification")
            | ("academy", "email:template:auth.login_link")
            | (_, "email:status")
    )
}

fn valid_key_id(key_id: &str) -> bool {
    key_id.len() == 32
        && key_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

async fn issue(
    pool: &SqlitePool,
    app_id: &str,
    scopes_arg: &str,
    expires_days: u32,
    daily_limit: u32,
) -> Result<auth::GeneratedApiKey, &'static str> {
    if !matches!(app_id, "gencv" | "academy" | "aksaria") {
        return Err("unsupported_app");
    }
    if !(1..=365).contains(&expires_days) || !(1..=10_000).contains(&daily_limit) {
        return Err("invalid_limits");
    }
    let scopes: Vec<&str> = scopes_arg.split(',').collect();
    if scopes.is_empty()
        || scopes.len() > 4
        || scopes.iter().any(|scope| !scope_allowed(app_id, scope))
        || scopes
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len()
            != scopes.len()
        || !scopes
            .iter()
            .any(|scope| scope.starts_with("email:template:"))
    {
        return Err("invalid_scopes");
    }

    let generated = auth::generate_api_key();
    let now = auth::now();
    let expiry = (chrono::Utc::now() + chrono::Duration::days(i64::from(expires_days)))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    sqlx::query(
        "INSERT INTO integration_keys (key_id,app_id,token_hash,scopes_json,created_at,expires_at,daily_send_limit) VALUES (?,?,?,?,?,?,?)",
    )
    .bind(&generated.key_id)
    .bind(app_id)
    .bind(&generated.token_hash)
    .bind(serde_json::to_string(&scopes).map_err(|_| "internal_error")?)
    .bind(now)
    .bind(&expiry)
    .bind(i64::from(daily_limit))
    .execute(pool)
    .await
    .map_err(|_| "internal_error")?;
    Ok(generated)
}

async fn revoke(pool: &SqlitePool, key_id: &str) -> Result<(), &'static str> {
    if !valid_key_id(key_id) {
        return Err("invalid_key_id");
    }
    let result = sqlx::query(
        "UPDATE integration_keys SET status='revoked',revoked_at=? WHERE key_id=? AND status='active'",
    )
    .bind(auth::now())
    .bind(key_id)
    .execute(pool)
    .await
    .map_err(|_| "internal_error")?;
    if result.rows_affected() == 1 {
        Ok(())
    } else {
        Err("key_not_active")
    }
}

/// Local operator command: returns a new plaintext token exactly once to the caller.
pub(crate) async fn run(pool: &SqlitePool, args: &[String]) -> Result<String, &'static str> {
    match args {
        [action, app_id, days, limit, scopes] if action == "create" => {
            let expires_days = days.parse().map_err(|_| "invalid_limits")?;
            let daily_limit = limit.parse().map_err(|_| "invalid_limits")?;
            let generated = issue(pool, app_id, scopes, expires_days, daily_limit).await?;
            Ok(format!("key_id={}\ntoken={}\n", generated.key_id, generated.token))
        }
        [action, key_id] if action == "revoke" => {
            revoke(pool, key_id).await?;
            Ok(format!("revoked key_id={key_id}\n"))
        }
        _ => Err("usage: integration-key create <gencv|academy|aksaria> <expires-days> <daily-limit> <comma-separated-scopes> | integration-key revoke <key-id>"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn creates_and_revokes_an_app_scoped_key_without_storing_plaintext() {
        let pool = iris_database::init_pool("sqlite::memory:").await.unwrap();
        let generated = issue(
            &pool,
            "gencv",
            "email:template:auth.verify,email:status",
            30,
            25,
        )
        .await
        .unwrap();
        let row: (String, String, String, String, i64) = sqlx::query_as(
            "SELECT app_id,token_hash,scopes_json,status,daily_send_limit FROM integration_keys WHERE key_id=?",
        )
        .bind(&generated.key_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row.0, "gencv");
        assert_eq!(row.1, generated.token_hash);
        assert_ne!(row.1, generated.token);
        assert_eq!(row.2, "[\"email:template:auth.verify\",\"email:status\"]");
        assert_eq!(row.3, "active");
        assert_eq!(row.4, 25);

        revoke(&pool, &generated.key_id).await.unwrap();
        let status: String =
            sqlx::query_scalar("SELECT status FROM integration_keys WHERE key_id=?")
                .bind(&generated.key_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(status, "revoked");
        assert_eq!(
            revoke(&pool, &generated.key_id).await,
            Err("key_not_active")
        );
    }

    #[tokio::test]
    async fn rejects_cross_app_scopes_and_unbounded_limits() {
        let pool = iris_database::init_pool("sqlite::memory:").await.unwrap();
        assert_eq!(
            issue(&pool, "academy", "email:template:auth.verify", 30, 10)
                .await
                .err(),
            Some("invalid_scopes")
        );
        assert_eq!(
            issue(&pool, "gencv", "email:template:auth.verify", 366, 10)
                .await
                .err(),
            Some("invalid_limits")
        );
        assert_eq!(
            issue(&pool, "gencv", "email:template:auth.verify", 30, 10_001)
                .await
                .err(),
            Some("invalid_limits")
        );
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM integration_keys")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(rows, 0);
    }
}
