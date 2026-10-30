# IRIS Technical Requirements Document (TRD)

**Product:** IRIS by Meow Labs\
**Primary URL:** `mail.meowlabs.id`\
**Status:** Draft v1.0

## 1. Technical direction

Build a lightweight Rust web application around an established mail
server. Do not implement SMTP, IMAP, JMAP, MIME parsing, or mail
delivery from scratch.

### Proposed stack

-   Rust stable
-   Axum for HTTP routing
-   Tokio async runtime
-   SQLx for application database access and migrations
-   SQLite for initial application metadata, provided the deployment is
    single-node and write concurrency remains modest
-   Vanilla TypeScript + Vite for the frontend
-   Stalwart Mail Server for SMTP, IMAP, JMAP, mailbox storage, and mail
    protocol handling
-   Nginx as public reverse proxy
-   Docker Compose for deployment
-   OpenTelemetry-compatible structured logs or tracing with
    sensitive-field redaction

Validate current Stalwart protocol support, licensing, configuration,
and API details against the selected release before implementation.

## 2. Logical architecture

``` text
Internet senders
      |
      | SMTP / MX
      v
mx.meowlabs.id
      |
      v
Stalwart Mail Server ---- mail storage
      |                         |
      | JMAP / supported API    |
      v                         |
Rust API (Axum)                 |
      |                         |
      +---- SQLite metadata     |
      |
      v
Vanilla TypeScript frontend
      |
      v
mail.meowlabs.id

Temporary addresses:
inbox.meowlabs.id -> MX routing -> Stalwart -> temporary-inbox mapping
```

The frontend must not connect directly to the mail server with
privileged credentials. All access is mediated by the Rust API or a
carefully scoped user-level JMAP session.

## 3. Domain and DNS requirements

Proposed names: - `meowlabs.id`: company website. - `mail.meowlabs.id`:
IRIS web application. - `mx.meowlabs.id`: mail server hostname. -
`inbox.meowlabs.id`: temporary address domain.

Required DNS and network configuration depends on the chosen mail-server
topology: - A/AAAA records for public hosts where applicable. - MX
records for `meowlabs.id` and `inbox.meowlabs.id`. - PTR/reverse DNS for
the sending IP if outbound mail is enabled. - SPF record authorizing the
actual outbound sender. - DKIM signing and public key records. - DMARC
policy and reporting mailbox. - Valid TLS certificates for web and mail
protocols. - Open only required ports. Common mail ports include SMTP
25, submission 587/465, IMAP 143/993, and HTTPS 443. Avoid exposing
unused protocols. - Confirm VPS provider policy permits inbound/outbound
SMTP and that port 25 is not blocked.

Never publish an MX record until the target mail server is reachable and
configured.

## 4. Application boundaries

### Rust API responsibilities

-   Authentication and sessions for the web application.
-   Authorization and mailbox identity mapping.
-   Temporary inbox creation and expiry.
-   Rate limiting and quotas.
-   Safe message metadata projection.
-   Mail-server API/JMAP integration.
-   Admin actions and audit events.
-   Health and readiness endpoints.

### Mail server responsibilities

-   SMTP reception and delivery.
-   Mailbox and message protocol behavior.
-   MIME parsing and message storage.
-   Authentication mechanisms supported by the server.
-   Mail queue and delivery retries.
-   DKIM signing and protocol-level policy where configured.

### Frontend responsibilities

-   Inbox, message detail, compose, and settings views.
-   Temporary inbox creation and expiry display.
-   Accessible navigation and responsive layout.
-   No secrets or privileged server credentials in browser code.

## 5. Integration strategy

Prefer JMAP for application-level email operations when supported and
suitable. Use per-user credentials or scoped sessions. Never use a
global administrator credential for routine user requests.

If a required JMAP operation is unavailable or unsupported in the
selected release, use a documented alternative API or a narrow
server-side integration. Avoid direct manipulation of mail storage
files.

## 6. Security requirements

-   HTTPS enforced at the reverse proxy.
-   Secure, HttpOnly, SameSite session cookies.
-   Session rotation after login and privilege changes.
-   CSRF protection for cookie-authenticated state-changing requests.
-   Password hashing with a modern password-hashing algorithm if the app
    stores passwords; preferably delegate mailbox credentials to the
    mail server where feasible.
-   Rate limits for login, inbox creation, message retrieval, and
    expensive searches.
-   Strict ownership checks on every mailbox and message operation.
-   Random temporary addresses generated with a cryptographically secure
    RNG.
-   Do not expose sequential IDs as authorization tokens.
-   HTML email rendered in a sandboxed iframe on a separate origin where
    feasible, with restrictive CSP and remote-content blocking by
    default.
-   Attachments served with safe `Content-Disposition`, MIME handling,
    and `X-Content-Type-Options: nosniff`.
-   Secrets managed through environment files or a secret manager, never
    committed to Git.
-   Audit admin changes, account changes, and security failures without
    recording email bodies.

## 7. Storage and retention

-   Mail server owns message content and attachment storage.
-   SQLite stores app-specific users, sessions, temporary inbox
    metadata, quotas, and audit events.
-   Avoid storing a second full copy of every message unless there is a
    clear product requirement.
-   Define backup scope for both mail-server data and app database.
-   Expired temporary inboxes must become inaccessible immediately, then
    be deleted asynchronously.
-   Deletion jobs must be idempotent and observable.
-   SQLite database and mail storage must not be placed on ephemeral
    container filesystems.

## 8. Operational requirements

-   Health endpoint: process and dependency status.
-   Readiness endpoint: indicates whether the app can serve requests.
-   Structured logs with request ID and redaction.
-   Metrics: API errors/latency, delivery queue state where available,
    disk space, storage usage, expiry-job failures, and rate-limit
    events.
-   Alert on disk usage, failed backups, mail queue growth, TLS expiry,
    and repeated SMTP delivery failures.
-   Keep admin interfaces private or strongly authenticated.
-   Run containers as non-root where supported and pin tested image
    versions.

## 9. Testing requirements

-   Unit tests for expiry calculations, random address handling,
    authorization, and rate limits.
-   Integration tests against a disposable mail-server environment.
-   End-to-end tests for login, receiving, reading, sending, temporary
    expiry, and attachment handling.
-   Security tests for IDOR, CSRF, XSS in email HTML, path traversal,
    oversized messages, and session fixation.
-   DNS validation and external delivery tests before production.
-   Backup restore drill.

## 10. Deployment topology

Initial production topology may use one VPS if resources and provider
networking are adequate. The mail server and application must have
persistent volumes, resource limits, backups, and monitoring. If the
existing Meow Labs VPS is resource-constrained or shares critical
workloads, prefer a separate mail VPS.

## 11. Definition of technical readiness

-   DNS and mail ports verified.
-   Mail server can receive external test messages.
-   Outbound test delivery works if sending is enabled.
-   SPF, DKIM, and DMARC validated.
-   App authorization tests pass.
-   Temporary expiry and cleanup verified.
-   Backup restore tested.
-   Monitoring and disk alerts active.
