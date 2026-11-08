-- Store the provider rejection reason on transactional email requests,
-- so a `failed` row is diagnosable from the admin dashboard instead of
-- a bare status. Filled by the send path only; NULL means the request
-- never reached the relay or no reason was returned.
ALTER TABLE integration_email_requests ADD COLUMN error_message TEXT;
