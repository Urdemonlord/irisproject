PRAGMA foreign_keys = ON;

CREATE TABLE app_users (
    id TEXT PRIMARY KEY,
    email TEXT NOT NULL UNIQUE,
    display_name TEXT NOT NULL,
    role TEXT NOT NULL DEFAULT 'user'
        CHECK (role IN ('user', 'admin')),
    status TEXT NOT NULL DEFAULT 'active'
        CHECK (status IN ('active', 'disabled', 'pending')),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE auth_sessions (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES app_users(id) ON DELETE CASCADE,
    token_hash TEXT NOT NULL UNIQUE,
    expires_at TEXT NOT NULL,
    created_at TEXT NOT NULL,
    last_seen_at TEXT,
    revoked_at TEXT
);

CREATE INDEX idx_auth_sessions_user ON auth_sessions(user_id);
CREATE INDEX idx_auth_sessions_expiry ON auth_sessions(expires_at);

CREATE TABLE permanent_mailboxes (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES app_users(id) ON DELETE CASCADE,
    email_address TEXT NOT NULL UNIQUE,
    mail_server_account_id TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'active'
        CHECK (status IN ('active', 'disabled', 'pending')),
    quota_bytes INTEGER NOT NULL DEFAULT 1073741824,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX idx_mailboxes_user ON permanent_mailboxes(user_id);

CREATE TABLE temporary_inboxes (
    id TEXT PRIMARY KEY,
    address TEXT NOT NULL UNIQUE,
    access_token_hash TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'active'
        CHECK (status IN ('active', 'expired', 'deletion_pending', 'deleted', 'blocked')),
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    last_accessed_at TEXT,
    created_ip_hash TEXT,
    message_count INTEGER NOT NULL DEFAULT 0,
    bytes_used INTEGER NOT NULL DEFAULT 0,
    deleted_at TEXT
);

CREATE INDEX idx_temp_inboxes_expiry ON temporary_inboxes(status, expires_at);
CREATE INDEX idx_temp_inboxes_created ON temporary_inboxes(created_at);

CREATE TABLE rate_limit_events (
    id TEXT PRIMARY KEY,
    subject_hash TEXT NOT NULL,
    action TEXT NOT NULL,
    occurred_at TEXT NOT NULL,
    outcome TEXT NOT NULL CHECK (outcome IN ('allowed', 'blocked'))
);

CREATE INDEX idx_rate_limit_subject_time
    ON rate_limit_events(subject_hash, occurred_at);

CREATE TABLE audit_events (
    id TEXT PRIMARY KEY,
    actor_user_id TEXT REFERENCES app_users(id) ON DELETE SET NULL,
    event_type TEXT NOT NULL,
    target_type TEXT,
    target_id TEXT,
    request_id TEXT,
    occurred_at TEXT NOT NULL,
    metadata_json TEXT
);

CREATE INDEX idx_audit_events_time ON audit_events(occurred_at);
CREATE INDEX idx_audit_events_actor ON audit_events(actor_user_id, occurred_at);

CREATE TABLE app_settings (
    key TEXT PRIMARY KEY,
    value_json TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    updated_by TEXT REFERENCES app_users(id) ON DELETE SET NULL
);
