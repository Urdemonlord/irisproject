//! Stars, labels and custom folders.
//!
//! Stars and labels are JMAP keywords on the message (`$flagged`, and the
//! label name itself). Custom folders are JMAP mailboxes without a role.
//! Only the list of label names is kept in SQLite (`mailbox_labels`), so a
//! label with no messages still exists.
use crate::{filters, owned_account, valid_message_id, ApiError, ApiResult, CurrentUser, S};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Extension, Json,
};
use iris_auth as auth;
use iris_config::limits::{MAX_CUSTOM_FOLDERS, MAX_FOLDER_NAME_CHARS, MAX_LABELS};
use iris_mail::{valid_label, CUSTOM_ROLE, FOLDER_ROLES};
use serde::Deserialize;
use serde_json::json;

fn bad(code: &'static str) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, code)
}

fn message_id(id: &str) -> Result<(), ApiError> {
    if valid_message_id(id) {
        Ok(())
    } else {
        Err(bad("invalid_request"))
    }
}

/// Lowercases and checks a label name from the client.
pub(crate) fn clean_label(name: &str) -> Result<String, ApiError> {
    let n = name.trim().to_lowercase();
    if valid_label(&n) {
        Ok(n)
    } else {
        Err(bad("invalid_label"))
    }
}

/// Trims and checks a custom folder name: 1-64 characters, no control
/// characters, no `/` (the mail server's hierarchy separator), and not the
/// name of a standard folder.
pub(crate) fn clean_folder_name(name: &str) -> Result<String, ApiError> {
    let n = name.trim().to_string();
    let len = n.chars().count();
    if len == 0
        || len > MAX_FOLDER_NAME_CHARS
        || n.chars().any(|c| c.is_control() || c == '/')
        || FOLDER_ROLES
            .iter()
            .any(|(_, std)| std.eq_ignore_ascii_case(&n))
    {
        return Err(bad("invalid_folder_name"));
    }
    Ok(n)
}

pub(crate) async fn label_exists(
    s: &crate::AppState,
    mailbox_id: &str,
    name: &str,
) -> Result<bool, ApiError> {
    let n: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM mailbox_labels WHERE mailbox_id=? AND name=?")
            .bind(mailbox_id)
            .bind(name)
            .fetch_one(&s.pool)
            .await?;
    Ok(n > 0)
}

// ----- stars -----

#[derive(Deserialize)]
pub(crate) struct StarBody {
    starred: bool,
}

pub(crate) async fn set_star(
    State(s): S,
    Extension(u): Extension<CurrentUser>,
    Path((mailbox_id, id)): Path<(String, String)>,
    Json(b): Json<StarBody>,
) -> ApiResult {
    let account = owned_account(&s, &u, &mailbox_id).await?;
    message_id(&id)?;
    s.jmap
        .set_keyword(
            &u.creds.user,
            &u.creds.pass,
            &account,
            &id,
            "$flagged",
            b.starred,
        )
        .await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

// ----- labels -----

pub(crate) async fn list_labels(
    State(s): S,
    Extension(u): Extension<CurrentUser>,
    Path(mailbox_id): Path<String>,
) -> ApiResult {
    owned_account(&s, &u, &mailbox_id).await?;
    let names: Vec<String> =
        sqlx::query_scalar("SELECT name FROM mailbox_labels WHERE mailbox_id=? ORDER BY name")
            .bind(&mailbox_id)
            .fetch_all(&s.pool)
            .await?;
    let labels: Vec<_> = names.into_iter().map(|n| json!({ "name": n })).collect();
    Ok(Json(json!({ "labels": labels })).into_response())
}

#[derive(Deserialize)]
pub(crate) struct LabelBody {
    name: String,
}

pub(crate) async fn create_label(
    State(s): S,
    Extension(u): Extension<CurrentUser>,
    Path(mailbox_id): Path<String>,
    Json(b): Json<LabelBody>,
) -> ApiResult {
    owned_account(&s, &u, &mailbox_id).await?;
    let name = clean_label(&b.name)?;
    // One statement, so the cap holds under concurrent creates; an existing
    // name is a no-op success (creating is idempotent).
    let r = sqlx::query(
        "INSERT OR IGNORE INTO mailbox_labels (mailbox_id, name, created_at) \
         SELECT ?, ?, ? WHERE (SELECT COUNT(*) FROM mailbox_labels WHERE mailbox_id = ?) < ?",
    )
    .bind(&mailbox_id)
    .bind(&name)
    .bind(auth::now())
    .bind(&mailbox_id)
    .bind(MAX_LABELS)
    .execute(&s.pool)
    .await?;
    if r.rows_affected() == 0 && !label_exists(&s, &mailbox_id, &name).await? {
        return Err(ApiError(StatusCode::CONFLICT, "too_many_labels"));
    }
    Ok((
        StatusCode::CREATED,
        Json(json!({ "label": { "name": name } })),
    )
        .into_response())
}

/// Deletes a label and removes it from the messages that carry it. A label
/// a filter still uses is refused, so a filter never points at nothing.
pub(crate) async fn delete_label(
    State(s): S,
    Extension(u): Extension<CurrentUser>,
    Path((mailbox_id, name)): Path<(String, String)>,
) -> ApiResult {
    let account = owned_account(&s, &u, &mailbox_id).await?;
    let name = clean_label(&name)?;
    if !label_exists(&s, &mailbox_id, &name).await? {
        return Err(ApiError(StatusCode::NOT_FOUND, "not_found"));
    }
    if filters::target_in_use(&s, &mailbox_id, "label", &name).await? {
        return Err(ApiError(StatusCode::CONFLICT, "label_in_use_by_filter"));
    }
    s.jmap
        .clear_keyword(&u.creds.user, &u.creds.pass, &account, &name)
        .await?;
    sqlx::query("DELETE FROM mailbox_labels WHERE mailbox_id=? AND name=?")
        .bind(&mailbox_id)
        .bind(&name)
        .execute(&s.pool)
        .await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[derive(Deserialize)]
pub(crate) struct MessageLabelBody {
    label: String,
    on: bool,
}

pub(crate) async fn set_message_label(
    State(s): S,
    Extension(u): Extension<CurrentUser>,
    Path((mailbox_id, id)): Path<(String, String)>,
    Json(b): Json<MessageLabelBody>,
) -> ApiResult {
    let account = owned_account(&s, &u, &mailbox_id).await?;
    message_id(&id)?;
    let name = clean_label(&b.label)?;
    // Removing is always allowed (it cleans up a label deleted elsewhere);
    // adding needs a defined label.
    if b.on && !label_exists(&s, &mailbox_id, &name).await? {
        return Err(ApiError(StatusCode::NOT_FOUND, "unknown_label"));
    }
    s.jmap
        .set_keyword(&u.creds.user, &u.creds.pass, &account, &id, &name, b.on)
        .await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

// ----- custom folders -----

#[derive(Deserialize)]
pub(crate) struct FolderBody {
    name: String,
}

pub(crate) async fn create_folder(
    State(s): S,
    Extension(u): Extension<CurrentUser>,
    Path(mailbox_id): Path<String>,
    Json(b): Json<FolderBody>,
) -> ApiResult {
    let account = owned_account(&s, &u, &mailbox_id).await?;
    let name = clean_folder_name(&b.name)?;
    let folders = s
        .jmap
        .folders(&u.creds.user, &u.creds.pass, &account)
        .await?;
    if folders.iter().any(|f| f.name.eq_ignore_ascii_case(&name)) {
        return Err(ApiError(StatusCode::CONFLICT, "folder_exists"));
    }
    if folders.iter().filter(|f| f.role == CUSTOM_ROLE).count() >= MAX_CUSTOM_FOLDERS {
        return Err(ApiError(StatusCode::CONFLICT, "too_many_folders"));
    }
    let id = s
        .jmap
        .create_folder(&u.creds.user, &u.creds.pass, &account, &name)
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({ "folder": { "id": id, "role": CUSTOM_ROLE, "name": name, "total": 0, "unread": 0 } })),
    )
        .into_response())
}

pub(crate) async fn rename_folder(
    State(s): S,
    Extension(u): Extension<CurrentUser>,
    Path((mailbox_id, folder_id)): Path<(String, String)>,
    Json(b): Json<FolderBody>,
) -> ApiResult {
    let account = owned_account(&s, &u, &mailbox_id).await?;
    message_id(&folder_id)?;
    let name = clean_folder_name(&b.name)?;
    let folders = s
        .jmap
        .folders(&u.creds.user, &u.creds.pass, &account)
        .await?;
    if !folders
        .iter()
        .any(|f| f.role == CUSTOM_ROLE && f.id == folder_id)
    {
        return Err(ApiError(StatusCode::NOT_FOUND, "not_found"));
    }
    if folders
        .iter()
        .any(|f| f.id != folder_id && f.name.eq_ignore_ascii_case(&name))
    {
        return Err(ApiError(StatusCode::CONFLICT, "folder_exists"));
    }
    s.jmap
        .rename_folder(&u.creds.user, &u.creds.pass, &account, &folder_id, &name)
        .await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Deletes an empty custom folder. A folder with messages answers 409
/// `folder_not_empty`; one a filter moves mail into answers 409
/// `folder_in_use_by_filter`.
pub(crate) async fn delete_folder(
    State(s): S,
    Extension(u): Extension<CurrentUser>,
    Path((mailbox_id, folder_id)): Path<(String, String)>,
) -> ApiResult {
    let account = owned_account(&s, &u, &mailbox_id).await?;
    message_id(&folder_id)?;
    let folders = s
        .jmap
        .folders(&u.creds.user, &u.creds.pass, &account)
        .await?;
    if !folders
        .iter()
        .any(|f| f.role == CUSTOM_ROLE && f.id == folder_id)
    {
        return Err(ApiError(StatusCode::NOT_FOUND, "not_found"));
    }
    if filters::target_in_use(&s, &mailbox_id, "move", &folder_id).await? {
        return Err(ApiError(StatusCode::CONFLICT, "folder_in_use_by_filter"));
    }
    s.jmap
        .delete_folder(&u.creds.user, &u.creds.pass, &account, &folder_id)
        .await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_lowercased_and_restricted() {
        assert_eq!(clean_label(" Work ").unwrap(), "work");
        assert_eq!(clean_label("to-do_2").unwrap(), "to-do_2");
        for bad in [
            "",
            "$seen",
            "-x",
            "a b",
            "a/b",
            "a~b",
            "é",
            "x".repeat(33).as_str(),
        ] {
            assert!(clean_label(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn folder_names() {
        assert_eq!(clean_folder_name("  Receipts ").unwrap(), "Receipts");
        assert_eq!(clean_folder_name("Projekt Ü").unwrap(), "Projekt Ü");
        for bad in ["", "  ", "a/b", "x\ny", "inbox", "TRASH", &"x".repeat(65)] {
            assert!(clean_folder_name(bad).is_err(), "{bad:?}");
        }
    }
}
