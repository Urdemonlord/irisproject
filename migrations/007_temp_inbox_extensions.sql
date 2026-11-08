-- Temp-inbox extend: count of 1-hour extensions used, so the cap survives
-- restarts and config changes (it cannot be derived from expires_at, since
-- the default lifetime at creation time is not stored per inbox).
-- 006 is deliberately left free: the contacts and DMARC branches both
-- claim 005, and whichever of them renumbers second at merge time takes 006.
ALTER TABLE temporary_inboxes ADD COLUMN extensions INTEGER NOT NULL DEFAULT 0;
