-- Outgoing mail held for the undo window: a compose send is queued here
-- and dispatched by the background sweeper once send_at passes, instead
-- of going out in the request. Undo deletes the row before then.
--
-- payload is the validated compose request as JSON (attachments stay
-- base64, as the web sent them). credential_sealed is the user's mail
-- app password sealed with the server secret alone (see
-- `iris_auth::seal_server`): the sweeper has no session token to open a
-- session-sealed credential with, so the row carries its own sealed copy.
-- The row is deleted on dispatch, on undo, or when its retry deadline
-- passes; failed rows are pruned by the retention job.
CREATE TABLE pending_sends (
  id TEXT PRIMARY KEY,
  user_id TEXT NOT NULL,
  mailbox_id TEXT NOT NULL,
  account_id TEXT NOT NULL,
  email TEXT NOT NULL,
  payload TEXT NOT NULL,
  credential_sealed TEXT NOT NULL,
  send_at TEXT NOT NULL,
  retry_until TEXT NOT NULL,
  next_try_at TEXT,
  attempts INTEGER NOT NULL DEFAULT 0,
  status TEXT NOT NULL DEFAULT 'pending',
  error TEXT,
  created_at TEXT NOT NULL
);
CREATE INDEX idx_pending_sends_due ON pending_sends(status, send_at);
