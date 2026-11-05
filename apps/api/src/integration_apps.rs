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
//! The registry is configuration, not code: set `IRIS_INTEGRATION_APPS` to
//! a JSON array of `{id, host, templates: [{id, subject, intro, closing}]}`
//! to replace it with your own apps. When the variable is unset, IRIS
//! uses the built-in example registry below (generic `*.example.com`
//! apps) so a fresh checkout works out of the box. An invalid value logs
//! a warning and falls back to the example registry.
//!
//! To onboard a new app, set the variable — no code change needed.

use serde::Deserialize;
use std::collections::HashSet;
use std::sync::LazyLock;

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

/// The active registry, loaded once from `IRIS_INTEGRATION_APPS`.
static REGISTRY: LazyLock<Vec<IntegrationApp>> =
    LazyLock::new(|| match std::env::var("IRIS_INTEGRATION_APPS") {
        Ok(raw) if !raw.trim().is_empty() => match parse_registry(&raw) {
            Ok(apps) => apps,
            Err(err) => {
                tracing::warn!(
                    "IRIS_INTEGRATION_APPS is invalid ({err}); using the example registry"
                );
                default_registry()
            }
        },
        _ => default_registry(),
    });

fn default_registry() -> Vec<IntegrationApp> {
    parse_registry(DEFAULT_REGISTRY_JSON).expect("example registry is valid")
}

fn parse_registry(raw: &str) -> Result<Vec<IntegrationApp>, String> {
    let apps: Vec<IntegrationApp> = serde_json::from_str(raw).map_err(|err| err.to_string())?;
    if apps.is_empty() {
        return Err("registry must list at least one app".to_string());
    }
    let mut app_ids = HashSet::new();
    let mut hosts = HashSet::new();
    for app in &apps {
        if app.id.is_empty() || app.host.is_empty() {
            return Err("app id and host must not be empty".to_string());
        }
        if !app_ids.insert(app.id.as_str()) {
            return Err(format!("duplicate app id {}", app.id));
        }
        if !hosts.insert(app.host.as_str()) {
            return Err(format!("duplicate host {}", app.host));
        }
        if app.templates.is_empty() {
            return Err(format!("app {} has no templates", app.id));
        }
        let mut template_ids = HashSet::new();
        for template in &app.templates {
            if template.id.is_empty()
                || template.subject.is_empty()
                || template.intro.is_empty()
                || template.closing.is_empty()
            {
                return Err(format!("app {} has an empty template field", app.id));
            }
            if !template_ids.insert(template.id.as_str()) {
                return Err(format!(
                    "duplicate template {} for app {}",
                    template.id, app.id
                ));
            }
        }
    }
    Ok(apps)
}

/// Look up a registered app by id.
pub(crate) fn app(app_id: &str) -> Option<&'static IntegrationApp> {
    REGISTRY.iter().find(|app| app.id == app_id)
}

impl IntegrationApp {
    /// Look up one of this app's templates by id.
    pub(crate) fn template(&self, template_id: &str) -> Option<&Template> {
        self.templates
            .iter()
            .find(|template| template.id == template_id)
    }
}

/// `email:status` is allowed for every registered app; a template scope
/// `email:template:<id>` is allowed only when the app owns that template.
pub(crate) fn scope_allowed(app_id: &str, scope: &str) -> bool {
    if scope == "email:status" {
        return app(app_id).is_some();
    }
    let Some(template_id) = scope.strip_prefix("email:template:") else {
        return false;
    };
    app(app_id).is_some_and(|app| app.template(template_id).is_some())
}

/// `"<alpha|beta|gamma>"`, built from the registry so the CLI usage
/// text never drifts from the registered apps.
pub(crate) fn app_id_choices() -> String {
    REGISTRY
        .iter()
        .map(|app| app.id.as_str())
        .collect::<Vec<_>>()
        .join("|")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_is_consistent() {
        let mut app_ids = HashSet::new();
        let mut hosts = HashSet::new();
        for app in REGISTRY.iter() {
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
        assert!(scope_allowed("alpha", "email:template:auth.verify"));
        assert!(scope_allowed("alpha", "email:template:auth.recovery"));
        assert!(scope_allowed("gamma", "email:template:auth.recovery"));
        assert!(scope_allowed("gamma", "email:template:auth.verification"));
        assert!(scope_allowed("beta", "email:template:auth.login_link"));
        assert!(scope_allowed("alpha", "email:status"));
        assert!(!scope_allowed("beta", "email:template:auth.verify"));
        assert!(!scope_allowed("alpha", "email:template:auth.login_link"));
        assert!(!scope_allowed("unknown", "email:status"));
        assert!(!scope_allowed("alpha", "email:template:auth.unknown"));
        assert!(!scope_allowed("alpha", "email:other"));
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
        ] {
            assert!(parse_registry(bad).is_err(), "{bad} must be rejected");
        }
    }
}
