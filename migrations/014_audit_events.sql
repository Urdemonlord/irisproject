-- The initial schema already created audit_events, but nothing ever
-- wrote to it: no code references the table, and no retention job
-- touches it. Rather than a second table, reshape it into the audit
-- trail the API records: rename the columns to the names the code
-- uses, and add the actor email and client IP the trail stores.
-- The rename also retargets the two indexes from 001 onto
-- (actor_user_id, created_at) and (created_at); the action index
-- below is the only new one. Rows are kept forever; no retention
-- prune touches this table.
ALTER TABLE audit_events RENAME COLUMN event_type TO action;
ALTER TABLE audit_events RENAME COLUMN occurred_at TO created_at;
ALTER TABLE audit_events RENAME COLUMN metadata_json TO detail;
ALTER TABLE audit_events ADD COLUMN actor_email TEXT NOT NULL DEFAULT '';
ALTER TABLE audit_events ADD COLUMN ip TEXT;
CREATE INDEX idx_audit_events_action_created ON audit_events(action, created_at);
