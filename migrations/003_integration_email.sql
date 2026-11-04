CREATE TABLE integration_keys (
    key_id TEXT PRIMARY KEY,
    app_id TEXT NOT NULL CHECK (length(app_id) BETWEEN 1 AND 80),
    token_hash TEXT NOT NULL UNIQUE CHECK (length(token_hash) = 64),
    scopes_json TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'revoked')),
    created_at TEXT NOT NULL,
    expires_at TEXT,
    revoked_at TEXT,
    last_used_at TEXT,
    daily_send_limit INTEGER NOT NULL DEFAULT 100 CHECK (daily_send_limit BETWEEN 1 AND 10000)
);

CREATE INDEX idx_integration_keys_app_status ON integration_keys(app_id, status);

CREATE TABLE integration_email_requests (
    id TEXT PRIMARY KEY,
    key_id TEXT NOT NULL REFERENCES integration_keys(key_id) ON DELETE RESTRICT,
    idempotency_key TEXT NOT NULL CHECK (length(idempotency_key) BETWEEN 1 AND 128),
    template_id TEXT NOT NULL,
    recipient_hash TEXT NOT NULL CHECK (length(recipient_hash) = 64),
    request_hash TEXT NOT NULL CHECK (length(request_hash) = 64),
    status TEXT NOT NULL CHECK (status IN ('sending', 'accepted', 'failed', 'unknown')),
    provider_message_id TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (key_id, idempotency_key)
);

CREATE INDEX idx_integration_email_requests_created ON integration_email_requests(created_at);
