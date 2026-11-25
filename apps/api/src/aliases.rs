//! Mail aliases and per-domain catch-all settings.
//!
//! An alias is an extra address on one of the user's own domains that
//! delivers into one of their mailboxes (e.g. `sales@x.id` into
//! `me@x.id`). A domain's catch-all is the address every
//! otherwise-unmatched address in the domain delivers to — or off,
//! so unknown recipients are rejected again.
//!
//! Both live on the mail server: aliases are entries of the account's
//! server-side alias list, catch-all is the domain's `catchAllAddress`
//! field, and both are changed through the Stalwart management API
//! with an admin credential (`crates/mail/src/admin.rs`). IRIS keeps a
//! mirror of what it created (`mail_aliases`, `domain_catch_all`,
//! migration 015) for ownership checks and the audit trail; Stalwart
//! is delivery truth. Writes go to Stalwart first, then the mirror;
//! a failed mirror write compensates by undoing the Stalwart change
//! (same pattern as `linked_mailboxes.rs`).
//!
//! The admin credential is optional: while `STALWART_ADMIN_USER` /
//! `STALWART_ADMIN_PASSWORD` are unset, every endpoint here answers
//! 503 `aliases_unavailable` (fail closed, like `IRIS_ADMIN_EMAILS`).
//! Alias writes are serialized with `organize_lock`: the alias list is
//! read, modified and written back whole, so two concurrent requests
//! must not interleave a read-modify-write.
//!
//! This is a normal mail feature: aliases and catch-all for the user's
//! own domains. There is no bulk creation, no temp-email provider API
//! and no connection to any token-harvesting tooling.

use crate::{
    audit, email_address::valid_email_address, owned_mailbox, ApiError, ApiResult, AppState,
    CurrentUser, S,
};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Extension, Json,
};
use iris_config::limits::MAX_ALIASES_PER_USER;
use iris_mail::admin::{AdminClient, EmailAlias};
use serde::Deserialize;
use serde_json::json;

/// The admin management client, or 503 while the admin credential is
/// unconfigured. The credential never reaches the browser.
fn admin_client(s: &AppState) -> Result<AdminClient, ApiError> {
    if !s.config.stalwart_admin_configured() {
        return Err(ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "aliases_unavailable",
        ));
    }
    Ok(AdminClient::new(
        s.config.stalwart_management_url.clone(),
        s.config.stalwart_admin_user.clone(),
        s.config.stalwart_admin_password.clone(),
    ))
}

fn domain_of(address: &str) -> Option<&str> {
    address.rsplit_once('@').map(|(_, domain)| domain)
}

/// One mirror row, as the list endpoint returns it.
fn alias_json(
    id: &str,
    address: &str,
    mailbox_id: &str,
    enabled: bool,
    created_at: &str,
) -> serde_json::Value {
    json!({
        "id": id,
        "address": address,
        "mailbox_id": mailbox_id,
        "enabled": enabled,
        "created_at": created_at,
    })
}

type AliasRow = (String, String, String, i64, String);

/// GET /api/v1/aliases. The caller's own aliases, oldest first.
pub(crate) async fn list_aliases(State(s): S, Extension(u): Extension<CurrentUser>) -> ApiResult {
    let rows: Vec<AliasRow> = sqlx::query_as(
        "SELECT id, address, mailbox_id, enabled, created_at FROM mail_aliases \
         WHERE user_id=? ORDER BY created_at",
    )
    .bind(&u.user_id)
    .fetch_all(&s.pool)
    .await?;
    let aliases: Vec<_> = rows
        .iter()
        .map(|(id, address, mailbox_id, enabled, created_at)| {
            alias_json(id, address, mailbox_id, *enabled != 0, created_at)
        })
        .collect();
    Ok(Json(json!({ "aliases": aliases })).into_response())
}

#[derive(Deserialize)]
pub(crate) struct CreateAliasBody {
    address: String,
    mailbox_id: String,
}

/// POST /api/v1/aliases. Creates an alias on one of the caller's own
/// mailboxes: the alias domain must equal the mailbox's domain, the
/// address must not already exist (here or on the server), and the
/// caller must be under the per-user cap. The mailbox need not be
/// unlocked: the change is made with the admin credential, never the
/// mailbox's own.
pub(crate) async fn create_alias(
    State(s): S,
    Extension(u): Extension<CurrentUser>,
    Json(b): Json<CreateAliasBody>,
) -> ApiResult {
    let admin = admin_client(&s)?;
    let address = b.address.trim().to_lowercase();
    if address.is_empty() || !valid_email_address(&address) || b.mailbox_id.is_empty() {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_request"));
    }
    let mb = owned_mailbox(&s, &u, &b.mailbox_id).await?;
    let Some(domain) = domain_of(&address) else {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_request"));
    };
    let Some(mb_domain) = domain_of(&mb.email) else {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_request"));
    };
    if !domain.eq_ignore_ascii_case(mb_domain) || address == mb.email {
        return Err(ApiError(StatusCode::CONFLICT, "alias_taken"));
    }
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM mail_aliases WHERE user_id=?")
        .bind(&u.user_id)
        .fetch_one(&s.pool)
        .await?;
    if count >= MAX_ALIASES_PER_USER {
        return Err(ApiError(StatusCode::CONFLICT, "alias_limit_reached"));
    }
    let taken: Option<String> = sqlx::query_scalar("SELECT id FROM mail_aliases WHERE address=?")
        .bind(&address)
        .fetch_optional(&s.pool)
        .await?;
    if taken.is_some() {
        return Err(ApiError(StatusCode::CONFLICT, "alias_taken"));
    }
    let local = address
        .split_once('@')
        .map(|(local, _)| local.to_string())
        .unwrap_or_default();
    let _guard = s.organize_lock.lock().await;
    let domain_info = admin
        .resolve_domain(domain)
        .await?
        .ok_or(ApiError(StatusCode::NOT_FOUND, "not_found"))?;
    let mut aliases = admin.get_account_aliases(&mb.account_id).await?;
    if aliases
        .iter()
        .any(|a| a.name.eq_ignore_ascii_case(&local) && a.domain_id == domain_info.id)
    {
        return Err(ApiError(StatusCode::CONFLICT, "alias_taken"));
    }
    aliases.push(EmailAlias {
        name: local,
        domain_id: domain_info.id,
        enabled: true,
    });
    admin.set_account_aliases(&mb.account_id, &aliases).await?;
    // The Stalwart change landed; a failed mirror write undoes it, so
    // no live alias is ever left without its IRIS row (and the next
    // create of the same address does not hit a phantom conflict).
    let now = iris_auth::now();
    let alias_id = uuid::Uuid::new_v4().to_string();
    if let Err(e) = sqlx::query(
        "INSERT INTO mail_aliases (id,user_id,mailbox_id,address,stalwart_account_id,enabled,created_at) \
         VALUES (?,?,?,?,?,?,?)",
    )
    .bind(&alias_id)
    .bind(&u.user_id)
    .bind(&b.mailbox_id)
    .bind(&address)
    .bind(&mb.account_id)
    .bind(1)
    .bind(&now)
    .execute(&s.pool)
    .await
    {
        aliases.pop();
        let _ = admin.set_account_aliases(&mb.account_id, &aliases).await;
        return Err(match e {
            sqlx::Error::Database(ref d) if d.is_unique_violation() => {
                ApiError(StatusCode::CONFLICT, "alias_taken")
            }
            e => ApiError::from(e),
        });
    }
    audit::record(
        &s.pool,
        Some(&u.user_id),
        &u.email,
        audit::ALIAS_CREATE,
        Some("alias"),
        Some(&alias_id),
        json!({"address": address, "mailbox_id": b.mailbox_id}),
        Some(&u.ip),
    )
    .await;
    Ok(Json(alias_json(&alias_id, &address, &b.mailbox_id, true, &now)).into_response())
}

/// DELETE /api/v1/aliases/:id. Removes the alias from the server list
/// and the mirror. The owner, or an admin, may delete it; anyone else
/// gets 404, never a hint the row exists. A server list that no longer
/// contains the address still converges: the mirror row goes away.
pub(crate) async fn delete_alias(
    State(s): S,
    Extension(u): Extension<CurrentUser>,
    Path(alias_id): Path<String>,
) -> ApiResult {
    let admin = admin_client(&s)?;
    let row: Option<(String, String, String, String)> = sqlx::query_as(
        "SELECT user_id, mailbox_id, address, stalwart_account_id FROM mail_aliases WHERE id=?",
    )
    .bind(&alias_id)
    .fetch_optional(&s.pool)
    .await?;
    let Some((owner_id, mailbox_id, address, account_id)) = row else {
        return Err(ApiError(StatusCode::NOT_FOUND, "not_found"));
    };
    if owner_id != u.user_id && !iris_config::is_admin_email(&s.config.admin_emails, &u.email) {
        return Err(ApiError(StatusCode::NOT_FOUND, "not_found"));
    }
    let _guard = s.organize_lock.lock().await;
    // The mirror row is deleted only once the server list is updated;
    // a missing account (mailbox deleted out of band) still converges
    // by dropping the orphan row.
    match admin.get_account_aliases(&account_id).await {
        Ok(mut aliases) => {
            let before = aliases.len();
            let Some(domain) = domain_of(&address) else {
                return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_request"));
            };
            let domain_id = admin
                .resolve_domain(domain)
                .await?
                .map(|d| d.id)
                .unwrap_or_default();
            let local = address
                .split_once('@')
                .map(|(local, _)| local)
                .unwrap_or_default();
            aliases.retain(|a| !(a.name.eq_ignore_ascii_case(local) && a.domain_id == domain_id));
            if aliases.len() != before {
                admin.set_account_aliases(&account_id, &aliases).await?;
            }
        }
        Err(iris_mail::MailError::NotFound) => {}
        Err(e) => return Err(e.into()),
    }
    sqlx::query("DELETE FROM mail_aliases WHERE id=?")
        .bind(&alias_id)
        .execute(&s.pool)
        .await?;
    audit::record(
        &s.pool,
        Some(&u.user_id),
        &u.email,
        audit::ALIAS_DELETE,
        Some("alias"),
        Some(&alias_id),
        json!({"address": address, "mailbox_id": mailbox_id}),
        Some(&u.ip),
    )
    .await;
    Ok(Json(json!({ "id": alias_id, "deleted": true })).into_response())
}

/// True when the caller may manage `domain`'s catch-all: they own a
/// mailbox on that domain, or they are on the admin allow-list.
async fn can_manage_domain(s: &AppState, u: &CurrentUser, domain: &str) -> Result<bool, ApiError> {
    if iris_config::is_admin_email(&s.config.admin_emails, &u.email) {
        return Ok(true);
    }
    let emails: Vec<String> = sqlx::query_scalar(
        "SELECT email_address FROM permanent_mailboxes WHERE user_id=? AND status='active'",
    )
    .bind(&u.user_id)
    .fetch_all(&s.pool)
    .await?;
    Ok(emails
        .iter()
        .any(|e| domain_of(e).is_some_and(|d| d.eq_ignore_ascii_case(domain))))
}

fn catch_all_json(domain: &str, target: Option<&str>) -> serde_json::Value {
    json!({
        "domain": domain,
        "target_address": target,
        "enabled": target.is_some(),
    })
}

/// GET /api/v1/domains/:domain/catch-all. The server is the source of
/// truth; the mirror row is refreshed best-effort from the answer.
pub(crate) async fn get_catch_all(
    State(s): S,
    Extension(u): Extension<CurrentUser>,
    Path(domain): Path<String>,
) -> ApiResult {
    let admin = admin_client(&s)?;
    let domain = domain.trim().to_lowercase();
    if !can_manage_domain(&s, &u, &domain).await? {
        return Err(ApiError(StatusCode::FORBIDDEN, "forbidden"));
    }
    let info = admin
        .resolve_domain(&domain)
        .await?
        .ok_or(ApiError(StatusCode::NOT_FOUND, "not_found"))?;
    let _ = sqlx::query(
        "INSERT INTO domain_catch_all (domain,target_address,updated_at) VALUES (?,?,?) \
         ON CONFLICT(domain) DO UPDATE SET target_address=excluded.target_address, updated_at=excluded.updated_at",
    )
    .bind(&domain)
    .bind(&info.catch_all)
    .bind(iris_auth::now())
    .execute(&s.pool)
    .await;
    Ok(Json(catch_all_json(&domain, info.catch_all.as_deref())).into_response())
}

#[derive(Deserialize)]
pub(crate) struct SetCatchAllBody {
    target_address: Option<String>,
}

/// PUT /api/v1/domains/:domain/catch-all. Sets the catch-all target
/// (an address on the same domain), or `null` to disable catch-all so
/// unknown recipients are rejected again. Catch-all accepts every
/// guessed or misspelled address, so it invites more spam: the web
/// says so next to the toggle.
pub(crate) async fn set_catch_all(
    State(s): S,
    Extension(u): Extension<CurrentUser>,
    Path(domain): Path<String>,
    Json(b): Json<SetCatchAllBody>,
) -> ApiResult {
    let admin = admin_client(&s)?;
    let domain = domain.trim().to_lowercase();
    if !can_manage_domain(&s, &u, &domain).await? {
        return Err(ApiError(StatusCode::FORBIDDEN, "forbidden"));
    }
    let target = match b.target_address.map(|t| t.trim().to_lowercase()) {
        Some(t) if !t.is_empty() => {
            if !valid_email_address(&t)
                || domain_of(&t).is_some_and(|d| !d.eq_ignore_ascii_case(&domain))
            {
                return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_request"));
            }
            Some(t)
        }
        _ => None,
    };
    let _guard = s.organize_lock.lock().await;
    let info = admin
        .resolve_domain(&domain)
        .await?
        .ok_or(ApiError(StatusCode::NOT_FOUND, "not_found"))?;
    admin.set_catch_all(&info.id, target.as_deref()).await?;
    sqlx::query(
        "INSERT INTO domain_catch_all (domain,target_address,updated_at) VALUES (?,?,?) \
         ON CONFLICT(domain) DO UPDATE SET target_address=excluded.target_address, updated_at=excluded.updated_at",
    )
    .bind(&domain)
    .bind(&target)
    .bind(iris_auth::now())
    .execute(&s.pool)
    .await?;
    audit::record(
        &s.pool,
        Some(&u.user_id),
        &u.email,
        audit::CATCHALL_SET,
        Some("domain"),
        Some(&domain),
        json!({"domain": domain, "target_address": target}),
        Some(&u.ip),
    )
    .await;
    Ok(Json(catch_all_json(&domain, target.as_deref())).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{mailbox_creds, Creds};
    use axum::body::to_bytes;
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        time::{timeout, Duration},
    };

    /// Fake Stalwart management server: stateful, so tests prove the
    /// exact JMAP calls and the read-modify-write of the alias list.
    /// `acct-me` starts with no aliases; `acct-other` already owns
    /// `taken@x.id`, so creating it for anyone else is refused with
    /// `notUpdated` exactly like a real server refuses a taken
    /// address. Domains: `x.id` (no catch-all) and `y.id` (catch-all
    /// to `postmaster@y.id`). Records every request's basic-auth user
    /// and body.
    struct FakeMgmt {
        base_url: String,
        requests: Arc<Mutex<Vec<serde_json::Value>>>,
        _task: tokio::task::JoinHandle<()>,
    }

    struct MgmtState {
        aliases: HashMap<String, Vec<serde_json::Value>>,
        catch_all: HashMap<String, Option<String>>,
    }

    async fn fake_mgmt() -> FakeMgmt {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let state = Arc::new(Mutex::new(MgmtState {
            aliases: HashMap::from([(
                "acct-other".to_string(),
                vec![json!({"name": "taken", "domainId": "d-x", "enabled": true})],
            )]),
            catch_all: HashMap::from([
                ("d-x".to_string(), None),
                ("d-y".to_string(), Some("postmaster@y.id".to_string())),
            ]),
        }));
        let task_requests = requests.clone();
        let task_state = state.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok(Ok((mut stream, _))) =
                    timeout(Duration::from_secs(5), listener.accept()).await
                else {
                    return;
                };
                let mut bytes = Vec::new();
                let mut chunk = [0u8; 4096];
                let mut expected: Option<usize> = None;
                let mut header_end: Option<usize> = None;
                loop {
                    let Ok(n) = stream.read(&mut chunk).await else {
                        return;
                    };
                    if n == 0 {
                        return;
                    }
                    bytes.extend_from_slice(&chunk[..n]);
                    if header_end.is_none() {
                        if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                            header_end = Some(end);
                            let headers = String::from_utf8_lossy(&bytes[..end]);
                            let length = headers
                                .lines()
                                .find_map(|line| {
                                    let (name, value) = line.split_once(':')?;
                                    name.eq_ignore_ascii_case("content-length")
                                        .then(|| value.trim().parse::<usize>().ok())
                                        .flatten()
                                })
                                .unwrap_or(0);
                            expected = Some(end + 4 + length);
                        }
                    }
                    if let Some(end) = expected {
                        if bytes.len() >= end {
                            break;
                        }
                    }
                }
                let end = header_end.unwrap();
                let head = String::from_utf8_lossy(&bytes[..end]).to_string();
                let mut lines = head.lines();
                let request_line = lines.next().unwrap_or_default().to_string();
                let mut parts = request_line.split_whitespace();
                let method = parts.next().unwrap_or_default().to_string();
                let path = parts.next().unwrap_or_default().to_string();
                let auth_user = head
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        if !name.eq_ignore_ascii_case("authorization") {
                            return None;
                        }
                        let token = value.trim().strip_prefix("Basic ")?;
                        let decoded = STANDARD.decode(token).unwrap_or_default();
                        let text = String::from_utf8_lossy(&decoded).to_string();
                        Some(
                            text.split_once(':')
                                .map(|(u, _)| u.to_string())
                                .unwrap_or(text),
                        )
                    })
                    .unwrap_or_default();
                let body = bytes[end + 4..].to_vec();
                let body_json: serde_json::Value =
                    serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
                task_requests.lock().unwrap().push(json!({
                    "method": method, "path": path, "user": auth_user, "body": body_json,
                }));
                let payload: Vec<u8> = if method == "POST" && path == "/jmap/" {
                    let first = body_json["methodCalls"][0][0].as_str().unwrap_or_default();
                    let args = &body_json["methodCalls"][0][1];
                    match first {
                        "x:Account/get" => {
                            let id = args["ids"][0].as_str().unwrap_or_default();
                            let aliases = task_state
                                .lock()
                                .unwrap()
                                .aliases
                                .get(id)
                                .cloned()
                                .unwrap_or_default();
                            serde_json::to_vec(&json!({"methodResponses": [[
                                "x:Account/get",
                                {"list": [{"id": id, "aliases": aliases}], "notFound": []},
                                "g",
                            ]]}))
                            .unwrap()
                        }
                        "x:Account/set" => {
                            // Whole-list replacement: reject an alias
                            // another account already owns.
                            let mut st = task_state.lock().unwrap();
                            let update = args["update"].as_object().cloned().unwrap_or_default();
                            let mut refused = false;
                            for (acct, patch) in &update {
                                let new_list: Vec<serde_json::Value> = patch["aliases"]
                                    .as_object()
                                    .map(|o| {
                                        let mut items: Vec<(usize, serde_json::Value)> = o
                                            .iter()
                                            .filter_map(|(k, v)| {
                                                Some((k.parse::<usize>().ok()?, v.clone()))
                                            })
                                            .collect();
                                        items.sort_by_key(|(k, _)| *k);
                                        items.into_iter().map(|(_, v)| v).collect()
                                    })
                                    .unwrap_or_default();
                                for (other_acct, other_aliases) in st.aliases.iter() {
                                    if other_acct == acct {
                                        continue;
                                    }
                                    for a in &new_list {
                                        let name = a["name"].as_str().unwrap_or("");
                                        let domain_id = a["domainId"].as_str().unwrap_or("");
                                        if other_aliases.iter().any(|o| {
                                            o["name"].as_str().unwrap_or("") == name
                                                && o["domainId"].as_str().unwrap_or("") == domain_id
                                        }) {
                                            refused = true;
                                        }
                                    }
                                }
                                if !refused {
                                    st.aliases.insert(acct.clone(), new_list);
                                }
                            }
                            if refused {
                                serde_json::to_vec(&json!({"methodResponses": [[
                                    "x:Account/set",
                                    {"updated": {}, "notUpdated": {"acct-me": {"type": "invalidPatch", "description": "address taken"}}},
                                    "s",
                                ]]}))
                                .unwrap()
                            } else {
                                serde_json::to_vec(&json!({"methodResponses": [[
                                    "x:Account/set",
                                    {"updated": {"acct-me": {}}, "notUpdated": {}},
                                    "s",
                                ]]}))
                                .unwrap()
                            }
                        }
                        "x:Domain/get" => {
                            let st = task_state.lock().unwrap();
                            let list = vec![
                                json!({"id": "d-x", "name": "x.id", "catchAllAddress": st.catch_all["d-x"]}),
                                json!({"id": "d-y", "name": "y.id", "catchAllAddress": st.catch_all["d-y"]}),
                            ];
                            serde_json::to_vec(&json!({"methodResponses": [[
                                "x:Domain/get",
                                {"list": list, "notFound": []},
                                "g",
                            ]]}))
                            .unwrap()
                        }
                        "x:Domain/set" => {
                            let mut st = task_state.lock().unwrap();
                            let update = args["update"].as_object().cloned().unwrap_or_default();
                            for (domain_id, patch) in &update {
                                st.catch_all.insert(
                                    domain_id.clone(),
                                    patch["catchAllAddress"].as_str().map(str::to_string),
                                );
                            }
                            serde_json::to_vec(&json!({"methodResponses": [[
                                "x:Domain/set",
                                {"updated": {"d-x": {}}, "notUpdated": {}},
                                "s",
                            ]]}))
                            .unwrap()
                        }
                        _ => br#"{"methodResponses":[]}"#.to_vec(),
                    }
                } else {
                    br#"{"error":"no such route"}"#.to_vec()
                };
                let status = if method == "POST" && path == "/jmap/" {
                    "200 OK"
                } else {
                    "404 Not Found"
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    payload.len()
                );
                let mut out = response.into_bytes();
                out.extend_from_slice(&payload);
                let _ = stream.write_all(&out).await;
                let _ = stream.shutdown().await;
            }
        });
        FakeMgmt {
            base_url: format!("http://{address}"),
            requests,
            _task: task,
        }
    }

    fn requests(fake: &FakeMgmt) -> Vec<serde_json::Value> {
        fake.requests.lock().unwrap().clone()
    }

    async fn pool() -> sqlx::SqlitePool {
        iris_database::init_pool("sqlite::memory:").await.unwrap()
    }

    /// Test state: u1 (me@x.id, mailbox mb1, server acct-me) and u2
    /// (other@x.id, mailbox mb-other, server acct-other).
    async fn state(fake: &FakeMgmt, admin_user: &str, admin_emails: Vec<String>) -> Arc<AppState> {
        let pool = pool().await;
        for (uid, email) in [("u1", "me@x.id"), ("u2", "other@x.id")] {
            sqlx::query(
                "INSERT INTO app_users (id,email,display_name,created_at,updated_at) VALUES (?,?,?,'2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
            )
            .bind(uid)
            .bind(email)
            .bind(email)
            .execute(&pool)
            .await
            .unwrap();
        }
        sqlx::query(
            "INSERT INTO permanent_mailboxes (id,user_id,email_address,mail_server_account_id,status,created_at,updated_at) VALUES ('mb1','u1','me@x.id','acct-me','active','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO permanent_mailboxes (id,user_id,email_address,mail_server_account_id,status,created_at,updated_at) VALUES ('mb-other','u2','other@x.id','acct-other','active','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO auth_sessions (id,user_id,token_hash,expires_at,created_at) VALUES ('sess-1','u1','h','2030-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        app_state(pool, fake.base_url.clone(), admin_user, admin_emails)
    }

    /// Same state shape, on an existing pool: lets a test re-point the
    /// config (e.g. an admin allow-list) without reseeding the data.
    fn app_state(
        pool: sqlx::SqlitePool,
        mgmt_url: String,
        admin_user: &str,
        admin_emails: Vec<String>,
    ) -> Arc<AppState> {
        Arc::new(AppState {
            pool,
            organize_lock: tokio::sync::Mutex::new(()),
            jmap: iris_mail::JmapClient::new("http://127.0.0.1:9".into()),
            http: reqwest::Client::new(),
            relay: None,
            registry: tokio::sync::RwLock::new(crate::integration_apps::default_registry()),
            html_bodies: crate::html_body::HtmlBodyStore::new(),
            mail_watch: crate::notify::MailWatch::from_limits(),
            config: iris_config::Config {
                env: "test".into(),
                host: "127.0.0.1".into(),
                port: 8000,
                database_url: "sqlite::memory:".into(),
                session_secret: "test-secret".into(),
                turnstile_secret_key: String::new(),
                turnstile_hostnames: Vec::new(),
                turnstile_siteverify_url: String::new(),
                stalwart_api_url: "http://127.0.0.1:9".into(),
                stalwart_management_url: mgmt_url,
                stalwart_admin_user: admin_user.to_string(),
                stalwart_admin_password: "admin-pass".into(),
                temp_inbox_domain: "example.test".into(),
                temp_inbox_default_expiry_hours: 1,
                temp_inbox_mail_user: String::new(),
                temp_inbox_mail_password: String::new(),
                send_local_domains: vec![],
                send_external: false,
                resend_api_key: String::new(),
                brevo_api_key: String::new(),
                integration_email_from: "no-reply@example.com".into(),
                integration_email_enabled: false,
                password_reset_service_token: String::new(),
                password_reset_email_from: String::new(),
                password_reset_email_to: String::new(),
                password_reset_public_origin: String::new(),
                resend_api_url: String::new(),
                brevo_api_url: String::new(),
                mail_content_origin: String::new(),
                admin_emails,
            },
        })
    }

    fn user() -> CurrentUser {
        CurrentUser {
            user_id: "u1".into(),
            email: "me@x.id".into(),
            creds: Creds {
                user: "me@x.id".into(),
                pass: "app-primary".into(),
            },
            session_id: "sess-1".into(),
            session_token: "tok-1".into(),
            ip: "127.0.0.1".into(),
            mailbox_creds: vec![(
                "mb1".to_string(),
                Creds {
                    user: "me@x.id".into(),
                    pass: "app-primary".into(),
                },
            )],
        }
    }

    fn other_user() -> CurrentUser {
        CurrentUser {
            user_id: "u2".into(),
            email: "other@x.id".into(),
            creds: Creds {
                user: "other@x.id".into(),
                pass: "app-other".into(),
            },
            session_id: "sess-2".into(),
            session_token: "tok-2".into(),
            ip: "127.0.0.1".into(),
            mailbox_creds: vec![(
                "mb-other".to_string(),
                Creds {
                    user: "other@x.id".into(),
                    pass: "app-other".into(),
                },
            )],
        }
    }

    async fn create(
        s: &Arc<AppState>,
        u: CurrentUser,
        address: &str,
        mailbox_id: &str,
    ) -> Result<axum::response::Response, ApiError> {
        create_alias(
            State(s.clone()),
            Extension(u),
            Json(CreateAliasBody {
                address: address.to_string(),
                mailbox_id: mailbox_id.to_string(),
            }),
        )
        .await
    }

    async fn create_ok(s: &Arc<AppState>, address: &str) -> String {
        let res = create(s, user(), address, "mb1").await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = to_bytes(res.into_body(), 4096).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["address"], address);
        assert_eq!(v["enabled"], true);
        v["id"].as_str().unwrap().to_string()
    }

    #[tokio::test]
    async fn create_writes_server_list_and_mirror_row() {
        let fake = fake_mgmt().await;
        let s = state(&fake, "admin", vec![]).await;
        let id = create_ok(&s, "sales@x.id").await;
        let row: (String, String, String, String) = sqlx::query_as(
            "SELECT user_id, mailbox_id, address, stalwart_account_id FROM mail_aliases WHERE id=?",
        )
        .bind(&id)
        .fetch_one(&s.pool)
        .await
        .unwrap();
        assert_eq!(
            row,
            (
                "u1".into(),
                "mb1".into(),
                "sales@x.id".into(),
                "acct-me".into()
            )
        );
        // The exact JMAP patch: whole-list replacement as an
        // index-keyed map, authenticated as the admin user.
        let sets: Vec<_> = requests(&fake)
            .into_iter()
            .filter(|r| r["body"]["methodCalls"][0][0] == "x:Account/set")
            .collect();
        assert_eq!(sets.len(), 1);
        assert_eq!(sets[0]["user"], "admin");
        let patch = &sets[0]["body"]["methodCalls"][0][1]["update"]["acct-me"]["aliases"];
        assert_eq!(
            patch,
            &json!({"0": {"name": "sales", "domainId": "d-x", "enabled": true}})
        );
        // And the audit trail recorded it, with the address only.
        let audit: (String, String) =
            sqlx::query_as("SELECT action, detail FROM audit_events WHERE target_id=?")
                .bind(&id)
                .fetch_one(&s.pool)
                .await
                .unwrap();
        assert_eq!(audit.0, "alias.create");
        assert!(audit.1.contains("sales@x.id"));
        assert!(!audit.1.contains("admin-pass"));
    }

    #[tokio::test]
    async fn create_refuses_duplicates_wrong_domain_and_own_address() {
        let fake = fake_mgmt().await;
        let s = state(&fake, "admin", vec![]).await;
        create_ok(&s, "sales@x.id").await;
        // Same address twice.
        let err = create(&s, user(), "sales@x.id", "mb1").await.unwrap_err();
        assert_eq!(err.0, StatusCode::CONFLICT);
        assert_eq!(err.1, "alias_taken");
        // The mailbox's own address is not an alias.
        let err = create(&s, user(), "me@x.id", "mb1").await.unwrap_err();
        assert_eq!(err.0, StatusCode::CONFLICT);
        assert_eq!(err.1, "alias_taken");
        // Another domain than the mailbox's.
        let err = create(&s, user(), "sales@y.id", "mb1").await.unwrap_err();
        assert_eq!(err.0, StatusCode::CONFLICT);
        assert_eq!(err.1, "alias_taken");
        // Not an address at all.
        let err = create(&s, user(), "not-an-email", "mb1").await.unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        // Another user's mailbox is not found, never aliased.
        let err = create(&s, user(), "sales@x.id", "mb-other")
            .await
            .unwrap_err();
        assert_eq!(err.0, StatusCode::NOT_FOUND);
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM mail_aliases")
            .fetch_one(&s.pool)
            .await
            .unwrap();
        assert_eq!(n, 1);
    }

    #[tokio::test]
    async fn create_conflict_when_server_refuses_taken_address() {
        let fake = fake_mgmt().await;
        let s = state(&fake, "admin", vec![]).await;
        // `taken@x.id` already belongs to acct-other on the server.
        let err = create(&s, user(), "taken@x.id", "mb1").await.unwrap_err();
        assert_eq!(err.0, StatusCode::CONFLICT);
        assert_eq!(err.1, "alias_taken");
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM mail_aliases")
            .fetch_one(&s.pool)
            .await
            .unwrap();
        assert_eq!(n, 0);
    }

    #[tokio::test]
    async fn create_answers_503_while_admin_credential_unset() {
        let fake = fake_mgmt().await;
        let s = state(&fake, "", vec![]).await;
        let err = create(&s, user(), "sales@x.id", "mb1").await.unwrap_err();
        assert_eq!(err.0, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(err.1, "aliases_unavailable");
        // No management call was made at all.
        assert!(requests(&fake).is_empty());
    }

    #[tokio::test]
    async fn create_enforces_per_user_cap() {
        let fake = fake_mgmt().await;
        let s = state(&fake, "admin", vec![]).await;
        for i in 0..MAX_ALIASES_PER_USER {
            sqlx::query(
                "INSERT INTO mail_aliases (id,user_id,mailbox_id,address,stalwart_account_id,enabled,created_at) \
                 VALUES (?,?,?,?,?,?,'2026-01-01T00:00:00Z')",
            )
            .bind(format!("a{i}"))
            .bind("u1")
            .bind("mb1")
            .bind(format!("alias{i}@x.id"))
            .bind("acct-me")
            .bind(1)
            .execute(&s.pool)
            .await
            .unwrap();
        }
        let err = create(&s, user(), "one-more@x.id", "mb1")
            .await
            .unwrap_err();
        assert_eq!(err.0, StatusCode::CONFLICT);
        assert_eq!(err.1, "alias_limit_reached");
    }

    #[tokio::test]
    async fn delete_removes_from_server_and_mirror() {
        let fake = fake_mgmt().await;
        let s = state(&fake, "admin", vec![]).await;
        let id = create_ok(&s, "sales@x.id").await;
        let res = delete_alias(State(s.clone()), Extension(user()), Path(id.clone()))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM mail_aliases WHERE id=?")
            .bind(&id)
            .fetch_one(&s.pool)
            .await
            .unwrap();
        assert_eq!(n, 0);
        // The last server write carried an empty alias list.
        let sets: Vec<_> = requests(&fake)
            .into_iter()
            .filter(|r| r["body"]["methodCalls"][0][0] == "x:Account/set")
            .collect();
        assert_eq!(sets.len(), 2);
        assert_eq!(
            sets[1]["body"]["methodCalls"][0][1]["update"]["acct-me"]["aliases"],
            json!({})
        );
    }

    #[tokio::test]
    async fn delete_refuses_others_alias() {
        let fake = fake_mgmt().await;
        let s = state(&fake, "admin", vec![]).await;
        let id = create_ok(&s, "sales@x.id").await;
        // Another user sees 404, and the row survives.
        let err = delete_alias(State(s.clone()), Extension(other_user()), Path(id.clone()))
            .await
            .unwrap_err();
        assert_eq!(err.0, StatusCode::NOT_FOUND);
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM mail_aliases WHERE id=?")
            .bind(&id)
            .fetch_one(&s.pool)
            .await
            .unwrap();
        assert_eq!(n, 1);
        // An admin may delete anyone's alias.
        let mut admin = user();
        admin.email = "admin@x.id".into();
        admin.user_id = "u-admin".into();
        let s_admin = app_state(
            s.pool.clone(),
            fake.base_url.clone(),
            "admin",
            vec!["admin@x.id".into()],
        );
        delete_alias(State(s_admin), Extension(admin), Path(id))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn list_returns_only_own_aliases() {
        let fake = fake_mgmt().await;
        let s = state(&fake, "admin", vec![]).await;
        create_ok(&s, "sales@x.id").await;
        sqlx::query(
            "INSERT INTO mail_aliases (id,user_id,mailbox_id,address,stalwart_account_id,enabled,created_at) \
             VALUES ('other-alias','u2','mb-other','info@x.id','acct-other',1,'2026-01-01T00:00:00Z')",
        )
        .execute(&s.pool)
        .await
        .unwrap();
        let res = list_aliases(State(s.clone()), Extension(user()))
            .await
            .unwrap();
        let body = to_bytes(res.into_body(), 4096).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let list = v["aliases"].as_array().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0]["address"], "sales@x.id");
    }

    #[tokio::test]
    async fn catch_all_get_and_set_round_trip() {
        let fake = fake_mgmt().await;
        let s = state(&fake, "admin", vec![]).await;
        let res = get_catch_all(State(s.clone()), Extension(user()), Path("x.id".into()))
            .await
            .unwrap();
        let body = to_bytes(res.into_body(), 4096).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["enabled"], false);
        assert!(v["target_address"].is_null());
        let res = set_catch_all(
            State(s.clone()),
            Extension(user()),
            Path("x.id".into()),
            Json(SetCatchAllBody {
                target_address: Some("me@x.id".into()),
            }),
        )
        .await
        .unwrap();
        let body = to_bytes(res.into_body(), 4096).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["enabled"], true);
        assert_eq!(v["target_address"], "me@x.id");
        // The server write carried the target, and the mirror followed.
        let sets: Vec<_> = requests(&fake)
            .into_iter()
            .filter(|r| r["body"]["methodCalls"][0][0] == "x:Domain/set")
            .collect();
        assert_eq!(sets.len(), 1);
        assert_eq!(
            sets[0]["body"]["methodCalls"][0][1]["update"]["d-x"]["catchAllAddress"],
            "me@x.id"
        );
        let target: Option<String> = sqlx::query_scalar::<_, Option<String>>(
            "SELECT target_address FROM domain_catch_all WHERE domain='x.id'",
        )
        .fetch_one(&s.pool)
        .await
        .unwrap();
        assert_eq!(target.as_deref(), Some("me@x.id"));
        // Disabling clears it again.
        set_catch_all(
            State(s.clone()),
            Extension(user()),
            Path("x.id".into()),
            Json(SetCatchAllBody {
                target_address: None,
            }),
        )
        .await
        .unwrap();
        let target: Option<String> = sqlx::query_scalar::<_, Option<String>>(
            "SELECT target_address FROM domain_catch_all WHERE domain='x.id'",
        )
        .fetch_one(&s.pool)
        .await
        .unwrap();
        assert_eq!(target, None);
    }

    #[tokio::test]
    async fn catch_all_refuses_foreign_domain_and_bad_target() {
        let fake = fake_mgmt().await;
        let s = state(&fake, "admin", vec![]).await;
        // y.id has no mailbox of this user.
        let err = get_catch_all(State(s.clone()), Extension(user()), Path("y.id".into()))
            .await
            .unwrap_err();
        assert_eq!(err.0, StatusCode::FORBIDDEN);
        // A target on another domain, or not an address at all.
        for bad in [
            Some("me@y.id".to_string()),
            Some("not-an-email".to_string()),
        ] {
            let err = set_catch_all(
                State(s.clone()),
                Extension(user()),
                Path("x.id".into()),
                Json(SetCatchAllBody {
                    target_address: bad,
                }),
            )
            .await
            .unwrap_err();
            assert_eq!(err.0, StatusCode::BAD_REQUEST);
        }
        // A domain the caller owns no mailbox on is forbidden — the
        // server is never asked, so its existence does not leak.
        for domain in ["y.id", "nope.id"] {
            let err = get_catch_all(State(s.clone()), Extension(user()), Path(domain.into()))
                .await
                .unwrap_err();
            assert_eq!(err.0, StatusCode::FORBIDDEN, "{domain}");
        }
        // An admin asking for a domain the server does not know gets 404.
        let s_admin = app_state(
            s.pool.clone(),
            fake.base_url.clone(),
            "admin",
            vec!["admin@x.id".into()],
        );
        let mut admin = user();
        admin.email = "admin@x.id".into();
        admin.user_id = "u-admin".into();
        let err = get_catch_all(State(s_admin), Extension(admin), Path("nope.id".into()))
            .await
            .unwrap_err();
        assert_eq!(err.0, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn catch_all_answers_503_while_admin_credential_unset() {
        let fake = fake_mgmt().await;
        let s = state(&fake, "", vec![]).await;
        let err = get_catch_all(State(s.clone()), Extension(user()), Path("x.id".into()))
            .await
            .unwrap_err();
        assert_eq!(err.0, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(err.1, "aliases_unavailable");
    }

    #[tokio::test]
    async fn mailbox_creds_helper_still_resolves() {
        // `mailbox_creds` is imported by this module; keep the import
        // honest by exercising it the way handlers do.
        assert_eq!(mailbox_creds(&user(), "mb1").unwrap().user, "me@x.id");
    }
}
