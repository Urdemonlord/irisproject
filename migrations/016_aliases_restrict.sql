-- Alias mailboxes: stop deleting alias rows with their mailbox.
--
-- Migration 015 created `mail_aliases.mailbox_id` as
-- `REFERENCES permanent_mailboxes(id) ON DELETE CASCADE`, so deleting
-- a mailbox silently deleted its alias rows (and, with them, the only
-- record that a live server alias has an IRIS owner). The unlink
-- handler refuses a mailbox that still owns aliases, but the schema
-- should not silently do the opposite behind its back: the mailbox
-- delete must fail while alias rows point at it, so the operator (or
-- the handler's 409) deals with the aliases first.
--
-- 015 is not edited in place: SQLx records each applied migration's
-- checksum, so a database that already applied 015 would fail startup
-- with a checksum mismatch. This migration changes the constraint
-- instead. SQLite has no ALTER CONSTRAINT, so the table is rebuilt
-- with the same columns in the same order and the same indexes, and
-- every row is carried over verbatim. No `PRAGMA foreign_keys=OFF`
-- is needed: the rows already satisfy the foreign keys (enforced on
-- every connection since the pool was introduced), and the table
-- being rebuilt is the child side, so dropping and renaming it never
-- touches a parent row. No explicit BEGIN/COMMIT either: the
-- migration runner wraps each file in its own transaction.
CREATE TABLE mail_aliases_new (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES app_users(id) ON DELETE CASCADE,
    mailbox_id TEXT NOT NULL REFERENCES permanent_mailboxes(id) ON DELETE RESTRICT,
    address TEXT NOT NULL UNIQUE,
    stalwart_account_id TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL
);

INSERT INTO mail_aliases_new
    (id, user_id, mailbox_id, address, stalwart_account_id, enabled, created_at)
    SELECT id, user_id, mailbox_id, address, stalwart_account_id, enabled, created_at
    FROM mail_aliases;

DROP TABLE mail_aliases;

ALTER TABLE mail_aliases_new RENAME TO mail_aliases;

CREATE INDEX idx_mail_aliases_user ON mail_aliases(user_id);
CREATE INDEX idx_mail_aliases_mailbox ON mail_aliases(mailbox_id);
