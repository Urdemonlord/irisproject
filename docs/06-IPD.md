# IRIS Implementation Plan Documentation (IPD)

**Product:** IRIS by Meow Labs\
**Primary URL:** `mail.meowlabs.id`\
**Status:** Draft v1.0

## 1. Delivery strategy

Implement IRIS in vertical slices. First prove that the mail server can
receive and expose email safely, then build permanent webmail, then
temporary inboxes, then operational hardening.

Do not start with a public anonymous temporary-mail service. Begin with
private development/testing and enable public access only after abuse
controls and deletion have been verified.

## 2. Phase 0: Infrastructure proof

Tasks: - \[ \] Confirm the VPS and available CPU, RAM, disk, and backup
capacity. - \[ \] Confirm provider SMTP port 25 policy and reverse DNS
support. - \[ \] Select and pin a supported Stalwart release. - \[ \]
Create a staging environment. - \[ \] Configure persistent storage and
Docker networking. - \[ \] Configure DNS plan for `mail.meowlabs.id`,
`mx.meowlabs.id`, and `inbox.meowlabs.id`. - \[ \] Obtain TLS
certificates. - \[ \] Configure MX, SPF, DKIM, and DMARC. - \[ \]
Receive a message from an external provider. - \[ \] Send a test message
if outbound sending is in scope. - \[ \] Document backup and restore.

**Exit criteria:** external email reception works in staging;
protocol/API integration is proven; storage survives container
recreation.

## 3. Phase 1: Application foundation

Tasks: - \[ \] Initialize Rust workspace and Axum application. - \[ \]
Add configuration loading and startup validation. - \[ \] Add structured
tracing with redaction. - \[ \] Add SQLx migrations and SQLite
connection management. - \[ \] Add health and readiness endpoints. - \[
\] Set up frontend with Vite and Vanilla TypeScript. - \[ \] Establish
shared design tokens and responsive app shell. - \[ \] Configure CI for
formatting, linting, tests, and build. - \[ \] Add request IDs and
controlled error responses.

**Exit criteria:** app starts reproducibly, migrations work, CI passes,
and no secrets are committed.

## 4. Phase 2: Permanent webmail MVP

Tasks: - \[ \] Implement sign-in and session management. - \[ \] Map
application users to mail-server accounts. - \[ \] Fetch a paginated
inbox list through JMAP or a documented supported API. - \[ \] Build
message detail view. - \[ \] Add safe plain-text and isolated/sanitized
HTML rendering. - \[ \] Add attachment listing and safe downloads. - \[
\] Implement compose and send. - \[ \] Implement logout and session
revocation. - \[ \] Add ownership and cross-mailbox authorization
tests. - \[ \] Test receiving and sending against external providers.

**Exit criteria:** an authorized user can receive, read, compose, and
send email without cross-mailbox access.

## 5. Phase 3: Temporary inbox MVP

Tasks: - \[ \] Define the temporary-address routing method for the
selected mail-server release. - \[ \] Implement cryptographically random
address generation. - \[ \] Implement inbox access capability/session. -
\[ \] Implement creation rate limits and quotas. - \[ \] Implement
temporary message list and detail. - \[ \] Add expiry timestamp and
visible countdown. - \[ \] Implement immediate access denial after
expiry. - \[ \] Implement idempotent cleanup worker. - \[ \] Delete
messages through supported server interfaces. - \[ \] Add abuse disable
switch and administrative visibility. - \[ \] Verify temporary inboxes
cannot access permanent mailboxes.

**Exit criteria:** test messages arrive in the correct temporary inbox,
expired inboxes are inaccessible, and cleanup is observable and
repeatable.

## 6. Phase 4: Hardening and release

Tasks: - \[ \] Add secure cookie settings, CSRF defenses, and session
rotation. - \[ \] Test email HTML isolation and XSS payloads. - \[ \]
Test oversized message and attachment rejection. - \[ \] Test rate
limits and temporary inbox enumeration resistance. - \[ \] Configure
disk, TLS, backup, and queue alerts. - \[ \] Perform backup restore
drill. - \[ \] Validate SPF, DKIM, and DMARC. - \[ \] Run external
deliverability tests. - \[ \] Add privacy notice and acceptable-use
policy. - \[ \] Add retention and deletion documentation. - \[ \] Review
dependency licenses and update strategy. - \[ \] Complete a production
readiness checklist.

**Exit criteria:** no known critical/high security issues remain;
operational recovery has been tested; public temporary inbox access is
explicitly approved.

## 7. Phase 5: Post-MVP

Possible additions: - Folders, labels, and more advanced search. -
Real-time updates via server-sent events or WebSockets. - Multiple
permanent mailboxes per user. - Mailbox alias management. - Admin
analytics and quota dashboard. - User self-service account management. -
Custom retention settings. - Domain verification and custom domains. -
Optional JMAP-native client enhancements.

## 8. Suggested repository layout

``` text
iris/
├── apps/
│   ├── api/                 # Rust + Axum
│   └── web/                 # Vite + Vanilla TypeScript
├── crates/
│   ├── config/
│   ├── auth/
│   ├── mail/
│   ├── temp_inbox/
│   ├── database/
│   └── observability/
├── migrations/
├── deploy/
│   ├── compose.yaml
│   ├── nginx/
│   └── stalwart/
├── docs/
│   ├── 01-PRD.md
│   ├── 02-TRD.md
│   ├── 03-AFD.md
│   ├── 04-DBD.md
│   ├── 05-BSD.md
│   └── 06-IPD.md
├── .env.example
├── Cargo.toml
└── README.md
```

This is a proposed structure, not a requirement to use a Rust workspace
if a single crate is sufficient initially.

## 9. CI quality gates

-   `cargo fmt --check`
-   `cargo clippy --all-targets --all-features -- -D warnings`
-   `cargo test`
-   Frontend typecheck and production build
-   Migration tests against a temporary database
-   API authorization integration tests
-   Dependency vulnerability review
-   Container configuration validation

## 10. Release checklist

-   [ ] Domain and DNS are correct.
-   [ ] TLS works for public endpoints.
-   [ ] SMTP reception and JMAP integration pass tests.
-   [ ] SPF, DKIM, and DMARC are validated.
-   [ ] Permanent mailbox login and message access work.
-   [ ] Sending and attachment behavior are tested.
-   [ ] Temporary inbox isolation and expiry work.
-   [ ] Rate limits and quotas are enabled.
-   [ ] Logs redact sensitive content.
-   [ ] Backups and restore have been tested.
-   [ ] Monitoring and alerts are active.
-   [ ] Privacy and acceptable-use policies are published.
-   [ ] Public anonymous access is enabled only after abuse review.
