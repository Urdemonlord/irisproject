-- DMARC aggregate reports imported by the dashboard scan.
--
-- One row per imported report. `total_messages` / `passed_messages` /
-- `failed_messages` count every `<record>` in the report, even when
-- `records_stored` is capped (see MAX_DMARC_RECORDS_PER_REPORT), so the
-- totals stay right while the per-source drill-down stays bounded.
-- A report is identified by the message it came from: re-scanning the
-- same inbox never imports it twice (the message unique key), and the
-- same report id for the same domain and window is one row per mailbox
-- even if it arrives twice (the report unique key).
CREATE TABLE dmarc_reports (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES app_users(id) ON DELETE CASCADE,
    mailbox_id TEXT NOT NULL,
    message_id TEXT NOT NULL,
    report_id TEXT NOT NULL,
    org_name TEXT NOT NULL,
    domain TEXT NOT NULL,
    date_begin INTEGER NOT NULL,
    date_end INTEGER NOT NULL,
    total_messages INTEGER NOT NULL,
    passed_messages INTEGER NOT NULL,
    failed_messages INTEGER NOT NULL,
    records_stored INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE (user_id, mailbox_id, message_id),
    UNIQUE (user_id, mailbox_id, report_id, domain, date_begin)
);

CREATE INDEX idx_dmarc_reports_user_domain ON dmarc_reports(user_id, mailbox_id, domain);
CREATE INDEX idx_dmarc_reports_user_created ON dmarc_reports(user_id, mailbox_id, created_at);

-- One row per `<record>` in a report, up to the per-report cap.
-- `passed` is 1 when either aligned check passed (DKIM or SPF), else 0.
CREATE TABLE dmarc_records (
    id TEXT PRIMARY KEY,
    report_id TEXT NOT NULL REFERENCES dmarc_reports(id) ON DELETE CASCADE,
    source_ip TEXT NOT NULL,
    count INTEGER NOT NULL,
    disposition TEXT NOT NULL,
    dkim_result TEXT NOT NULL,
    spf_result TEXT NOT NULL,
    passed INTEGER NOT NULL CHECK (passed IN (0, 1))
);

CREATE INDEX idx_dmarc_records_report ON dmarc_records(report_id);
CREATE INDEX idx_dmarc_records_ip ON dmarc_records(source_ip);
