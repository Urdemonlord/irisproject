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
//! read, modified and written back whole, so two concurrent IRIS
//! requests must not interleave a read-modify-write. That lock cannot
//! cover an out-of-band writer (WebUI/CLI), and Stalwart's management
//! API offers no conditional write for the alias list (no etag or
//! `ifInState` on `x:Account/set`: it is an unconditional whole-list
//! replacement), so whole-list writes additionally go through
//! `update_alias_list`: re-read before the write, compare with the
//! list the change was computed from, verify after the write, and
//! retry while the wanted state does not stick. That closes the race
//! for every change the re-read or the verify can see, and repairs a
//! concurrent writer overwriting our change. One window remains and
//! is inherent to the API, not to this code: an out-of-band change
//! landing between the final pre-write read and the `set` is still
//! overwritten, because nothing in the read or the write can observe
//! it. The window is a single round trip, and the verify step means
//! the loss is one-sided (our change can be re-applied; theirs
//! cannot be detected).
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

/// True when `list` already holds an alias with this local part on
/// this domain. Dedupe key everywhere the list is merged: lowercased
/// name plus domain id.
fn alias_present(list: &[EmailAlias], name: &str, domain_id: &str) -> bool {
    list.iter()
        .any(|a| a.name.eq_ignore_ascii_case(name) && a.domain_id == domain_id)
}

/// True when two alias lists hold the same entries, ignoring order:
/// list order carries no meaning, and an out-of-band writer may
/// return the entries in a different order than we wrote them.
fn same_alias_list(a: &[EmailAlias], b: &[EmailAlias]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut a: Vec<&EmailAlias> = a.iter().collect();
    let mut b: Vec<&EmailAlias> = b.iter().collect();
    let key = |e: &&EmailAlias| (e.name.to_lowercase(), e.domain_id.clone(), e.enabled);
    a.sort_by_key(key);
    b.sort_by_key(key);
    a == b
}

/// Maximum rounds of the read-compare-write-verify loop for one
/// whole-list alias write. One round is enough when nothing moves
/// under us; three bounds the retry while a concurrent writer is
/// actively rewriting the same list.
const ALIAS_WRITE_ROUNDS: usize = 3;

/// Applies `mutate` to the account's current server alias list and
/// writes it back, with a bounded verify-and-retry loop.
///
/// Each round re-reads the list, applies `mutate` to what the server
/// holds now (never to a stale snapshot), writes the result, and
/// re-reads again to verify the wanted state stuck. The round is
/// skipped with no write when the state already holds, and retried
/// when the verify shows a concurrent whole-list writer overwrote
/// our change. Returns the verified post-write list and whether a
/// `set` was issued that the server holds (callers need the flag to
/// know whether a failed later step must undo a change of ours, or
/// leave an entry that predates us alone).
///
/// Error semantics, kept from the callers' previous inline code: a
/// `Conflict`/`Protocol` refusal is definitive and returns at once
/// (retrying the same list cannot help); an `Unavailable` (transport
/// or timeout, where the write may have landed while the response
/// was lost) re-reads once, and only a list that already holds the
/// wanted state counts as landed, otherwise the original error is
/// returned. Rounds exhausted without the state sticking is an
/// `Unavailable`: the request may be retried, and no mirror row is
/// written on the strength of an unconfirmed server list.
async fn update_alias_list(
    admin: &AdminClient,
    account_id: &str,
    mut mutate: impl FnMut(&mut Vec<EmailAlias>),
) -> Result<(Vec<EmailAlias>, bool), iris_mail::MailError> {
    for _ in 0..ALIAS_WRITE_ROUNDS {
        let fresh = admin.get_account_aliases(account_id).await?;
        let mut desired = fresh.clone();
        mutate(&mut desired);
        if same_alias_list(&desired, &fresh) {
            // The wanted state already holds: another attempt, or an
            // out-of-band writer, already made this change. No write.
            return Ok((fresh, false));
        }
        match admin.set_account_aliases(account_id, &desired).await {
            Ok(()) => {
                let verified = admin.get_account_aliases(account_id).await?;
                let mut after = verified.clone();
                mutate(&mut after);
                if same_alias_list(&after, &verified) {
                    return Ok((verified, true));
                }
                // Our write did not stick: a concurrent whole-list
                // writer overwrote it after our set. Loop and
                // re-apply on top of what the server holds now.
            }
            Err(e) => {
                if matches!(e, iris_mail::MailError::Unavailable(_)) {
                    // Ambiguous: the write may have landed while the
                    // response was lost. Re-read; only a list that
                    // already holds the wanted state counts as
                    // landed. (A Conflict refusal is definitive and
                    // needs no reconcile.)
                    match admin.get_account_aliases(account_id).await {
                        Ok(current) => {
                            let mut after = current.clone();
                            mutate(&mut after);
                            if same_alias_list(&after, &current) {
                                return Ok((current, true));
                            }
                            return Err(e);
                        }
                        Err(read_err) => {
                            tracing::error!(
                                "update_alias_list: reconcile re-read failed for account {account_id}: {read_err}"
                            );
                            return Err(e);
                        }
                    }
                }
                return Err(e);
            }
        }
    }
    Err(iris_mail::MailError::Unavailable(
        "alias list did not converge after repeated writes; retry the request".into(),
    ))
}

/// GET /api/v1/aliases. The caller's own aliases, oldest first.
pub(crate) async fn list_aliases(State(s): S, Extension(u): Extension<CurrentUser>) -> ApiResult {
    // Fail closed like the other endpoints: without the admin
    // credential the mirror may hold aliases the server no longer
    // has, so answer 503 instead of serving stale rows.
    admin_client(&s)?;
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
    let Some(domain) = domain_of(&address) else {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_request"));
    };
    let local = address
        .split_once('@')
        .map(|(local, _)| local.to_string())
        .unwrap_or_default();
    // Serialize the whole read-modify-write: the mailbox lookup, the
    // cap and mirror checks, the server read and the mirror insert
    // all happen under the lock, so two concurrent creates cannot
    // both pass at 19 aliases and insert the 20th and 21st, and a
    // concurrent unlink cannot delete the mailbox between the lookup
    // and the server write (the lookup would then be stale and the
    // alias would land on a deleted mailbox).
    let _guard = s.organize_lock.lock().await;
    let mb = owned_mailbox(&s, &u, &b.mailbox_id).await?;
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
    let domain_info = admin
        .resolve_domain(domain)
        .await?
        .ok_or(ApiError(StatusCode::NOT_FOUND, "not_found"))?;
    let mut aliases = admin.get_account_aliases(&mb.account_id).await?;
    // Whether the server change actually landed: only then does a
    // failed mirror insert need to undo it.
    let mut server_changed = false;
    if !alias_present(&aliases, &local, &domain_info.id) {
        // Write through the verify-and-retry loop (see
        // `update_alias_list`): the change is computed from a fresh
        // read, so an alias added out of band between the reads is
        // kept instead of erased by our whole-list write, and a
        // concurrent writer overwriting our change is repaired.
        (aliases, server_changed) = update_alias_list(&admin, &mb.account_id, |list| {
            if !alias_present(list, &local, &domain_info.id) {
                list.push(EmailAlias {
                    name: local.clone(),
                    domain_id: domain_info.id.clone(),
                    enabled: true,
                });
            }
        })
        .await?;
    }
    // The entry we now own on the server: written by us above, a
    // leftover of a previous attempt whose mirror write never landed
    // (self-heal), or added out of band while we worked. Self-heal
    // only reconciles an entry that can be ours. IRIS only ever
    // creates enabled aliases and has no toggle endpoint, so a
    // disabled entry is provably not from a failed IRIS create: it
    // is refused as taken, never adopted and never reported as
    // enabled while Stalwart still rejects its mail. An enabled
    // entry with no mirror row cannot be proven either way from the
    // server data (an EmailAlias carries no owner marker), so it is
    // adopted: adoption changes nothing on the server, it only
    // records the mirror row for an alias that already delivers into
    // the caller's own mailbox, and refusing it would orphan every
    // alias whose mirror write failed, which is the failure the
    // self-heal exists for.
    let Some(server_entry) = aliases
        .iter()
        .find(|a| a.name.eq_ignore_ascii_case(&local) && a.domain_id == domain_info.id)
    else {
        return Err(ApiError(StatusCode::BAD_GATEWAY, "mail_server_unavailable"));
    };
    if !server_entry.enabled {
        return Err(ApiError(StatusCode::CONFLICT, "alias_taken"));
    }
    let enabled = server_entry.enabled;
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
    .bind(i64::from(enabled))
    .bind(&now)
    .execute(&s.pool)
    .await
    {
        if server_changed {
            // Undo by removing exactly our entry from the list as
            // the server now holds it, never by popping the last
            // entry: the list is re-read first, so out-of-band
            // aliases added since are kept, and the verify-and-retry
            // loop applies here too.
            if let Err(undo_err) = update_alias_list(&admin, &mb.account_id, |list| {
                list.retain(|a| {
                    !(a.name.eq_ignore_ascii_case(&local) && a.domain_id == domain_info.id)
                });
            })
            .await
            {
                tracing::error!(
                    "create_alias: failed to undo server alias for account {} address {address}: {undo_err}",
                    mb.account_id
                );
            }
        }
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
    Ok(Json(alias_json(
        &alias_id,
        &address,
        &b.mailbox_id,
        enabled,
        &now,
    ))
    .into_response())
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
    // The row is loaded and authorized only after the lock is held: a
    // delete that waited behind a delete-and-recreate must see the new
    // row, never remove the new server alias while deleting the old
    // mirror id.
    let _guard = s.organize_lock.lock().await;
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
    let Some(domain) = domain_of(&address) else {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_request"));
    };
    let local = address
        .split_once('@')
        .map(|(local, _)| local)
        .unwrap_or_default();
    let domain_id = admin
        .resolve_domain(domain)
        .await?
        .map(|d| d.id)
        .unwrap_or_default();
    // The mirror row is deleted only once the server list is updated;
    // a missing account (mailbox deleted out of band) still converges
    // by dropping the orphan row. `removed` remembers the entry the
    // server write took away, so a failed mirror delete can put it
    // back. It is set only when this request actually removed the
    // entry: when the server no longer holds it, there is nothing to
    // restore.
    let mut removed: Option<EmailAlias> = None;
    match admin.get_account_aliases(&account_id).await {
        Ok(aliases) => {
            if alias_present(&aliases, local, &domain_id) {
                // The removal goes through the same verify-and-retry
                // loop as create (see `update_alias_list`): an alias
                // added out of band between the reads survives our
                // whole-list write, and a concurrent writer putting
                // our entry back is retried instead of leaving a
                // live alias with no mirror row. What this request's
                // accepted write removed is captured inside the
                // mutate closure: the closure runs on the fresh list
                // each round, so it sees the entry exactly as the
                // server held it when the write was computed —
                // including an out-of-band disable that landed after
                // the preliminary read above. Each round re-captures
                // (the closure also runs on the verify copy, where
                // the entry is already gone), so the value reflects
                // the last write attempt. Restoring that entry,
                // rather than the preliminary snapshot, is what keeps
                // a failed mirror delete from silently re-enabling an
                // alias somebody else disabled.
                let mut removed_entry: Option<EmailAlias> = None;
                let (_after, wrote) = update_alias_list(&admin, &account_id, |list| {
                    list.retain(|a| {
                        let hit = a.name.eq_ignore_ascii_case(local) && a.domain_id == domain_id;
                        if hit {
                            removed_entry = Some(a.clone());
                        }
                        !hit
                    });
                })
                .await?;
                if wrote {
                    removed = removed_entry;
                }
            }
        }
        Err(iris_mail::MailError::NotFound) => {}
        Err(e) => return Err(e.into()),
    }
    if let Err(e) = sqlx::query("DELETE FROM mail_aliases WHERE id=?")
        .bind(&alias_id)
        .execute(&s.pool)
        .await
    {
        // The server alias is already gone: put it back rather than
        // leaving server and mirror inconsistent. The restore is a
        // fresh read-modify-write through the same loop, not a
        // replay of a stale list, so out-of-band aliases added
        // since are kept.
        if let Some(entry) = removed {
            if let Err(undo_err) = update_alias_list(&admin, &account_id, |list| {
                if !alias_present(list, &entry.name, &entry.domain_id) {
                    list.push(entry.clone());
                }
            })
            .await
            {
                tracing::error!(
                    "delete_alias: failed to restore server alias for account {account_id} address {address}: {undo_err}"
                );
            }
        }
        return Err(e.into());
    }
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

/// GET /api/v1/domains. The domains the caller can use: every server
/// domain for an admin, the distinct domains of the caller's own
/// active mailboxes otherwise. Sorted by name.
pub(crate) async fn list_domains(State(s): S, Extension(u): Extension<CurrentUser>) -> ApiResult {
    let admin = admin_client(&s)?;
    let mut names: Vec<String> = if iris_config::is_admin_email(&s.config.admin_emails, &u.email) {
        admin
            .list_domains()
            .await?
            .into_iter()
            .map(|d| d.name)
            .collect()
    } else {
        let emails: Vec<String> = sqlx::query_scalar(
            "SELECT email_address FROM permanent_mailboxes WHERE user_id=? AND status='active'",
        )
        .bind(&u.user_id)
        .fetch_all(&s.pool)
        .await?;
        emails
            .iter()
            .filter_map(|e| domain_of(e).map(|d| d.to_lowercase()))
            .collect()
    };
    names.sort();
    names.dedup();
    let domains: Vec<_> = names.iter().map(|n| json!({ "name": n })).collect();
    Ok(Json(json!({ "domains": domains })).into_response())
}

/// GET /api/v1/domains/catch-all. Every catch-all setting the caller
/// may manage, in one request backed by a single domain-catalog read:
/// the web used to GET each domain in turn (four concurrent workers,
/// still one whole-catalog read per domain under `organize_lock`),
/// which made an admin with many domains pay one catalog read per
/// domain and apply the results in domain order, so a 503 on an early
/// domain could swallow a 401 on a later one and a failed domain
/// silently rendered Off. One call returns the settings for every
/// domain the caller may manage (every server domain for an admin,
/// the caller's own mailbox domains otherwise) plus the managed
/// domains the server does not know, so the web can render those as
/// missing instead of a global error. The mirror
/// (`domain_catch_all`) is not refreshed here: nothing in production
/// reads it (only the per-domain GET upserts it), and a read that
/// wrote rows would need the lock for no benefit.
pub(crate) async fn list_catch_alls(
    State(s): S,
    Extension(u): Extension<CurrentUser>,
) -> ApiResult {
    let admin = admin_client(&s)?;
    let is_admin = iris_config::is_admin_email(&s.config.admin_emails, &u.email);
    let mut managed: Vec<String> = if is_admin {
        Vec::new()
    } else {
        let emails: Vec<String> = sqlx::query_scalar(
            "SELECT email_address FROM permanent_mailboxes WHERE user_id=? AND status='active'",
        )
        .bind(&u.user_id)
        .fetch_all(&s.pool)
        .await?;
        emails
            .iter()
            .filter_map(|e| domain_of(e).map(|d| d.to_lowercase()))
            .collect()
    };
    let catalog = admin.list_domains().await?;
    let mut catch_alls = Vec::new();
    let mut missing = Vec::new();
    if is_admin {
        for d in &catalog {
            catch_alls.push(catch_all_json(
                &d.name.to_lowercase(),
                d.catch_all.as_deref(),
            ));
        }
    } else {
        managed.sort();
        managed.dedup();
        for name in &managed {
            match catalog.iter().find(|d| d.name.eq_ignore_ascii_case(name)) {
                Some(d) => catch_alls.push(catch_all_json(
                    &d.name.to_lowercase(),
                    d.catch_all.as_deref(),
                )),
                None => missing.push(json!({ "name": name })),
            }
        }
    }
    Ok(Json(json!({ "catch_alls": catch_alls, "missing": missing })).into_response())
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
    // Hold the lock from the server read through the mirror upsert,
    // so a concurrent PUT cannot commit a new target before this GET
    // writes its stale target back to `domain_catch_all`.
    let _guard = s.organize_lock.lock().await;
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
    // The lock comes before the ownership check: a mailbox that
    // passes `can_manage_domain` could be unlinked before this PUT
    // takes the lock, and the PUT would then change a domain the
    // caller no longer owns. Holding the lock across the check and
    // the write makes the check see the committed unlink.
    let _guard = s.organize_lock.lock().await;
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
    let info = admin
        .resolve_domain(&domain)
        .await?
        .ok_or(ApiError(StatusCode::NOT_FOUND, "not_found"))?;
    let previous_target = info.catch_all.clone();
    // An ambiguous write (transport failure: the update may have
    // landed while the response was lost) is reconciled by reading
    // the domain back: if the requested target is there, the write
    // landed and the mirror/audit steps below still run; if the
    // server still holds something else, the original error is
    // returned; if the readback itself fails, the original error is
    // returned and the failure is logged — the mirror keeps the old
    // target and the next GET refreshes it from the server, so the
    // disagreement stays visible instead of silent.
    if let Err(e) = admin.set_catch_all(&info.id, target.as_deref()).await {
        if matches!(e, iris_mail::MailError::Unavailable(_)) {
            match admin.resolve_domain(&domain).await {
                Ok(Some(current)) if current.catch_all == target => {}
                Ok(_) => return Err(e.into()),
                Err(read_err) => {
                    tracing::error!(
                        "set_catch_all: reconcile readback failed for domain {domain}: {read_err}"
                    );
                    return Err(e.into());
                }
            }
        } else {
            return Err(e.into());
        }
    }
    if let Err(e) = sqlx::query(
        "INSERT INTO domain_catch_all (domain,target_address,updated_at) VALUES (?,?,?) \
         ON CONFLICT(domain) DO UPDATE SET target_address=excluded.target_address, updated_at=excluded.updated_at",
    )
    .bind(&domain)
    .bind(&target)
    .bind(iris_auth::now())
    .execute(&s.pool)
    .await
    {
        // The Stalwart change already landed: restore the previous
        // target rather than leaving server and mirror disagreeing.
        if let Err(restore_err) = admin
            .set_catch_all(&info.id, previous_target.as_deref())
            .await
        {
            tracing::error!(
                "set_catch_all: failed to restore previous catch-all for domain {domain}: {restore_err}"
            );
        }
        return Err(e.into());
    }
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
    use std::sync::{
        atomic::{AtomicBool, AtomicU8, Ordering},
        Arc, Mutex,
    };
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
    /// and body. Tests may seed server aliases directly, and may arm
    /// the next `x:Account/set` to drop its response — with the
    /// update committed (a lost response after a landed write) or
    /// without it (the write never landed) — or to have its committed
    /// list replaced right after, as if a concurrent out-of-band
    /// writer overwrote it.
    /// Armed one-shot hook for `disable_alias_on_get`: (account,
    /// alias name, domain id, Nth get) — disable that entry on the
    /// Nth `x:Account/get` for its account.
    type DisableOnGet = Arc<Mutex<Option<(String, String, String, usize)>>>;

    struct FakeMgmt {
        base_url: String,
        requests: Arc<Mutex<Vec<serde_json::Value>>>,
        state: Arc<Mutex<MgmtState>>,
        set_fail_mode: Arc<AtomicU8>,
        replace_after_set: Arc<Mutex<Option<Vec<serde_json::Value>>>>,
        domain_set_fail_mode: Arc<AtomicU8>,
        disable_on_get: DisableOnGet,
        _task: tokio::task::JoinHandle<()>,
    }

    impl FakeMgmt {
        /// Puts an alias straight onto the server for `account`, as
        /// if another management client (WebUI/CLI, or a previous
        /// attempt whose mirror write never landed) had created it.
        fn seed_alias(&self, account: &str, name: &str, domain_id: &str) {
            self.seed_alias_enabled(account, name, domain_id, true);
        }

        /// Same, with an explicit `enabled` flag: a disabled entry
        /// is what a pre-existing alias disabled out of band looks
        /// like on the server.
        fn seed_alias_enabled(&self, account: &str, name: &str, domain_id: &str, enabled: bool) {
            self.state
                .lock()
                .unwrap()
                .aliases
                .entry(account.to_string())
                .or_default()
                .push(json!({"name": name, "domainId": domain_id, "enabled": enabled}));
        }

        /// The server's current alias list for `account`, in order.
        fn server_aliases(&self, account: &str) -> Vec<serde_json::Value> {
            self.state
                .lock()
                .unwrap()
                .aliases
                .get(account)
                .cloned()
                .unwrap_or_default()
        }

        /// Drops the next `x:Account/set` response without answering.
        /// `commit` decides whether the update still lands on the
        /// server first: true simulates a response lost after the
        /// write, false a write that never happened.
        fn drop_next_set_response(&self, commit: bool) {
            self.set_fail_mode
                .store(if commit { 1 } else { 2 }, Ordering::SeqCst);
        }

        /// Drops the next `x:Domain/set` response without answering,
        /// with the same commit semantics as `drop_next_set_response`.
        fn drop_next_domain_set_response(&self, commit: bool) {
            self.domain_set_fail_mode
                .store(if commit { 1 } else { 2 }, Ordering::SeqCst);
        }

        /// Disables one alias on the Nth `x:Account/get` for its
        /// account, as if another management client disabled it
        /// between two reads of the same handler.
        fn disable_alias_on_get(&self, account: &str, name: &str, domain_id: &str, nth_get: usize) {
            *self.disable_on_get.lock().unwrap() = Some((
                account.to_string(),
                name.to_string(),
                domain_id.to_string(),
                nth_get,
            ));
        }

        /// Replaces the account's alias list right after the next
        /// committed `x:Account/set`, as if a concurrent out-of-band
        /// writer overwrote our whole-list write with its own list.
        /// One-shot: the replacement is consumed by that set.
        fn replace_after_next_set(&self, aliases: Vec<serde_json::Value>) {
            *self.replace_after_set.lock().unwrap() = Some(aliases);
        }
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
        let set_fail_mode = Arc::new(AtomicU8::new(0));
        let set_dropped = Arc::new(AtomicBool::new(false));
        let replace_after_set: Arc<Mutex<Option<Vec<serde_json::Value>>>> =
            Arc::new(Mutex::new(None));
        let domain_set_fail_mode = Arc::new(AtomicU8::new(0));
        let domain_set_dropped = Arc::new(AtomicBool::new(false));
        let disable_on_get: DisableOnGet = Arc::new(Mutex::new(None));
        let get_counts: Arc<Mutex<HashMap<String, usize>>> = Arc::new(Mutex::new(HashMap::new()));
        let task_set_fail_mode = set_fail_mode.clone();
        let task_set_dropped = set_dropped.clone();
        let task_replace_after_set = replace_after_set.clone();
        let task_domain_set_fail_mode = domain_set_fail_mode.clone();
        let task_domain_set_dropped = domain_set_dropped.clone();
        let task_disable_on_get = disable_on_get.clone();
        let task_get_counts = get_counts.clone();
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
                            // One-shot test hook (see
                            // `disable_alias_on_get`): on the Nth get
                            // for this account, disable the armed
                            // entry first, as if another client had
                            // just disabled it.
                            {
                                let mut counts = task_get_counts.lock().unwrap();
                                let n = counts.entry(id.to_string()).or_insert(0);
                                *n += 1;
                                if let Some((acct, name, domain_id, nth)) =
                                    task_disable_on_get.lock().unwrap().clone()
                                {
                                    if acct == id && *n == nth {
                                        if let Some(list) =
                                            task_state.lock().unwrap().aliases.get_mut(&acct)
                                        {
                                            for a in list.iter_mut() {
                                                if a["name"].as_str().unwrap_or("") == name
                                                    && a["domainId"].as_str().unwrap_or("")
                                                        == domain_id
                                                {
                                                    a["enabled"] = json!(false);
                                                }
                                            }
                                        }
                                    }
                                }
                            }
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
                            // another account already owns. An armed
                            // drop mode (see `drop_next_set_response`)
                            // closes the connection without answering:
                            // mode 1 still commits first, mode 2 does
                            // not.
                            let fail_mode = task_set_fail_mode.swap(0, Ordering::SeqCst);
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
                                if !refused && fail_mode != 2 {
                                    st.aliases.insert(acct.clone(), new_list);
                                }
                            }
                            // An armed concurrent writer: right after
                            // our committed set, its own whole-list
                            // write replaces the list we just wrote.
                            if !refused && fail_mode != 2 {
                                if let Some(replacement) =
                                    task_replace_after_set.lock().unwrap().take()
                                {
                                    for acct in update.keys() {
                                        st.aliases.insert(acct.clone(), replacement.clone());
                                    }
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
                                if fail_mode != 0 {
                                    task_set_dropped.store(true, Ordering::SeqCst);
                                }
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
                            // An armed drop mode (see
                            // `drop_next_domain_set_response`) closes
                            // the connection without answering: mode 1
                            // still commits first, mode 2 does not.
                            let fail_mode = task_domain_set_fail_mode.swap(0, Ordering::SeqCst);
                            let mut st = task_state.lock().unwrap();
                            let update = args["update"].as_object().cloned().unwrap_or_default();
                            for (domain_id, patch) in &update {
                                if fail_mode != 2 {
                                    st.catch_all.insert(
                                        domain_id.clone(),
                                        patch["catchAllAddress"].as_str().map(str::to_string),
                                    );
                                }
                            }
                            if fail_mode != 0 {
                                task_domain_set_dropped.store(true, Ordering::SeqCst);
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
                // An armed drop: close without answering, so the
                // client sees a transport failure.
                if task_set_dropped.swap(false, Ordering::SeqCst)
                    || task_domain_set_dropped.swap(false, Ordering::SeqCst)
                {
                    let _ = stream.shutdown().await;
                    continue;
                }
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
            state,
            set_fail_mode,
            replace_after_set,
            domain_set_fail_mode,
            disable_on_get,
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
    async fn create_self_heals_when_server_already_has_alias() {
        let fake = fake_mgmt().await;
        let s = state(&fake, "admin", vec![]).await;
        // A previous attempt committed on the server, but its mirror
        // write never landed: the server already holds the alias and
        // no row owns it. Creating it again must not answer
        // `alias_taken`; it heals the mirror instead.
        fake.seed_alias("acct-me", "sales", "d-x");
        let id = create_ok(&s, "sales@x.id").await;
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM mail_aliases WHERE id=?")
            .bind(&id)
            .fetch_one(&s.pool)
            .await
            .unwrap();
        assert_eq!(n, 1);
        // No server write was needed: the alias was already there,
        // and it was not duplicated.
        let sets: Vec<_> = requests(&fake)
            .into_iter()
            .filter(|r| r["body"]["methodCalls"][0][0] == "x:Account/set")
            .collect();
        assert_eq!(sets.len(), 0);
        assert_eq!(
            fake.server_aliases("acct-me"),
            vec![json!({"name": "sales", "domainId": "d-x", "enabled": true})]
        );
    }

    #[tokio::test]
    async fn create_refuses_disabled_server_alias() {
        let fake = fake_mgmt().await;
        let s = state(&fake, "admin", vec![]).await;
        // A matching server alias that is disabled cannot be from a
        // failed IRIS create (IRIS only creates enabled aliases):
        // adopting it would report the alias as enabled while
        // Stalwart still rejects its mail.
        fake.seed_alias_enabled("acct-me", "sales", "d-x", false);
        let err = create(&s, user(), "sales@x.id", "mb1").await.unwrap_err();
        assert_eq!(err.0, StatusCode::CONFLICT);
        assert_eq!(err.1, "alias_taken");
        // No mirror row, and no server write touched the list: the
        // disabled entry is left exactly as it was.
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM mail_aliases")
            .fetch_one(&s.pool)
            .await
            .unwrap();
        assert_eq!(n, 0);
        let sets: Vec<_> = requests(&fake)
            .into_iter()
            .filter(|r| r["body"]["methodCalls"][0][0] == "x:Account/set")
            .collect();
        assert_eq!(sets.len(), 0);
        assert_eq!(
            fake.server_aliases("acct-me"),
            vec![json!({"name": "sales", "domainId": "d-x", "enabled": false})]
        );
    }

    #[tokio::test]
    async fn create_retries_when_concurrent_writer_overwrites_set() {
        let fake = fake_mgmt().await;
        let s = state(&fake, "admin", vec![]).await;
        // A concurrent out-of-band writer replaces our whole-list
        // write with its own list right after our set commits: the
        // post-write verify must notice our alias is gone and
        // re-apply it on top of the other writer's list, instead of
        // writing a mirror row for an alias the server no longer has.
        fake.replace_after_next_set(vec![
            json!({"name": "other", "domainId": "d-x", "enabled": true}),
        ]);
        let id = create_ok(&s, "sales@x.id").await;
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM mail_aliases WHERE id=?")
            .bind(&id)
            .fetch_one(&s.pool)
            .await
            .unwrap();
        assert_eq!(n, 1);
        // Two sets: the overwritten first attempt, then the retry.
        let sets: Vec<_> = requests(&fake)
            .into_iter()
            .filter(|r| r["body"]["methodCalls"][0][0] == "x:Account/set")
            .collect();
        assert_eq!(sets.len(), 2);
        assert_eq!(
            fake.server_aliases("acct-me"),
            vec![
                json!({"name": "other", "domainId": "d-x", "enabled": true}),
                json!({"name": "sales", "domainId": "d-x", "enabled": true}),
            ]
        );
    }

    #[tokio::test]
    async fn create_preserves_out_of_band_alias() {
        let fake = fake_mgmt().await;
        let s = state(&fake, "admin", vec![]).await;
        // An alias another management client added to this account:
        // our whole-list write must carry it along, not erase it.
        fake.seed_alias("acct-me", "other", "d-x");
        create_ok(&s, "sales@x.id").await;
        let sets: Vec<_> = requests(&fake)
            .into_iter()
            .filter(|r| r["body"]["methodCalls"][0][0] == "x:Account/set")
            .collect();
        assert_eq!(sets.len(), 1);
        assert_eq!(
            sets[0]["body"]["methodCalls"][0][1]["update"]["acct-me"]["aliases"],
            json!({
                "0": {"name": "other", "domainId": "d-x", "enabled": true},
                "1": {"name": "sales", "domainId": "d-x", "enabled": true},
            })
        );
        assert_eq!(
            fake.server_aliases("acct-me"),
            vec![
                json!({"name": "other", "domainId": "d-x", "enabled": true}),
                json!({"name": "sales", "domainId": "d-x", "enabled": true}),
            ]
        );
    }

    #[tokio::test]
    async fn create_reconciles_when_set_response_is_lost() {
        let fake = fake_mgmt().await;
        let s = state(&fake, "admin", vec![]).await;
        // The server commits, then the response is lost: the create
        // must re-read, find the alias landed, and still write the
        // mirror row instead of failing.
        fake.drop_next_set_response(true);
        let id = create_ok(&s, "sales@x.id").await;
        let row: (String, String) =
            sqlx::query_as("SELECT mailbox_id, address FROM mail_aliases WHERE id=?")
                .bind(&id)
                .fetch_one(&s.pool)
                .await
                .unwrap();
        assert_eq!(row, ("mb1".into(), "sales@x.id".into()));
        assert_eq!(
            fake.server_aliases("acct-me"),
            vec![json!({"name": "sales", "domainId": "d-x", "enabled": true})]
        );
    }

    #[tokio::test]
    async fn create_reports_original_error_when_lost_set_did_not_land() {
        let fake = fake_mgmt().await;
        let s = state(&fake, "admin", vec![]).await;
        // The response is lost and the write never landed: the
        // reconcile re-read finds nothing, so the original transport
        // error is returned and no mirror row is written.
        fake.drop_next_set_response(false);
        let err = create(&s, user(), "sales@x.id", "mb1").await.unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_GATEWAY);
        assert_eq!(err.1, "mail_server_unavailable");
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM mail_aliases")
            .fetch_one(&s.pool)
            .await
            .unwrap();
        assert_eq!(n, 0);
        assert!(fake.server_aliases("acct-me").is_empty());
    }

    #[tokio::test]
    async fn list_answers_503_while_admin_credential_unset() {
        let fake = fake_mgmt().await;
        let s = state(&fake, "", vec![]).await;
        let err = list_aliases(State(s.clone()), Extension(user()))
            .await
            .unwrap_err();
        assert_eq!(err.0, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(err.1, "aliases_unavailable");
        assert!(requests(&fake).is_empty());
    }

    #[tokio::test]
    async fn list_domains_admin_sees_all_domains() {
        let fake = fake_mgmt().await;
        let s = state(&fake, "admin", vec!["admin@x.id".into()]).await;
        let mut admin = user();
        admin.email = "admin@x.id".into();
        admin.user_id = "u-admin".into();
        let res = list_domains(State(s.clone()), Extension(admin))
            .await
            .unwrap();
        let body = to_bytes(res.into_body(), 4096).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["domains"], json!([{"name": "x.id"}, {"name": "y.id"}]));
    }

    #[tokio::test]
    async fn list_domains_non_admin_sees_own_domains_only() {
        let fake = fake_mgmt().await;
        let s = state(&fake, "admin", vec![]).await;
        // u1's only mailbox is on x.id: y.id must not leak.
        let res = list_domains(State(s.clone()), Extension(user()))
            .await
            .unwrap();
        let body = to_bytes(res.into_body(), 4096).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["domains"], json!([{"name": "x.id"}]));
        // A second own mailbox on y.id adds that domain, and only
        // that one; no server call is made for non-admins.
        sqlx::query(
            "INSERT INTO permanent_mailboxes (id,user_id,email_address,mail_server_account_id,status,created_at,updated_at) VALUES ('mb-y','u1','you@y.id','acct-me','active','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
        )
        .execute(&s.pool)
        .await
        .unwrap();
        let res = list_domains(State(s.clone()), Extension(user()))
            .await
            .unwrap();
        let body = to_bytes(res.into_body(), 4096).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["domains"], json!([{"name": "x.id"}, {"name": "y.id"}]));
        assert!(requests(&fake).is_empty());
    }

    #[tokio::test]
    async fn list_domains_answers_503_while_admin_credential_unset() {
        let fake = fake_mgmt().await;
        let s = state(&fake, "", vec![]).await;
        let err = list_domains(State(s.clone()), Extension(user()))
            .await
            .unwrap_err();
        assert_eq!(err.0, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(err.1, "aliases_unavailable");
        assert!(requests(&fake).is_empty());
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
    async fn list_catch_alls_admin_sees_all_in_one_call() {
        let fake = fake_mgmt().await;
        let s = state(&fake, "admin", vec!["admin@x.id".into()]).await;
        let mut admin = user();
        admin.email = "admin@x.id".into();
        admin.user_id = "u-admin".into();
        let res = list_catch_alls(State(s.clone()), Extension(admin))
            .await
            .unwrap();
        let body = to_bytes(res.into_body(), 4096).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            v["catch_alls"],
            json!([
                {"domain": "x.id", "target_address": null, "enabled": false},
                {"domain": "y.id", "target_address": "postmaster@y.id", "enabled": true},
            ])
        );
        assert_eq!(v["missing"], json!([]));
        // One management call backs the whole answer.
        let gets: Vec<_> = requests(&fake)
            .into_iter()
            .filter(|r| r["body"]["methodCalls"][0][0] == "x:Domain/get")
            .collect();
        assert_eq!(gets.len(), 1);
    }

    #[tokio::test]
    async fn list_catch_alls_non_admin_sees_own_domains_and_missing() {
        let fake = fake_mgmt().await;
        let s = state(&fake, "admin", vec![]).await;
        // A second mailbox on a domain the server does not know.
        sqlx::query(
            "INSERT INTO permanent_mailboxes (id,user_id,email_address,mail_server_account_id,status,created_at,updated_at) VALUES ('mb-z','u1','me@z.id','acct-me','active','2026-01-01T00:00:00Z','2026-01-01T00:00:00Z')",
        )
        .execute(&s.pool)
        .await
        .unwrap();
        let res = list_catch_alls(State(s.clone()), Extension(user()))
            .await
            .unwrap();
        let body = to_bytes(res.into_body(), 4096).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            v["catch_alls"],
            json!([{"domain": "x.id", "target_address": null, "enabled": false}])
        );
        assert_eq!(v["missing"], json!([{"name": "z.id"}]));
        // y.id is on the server but not the caller's, so it leaks
        // neither as a setting nor as missing.
        assert!(!body.windows(4).any(|w| w == b"y.id"));
    }

    #[tokio::test]
    async fn list_catch_alls_answers_503_while_admin_credential_unset() {
        let fake = fake_mgmt().await;
        let s = state(&fake, "", vec![]).await;
        let err = list_catch_alls(State(s.clone()), Extension(user()))
            .await
            .unwrap_err();
        assert_eq!(err.0, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(err.1, "aliases_unavailable");
        assert!(requests(&fake).is_empty());
    }

    #[tokio::test]
    async fn set_catch_all_reconciles_lost_response_when_write_landed() {
        let fake = fake_mgmt().await;
        let s = state(&fake, "admin", vec![]).await;
        // The Domain/set response is lost, but the update committed.
        fake.drop_next_domain_set_response(true);
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
        // The mirror followed the landed write.
        let target: Option<String> =
            sqlx::query_scalar("SELECT target_address FROM domain_catch_all WHERE domain='x.id'")
                .fetch_one(&s.pool)
                .await
                .unwrap();
        assert_eq!(target.as_deref(), Some("me@x.id"));
    }

    #[tokio::test]
    async fn set_catch_all_returns_error_when_lost_write_never_landed() {
        let fake = fake_mgmt().await;
        let s = state(&fake, "admin", vec![]).await;
        // The Domain/set response is lost and the update never
        // committed: the readback proves it, so the original error
        // is returned instead of a phantom success.
        fake.drop_next_domain_set_response(false);
        let err = set_catch_all(
            State(s.clone()),
            Extension(user()),
            Path("x.id".into()),
            Json(SetCatchAllBody {
                target_address: Some("me@x.id".into()),
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(err.0, StatusCode::BAD_GATEWAY);
        assert_eq!(err.1, "mail_server_unavailable");
        // Server and mirror both keep the old (unset) target.
        let res = get_catch_all(State(s.clone()), Extension(user()), Path("x.id".into()))
            .await
            .unwrap();
        let body = to_bytes(res.into_body(), 4096).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["enabled"], false);
        assert!(v["target_address"].is_null());
    }

    #[tokio::test]
    async fn delete_restore_puts_back_the_entry_the_write_removed() {
        let fake = fake_mgmt().await;
        let s = state(&fake, "admin", vec![]).await;
        fake.seed_alias("acct-me", "sales", "d-x");
        sqlx::query(
            "INSERT INTO mail_aliases (id,user_id,mailbox_id,address,stalwart_account_id,enabled,created_at) \
             VALUES ('al-del','u1','mb1','sales@x.id','acct-me',1,'2026-01-01T00:00:00Z')",
        )
        .execute(&s.pool)
        .await
        .unwrap();
        // Another client disables the alias on the second server
        // read (the fresh read inside `update_alias_list`), after
        // the preliminary read saw it enabled.
        fake.disable_alias_on_get("acct-me", "sales", "d-x", 2);
        // Make the mirror delete fail, so the handler must restore
        // the server alias it just removed.
        sqlx::query(
            "CREATE TRIGGER fail_alias_delete BEFORE DELETE ON mail_aliases \
             BEGIN SELECT RAISE(ABORT, 'delete blocked for test'); END;",
        )
        .execute(&s.pool)
        .await
        .unwrap();
        let err = delete_alias(State(s.clone()), Extension(user()), Path("al-del".into()))
            .await
            .unwrap_err();
        assert_eq!(err.0, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(err.1, "internal_error");
        // The alias is back exactly as the accepted write removed
        // it: disabled, not re-enabled from the preliminary
        // snapshot the handler read first.
        let server = fake.server_aliases("acct-me");
        assert_eq!(server.len(), 1);
        assert_eq!(server[0]["name"], "sales");
        assert_eq!(server[0]["enabled"], json!(false));
        // And the mirror row survives the refused delete.
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM mail_aliases WHERE id='al-del'")
            .fetch_one(&s.pool)
            .await
            .unwrap();
        assert_eq!(n, 1);
    }

    #[tokio::test]
    async fn mailbox_creds_helper_still_resolves() {
        // `mailbox_creds` is imported by this module; keep the import
        // honest by exercising it the way handlers do.
        assert_eq!(mailbox_creds(&user(), "mb1").unwrap().user, "me@x.id");
    }
}
