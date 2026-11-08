//! Single source of truth for integration apps.
//!
//! Every app that may send transactional email through IRIS is registered
//! here exactly once, together with the HTTPS host its `action_url` must
//! point at and the email templates it owns. Everything that used to be
//! hard-coded in four places reads from the registry:
//!
//! * `integration_keys` — which apps exist and which scopes each app may
//!   hold (`email:template:<id>` per owned template, plus `email:status`);
//! * `transactional_email` — which host is allowed in `action_url` per app
//!   and which template (subject/intro/closing) renders for an app.
//!
//! The registry lives in the database (`integration_apps` /
//! `integration_app_templates`, migration 009) and is loaded into
//! `AppState::registry` at startup. `IRIS_INTEGRATION_APPS` only seeds
//! the tables when they are empty: set it to a JSON array of
//! `{id, host, templates: [{id, subject, intro, closing}]}` to seed your
//! own apps, or leave it unset to seed the built-in example registry
//! below (generic `*.example.com` apps) so a fresh checkout works out
//! of the box. An invalid value logs a warning and seeds the example
//! registry instead. After seeding, the database is the source of
//! truth; the admin dashboard manages apps from there, and every
//! mutation refreshes the in-memory registry at once, so no restart
//! is needed for a new app to send.
//!
//! To onboard a new app, register it on the Admin page, no code
//! change and no server restart needed.

use serde::Deserialize;
use sqlx::SqlitePool;
use std::collections::HashSet;

/// At most this many templates per app: a key holds at most 4 scopes
/// (one per template plus `email:status`), so a fifth template could
/// never be sent with.
pub(crate) const MAX_TEMPLATES_PER_APP: usize = 4;
/// Template subjects render as a mail header: one short line.
pub(crate) const MAX_SUBJECT_CHARS: usize = 200;
/// Template intro/closing render as plain-text paragraphs: two short
/// paragraphs each, far more than any auth mail needs.
pub(crate) const MAX_TEMPLATE_TEXT_CHARS: usize = 2000;

/// One transactional email template owned by an app.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Template {
    /// Template id as used by callers, e.g. `"auth.verify"`.
    /// The matching key scope is `email:template:<id>`.
    pub(crate) id: String,
    pub(crate) subject: String,
    pub(crate) intro: String,
    pub(crate) closing: String,
}

/// One registered integration app.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct IntegrationApp {
    pub(crate) id: String,
    /// Exact host allowed in `action_url` for this app (https, port 443).
    pub(crate) host: String,
    /// Templates this app may send; each one also grants the matching
    /// `email:template:<id>` scope for keys issued to this app.
    pub(crate) templates: Vec<Template>,
}

/// Generic example apps, used when `IRIS_INTEGRATION_APPS` is unset.
/// Nothing here belongs to any real deployment.
const DEFAULT_REGISTRY_JSON: &str = r#"[
  {"id":"alpha","host":"alpha.example.com","templates":[
    {"id":"auth.verify","subject":"Verify your email","intro":"Use this link to verify your email address.","closing":"If you did not create this account, ignore this message."},
    {"id":"auth.recovery","subject":"Reset your password","intro":"Use this link to reset your password.","closing":"If you did not request a password reset, ignore this message."}]},
  {"id":"beta","host":"beta.example.com","templates":[
    {"id":"auth.login_link","subject":"Your sign-in link","intro":"Use this one-time link to sign in.","closing":"If you did not request this link, ignore this message."}]},
  {"id":"gamma","host":"gamma.example.com","templates":[
    {"id":"auth.recovery","subject":"Reset your password","intro":"Use this link to reset your password.","closing":"If you did not request this password reset, ignore this message."},
    {"id":"auth.verification","subject":"Verify your email","intro":"Use this link to verify your email address. The link is valid for 24 hours.","closing":"If you did not sign up, ignore this message."}]}
]"#;

/// The example registry, parsed from the constant above. The constant
/// is valid by construction; a parse failure here is a bug, not input.
pub(crate) fn default_registry() -> Vec<IntegrationApp> {
    parse_registry(DEFAULT_REGISTRY_JSON).expect("example registry is valid")
}

/// App ids are lowercase slugs: a lowercase letter first, then
/// lowercase letters, digits and hyphens, at most 64 chars.
pub(crate) fn valid_app_slug(app: &str) -> bool {
    !app.is_empty()
        && app.len() <= 64
        && app.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
        && app
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Template ids are lowercase slugs that may also carry dots and
/// underscores (`auth.verify`, `auth.login_link`): a lowercase letter
/// first, then lowercase letters, digits, dots, hyphens and
/// underscores, at most 64 chars.
pub(crate) fn valid_template_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
        && id.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-' || b == b'_'
        })
}

/// Which part of an app failed validation, with the reason. The admin
/// handlers map each variant to its error code; `parse_registry`
/// renders it as a plain message.
pub(crate) enum AppValidation {
    App(String),
    Host(String),
    Templates(String),
}

impl std::fmt::Display for AppValidation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let msg = match self {
            AppValidation::App(m) | AppValidation::Host(m) | AppValidation::Templates(m) => m,
        };
        f.write_str(msg)
    }
}

/// The one shared validation for an app, used by `parse_registry` for
/// the env seed and by the admin endpoints for dashboard input, so
/// both paths accept exactly the same apps.
pub(crate) fn validate_app(app: &IntegrationApp) -> Result<(), AppValidation> {
    if !valid_app_slug(&app.id) {
        return Err(AppValidation::App(format!(
            "app id {} must be a lowercase slug",
            app.id
        )));
    }
    if !crate::valid_dns_name(&app.host) {
        return Err(AppValidation::Host(format!(
            "host {} must be a bare hostname with no scheme, port, or userinfo",
            app.host
        )));
    }
    if app.templates.is_empty() || app.templates.len() > MAX_TEMPLATES_PER_APP {
        return Err(AppValidation::Templates(format!(
            "app {} must own 1 to {MAX_TEMPLATES_PER_APP} templates",
            app.id
        )));
    }
    let mut template_ids = HashSet::new();
    for template in &app.templates {
        if !valid_template_id(&template.id) {
            return Err(AppValidation::Templates(format!(
                "template id {} for app {} must be a lowercase slug",
                template.id, app.id
            )));
        }
        if !template_ids.insert(template.id.as_str()) {
            return Err(AppValidation::Templates(format!(
                "duplicate template {} for app {}",
                template.id, app.id
            )));
        }
        if template.subject.is_empty() || template.subject.chars().count() > MAX_SUBJECT_CHARS {
            return Err(AppValidation::Templates(format!(
                "template {} for app {} needs a subject of 1 to {MAX_SUBJECT_CHARS} chars",
                template.id, app.id
            )));
        }
        for (field, value) in [("intro", &template.intro), ("closing", &template.closing)] {
            if value.is_empty() || value.chars().count() > MAX_TEMPLATE_TEXT_CHARS {
                return Err(AppValidation::Templates(format!(
                    "template {} for app {} needs a {field} of 1 to {MAX_TEMPLATE_TEXT_CHARS} chars",
                    template.id, app.id
                )));
            }
        }
    }
    Ok(())
}

fn parse_registry(raw: &str) -> Result<Vec<IntegrationApp>, String> {
    let apps: Vec<IntegrationApp> = serde_json::from_str(raw).map_err(|err| err.to_string())?;
    if apps.is_empty() {
        return Err("registry must list at least one app".to_string());
    }
    let mut app_ids = HashSet::new();
    let mut hosts = HashSet::new();
    for app in &apps {
        validate_app(app).map_err(|err| err.to_string())?;
        if !app_ids.insert(app.id.as_str()) {
            return Err(format!("duplicate app id {}", app.id));
        }
        if !hosts.insert(app.host.as_str()) {
            return Err(format!("duplicate host {}", app.host));
        }
    }
    Ok(apps)
}

/// All registered apps, ordered by id, from the database.
pub(crate) async fn load_apps(pool: &SqlitePool) -> Result<Vec<IntegrationApp>, sqlx::Error> {
    load_stored_apps(pool)
        .await
        .map(|rows| rows.into_iter().map(|(app, _)| app).collect())
}

/// One row of the apps-plus-templates join: app id, host, created
/// time, then one template's fields (all NULL when the app has no
/// templates yet, which cannot happen through the API but the LEFT
/// JOIN still allows).
type AppRow = (
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

/// A registered app with the time it was registered, for the admin list.
pub(crate) async fn load_stored_apps(
    pool: &SqlitePool,
) -> Result<Vec<(IntegrationApp, String)>, sqlx::Error> {
    let rows: Vec<AppRow> = sqlx::query_as(
        "SELECT a.id, a.host, a.created_at, t.template_id, t.subject, t.intro, t.closing \
         FROM integration_apps a LEFT JOIN integration_app_templates t ON t.app_id = a.id \
         ORDER BY a.id, t.template_id",
    )
    .fetch_all(pool)
    .await?;
    let mut apps: Vec<(IntegrationApp, String)> = Vec::new();
    for (id, host, created_at, template_id, subject, intro, closing) in rows {
        if apps.last().is_none_or(|(app, _)| app.id != id) {
            apps.push((
                IntegrationApp {
                    id,
                    host,
                    templates: Vec::new(),
                },
                created_at,
            ));
        }
        if let (Some(template_id), Some(subject), Some(intro), Some(closing)) =
            (template_id, subject, intro, closing)
        {
            apps.last_mut().unwrap().0.templates.push(Template {
                id: template_id,
                subject,
                intro,
                closing,
            });
        }
    }
    Ok(apps)
}

/// Seed the tables from `IRIS_INTEGRATION_APPS` (or the example
/// registry when it is unset or invalid) the first time the server
/// runs against an empty registry, then return the registry. A
/// non-empty registry is never reseeded: the database is the source
/// of truth after the first seed, so an operator editing the env var
/// afterwards changes nothing until the tables are cleared by hand.
pub(crate) async fn seed_registry(
    pool: &SqlitePool,
    env_raw: Option<String>,
) -> Result<Vec<IntegrationApp>, sqlx::Error> {
    let existing = load_apps(pool).await?;
    if !existing.is_empty() {
        return Ok(existing);
    }
    let apps = match env_raw {
        Some(raw) if !raw.trim().is_empty() => match parse_registry(&raw) {
            Ok(apps) => apps,
            Err(err) => {
                tracing::warn!(
                    "IRIS_INTEGRATION_APPS is invalid ({err}); seeding the example registry"
                );
                default_registry()
            }
        },
        _ => default_registry(),
    };
    let now = iris_auth::now();
    let mut tx = pool.begin().await?;
    for app in &apps {
        sqlx::query("INSERT INTO integration_apps (id, host, created_at) VALUES (?,?,?)")
            .bind(&app.id)
            .bind(&app.host)
            .bind(&now)
            .execute(&mut *tx)
            .await?;
        for template in &app.templates {
            sqlx::query(
                "INSERT INTO integration_app_templates (app_id, template_id, subject, intro, closing) VALUES (?,?,?,?,?)",
            )
            .bind(&app.id)
            .bind(&template.id)
            .bind(&template.subject)
            .bind(&template.intro)
            .bind(&template.closing)
            .execute(&mut *tx)
            .await?;
        }
    }
    tx.commit().await?;
    Ok(apps)
}

/// Look up a registered app by id. Returns an owned clone: the
/// registry is a snapshot that admin mutations replace, so no caller
/// may hold a reference into it.
pub(crate) fn app(apps: &[IntegrationApp], app_id: &str) -> Option<IntegrationApp> {
    apps.iter().find(|app| app.id == app_id).cloned()
}

impl IntegrationApp {
    /// Look up one of this app's templates by id.
    pub(crate) fn template(&self, template_id: &str) -> Option<&Template> {
        self.templates
            .iter()
            .find(|template| template.id == template_id)
    }

    /// Every scope a key for this app may hold: one per owned template,
    /// plus `email:status`. Used as the default when the admin creates
    /// a key without picking scopes.
    pub(crate) fn all_scopes(&self) -> Vec<String> {
        let mut scopes: Vec<String> = self
            .templates
            .iter()
            .map(|template| format!("email:template:{}", template.id))
            .collect();
        scopes.push("email:status".to_string());
        scopes
    }
}

/// `email:status` is allowed for every registered app; a template scope
/// `email:template:<id>` is allowed only when the app owns that template.
pub(crate) fn scope_allowed(apps: &[IntegrationApp], app_id: &str, scope: &str) -> bool {
    if scope == "email:status" {
        return app(apps, app_id).is_some();
    }
    let Some(template_id) = scope.strip_prefix("email:template:") else {
        return false;
    };
    app(apps, app_id).is_some_and(|app| app.template(template_id).is_some())
}

/// `"<alpha|beta|gamma>"`, built from the registry so the CLI usage
/// text never drifts from the registered apps.
pub(crate) fn app_id_choices(apps: &[IntegrationApp]) -> String {
    apps.iter()
        .map(|app| app.id.as_str())
        .collect::<Vec<_>>()
        .join("|")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_is_consistent() {
        let apps = default_registry();
        let mut app_ids = HashSet::new();
        let mut hosts = HashSet::new();
        for app in &apps {
            assert!(
                app_ids.insert(app.id.as_str()),
                "duplicate app id {}",
                app.id
            );
            assert!(
                hosts.insert(app.host.as_str()),
                "duplicate host {}",
                app.host
            );
            assert!(!app.templates.is_empty(), "app {} has no templates", app.id);
            let mut template_ids = HashSet::new();
            for template in &app.templates {
                assert!(
                    template_ids.insert(template.id.as_str()),
                    "duplicate template {} for app {}",
                    template.id,
                    app.id
                );
                assert!(!template.subject.is_empty());
                assert!(!template.intro.is_empty());
                assert!(!template.closing.is_empty());
            }
        }
        assert_eq!(app_ids.len(), 3);
    }

    #[test]
    fn scope_rules_follow_the_registry() {
        let apps = default_registry();
        assert!(scope_allowed(&apps, "alpha", "email:template:auth.verify"));
        assert!(scope_allowed(
            &apps,
            "alpha",
            "email:template:auth.recovery"
        ));
        assert!(scope_allowed(
            &apps,
            "gamma",
            "email:template:auth.recovery"
        ));
        assert!(scope_allowed(
            &apps,
            "gamma",
            "email:template:auth.verification"
        ));
        assert!(scope_allowed(
            &apps,
            "beta",
            "email:template:auth.login_link"
        ));
        assert!(scope_allowed(&apps, "alpha", "email:status"));
        assert!(!scope_allowed(&apps, "beta", "email:template:auth.verify"));
        assert!(!scope_allowed(
            &apps,
            "alpha",
            "email:template:auth.login_link"
        ));
        assert!(!scope_allowed(&apps, "unknown", "email:status"));
        assert!(!scope_allowed(
            &apps,
            "alpha",
            "email:template:auth.unknown"
        ));
        assert!(!scope_allowed(&apps, "alpha", "email:other"));
    }

    #[test]
    fn registry_parses_from_json_and_rejects_bad_input() {
        let apps = parse_registry(
            r#"[{"id":"myapp","host":"app.example.com","templates":[
                {"id":"auth.verify","subject":"Verify","intro":"Verify.","closing":"Ignore."}]}]"#,
        )
        .unwrap();
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].id, "myapp");

        for bad in [
            "",
            "not json",
            "[]",
            r#"[{"id":"a","host":"a.example.com","templates":[]}]"#,
            r#"[{"id":"a","host":"a.example.com","templates":[{"id":"t","subject":"","intro":"x","closing":"y"}]}]"#,
            r#"[{"id":"a","host":"a.example.com","templates":[{"id":"t","subject":"s","intro":"x","closing":"y"}]},{"id":"a","host":"b.example.com","templates":[{"id":"t","subject":"s","intro":"x","closing":"y"}]}]"#,
            // Shared validation: a bad slug, a host with a scheme, and
            // five templates are all rejected here as well as in the
            // admin endpoints.
            r#"[{"id":"Bad App","host":"a.example.com","templates":[{"id":"t","subject":"s","intro":"x","closing":"y"}]}]"#,
            r#"[{"id":"a","host":"https://a.example.com","templates":[{"id":"t","subject":"s","intro":"x","closing":"y"}]}]"#,
            r#"[{"id":"a","host":"a.example.com:8443","templates":[{"id":"t","subject":"s","intro":"x","closing":"y"}]}]"#,
        ] {
            assert!(parse_registry(bad).is_err(), "{bad} must be rejected");
        }
    }

    #[test]
    fn shared_validation_covers_ids_hosts_and_template_limits() {
        let good = IntegrationApp {
            id: "myapp".into(),
            host: "app.example.com".into(),
            templates: vec![Template {
                id: "auth.verify".into(),
                subject: "Verify".into(),
                intro: "Verify.".into(),
                closing: "Ignore.".into(),
            }],
        };
        assert!(validate_app(&good).is_ok());
        // Bad app id.
        let mut bad = good.clone();
        bad.id = "Bad App!".into();
        assert!(matches!(validate_app(&bad), Err(AppValidation::App(_))));
        // Host with a scheme, a port, userinfo, or a single label.
        for host in [
            "https://app.example.com",
            "app.example.com:8443",
            "user@app.example.com",
            "localhost",
        ] {
            let mut bad = good.clone();
            bad.host = host.into();
            assert!(
                matches!(validate_app(&bad), Err(AppValidation::Host(_))),
                "host {host} must be rejected"
            );
        }
        // Five templates exceed the scope cap of four.
        let mut bad = good.clone();
        bad.templates = (0..5)
            .map(|i| Template {
                id: format!("t{i}"),
                subject: "S".into(),
                intro: "I".into(),
                closing: "C".into(),
            })
            .collect();
        assert!(matches!(
            validate_app(&bad),
            Err(AppValidation::Templates(_))
        ));
        // Duplicate template ids, a bad template id, and an over-long
        // subject are all template errors.
        let mut bad = good.clone();
        bad.templates.push(Template {
            id: "auth.verify".into(),
            subject: "Again".into(),
            intro: "I".into(),
            closing: "C".into(),
        });
        assert!(matches!(
            validate_app(&bad),
            Err(AppValidation::Templates(_))
        ));
        let mut bad = good.clone();
        bad.templates[0].id = "Bad Template".into();
        assert!(matches!(
            validate_app(&bad),
            Err(AppValidation::Templates(_))
        ));
        let mut bad = good.clone();
        bad.templates[0].subject = "x".repeat(201);
        assert!(matches!(
            validate_app(&bad),
            Err(AppValidation::Templates(_))
        ));
        let mut bad = good.clone();
        bad.templates[0].intro = "x".repeat(2001);
        assert!(matches!(
            validate_app(&bad),
            Err(AppValidation::Templates(_))
        ));
    }

    fn seed_json() -> String {
        r#"[{"id":"seeded","host":"seeded.example.com","templates":[
            {"id":"auth.verify","subject":"Verify","intro":"Verify.","closing":"Ignore."}]}]"#
            .to_string()
    }

    #[tokio::test]
    async fn seeding_from_env_wins_and_is_not_repeated() {
        let pool = iris_database::init_pool("sqlite::memory:").await.unwrap();
        let seeded = seed_registry(&pool, Some(seed_json())).await.unwrap();
        assert_eq!(seeded.len(), 1);
        assert_eq!(seeded[0].id, "seeded");
        // The tables now hold the seed, so a second seed with a
        // different env value changes nothing.
        let again = seed_registry(
            &pool,
            Some(
                r#"[{"id":"other","host":"other.example.com","templates":[
                {"id":"auth.verify","subject":"Verify","intro":"Verify.","closing":"Ignore."}]}]"#
                    .to_string(),
            ),
        )
        .await
        .unwrap();
        assert_eq!(again.len(), 1);
        assert_eq!(again[0].id, "seeded");
        let loaded = load_apps(&pool).await.unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].host, "seeded.example.com");
        assert_eq!(loaded[0].templates[0].id, "auth.verify");
    }

    #[tokio::test]
    async fn seeding_without_env_uses_the_example_registry() {
        let pool = iris_database::init_pool("sqlite::memory:").await.unwrap();
        let seeded = seed_registry(&pool, None).await.unwrap();
        assert_eq!(seeded.len(), 3);
        assert!(seeded.iter().any(|a| a.id == "alpha"));
        let stored = load_stored_apps(&pool).await.unwrap();
        assert_eq!(stored.len(), 3);
        assert!(stored.iter().all(|(_, created_at)| !created_at.is_empty()));
    }

    #[tokio::test]
    async fn seeding_with_invalid_env_falls_back_to_the_example_registry() {
        let pool = iris_database::init_pool("sqlite::memory:").await.unwrap();
        let seeded = seed_registry(&pool, Some("not json".to_string()))
            .await
            .unwrap();
        assert_eq!(seeded.len(), 3);
        assert!(seeded.iter().any(|a| a.id == "alpha"));
        assert_eq!(load_apps(&pool).await.unwrap().len(), 3);
    }
}
