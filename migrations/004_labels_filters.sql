-- Labels and mail filters for permanent mailboxes.
--
-- Labels live on the mail server as JMAP keywords; this table is only the
-- list of label names a mailbox has defined, so an unused label does not
-- disappear and the UI can offer it.
CREATE TABLE mailbox_labels (
    mailbox_id TEXT NOT NULL REFERENCES permanent_mailboxes(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (mailbox_id, name)
);

-- Filter rules. The source of truth is here; every change regenerates the
-- mailbox's `iris-filters` Sieve script on the mail server, which applies
-- the rules to new mail at delivery time.
CREATE TABLE mail_filters (
    id TEXT PRIMARY KEY,
    mailbox_id TEXT NOT NULL REFERENCES permanent_mailboxes(id) ON DELETE CASCADE,
    field TEXT NOT NULL CHECK (field IN ('from', 'to', 'subject')),
    value TEXT NOT NULL,
    action TEXT NOT NULL CHECK (action IN ('move', 'label', 'star', 'read')),
    -- move: target mailbox id; label: label name; star/read: NULL.
    target TEXT,
    created_at TEXT NOT NULL
);

CREATE INDEX idx_mail_filters_mailbox ON mail_filters(mailbox_id, created_at);
