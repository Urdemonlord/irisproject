-- Per-mailbox signatures, appended by the composer. text and html are
-- the same signature in two forms: the editor saves one and the server
-- derives the other, so the composer can append the right form for its
-- mode. An empty signature (both blank) is stored as no row at all.
CREATE TABLE signatures (
  user_id TEXT NOT NULL,
  mailbox_id TEXT NOT NULL,
  text TEXT NOT NULL DEFAULT '',
  html TEXT NOT NULL DEFAULT '',
  updated_at TEXT NOT NULL,
  PRIMARY KEY (user_id, mailbox_id)
);
