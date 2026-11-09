-- Per-session app passwords for linked mailboxes. The primary mailbox's
-- app password lives on the session row itself (migration 002); a linked
-- mailbox mints its own at link/unlock time, sealed the same way
-- (session secret + session token), and dies with the session: logout
-- destroys the app passwords and deletes these rows, and the retention
-- job's session prune removes them through ON DELETE CASCADE. The user's
-- real mail password is never stored anywhere.
CREATE TABLE session_mailbox_credentials (
  session_id TEXT NOT NULL REFERENCES auth_sessions(id) ON DELETE CASCADE,
  mailbox_id TEXT NOT NULL REFERENCES permanent_mailboxes(id) ON DELETE CASCADE,
  mail_credential_sealed TEXT NOT NULL,
  mail_credential_id TEXT NOT NULL,
  created_at TEXT NOT NULL,
  PRIMARY KEY (session_id, mailbox_id)
);
CREATE INDEX idx_session_mailbox_creds_session ON session_mailbox_credentials(session_id);
