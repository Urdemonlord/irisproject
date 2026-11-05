//! Single source of truth for integration apps.
//!
//! Every app that may send transactional email through IRIS is registered
//! here exactly once, together with the HTTPS host its `action_url` must
//! point at and the email templates it owns. Everything that used to be
//! hard-coded in four places reads from [`INTEGRATION_APPS`]:
//!
//! * `integration_keys` — which apps exist and which scopes each app may
//!   hold (`email:template:<id>` per owned template, plus `email:status`);
//! * `transactional_email` — which host is allowed in `action_url` per app
//!   and which template (subject/intro/closing) renders for an app.
//!
//! To onboard a new app, add one entry below. Nothing else needs to change.

/// One transactional email template owned by an app.
pub(crate) struct Template {
    /// Template id as used by callers, e.g. `"auth.verify"`.
    /// The matching key scope is `email:template:<id>`.
    pub(crate) id: &'static str,
    pub(crate) subject: &'static str,
    pub(crate) intro: &'static str,
    pub(crate) closing: &'static str,
}

/// One registered integration app.
pub(crate) struct IntegrationApp {
    pub(crate) id: &'static str,
    /// Exact host allowed in `action_url` for this app (https, port 443).
    pub(crate) host: &'static str,
    /// Templates this app may send; each one also grants the matching
    /// `email:template:<id>` scope for keys issued to this app.
    pub(crate) templates: &'static [Template],
}

pub(crate) const INTEGRATION_APPS: &[IntegrationApp] = &[
    IntegrationApp {
        id: "gencv",
        host: "gencv.meowlabs.id",
        templates: &[
            Template {
                id: "auth.verify",
                subject: "Verify your GenCV email",
                intro: "Verify your email address to finish setting up GenCV.",
                closing: "If you did not create a GenCV account, ignore this message.",
            },
            Template {
                id: "auth.recovery",
                subject: "Reset your GenCV password",
                intro: "Use this link to reset your GenCV password.",
                closing: "If you did not request a password reset, ignore this message.",
            },
        ],
    },
    IntegrationApp {
        id: "academy",
        host: "academy.meowlabs.id",
        templates: &[Template {
            id: "auth.login_link",
            subject: "Your MeowLabs Academy sign-in link",
            intro: "Use this one-time link to sign in to MeowLabs Academy.",
            closing: "If you did not request this link, ignore this message.",
        }],
    },
    IntegrationApp {
        id: "aksaria",
        host: "aksaria.meowlabs.id",
        templates: &[
            Template {
                id: "auth.recovery",
                subject: "Reset kata sandi Aksaria",
                intro: "Gunakan tautan ini untuk mengatur ulang kata sandi Aksaria.",
                closing: "Jika kamu tidak meminta reset ini, abaikan pesan ini.",
            },
            Template {
                id: "auth.verification",
                subject: "Verifikasi email Aksaria",
                intro: "Gunakan tautan ini untuk memverifikasi email akun Aksaria. Tautan berlaku 24 jam.",
                closing: "Jika kamu tidak mendaftar di Aksaria, abaikan pesan ini.",
            },
        ],
    },
];

/// Look up a registered app by id.
pub(crate) fn app(app_id: &str) -> Option<&'static IntegrationApp> {
    INTEGRATION_APPS.iter().find(|app| app.id == app_id)
}

impl IntegrationApp {
    /// Look up one of this app's templates by id.
    pub(crate) fn template(&self, template_id: &str) -> Option<&'static Template> {
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

/// `"<gencv|academy|aksaria>"`, built from the registry so the CLI usage
/// text never drifts from the registered apps.
pub(crate) fn app_id_choices() -> String {
    INTEGRATION_APPS
        .iter()
        .map(|app| app.id)
        .collect::<Vec<_>>()
        .join("|")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn registry_is_consistent() {
        let mut app_ids = HashSet::new();
        let mut hosts = HashSet::new();
        for app in INTEGRATION_APPS {
            assert!(app_ids.insert(app.id), "duplicate app id {}", app.id);
            assert!(hosts.insert(app.host), "duplicate host {}", app.host);
            assert!(!app.templates.is_empty(), "app {} has no templates", app.id);
            let mut template_ids = HashSet::new();
            for template in app.templates {
                assert!(
                    template_ids.insert(template.id),
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
        assert!(scope_allowed("gencv", "email:template:auth.verify"));
        assert!(scope_allowed("gencv", "email:template:auth.recovery"));
        assert!(scope_allowed("aksaria", "email:template:auth.recovery"));
        assert!(scope_allowed("aksaria", "email:template:auth.verification"));
        assert!(scope_allowed("academy", "email:template:auth.login_link"));
        assert!(scope_allowed("gencv", "email:status"));
        assert!(!scope_allowed("academy", "email:template:auth.verify"));
        assert!(!scope_allowed("gencv", "email:template:auth.login_link"));
        assert!(!scope_allowed("unknown", "email:status"));
        assert!(!scope_allowed("gencv", "email:template:auth.unknown"));
        assert!(!scope_allowed("gencv", "email:other"));
    }
}
