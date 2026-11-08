//! DMARC dashboard: aggregate DMARC reports across domains.
//!
//! Reports arrive as mail: a receiving server sends one aggregate XML
//! report per domain per day as an attachment (`.xml`, `.xml.gz`, or
//! `.zip`). The dashboard does not watch for them in the background; the
//! user scans the inbox on demand and the parsed reports are stored in
//! SQLite, so the dashboard is instant afterwards and a re-scan never
//! imports the same report twice.
//!
//! Storage is per user *and* per mailbox (`user_id`, `mailbox_id`): a
//! report imported from one mailbox is invisible from another, and from
//! another user. The scan only ever uses the signed-in user's own JMAP
//! credentials, like every other mailbox read.
use crate::{
    check_read_limit, owned_account, sanitized_filename, ApiError, ApiResult, AppState,
    CurrentUser, S,
};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Extension, Json,
};
use iris_config::limits::{
    MAX_DMARC_RECORDS_PER_REPORT, MAX_DMARC_REPORTS_PER_USER, MAX_DMARC_SCAN_ATTACHMENT_BYTES,
    MAX_DMARC_SCAN_MESSAGES,
};
use iris_mail::{
    dmarc::{dmarc_passed, parse_aggregate_report, DmarcReport},
    extract_text_attachment, MessageSummary,
};
use serde::Serialize;
use serde_json::json;
use sqlx::{FromRow, SqlitePool};
use std::collections::HashSet;

/// A report-shaped attachment: the same name / type test the preview
/// extractor applies, checked *before* downloading so a scan never
/// downloads a 5 MB invoice PDF just to learn it is not a report.
fn looks_like_report(name: &str, content_type: &str) -> bool {
    let lower_name = name.to_lowercase();
    let lower_type = content_type.to_lowercase();
    lower_name.ends_with(".xml")
        || lower_name.ends_with(".gz")
        || lower_name.ends_with(".zip")
        || lower_type.contains("xml")
        || lower_type.contains("gzip")
        || lower_type.contains("x-gzip")
        || lower_type.contains("zip")
}

/// Stores one parsed report and its per-source rows. Returns `true` when
/// the report is new, `false` when it was already imported (same message
/// or same report id / domain / window) or the user's report cap is hit.
/// Totals count every record in the report; only the stored rows are
/// capped, so the totals stay right while the drill-down stays bounded.
async fn store_report(
    pool: &SqlitePool,
    user_id: &str,
    mailbox_id: &str,
    message_id: &str,
    report: &DmarcReport,
) -> Result<bool, sqlx::Error> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM dmarc_reports WHERE user_id = ?")
        .bind(user_id)
        .fetch_one(pool)
        .await?;
    if count >= MAX_DMARC_REPORTS_PER_USER {
        return Ok(false);
    }
    let total: i64 = report.records.iter().map(|r| r.count as i64).sum();
    let passed: i64 = report
        .records
        .iter()
        .filter(|r| dmarc_passed(r))
        .map(|r| r.count as i64)
        .sum();
    let id = uuid::Uuid::new_v4().to_string();
    let inserted = sqlx::query(
        "INSERT OR IGNORE INTO dmarc_reports
            (id, user_id, mailbox_id, message_id, report_id, org_name, domain,
             date_begin, date_end, total_messages, passed_messages, failed_messages,
             records_stored, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(user_id)
    .bind(mailbox_id)
    .bind(message_id)
    .bind(&report.report_id)
    .bind(&report.org_name)
    .bind(&report.domain)
    .bind(report.date_begin)
    .bind(report.date_end)
    .bind(total)
    .bind(passed)
    .bind(total - passed)
    .bind(report.records.len().min(MAX_DMARC_RECORDS_PER_REPORT) as i64)
    .bind(iris_auth::now())
    .execute(pool)
    .await?
    .rows_affected();
    if inserted == 0 {
        return Ok(false);
    }
    for r in report.records.iter().take(MAX_DMARC_RECORDS_PER_REPORT) {
        sqlx::query(
            "INSERT INTO dmarc_records
                (id, report_id, source_ip, count, disposition, dkim_result, spf_result, passed)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(&id)
        .bind(&r.source_ip)
        .bind(r.count as i64)
        .bind(&r.disposition)
        .bind(&r.dkim_result)
        .bind(&r.spf_result)
        .bind(dmarc_passed(r) as i64)
        .execute(pool)
        .await?;
    }
    Ok(true)
}

/// Tries one inbox message as a DMARC report: the first attachment that
/// downloads, extracts, and parses as an aggregate report is imported.
/// Anything else (not a report, too big, undecodable) is not an error;
/// the message is simply not a report.
async fn import_message(
    s: &AppState,
    u: &CurrentUser,
    mailbox_id: &str,
    account: &str,
    m: &MessageSummary,
) -> Result<bool, ApiError> {
    let detail = s
        .jmap
        .get_message(&u.creds.user, &u.creds.pass, account, &m.id)
        .await?;
    for a in &detail.attachments {
        if a.size > MAX_DMARC_SCAN_ATTACHMENT_BYTES || !looks_like_report(&a.name, &a.content_type)
        {
            continue;
        }
        let bytes = s
            .jmap
            .download_blob_bytes(
                &u.creds.user,
                &u.creds.pass,
                account,
                &a.blob_id,
                &sanitized_filename(&a.name),
            )
            .await?;
        let text = match extract_text_attachment(&a.name, &a.content_type, &bytes) {
            Ok(out) => out.text,
            Err(_) => continue,
        };
        let report = match parse_aggregate_report(&text) {
            Ok(r) => r,
            Err(_) => continue,
        };
        return store_report(&s.pool, &u.user_id, mailbox_id, &m.id, &report)
            .await
            .map_err(ApiError::from);
    }
    Ok(false)
}

/// Scans the inbox for DMARC aggregate reports and imports what is new.
/// Bounded: at most `MAX_DMARC_SCAN_MESSAGES` attachment messages,
/// newest first, one report per message. A message already imported is
/// skipped without a detail fetch.
pub(crate) async fn scan(
    State(s): S,
    Extension(u): Extension<CurrentUser>,
    Path(mailbox_id): Path<String>,
) -> ApiResult {
    let account = owned_account(&s, &u, &mailbox_id).await?;
    check_read_limit(&s, &u).await?;
    let known: HashSet<String> = sqlx::query_scalar(
        "SELECT message_id FROM dmarc_reports WHERE user_id = ? AND mailbox_id = ?",
    )
    .bind(&u.user_id)
    .bind(&mailbox_id)
    .fetch_all(&s.pool)
    .await?
    .into_iter()
    .collect();
    let candidates = s
        .jmap
        .list_attachment_messages(
            &u.creds.user,
            &u.creds.pass,
            &account,
            MAX_DMARC_SCAN_MESSAGES,
        )
        .await?;
    let mut scanned = 0;
    let mut imported = 0;
    let mut skipped = 0;
    for m in &candidates {
        if known.contains(&m.id) {
            skipped += 1;
            continue;
        }
        scanned += 1;
        match import_message(&s, &u, &mailbox_id, &account, m).await {
            Ok(true) => imported += 1,
            Ok(false) => skipped += 1,
            Err(e) => {
                tracing::warn!("dmarc scan: message {} failed: {}", m.id, e.1);
                skipped += 1;
            }
        }
    }
    Ok(
        Json(json!({ "scanned": scanned, "imported": imported, "skipped": skipped }))
            .into_response(),
    )
}

#[derive(Debug, Serialize, FromRow)]
pub(crate) struct DomainRow {
    pub(crate) domain: String,
    pub(crate) reports: i64,
    pub(crate) messages: i64,
    pub(crate) passed: i64,
    pub(crate) failed: i64,
    pub(crate) last_end: i64,
}

#[derive(Debug, Serialize, FromRow)]
pub(crate) struct OffenderRow {
    pub(crate) source_ip: String,
    pub(crate) messages: i64,
    pub(crate) passed: i64,
    pub(crate) failed: i64,
}

#[derive(Debug, Serialize)]
pub(crate) struct Summary {
    pub(crate) reports: i64,
    pub(crate) domains: i64,
    pub(crate) messages: i64,
    pub(crate) passed: i64,
    pub(crate) failed: i64,
    pub(crate) first_begin: Option<i64>,
    pub(crate) last_end: Option<i64>,
    pub(crate) per_domain: Vec<DomainRow>,
    pub(crate) offenders: Vec<OffenderRow>,
}

async fn summary_for(
    pool: &SqlitePool,
    user_id: &str,
    mailbox_id: &str,
) -> Result<Summary, sqlx::Error> {
    let (reports, domains, messages, passed, failed, first_begin, last_end): (
        i64,
        i64,
        i64,
        i64,
        i64,
        Option<i64>,
        Option<i64>,
    ) = sqlx::query_as(
        "SELECT COUNT(*), COUNT(DISTINCT domain),
                COALESCE(SUM(total_messages), 0), COALESCE(SUM(passed_messages), 0),
                COALESCE(SUM(failed_messages), 0), MIN(date_begin), MAX(date_end)
         FROM dmarc_reports WHERE user_id = ? AND mailbox_id = ?",
    )
    .bind(user_id)
    .bind(mailbox_id)
    .fetch_one(pool)
    .await?;
    let per_domain: Vec<DomainRow> = sqlx::query_as(
        "SELECT domain, COUNT(*) AS reports,
                COALESCE(SUM(total_messages), 0) AS messages,
                COALESCE(SUM(passed_messages), 0) AS passed,
                COALESCE(SUM(failed_messages), 0) AS failed,
                MAX(date_end) AS last_end
         FROM dmarc_reports WHERE user_id = ? AND mailbox_id = ?
         GROUP BY domain ORDER BY messages DESC",
    )
    .bind(user_id)
    .bind(mailbox_id)
    .fetch_all(pool)
    .await?;
    let offenders: Vec<OffenderRow> = sqlx::query_as(
        "SELECT r.source_ip,
                COALESCE(SUM(r.count), 0) AS messages,
                COALESCE(SUM(CASE WHEN r.passed = 1 THEN r.count ELSE 0 END), 0) AS passed,
                COALESCE(SUM(CASE WHEN r.passed = 0 THEN r.count ELSE 0 END), 0) AS failed
         FROM dmarc_records r
         JOIN dmarc_reports p ON p.id = r.report_id
         WHERE p.user_id = ? AND p.mailbox_id = ?
         GROUP BY r.source_ip
         ORDER BY failed DESC, messages DESC LIMIT 10",
    )
    .bind(user_id)
    .bind(mailbox_id)
    .fetch_all(pool)
    .await?;
    Ok(Summary {
        reports,
        domains,
        messages,
        passed,
        failed,
        first_begin,
        last_end,
        per_domain,
        offenders,
    })
}

pub(crate) async fn summary(
    State(s): S,
    Extension(u): Extension<CurrentUser>,
    Path(mailbox_id): Path<String>,
) -> ApiResult {
    let _account = owned_account(&s, &u, &mailbox_id).await?;
    check_read_limit(&s, &u).await?;
    let summary = summary_for(&s.pool, &u.user_id, &mailbox_id).await?;
    Ok(Json(json!({ "summary": summary })).into_response())
}

#[derive(Debug, Serialize, FromRow)]
pub(crate) struct ReportRow {
    pub(crate) id: String,
    pub(crate) org_name: String,
    pub(crate) report_id: String,
    pub(crate) date_begin: i64,
    pub(crate) date_end: i64,
    pub(crate) total_messages: i64,
    pub(crate) passed_messages: i64,
    pub(crate) failed_messages: i64,
}

#[derive(Debug, Serialize, FromRow)]
pub(crate) struct SourceRow {
    pub(crate) source_ip: String,
    pub(crate) messages: i64,
    pub(crate) passed: i64,
    pub(crate) failed: i64,
}

#[derive(Debug, Serialize)]
pub(crate) struct DomainDetail {
    pub(crate) domain: String,
    pub(crate) reports: i64,
    pub(crate) messages: i64,
    pub(crate) passed: i64,
    pub(crate) failed: i64,
    pub(crate) report_list: Vec<ReportRow>,
    pub(crate) sources: Vec<SourceRow>,
}

/// A domain as it appears in a path segment: lowercase labels of letters,
/// digits and hyphens. Anything else (including an empty label, so a
/// trailing dot) is rejected before it reaches SQL.
pub(crate) fn valid_domain(domain: &str) -> bool {
    let d = domain.to_lowercase();
    !d.is_empty()
        && d.len() <= 253
        && d.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
                && !label.starts_with('-')
                && !label.ends_with('-')
        })
}

async fn domain_for(
    pool: &SqlitePool,
    user_id: &str,
    mailbox_id: &str,
    domain: &str,
) -> Result<Option<DomainDetail>, sqlx::Error> {
    let totals: Option<(i64, i64, i64, i64)> = sqlx::query_as(
        "SELECT COUNT(*), COALESCE(SUM(total_messages), 0),
                COALESCE(SUM(passed_messages), 0), COALESCE(SUM(failed_messages), 0)
         FROM dmarc_reports WHERE user_id = ? AND mailbox_id = ? AND domain = ?",
    )
    .bind(user_id)
    .bind(mailbox_id)
    .bind(domain)
    .fetch_optional(pool)
    .await?;
    let (reports, messages, passed, failed) = match totals {
        Some((0, _, _, _)) | None => return Ok(None),
        Some(t) => t,
    };
    let report_list: Vec<ReportRow> = sqlx::query_as(
        "SELECT id, org_name, report_id, date_begin, date_end,
                total_messages, passed_messages, failed_messages
         FROM dmarc_reports WHERE user_id = ? AND mailbox_id = ? AND domain = ?
         ORDER BY date_end DESC",
    )
    .bind(user_id)
    .bind(mailbox_id)
    .bind(domain)
    .fetch_all(pool)
    .await?;
    let sources: Vec<SourceRow> = sqlx::query_as(
        "SELECT r.source_ip,
                COALESCE(SUM(r.count), 0) AS messages,
                COALESCE(SUM(CASE WHEN r.passed = 1 THEN r.count ELSE 0 END), 0) AS passed,
                COALESCE(SUM(CASE WHEN r.passed = 0 THEN r.count ELSE 0 END), 0) AS failed
         FROM dmarc_records r
         JOIN dmarc_reports p ON p.id = r.report_id
         WHERE p.user_id = ? AND p.mailbox_id = ? AND p.domain = ?
         GROUP BY r.source_ip ORDER BY messages DESC",
    )
    .bind(user_id)
    .bind(mailbox_id)
    .bind(domain)
    .fetch_all(pool)
    .await?;
    Ok(Some(DomainDetail {
        domain: domain.to_string(),
        reports,
        messages,
        passed,
        failed,
        report_list,
        sources,
    }))
}

pub(crate) async fn domain_detail(
    State(s): S,
    Extension(u): Extension<CurrentUser>,
    Path((mailbox_id, domain)): Path<(String, String)>,
) -> ApiResult {
    let _account = owned_account(&s, &u, &mailbox_id).await?;
    if !valid_domain(&domain) {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_domain"));
    }
    check_read_limit(&s, &u).await?;
    let detail = domain_for(&s.pool, &u.user_id, &mailbox_id, &domain.to_lowercase()).await?;
    match detail {
        Some(d) => Ok(Json(json!({ "domain": d })).into_response()),
        None => Err(ApiError(StatusCode::NOT_FOUND, "not_found")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iris_mail::dmarc::DmarcRecord;

    fn fixture_report() -> DmarcReport {
        DmarcReport {
            org_name: "google.com".to_string(),
            report_id: "123456789".to_string(),
            domain: "example.com".to_string(),
            date_begin: 1761705600,
            date_end: 1761792000,
            records: vec![
                DmarcRecord {
                    source_ip: "192.0.2.10".to_string(),
                    count: 42,
                    disposition: "none".to_string(),
                    dkim_result: "pass".to_string(),
                    spf_result: "pass".to_string(),
                },
                DmarcRecord {
                    source_ip: "203.0.113.7".to_string(),
                    count: 3,
                    disposition: "quarantine".to_string(),
                    dkim_result: "fail".to_string(),
                    spf_result: "fail".to_string(),
                },
            ],
        }
    }

    async fn test_pool() -> SqlitePool {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("iris-dmarc-test-{nanos}.db"));
        let url = format!("sqlite://{}?mode=rwc", path.display());
        iris_database::init_pool(&url).await.unwrap()
    }

    /// Reports reference their owner, so the test user must exist first.
    async fn seed_user(pool: &SqlitePool, user_id: &str) {
        sqlx::query(
            "INSERT INTO app_users (id, email, display_name, role, status, created_at, updated_at)
             VALUES (?, ?, ?, 'user', 'active', ?, ?)",
        )
        .bind(user_id)
        .bind(format!("{user_id}@example.com"))
        .bind(user_id)
        .bind(iris_auth::now())
        .bind(iris_auth::now())
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn store_is_idempotent_and_summary_aggregates() {
        let pool = test_pool().await;
        seed_user(&pool, "u1").await;
        let report = fixture_report();
        assert!(store_report(&pool, "u1", "mb1", "msg1", &report)
            .await
            .unwrap());
        // Same message again: already imported.
        assert!(!store_report(&pool, "u1", "mb1", "msg1", &report)
            .await
            .unwrap());
        // Same report id / domain / window via another message: also one row.
        assert!(!store_report(&pool, "u1", "mb1", "msg2", &report)
            .await
            .unwrap());
        let s = summary_for(&pool, "u1", "mb1").await.unwrap();
        assert_eq!(s.reports, 1);
        assert_eq!(s.domains, 1);
        assert_eq!(s.messages, 45);
        assert_eq!(s.passed, 42);
        assert_eq!(s.failed, 3);
        assert_eq!(s.per_domain.len(), 1);
        assert_eq!(s.per_domain[0].domain, "example.com");
        // Worst offender first: the failing source, 3 failed of 3.
        assert_eq!(s.offenders.len(), 2);
        assert_eq!(s.offenders[0].source_ip, "203.0.113.7");
        assert_eq!(s.offenders[0].failed, 3);
        assert_eq!(s.offenders[1].source_ip, "192.0.2.10");
        assert_eq!(s.offenders[1].failed, 0);
    }

    #[tokio::test]
    async fn summary_and_detail_are_isolated_per_user_and_mailbox() {
        let pool = test_pool().await;
        seed_user(&pool, "u1").await;
        seed_user(&pool, "u2").await;
        let report = fixture_report();
        assert!(store_report(&pool, "u1", "mb1", "msg1", &report)
            .await
            .unwrap());
        // Same report content for another user and another mailbox: separate rows.
        assert!(store_report(&pool, "u2", "mb1", "msg1", &report)
            .await
            .unwrap());
        assert!(store_report(&pool, "u1", "mb2", "msg1", &report)
            .await
            .unwrap());
        assert_eq!(summary_for(&pool, "u1", "mb1").await.unwrap().reports, 1);
        assert_eq!(summary_for(&pool, "u2", "mb1").await.unwrap().reports, 1);
        assert_eq!(summary_for(&pool, "u1", "mb2").await.unwrap().reports, 1);
        assert_eq!(summary_for(&pool, "u1", "mb9").await.unwrap().reports, 0);
        let d = domain_for(&pool, "u1", "mb1", "example.com")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(d.reports, 1);
        assert_eq!(d.messages, 45);
        assert_eq!(d.sources.len(), 2);
        assert_eq!(d.sources[0].source_ip, "192.0.2.10");
        assert_eq!(d.sources[0].messages, 42);
        assert_eq!(d.report_list.len(), 1);
        assert_eq!(d.report_list[0].report_id, "123456789");
        // No such domain for this user: not found, not an empty page.
        assert!(domain_for(&pool, "u1", "mb1", "other.com")
            .await
            .unwrap()
            .is_none());
        assert!(domain_for(&pool, "u2", "mb1", "other.com")
            .await
            .unwrap()
            .is_none());
    }

    #[test]
    fn domain_validation_rejects_garbage() {
        assert!(valid_domain("example.com"));
        assert!(valid_domain("mail.example.co.id"));
        assert!(valid_domain("EXAMPLE.COM"));
        assert!(!valid_domain(""));
        assert!(!valid_domain("example.com."));
        assert!(!valid_domain("-example.com"));
        assert!(!valid_domain("exa_mple.com"));
        assert!(!valid_domain("example.com/path"));
        assert!(!valid_domain("exa mple.com"));
        assert!(!valid_domain(".."));
    }
}
