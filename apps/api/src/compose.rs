//! Folders, message actions (move / read / delete), drafts and sending.
use crate::email_address::valid_compose_address;
use crate::{
    check_read_limit, owned_account, valid_message_id, ApiError, ApiResult, AppState, CurrentUser,
    S,
};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Extension, Json,
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use iris_auth as auth;
use iris_config::limits::{
    MAX_ATTACHMENT_NAME_CHARS, MAX_BODY, MAX_COMPOSE_ATTACHMENTS,
    MAX_COMPOSE_ATTACHMENTS_TOTAL_BYTES, MAX_COMPOSE_ATTACHMENT_BYTES, MAX_HEADER_IDS,
    MAX_HEADER_ID_LEN, MAX_RECIPIENTS, MAX_SUBJECT, SENDS_PER_HOUR,
};
use iris_mail::{Compose, ComposeAttachment, FOLDER_ROLES};
use serde::Deserialize;
use serde_json::json;

pub(crate) fn folder_role(name: &str) -> Result<&'static str, ApiError> {
    FOLDER_ROLES
        .iter()
        .map(|(r, _)| *r)
        .find(|r| *r == name)
        .ok_or(ApiError(StatusCode::BAD_REQUEST, "invalid_folder"))
}

fn message_id(id: &str) -> Result<(), ApiError> {
    if valid_message_id(id) {
        Ok(())
    } else {
        Err(ApiError(StatusCode::BAD_REQUEST, "invalid_request"))
    }
}

pub(crate) async fn folders(
    State(s): S,
    Extension(u): Extension<CurrentUser>,
    Path(mailbox_id): Path<String>,
) -> ApiResult {
    let account = owned_account(&s, &u, &mailbox_id).await?;
    check_read_limit(&s, &u).await?;
    let folders = s
        .jmap
        .folders(&u.creds.user, &u.creds.pass, &account)
        .await?;
    Ok(Json(json!({ "folders": folders })).into_response())
}

#[derive(Deserialize)]
pub(crate) struct MoveBody {
    /// A standard folder role.
    folder: Option<String>,
    /// Or a custom folder id.
    folder_id: Option<String>,
}

pub(crate) async fn move_message(
    State(s): S,
    Extension(u): Extension<CurrentUser>,
    Path((mailbox_id, id)): Path<(String, String)>,
    Json(b): Json<MoveBody>,
) -> ApiResult {
    let account = owned_account(&s, &u, &mailbox_id).await?;
    message_id(&id)?;
    if let Some(target) = b.folder_id {
        message_id(&target)?;
        let folders = s
            .jmap
            .folders(&u.creds.user, &u.creds.pass, &account)
            .await?;
        if !folders
            .iter()
            .any(|f| f.id == target && f.role == iris_mail::CUSTOM_ROLE)
        {
            return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_folder"));
        }
        s.jmap
            .move_to_id(&u.creds.user, &u.creds.pass, &account, &id, &target)
            .await?;
        return Ok(StatusCode::NO_CONTENT.into_response());
    }
    let role = folder_role(b.folder.as_deref().unwrap_or_default())?;
    if role == "drafts" {
        // Drafts are filled by composing, not by moving mail in. Sent is
        // allowed: Undo moves a message back to where it was, and a message
        // that was in Sent is a sent message.
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_folder"));
    }
    s.jmap
        .move_to(&u.creds.user, &u.creds.pass, &account, &id, role)
        .await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[derive(Deserialize)]
pub(crate) struct SeenBody {
    seen: bool,
}

pub(crate) async fn set_seen(
    State(s): S,
    Extension(u): Extension<CurrentUser>,
    Path((mailbox_id, id)): Path<(String, String)>,
    Json(b): Json<SeenBody>,
) -> ApiResult {
    let account = owned_account(&s, &u, &mailbox_id).await?;
    message_id(&id)?;
    s.jmap
        .set_seen(&u.creds.user, &u.creds.pass, &account, &id, b.seen)
        .await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Permanent delete — only for messages already in Trash, or drafts.
pub(crate) async fn delete_message(
    State(s): S,
    Extension(u): Extension<CurrentUser>,
    Path((mailbox_id, id)): Path<(String, String)>,
) -> ApiResult {
    let account = owned_account(&s, &u, &mailbox_id).await?;
    message_id(&id)?;
    let roles = s
        .jmap
        .message_roles(&u.creds.user, &u.creds.pass, &account, &id)
        .await?;
    if !roles.iter().any(|r| r == "trash" || r == "drafts") {
        return Err(ApiError(StatusCode::CONFLICT, "move_to_trash_first"));
    }
    s.jmap
        .destroy_one(&u.creds.user, &u.creds.pass, &account, &id)
        .await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[derive(Deserialize, Default)]
pub(crate) struct ComposeBody {
    #[serde(default)]
    to: Vec<String>,
    #[serde(default)]
    cc: Vec<String>,
    #[serde(default)]
    bcc: Vec<String>,
    #[serde(default)]
    subject: String,
    #[serde(default)]
    text: String,
    in_reply_to: Option<String>,
    #[serde(default)]
    references: Vec<String>,
    /// Existing draft this replaces (deleted on save/send).
    draft_id: Option<String>,
    /// Files to attach: base64-encoded content, decoded and size-checked in
    /// `build()`. Kept as JSON (no multipart anywhere in this codebase).
    #[serde(default)]
    attachments: Vec<AttachmentBody>,
}

/// One attachment as the web sends it: base64 content + metadata.
#[derive(Deserialize)]
pub(crate) struct AttachmentBody {
    #[serde(default)]
    name: String,
    #[serde(default)]
    content_type: String,
    #[serde(default)]
    data: String,
}

fn clean_list(v: &[String]) -> Result<Vec<String>, ApiError> {
    v.iter()
        .map(|a| a.trim().to_lowercase())
        .filter(|a| !a.is_empty())
        .map(|a| {
            if valid_compose_address(&a) {
                Ok(a)
            } else {
                Err(ApiError(StatusCode::BAD_REQUEST, "invalid_recipient"))
            }
        })
        .collect()
}

/// Header ids for threading: angle brackets stripped, no whitespace.
fn clean_ids(v: impl IntoIterator<Item = String>) -> Vec<String> {
    v.into_iter()
        .map(|x| x.trim().trim_matches(|c| c == '<' || c == '>').to_string())
        .filter(|x| {
            !x.is_empty() && x.len() <= MAX_HEADER_ID_LEN && !x.chars().any(char::is_whitespace)
        })
        .take(MAX_HEADER_IDS)
        .collect()
}

/// Decodes and bounds-checks the attachments from a compose request.
/// Filenames are validated as plain names, never paths: no separators,
/// no `.`/`..`, max 255 chars. A missing content type becomes
/// `application/octet-stream`; anything else must be a control-char-free
/// token (it lands in an upload `Content-Type` header, so CR/LF would be
/// header injection). All failures are one `invalid_attachment` error: the
/// client only needs to know the attachments were refused.
fn clean_attachments(v: &[AttachmentBody]) -> Result<Vec<ComposeAttachment>, ApiError> {
    let bad = || ApiError(StatusCode::BAD_REQUEST, "invalid_attachment");
    if v.len() > MAX_COMPOSE_ATTACHMENTS {
        return Err(bad());
    }
    let mut total = 0usize;
    let mut out = Vec::with_capacity(v.len());
    for a in v {
        let name = a.name.as_str();
        if name.is_empty()
            || name.chars().count() > MAX_ATTACHMENT_NAME_CHARS
            || name.contains(['/', '\\', '\0'])
            || name == "."
            || name == ".."
        {
            return Err(bad());
        }
        let content_type = if a.content_type.trim().is_empty() {
            "application/octet-stream".to_string()
        } else if a
            .content_type
            .chars()
            .any(|ch| (ch as u32) < 0x20 || (ch as u32) == 0x7f)
        {
            return Err(bad());
        } else {
            a.content_type.clone()
        };
        let bytes = BASE64.decode(&a.data).map_err(|_| bad())?;
        if bytes.len() > MAX_COMPOSE_ATTACHMENT_BYTES {
            return Err(bad());
        }
        total += bytes.len();
        if total > MAX_COMPOSE_ATTACHMENTS_TOTAL_BYTES {
            return Err(bad());
        }
        out.push(ComposeAttachment {
            name: name.to_string(),
            content_type,
            bytes,
        });
    }
    Ok(out)
}

fn build(u: &CurrentUser, b: ComposeBody) -> Result<(Compose, Option<String>), ApiError> {
    if b.subject.len() > MAX_SUBJECT || b.text.len() > MAX_BODY || b.subject.contains(['\r', '\n'])
    {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_request"));
    }
    let (to, cc, bcc) = (clean_list(&b.to)?, clean_list(&b.cc)?, clean_list(&b.bcc)?);
    if to.len() + cc.len() + bcc.len() > MAX_RECIPIENTS {
        return Err(ApiError(StatusCode::BAD_REQUEST, "too_many_recipients"));
    }
    if let Some(d) = &b.draft_id {
        message_id(d)?;
    }
    let c = Compose {
        from: u.email.clone(),
        to,
        cc,
        bcc,
        subject: b.subject,
        text: b.text,
        in_reply_to: clean_ids(b.in_reply_to).into_iter().next(),
        references: clean_ids(b.references),
        attachments: clean_attachments(&b.attachments)?,
    };
    Ok((c, b.draft_id))
}

pub(crate) async fn save_draft(
    State(s): S,
    Extension(u): Extension<CurrentUser>,
    Path(mailbox_id): Path<String>,
    Json(b): Json<ComposeBody>,
) -> ApiResult {
    let account = owned_account(&s, &u, &mailbox_id).await?;
    let (c, replace) = build(&u, b)?;
    let id = s
        .jmap
        .save_draft(
            &u.creds.user,
            &u.creds.pass,
            &account,
            &c,
            replace.as_deref(),
        )
        .await?;
    Ok(Json(json!({ "id": id })).into_response())
}

/// First recipient outside our own domains, if any.
fn first_external<'a>(s: &AppState, c: &'a Compose) -> Option<&'a str> {
    c.to.iter()
        .chain(&c.cc)
        .chain(&c.bcc)
        .find(|a| {
            let d = a.rsplit_once('@').map(|x| x.1).unwrap_or_default();
            !s.config.send_local_domains.iter().any(|l| d == l)
        })
        .map(String::as_str)
}

pub(crate) async fn send(
    State(s): S,
    Extension(u): Extension<CurrentUser>,
    Path(mailbox_id): Path<String>,
    Json(b): Json<ComposeBody>,
) -> ApiResult {
    let account = owned_account(&s, &u, &mailbox_id).await?;
    let (c, draft) = build(&u, b)?;
    if c.to.is_empty() && c.cc.is_empty() && c.bcc.is_empty() {
        return Err(ApiError(StatusCode::BAD_REQUEST, "no_recipients"));
    }
    let external = first_external(&s, &c).is_some();
    if external && s.relay.is_none() && !s.config.send_external {
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "external_sending_disabled",
        ));
    }

    let subject = auth::hash_token(&format!("send:{}", u.user_id));
    let since = (chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339();
    // Reserve a slot atomically BEFORE delivery so two concurrent sends
    // cannot both pass the hourly cap (TOCTOU). The single INSERT is an
    // atomic statement under SQLite's write lock: the second request sees
    // the first's committed row and gets 0 rows_affected -> 429.
    let reserved = sqlx::query(
        "INSERT INTO rate_limit_events (id, subject_hash, action, occurred_at, outcome) \
         SELECT ?, ?, 'send', ?, 'allowed' \
         WHERE (SELECT COUNT(*) FROM rate_limit_events WHERE subject_hash = ? AND action = 'send' AND occurred_at > ?) < ?",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .bind(&subject)
    .bind(auth::now())
    .bind(&subject)
    .bind(&since)
    .bind(SENDS_PER_HOUR)
    .execute(&s.pool)
    .await?;
    if reserved.rows_affected() == 0 {
        return Err(ApiError(StatusCode::TOO_MANY_REQUESTS, "too_many_sends"));
    }

    // The Sent copy and the rate-limit row are bookkeeping *after* delivery:
    // failing either must never turn an already-delivered message into an
    // error (a client retry would double-send) or silently drop the Sent
    // copy. The relay branch therefore reports the copy outcome honestly
    // (`sent_copy: false`) while still answering success; the JMAP branch
    // files Sent atomically with submission, so its copy always holds.
    let sent_copy = match (&s.relay, external) {
        // Other domains go out through the relay (it also delivers our local recipients via MX);
        // the copy for Sent is filed afterwards. A failed copy doesn't undo a sent message.
        (Some(relay), true) => {
            relay.send(&c).await?;
            match s
                .jmap
                .store_sent(&u.creds.user, &u.creds.pass, &account, &c, draft.as_deref())
                .await
            {
                Ok(()) => true,
                Err(e) => {
                    tracing::error!("sent via relay but Sent copy failed: {e}");
                    false
                }
            }
        }
        _ => {
            s.jmap
                .send(&u.creds.user, &u.creds.pass, &account, &c, draft.as_deref())
                .await?;
            true
        }
    };
    // Best effort only: the message is already delivered, so a failed
    // bookkeeping insert must not fail the request (and invite a retry
    // that would double-send). Cost: this one send is not counted
    // against the hourly limit.
    // (Reservation now happens atomically before delivery, so this is
    // no longer needed.)
    Ok(Json(json!({ "sent": true, "sent_copy": sent_copy })).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses() {
        assert!(valid_compose_address("a@example.com"));
        assert!(valid_compose_address("first.last+tag@sub.example.co"));
        for bad in [
            "", "a", "@x.id", "a@", "a@x", "a@.x", "a@x.", "a b@x.id", "a@x..id", "<a@x.id>",
            "a,b@x.id", "a@x.id\n",
        ] {
            assert!(!valid_compose_address(bad), "{bad:?}");
        }
    }

    #[test]
    fn folder_whitelist() {
        assert_eq!(folder_role("trash").unwrap(), "trash");
        assert!(folder_role("junk").is_err());
        assert!(folder_role("../x").is_err());
    }

    #[test]
    fn header_ids_are_cleaned() {
        assert_eq!(
            clean_ids(vec!["<a@b>".into(), " ".into(), "x y".into()]),
            vec!["a@b".to_string()]
        );
    }

    fn test_user() -> CurrentUser {
        CurrentUser {
            user_id: "u1".into(),
            email: "me@x.id".into(),
            creds: crate::Creds {
                user: "me@x.id".into(),
                pass: "secret".into(),
            },
        }
    }

    fn att(name: &str, content_type: &str, bytes: &[u8]) -> AttachmentBody {
        AttachmentBody {
            name: name.into(),
            content_type: content_type.into(),
            data: BASE64.encode(bytes),
        }
    }

    fn body_with(attachments: Vec<AttachmentBody>) -> ComposeBody {
        ComposeBody {
            to: vec!["b@x.id".into()],
            attachments,
            ..Default::default()
        }
    }

    fn invalid(b: ComposeBody) {
        let err = build(&test_user(), b).expect_err("must be rejected");
        assert_eq!(err.0, StatusCode::BAD_REQUEST);
        assert_eq!(err.1, "invalid_attachment");
    }

    #[test]
    fn attachments_happy_path() {
        let (c, _) = build(
            &test_user(),
            body_with(vec![
                att("notes.txt", "text/plain", b"hello"),
                att("blob.bin", "", b"\x00\x01"),
            ]),
        )
        .expect("valid attachments");
        assert_eq!(c.attachments.len(), 2);
        assert_eq!(c.attachments[0].name, "notes.txt");
        assert_eq!(c.attachments[0].content_type, "text/plain");
        assert_eq!(c.attachments[0].bytes, b"hello");
        // Empty content type defaults; it is never sent as an empty header.
        assert_eq!(c.attachments[1].content_type, "application/octet-stream");
    }

    #[test]
    fn attachments_reject_bad_base64() {
        let mut b = body_with(vec![att("a.txt", "text/plain", b"x")]);
        b.attachments[0].data = "!!!not base64!!!".into();
        invalid(b);
    }

    #[test]
    fn attachments_reject_oversize_file() {
        invalid(body_with(vec![att(
            "big.bin",
            "application/octet-stream",
            &vec![0u8; MAX_COMPOSE_ATTACHMENT_BYTES + 1],
        )]));
    }

    #[test]
    fn attachments_reject_oversize_total() {
        // Each file is under its own cap; the sum passes the total cap.
        let each = vec![0u8; 4_000_000];
        invalid(body_with(vec![
            att("a.bin", "application/octet-stream", &each),
            att("b.bin", "application/octet-stream", &each),
            att("c.bin", "application/octet-stream", &each),
        ]));
    }

    #[test]
    fn attachments_reject_bad_filename() {
        for name in ["", "a/b.txt", "a\\b.txt", ".", "..", &"x".repeat(256)] {
            invalid(body_with(vec![att(name, "text/plain", b"x")]));
        }
    }

    #[test]
    fn attachments_reject_bad_content_type() {
        invalid(body_with(vec![att(
            "a.txt",
            "text/plain\r\nInjected: yes",
            b"x",
        )]));
    }

    #[test]
    fn attachments_reject_too_many() {
        let many = (0..MAX_COMPOSE_ATTACHMENTS + 1)
            .map(|i| att(&format!("f{i}.txt"), "text/plain", b"x"))
            .collect();
        invalid(body_with(many));
    }
}
