-- Registered integration apps, managed from the admin dashboard.
-- The runtime registry is loaded from these tables at startup (see
-- `integration_apps::seed_registry`); `IRIS_INTEGRATION_APPS` only
-- seeds them when they are empty. Deleting an app is refused while
-- any active key exists for it, so past delivery rows keep a valid
-- `app_id` and keys never dangle.
CREATE TABLE integration_apps (
    id TEXT PRIMARY KEY,
    host TEXT NOT NULL UNIQUE,
    created_at TEXT NOT NULL
);

CREATE TABLE integration_app_templates (
    app_id TEXT NOT NULL REFERENCES integration_apps(id) ON DELETE CASCADE,
    template_id TEXT NOT NULL,
    subject TEXT NOT NULL,
    intro TEXT NOT NULL,
    closing TEXT NOT NULL,
    PRIMARY KEY (app_id, template_id)
);
