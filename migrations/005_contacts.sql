-- Address book: one contact list per user, shared across their mailboxes.
-- Email is stored lowercased (see contacts::clean_email) so the per-user
-- unique email matches case-insensitively.
CREATE TABLE contacts (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES app_users(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    email TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (user_id, email)
);

CREATE INDEX idx_contacts_user_name ON contacts(user_id, name);
CREATE INDEX idx_contacts_user_email ON contacts(user_id, email);
