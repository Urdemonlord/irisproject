-- Mail aliases and per-domain catch-all settings (alias feature).
--
-- `mail_aliases` mirrors the aliases IRIS created on the mail server:
-- Stalwart is delivery truth (the alias lives in the account's
-- server-side alias list), the IRIS row is the ownership/audit mirror
-- (which user, which mailbox, which server account). Rows are written
-- only after the Stalwart change lands; a failed write compensates by
-- undoing the Stalwart change (same pattern as linked_mailboxes).
-- `enabled` mirrors the server flag at creation time (always true:
-- IRIS creates enabled aliases only; there is no toggle endpoint).
-- `address` is globally unique: one address belongs to one account.
--
-- `domain_catch_all` mirrors the domain's catch-all target: the
-- mailbox every otherwise-unmatched address in the domain delivers
-- to. A NULL target means catch-all is off (unknown recipients are
-- rejected); that is the only sense in which the row is "disabled",
-- so there is no separate enabled column to drift out of sync.
CREATE TABLE mail_aliases (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES app_users(id) ON DELETE CASCADE,
    mailbox_id TEXT NOT NULL REFERENCES permanent_mailboxes(id) ON DELETE RESTRICT,
    address TEXT NOT NULL UNIQUE,
    stalwart_account_id TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL
);

CREATE INDEX idx_mail_aliases_user ON mail_aliases(user_id);
CREATE INDEX idx_mail_aliases_mailbox ON mail_aliases(mailbox_id);

CREATE TABLE domain_catch_all (
    domain TEXT PRIMARY KEY,
    target_address TEXT,
    updated_at TEXT NOT NULL
);
