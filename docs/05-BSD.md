# IRIS Backend Schema Documentation (BSD)

**Product:** IRIS by Meow Labs\
**Status:** Draft v1.0\
**Database:** SQLite for application metadata; message bodies and
attachments remain in mail-server storage.

## 1. Schema principles

-   Do not duplicate complete email bodies in the application database
    unless required by a documented feature.
-   Store identifiers returned by the mail server as opaque strings.
-   All timestamps use UTC in ISO 8601 at the API boundary and integer
    epoch or a consistent UTC representation in storage.
-   Use foreign keys and migrations.
-   Enforce authorization in application services, not only in the UI.
-   Temporary inbox records must be separable from permanent mailbox
    identities.
-   Avoid storing raw temporary access tokens; store a cryptographic
    hash.

## 2. Proposed SQLite schema

``` sql
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

CREATE TABLE temp_message_refs (
    id TEXT PRIMARY KEY,
    inbox_id TEXT NOT NULL REFERENCES temporary_inboxes(id) ON DELETE CASCADE,
    mail_server_message_id TEXT NOT NULL,
    received_at TEXT NOT NULL,
    size_bytes INTEGER NOT NULL DEFAULT 0,
    UNIQUE(inbox_id, mail_server_message_id)
);

CREATE INDEX idx_temp_message_refs_inbox ON temp_message_refs(inbox_id, received_at);

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
```

## 3. Schema notes

-   `app_users` represents application identities, not necessarily every
    mailbox account in the mail server.
-   `permanent_mailboxes.mail_server_account_id` maps an app mailbox to
    the mail-server identity. Treat it as an opaque reference.
-   `temporary_inboxes.access_token_hash` stores a hash of a
    high-entropy access token. Do not store the raw token.
-   `temp_message_refs` stores only references and metadata, not full
    message bodies.
-   `created_ip_hash` should be a keyed hash or otherwise
    privacy-preserving value. Define retention and access rules.
-   `metadata_json` must exclude passwords, tokens, message bodies, and
    attachment contents.
-   Do not rely on `message_count` and `bytes_used` as the only quota
    enforcement mechanism. Reconcile against authoritative mail-server
    state.

## 4. API data models

### Temporary inbox response

``` json
{
  "id": "opaque-id",
  "address": "random@inbox.meowlabs.id",
  "access_token": "returned-once",
  "created_at": "2026-10-30T00:00:00Z",
  "expires_at": "2026-10-30T01:00:00Z"
}
```

Return `access_token` only once over HTTPS. Do not include it in logs or
analytics. Consider using a secure, HttpOnly cookie instead of returning
a token to JavaScript, depending on the chosen anonymous-access flow.

### Message summary

``` json
{
  "id": "opaque-message-id",
  "from": "sender@example.net",
  "subject": "Example message",
  "received_at": "2026-10-30T00:20:00Z",
  "is_read": false,
  "has_attachments": false,
  "size_bytes": 2048
}
```

The message summary must not contain the body. The API must verify the
requesting user or temporary-inbox capability before returning message
details.

## 5. Suggested API routes

``` text
GET    /api/v1/health
GET    /api/v1/session
POST   /api/v1/auth/login
POST   /api/v1/auth/logout

GET    /api/v1/mailboxes
GET    /api/v1/mailboxes/{mailbox_id}/messages
GET    /api/v1/mailboxes/{mailbox_id}/messages/{message_id}
POST   /api/v1/mailboxes/{mailbox_id}/messages
POST   /api/v1/mailboxes/{mailbox_id}/messages/{message_id}/reply
POST   /api/v1/mailboxes/{mailbox_id}/messages/{message_id}/forward
GET    /api/v1/mailboxes/{mailbox_id}/messages/{message_id}/attachments/{attachment_id}

POST   /api/v1/temp-inboxes
GET    /api/v1/temp-inboxes/{inbox_id}
GET    /api/v1/temp-inboxes/{inbox_id}/messages
GET    /api/v1/temp-inboxes/{inbox_id}/messages/{message_id}
POST   /api/v1/temp-inboxes/{inbox_id}/extend
DELETE /api/v1/temp-inboxes/{inbox_id}

GET    /api/v1/admin/health
GET    /api/v1/admin/temp-inboxes
POST   /api/v1/admin/temp-inboxes/{inbox_id}/block
```

Routes are proposals. Prefer a session/capability design that does not
expose sensitive access tokens in URLs.

## 6. Data lifecycle

-   Sessions: delete expired/revoked records according to a short
    operational retention policy.
-   Temporary inbox: deny access at `expires_at`, then move to
    `deletion_pending`.
-   Temporary message references: delete with the inbox or retain
    minimal audit references only if justified.
-   Mail-server message deletion: use supported server APIs; do not
    delete files directly.
-   Audit events: retain only as long as operational/security needs
    require.
-   Rate-limit events: use a short retention period and avoid raw IP
    storage where possible.

## 7. Required migrations and constraints

-   Create initial schema via a versioned migration.
-   Add checks for non-negative quota and size fields.
-   Add unique constraints to prevent duplicate mailbox/address
    mappings.
-   Add transaction boundaries for inbox creation and expiry
    transitions.
-   Test cascade behavior before production.
-   Provide a backup and restore procedure for the SQLite database.
