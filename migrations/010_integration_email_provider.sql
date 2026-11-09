-- Which relay delivered (or last attempted) each transactional send:
-- `resend` or `brevo`. NULL for rows written before this migration,
-- and for requests that never reached a relay.
ALTER TABLE integration_email_requests ADD COLUMN provider TEXT;
