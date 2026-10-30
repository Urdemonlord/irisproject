-- Per-session Stalwart app password, sealed with a key derived from the session cookie
-- token (see iris_auth::seal). The user's real mail password is never stored.
ALTER TABLE auth_sessions ADD COLUMN mail_credential_sealed TEXT;
ALTER TABLE auth_sessions ADD COLUMN mail_credential_id TEXT;
