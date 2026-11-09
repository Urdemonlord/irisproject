<p align="left">
  <img src="docs/assets/iris-logo.svg" alt="IRIS — self-hosted webmail" width="360">
</p>

# IRIS

[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)

A self-hosted email platform combining permanent mailboxes and temporary inboxes.

- **Permanent mailboxes** — sign in with your mail-server account; five fixed
  folders (Inbox, Sent, Drafts, Archive, Trash), server-side search,
  plain-text compose with autosave drafts, reply/forward, attachments,
  undo send (10 second window), raw view, and DMARC report previews.
  Rich-text compose (bold, italic, lists, links) sends
  multipart/alternative with a sanitized HTML part, and each mailbox
  can have its own signature, set on the Organize page.
- **Temporary inboxes** — anonymous, receive-only addresses that live 1 hour,
  no sign-up. Web UI: `/#/temp`.
- **Transactional email API** — scoped API keys let your own apps send
  auth emails (verify, password reset, sign-in link) through IRIS
  (see [`docs/INTEGRATIONS.md`](docs/INTEGRATIONS.md)).
- **Separate HTML-mail origin** — HTML bodies can render in an isolated
  origin behind one-time tokens instead of the app origin (see
  [`docs/separate-mail-origin.md`](docs/separate-mail-origin.md)).

## Documentation

- [PRD](docs/01-PRD.md) — Product Requirements
- [TRD](docs/02-TRD.md) — Technical Requirements
- [AFD](docs/03-AFD.md) — Application Flow
- [DBD](docs/04-DBD.md) — Design Brief
- [BSD](docs/05-BSD.md) — Backend Schema
- [IPD](docs/06-IPD.md) — Implementation Plan
- [Design System](docs/design.md) — UI/UX Design
- [Production Readiness](docs/07-PRODUCTION-READINESS.md) — launch checklist + DNS status
- [Integrations](docs/INTEGRATIONS.md): transactional email API: keys, sending, limits, troubleshooting
- [Roadmap](ROADMAP.md) — what is built and what is planned

## Prerequisites

- Rust stable toolchain (`cargo`)
- Node.js 22 (`npm`)
- A mail server with a JMAP API — IRIS is built against
  [Stalwart](https://stalw.art/) (JMAP on loopback, e.g.
  `http://127.0.0.1:8092`). One account per permanent mailbox, plus one
  catch-all account receiving `*@TEMP_INBOX_DOMAIN`.
- A TLS reverse proxy for production (examples in [`deploy/`](deploy/):
  nginx or Caddy).
- DNS for your domains: MX, SPF, DKIM, DMARC, PTR — see
  [`docs/07-PRODUCTION-READINESS.md`](docs/07-PRODUCTION-READINESS.md) §4.

## Install with your AI agent

The fastest path on a fresh server: paste this into your coding agent
(Claude Code, Codex, Cursor, or any agent with shell access):

```text
Install IRIS on this server. Repo: https://github.com/Urdemonlord/irisproject
Read README.md and docs/07-PRODUCTION-READINESS.md first — do not guess
setup steps from memory.
Ask me for: the public mail domain (e.g. mail.example.com), and whether
a mail server already exists.
If there is no mail server yet, install Stalwart from its official image
(Stalwart is the only mail server IRIS is built against) and create one
permanent account plus one catch-all account for the temp inbox.
Then run: git clone <repo> && ./scripts/install.sh
Follow README Deployment to wire systemd + nginx/Caddy + DNS (MX, SPF,
DKIM, DMARC, PTR). Verify with the README checklist and show me the URLs
plus the health output.
Never commit .env, never print secrets, never invent a domain or DNS
value — ask when unsure.
```

What the agent will ask you (it cannot invent these): the public mail
domain, and whether a mail server already exists.

What [`scripts/install.sh`](scripts/install.sh) does: checks for cargo,
Node.js 22+, npm, openssl and curl; creates `.env` (mode 600) with a
generated `SESSION_SECRET`; builds the release API binary and the web
bundle; warns when the mail server is unreachable.

What it does NOT do: install a mail server, touch DNS, enable systemd
units, issue TLS certificates, or send any mail. Those need your domain
and are covered in [Deployment](#deployment).

## One-package install (Debian/Ubuntu)

Prefer a single install step over the agent flow above? The official
`.deb` bundles IRIS, Stalwart and Caddy with automatic TLS: see
[`packaging/README.md`](packaging/README.md). Its version follows the
IRIS release, and the `.deb`, tarball and `install.sh` are attached
to each GitHub release.

## Quick start (local dev)

```bash
git clone https://github.com/Urdemonlord/irisproject.git
cd irisproject
cp .env.example .env   # then edit: SESSION_SECRET, STALWART_API_URL, TEMP_INBOX_* values
cargo run -p iris-api  # API on http://127.0.0.1:8000 (migrations run on start)
npm --prefix apps/web install
npm --prefix apps/web run dev   # web on http://127.0.0.1:5173, proxies /api + /auth to the API
```

Open http://127.0.0.1:5173, sign in with a mail-server account, or grab a
temporary inbox at `/#/temp`. No seed data and no demo accounts: everything
you see comes from your own mail server.

## Temporary inbox

Anonymous, receive-only, 1-hour addresses at `TEMP_INBOX_DOMAIN`.
Web UI: `/#/temp`. The mail server routes `*@TEMP_INBOX_DOMAIN` to one
catch-all mailbox (`TEMP_INBOX_MAIL_USER`); each inbox sees only messages
whose `To:` is its address and that arrived after creation.

```text
POST /api/v1/temp-inboxes                          -> 201 {id, address, access_token, expires_at, expires_in_seconds}
GET  /api/v1/temp-inboxes/:id                      -> status + countdown   (Authorization: Bearer <access_token>)
GET  /api/v1/temp-inboxes/:id/messages             -> message summaries
GET  /api/v1/temp-inboxes/:id/messages/:message_id -> message detail
```

Wrong/missing token: 404/401. Past `expires_at`: 410 `expired` (row flipped to `expired`).
Limits: 10 creates/hour per IP (`X-Real-IP` trusted only from loopback proxy), 3 active per browser (`iris_temp_client` cookie).
Expired inboxes are purged by the in-process cleanup job (see Deployment). Not yet: extend/delete endpoints, Bcc delivery in the UI (To: header only).

## Stars, labels, folders and filters

Signed-in mailboxes can star messages, tag them with labels, keep their
own folders next to the five standard ones, and run filters on new mail.
Web UI: the sidebar (Starred, your folders and labels) and `/#/organize`.

- **Stars** are the JMAP `$flagged` keyword; **labels** are plain JMAP
  keywords (`a-z 0-9 _ -`, up to 32 chars). Label names are listed in
  SQLite (`mailbox_labels`) so an unused label still exists.
- **Custom folders** are JMAP mailboxes without a role. Only empty folders
  can be deleted; mail is never deleted with a folder.
- **Filters** (`mail_filters`) are compiled into one Sieve script,
  `iris-filters`, installed through JMAP for Sieve (RFC 9661) and made the
  account's active script, so rules run at delivery even when IRIS is
  closed. Activating it deactivates any other active Sieve script on that
  account. Requires a mail server with `urn:ietf:params:jmap:sieve` and the
  `fileinto`, `mailboxid` and `imap4flags` extensions (Stalwart has them).

```text
GET    /api/v1/mailboxes/:mb/messages?view=starred | label=<name> | folder_id=<id> | folder=<role>
POST   /api/v1/mailboxes/:mb/messages/:id/star     {starred}
POST   /api/v1/mailboxes/:mb/messages/:id/labels   {label, on}
POST   /api/v1/mailboxes/:mb/messages/:id/move     {folder} | {folder_id}
GET    /api/v1/mailboxes/:mb/labels                -> {labels: [{name}]}
POST   /api/v1/mailboxes/:mb/labels                {name}
DELETE /api/v1/mailboxes/:mb/labels/:name          (409 if a filter uses it)
POST   /api/v1/mailboxes/:mb/folders               {name}
PATCH  /api/v1/mailboxes/:mb/folders/:id           {name}
DELETE /api/v1/mailboxes/:mb/folders/:id           (409 folder_not_empty / folder_in_use_by_filter)
GET    /api/v1/mailboxes/:mb/filters               -> {filters: [{id, field, value, action, target}]}
POST   /api/v1/mailboxes/:mb/filters               {field: from|to|subject, value, action: move|label|star|read, target}
DELETE /api/v1/mailboxes/:mb/filters/:id
```

Limits: 50 labels, 50 custom folders and 50 filters per mailbox.

## Configuration

Every variable in [`.env.example`](.env.example) is read by the code
(`crates/config/src/lib.rs`, `apps/api/src/integration_apps.rs`). Copy it to
`.env` (git-ignored, mode 600) and edit.

| Variable | Default | Notes |
|---|---|---|
| `IRIS_ENV` | `development` | `production` enables Secure cookies and refuses a weak `SESSION_SECRET` |
| `IRIS_HOST` / `IRIS_PORT` | `127.0.0.1` / `8000` | API listen address (put a proxy in front) |
| `DATABASE_URL` | `sqlite:iris.db` | SQLite; use an absolute path on a persistent volume in production, e.g. `sqlite:/var/lib/iris/iris.db` |
| `SESSION_SECRET` | `dev-secret-change-in-prod` | `openssl rand -hex 32`. Also the key for sealed mail credentials: rotating it logs everyone out |
| `TURNSTILE_SECRET_KEY` | (empty) | Cloudflare Turnstile secret key for login verification; empty = logins skip the challenge. The public half is the web build-time variable `VITE_TURNSTILE_SITE_KEY` — see "Login CAPTCHA" below |
| `TURNSTILE_HOSTNAMES` | (empty) | Comma-separated frontend hostnames a Turnstile token must have been minted on (e.g. `mail.example.com`); while the secret is set, an empty list rejects every login. Loopback hostnames (`localhost`, `127.0.0.1`, `::1`) are rejected at startup in production (allowed outside production, for localhost testing) |
| `STALWART_API_URL` | `http://localhost:8092` | JMAP API, loopback only |
| `TEMP_INBOX_DOMAIN` | `inbox.example.com` | Domain for temporary addresses; needs a catch-all mailbox |
| `TEMP_INBOX_DEFAULT_EXPIRY_HOURS` | `1` | Temp inbox lifetime |
| `TEMP_INBOX_MAIL_USER`, `TEMP_INBOX_MAIL_PASSWORD` | (empty) | Catch-all mailbox credentials |
| `SEND_LOCAL_DOMAINS` | `example.com` | Domains IRIS may send to while no outbound relay is configured (comma-separated) |
| `SEND_EXTERNAL` | `false` | Allow sending to any domain once the mail server has a working outbound relay |
| `IRIS_INTEGRATION_EMAIL_FROM` | (empty) | Fixed sender for the transactional email API; leave blank to disable |
| `IRIS_INTEGRATION_EMAIL_ENABLED` | `false` | Explicit opt-in for integration email delivery |
| `IRIS_INTEGRATION_APPS` | (unset) | JSON registry of apps allowed to send transactional email; when unset, a built-in example registry is used — see `apps/api/src/integration_apps.rs` |
| `RESEND_API_KEY` | (empty) | When set, mail to other domains is relayed through Resend (first in the failover chain) |
| `IRIS_RESEND_API_URL` | (empty) | Local fake relay override; accepted only outside `production` and only on localhost |
| `BREVO_API_KEY` | (empty) | When set, Brevo is the second provider in the failover chain, tried when Resend answers with an HTTP error |
| `IRIS_BREVO_API_URL` | (empty) | Local fake relay override; accepted only outside `production` and only on localhost |
| `IRIS_PASSWORD_RESET_SERVICE_TOKEN`, `IRIS_PASSWORD_RESET_EMAIL_FROM`, `IRIS_PASSWORD_RESET_EMAIL_TO`, `IRIS_PASSWORD_RESET_PUBLIC_ORIGIN` | (empty) | Optional password-reset webhook for a trusted companion app; all must be set (plus a relay key, `RESEND_API_KEY` or `BREVO_API_KEY`) for the endpoint to answer. Deprecated `IRIS_TIMELESS_*` / `TIMELESS_PUBLIC_ORIGIN` names still work as fallback — see "Password-reset webhook" below |
| `MAIL_CONTENT_ORIGIN` | (empty) | Separate origin serving HTML mail bodies (bare `https://host`, no path); empty = same-origin srcdoc frame |
| `IRIS_ADMIN_EMAILS` | (empty) | Comma-separated emails allowed to see the read-only admin dashboard (`#/admin`); empty = nobody is an admin, every admin request answers 403 |

### Login CAPTCHA (Cloudflare Turnstile)

Optional bot gate on the sign-in form. When `TURNSTILE_SECRET_KEY` is
set, `POST /auth/login` accepts an optional `turnstile_token` and
verifies it server-side against Cloudflare's Siteverify API *before*
the mail server is contacted. The check requires `success === true`,
the `action` `"login"`, and a `hostname` inside `TURNSTILE_HOSTNAMES`;
a missing/invalid token is rejected with `turnstile_failed` (400). A
Siteverify outage — and a server-side error such as a wrong secret
(`invalid-input-secret`) — fails closed with `turnstile_unavailable`
(503). Failed verifications are capped per IP (20 per 15 minutes);
past the cap logins answer `too_many_attempts` (429) without calling
Siteverify. While the secret is set but `TURNSTILE_HOSTNAMES` is empty, every
login is rejected — set the allowlist to the same hostnames the
widget is registered for. Loopback hostnames (`localhost`,
`127.0.0.1`, `::1`) are rejected at startup in production (allowed
outside production, so localhost Turnstile testing works). When the secret is
unset, the API skips verification and logins are accepted without a
server-side Turnstile check (the API logs a startup warning) — but
that alone does not remove the challenge: if the web bundle was built
with `VITE_TURNSTILE_SITE_KEY`, the login form still renders the
widget and blocks submit until a token is minted, so users still see
(and must pass) the challenge even though the server never verifies
the token. To remove the challenge entirely, rebuild the web without
the site key.

The web widget needs the public site key at build time:
`VITE_TURNSTILE_SITE_KEY` (empty = no widget; see
`apps/web/vite.config.ts`). Keep the site key in `.env` as
`TURNSTILE_SITE_KEY` and pass it to the web build as
`VITE_TURNSTILE_SITE_KEY`. Create both keys in the Cloudflare
dashboard → Turnstile → Add site (hostname: your mail domain, e.g.
`mail.example.com`), then set the secret in `.env` and rebuild the web
with the site key. Tokens are single-use and expire after 5 minutes;
the form mints a fresh one after every login attempt.

IRIS sets no Content-Security-Policy on the app shell, so no CSP
change is needed for the Turnstile script
(`https://challenges.cloudflare.com/turnstile/v0/api.js`) and its
challenge iframe. The strict CSP in the mail-body renderer is
untouched — Turnstile never runs inside untrusted mail HTML.

### Password-reset webhook

An optional integration for a trusted companion app that needs to email a
password-reset link but has no mail-sending capability of its own:
`POST /internal/v1/password-reset-webhook` with
`Authorization: Bearer <IRIS_PASSWORD_RESET_SERVICE_TOKEN>` and JSON body
`{"token": "<43-char url-safe token>"}`. IRIS sends a fixed-template
message from `IRIS_PASSWORD_RESET_EMAIL_FROM` to the fixed recipient
`IRIS_PASSWORD_RESET_EMAIL_TO` (never taken from the request), containing
a link to `https://<IRIS_PASSWORD_RESET_PUBLIC_ORIGIN>/admin/reset#token=<token>`.
The endpoint answers `202 {"accepted": true}`; `401` for a wrong or
missing bearer token, `503` while any of the four variables (or a relay
key) is unset, `400` for a malformed token, `502` when
delivery fails. It is opt-in: with any variable unset the route still
exists but always answers `503`.

Deprecated names, kept as fallback when the generic name is unset or
empty: `IRIS_TIMELESS_SERVICE_TOKEN`, `IRIS_TIMELESS_EMAIL_FROM`,
`IRIS_TIMELESS_EMAIL_TO`, `TIMELESS_PUBLIC_ORIGIN`. The deprecated route
`POST /internal/v1/timeless-password-reset` serves the same handler.

The first companion app (Timeless) still calls the deprecated route, and
its client allow-lists that exact path — moving it to the generic route
is a coordinated change on the caller side: accept
`/internal/v1/password-reset-webhook` in its endpoint check (currently
`web/src/lib/server/password-recovery.js`, the `pathname !== ...` guard)
and point its reset URL at the new path. No IRIS-side change is needed
for that switch.

Public site settings (build-time, all optional — defaults in `apps/web/vite.config.ts`):

| Variable | Default | Used for |
|---|---|---|
| `VITE_SITE_URL` | `https://mail.example.com` | canonical URLs, Open Graph image, `robots.txt`, `sitemap.xml` |
| `VITE_CONTACT_EMAIL` | `admin@example.com` | footer + Privacy/Terms contact |
| `VITE_CONTACT_ADDRESS` | empty (hidden) | postal address on Privacy/Terms |
| `VITE_ANALYTICS_SRC` | empty (off) | Umami tracking script |
| `VITE_ANALYTICS_WEBSITE_ID` | empty (off) | Umami website ID. Analytics loads only when set **and** the visitor allows it in the cookie notice; only route names are sent. |
| `VITE_MAIL_CONTENT_ORIGIN` | empty (srcdoc) | separate HTML-mail origin for the web build |
| `VITE_TURNSTILE_SITE_KEY` | empty (no widget) | Cloudflare Turnstile site key for the login widget; must be set at build time whenever `TURNSTILE_SECRET_KEY` is set — otherwise the login form has no widget and every login fails. See "Login CAPTCHA" above |

Static pages: `/privacy/`, `/terms/`, `/404.html`. The build also emits `robots.txt` and `sitemap.xml`.

## Deployment

Single host: Stalwart (mail) + `iris-api` (Rust, SQLite) + static web build behind a TLS reverse proxy.

Or run the whole stack (API + web + Stalwart) with Docker Compose — the
fastest way to try IRIS on a server with Docker:

```bash
SESSION_SECRET=$(openssl rand -hex 32) docker compose -f deploy/compose/docker-compose.yaml up -d --build
# open http://127.0.0.1:8080
```

Then create Stalwart accounts (one permanent user per mailbox + the
catch-all `TEMP_INBOX_MAIL_USER`) and set a real `TEMP_INBOX_DOMAIN`
before real use. Known limitation: in the Compose stack, per-IP rate
limits see the proxy's IP, not the real client IP (the API trusts
`X-Real-IP` only from a loopback proxy) — the systemd + nginx deployment
below does not have this problem.

### 1. Mail server (Stalwart)

- Accounts: one permanent user per mailbox, plus the catch-all `TEMP_INBOX_MAIL_USER` receiving `*@TEMP_INBOX_DOMAIN`.
- Set max message size to 25 MiB (`x:MtaStageData` and `x:Email` `maxMessageSize: 26214400`), restart, confirm EHLO shows `SIZE 26214400`.
- Expose JMAP only on loopback (`STALWART_API_URL=http://127.0.0.1:8092`); public ports: 25 (+587/993 if users use IMAP/SMTP clients).
- DNS: MX, SPF, DKIM, DMARC, PTR. Current status and required changes: `docs/07-PRODUCTION-READINESS.md` §4.

### 2. API

```bash
cargo build --release -p iris-api
cp .env.example .env   # then edit
```

Migrations run automatically on start. Run under systemd (`Restart=always`) as an unprivileged user.
The cleanup job runs inside the API every 60 s (log line `cleanup marked=… purged=…`). As a safety net for when the API is down, `iris-api cleanup-once` runs one retention pass and exits; [`deploy/systemd/iris-cleanup.timer`](deploy/systemd/iris-cleanup.timer) runs it every 15 min. The timer is inert until activated — see [`deploy/systemd/README.md`](deploy/systemd/README.md) for the activation steps and rollback; overlapping runs with a live API are safe.

Login does not keep the user's password: it mints a Stalwart app password per session (expires with the
session, destroyed on logout) and stores it ChaCha20-Poly1305-sealed in SQLite.

### 3. Web + reverse proxy

The web build needs the Turnstile site key whenever the secret is set
(`TURNSTILE_SECRET_KEY` in `.env`): without it the bundle has no login
widget and every login fails. Read it from `.env` the same way below:

```bash
npm --prefix apps/web ci && VITE_TURNSTILE_SITE_KEY=$(grep '^TURNSTILE_SITE_KEY=' .env | cut -d= -f2- | tr -d ' ') npm --prefix apps/web run build   # -> apps/web/dist
```

Serve `apps/web/dist` and proxy `/api` and `/auth` to the API on the same origin (CSRF cookie + `SameSite`
rely on it). Proxy must set `X-Real-IP` (trusted only from loopback).

With nginx, use [`deploy/nginx/mail.example.com`](deploy/nginx/mail.example.com)
plus the [`iris-headers.conf`](deploy/nginx/iris-headers.conf) snippet:

```bash
VITE_SITE_URL=https://mail.example.com VITE_TURNSTILE_SITE_KEY=$(grep '^TURNSTILE_SITE_KEY=' .env | cut -d= -f2- | tr -d ' ') npm --prefix apps/web run build
rsync -a --delete apps/web/dist/ /srv/iris/dist/
sudo cp deploy/nginx/iris-headers.conf /etc/nginx/snippets/
sudo cp deploy/nginx/mail.example.com /etc/nginx/sites-available/
sudo nginx -t && sudo systemctl reload nginx
```

Alternatively, with Caddy, use [`deploy/caddy/Caddyfile`](deploy/caddy/Caddyfile) (Caddy ≥ 2.8). It proxies
`/api` + `/auth`, serves `dist` with long caching for hashed assets, shows `404.html` for unknown pages and sets
security headers. Install on the server:

```bash
rsync -a --delete apps/web/dist/ /srv/iris/dist/
sudo cp deploy/caddy/Caddyfile /etc/caddy/Caddyfile
sudo caddy validate --config /etc/caddy/Caddyfile && sudo systemctl reload caddy
```

### 4. Verify before go-live

```bash
cargo test --workspace
npm --prefix apps/web run build
python3 scripts/license_audit.py                       # no GPL/LGPL/AGPL/SSPL deps
IRIS_URL=https://mail.example.com IRIS_USER=... IRIS_PASS=... \
  SMTP_HOST=mx.example.com python3 scripts/security_smoke.py   # CSRF, rotation, creds, 25 MiB limit
```

Then work through `docs/07-PRODUCTION-READINESS.md` (TLS, backups, monitoring, policies).

## Contributing

See [`CONTRIBUTING.md`](CONTRIBUTING.md). Security issues: see [`SECURITY.md`](SECURITY.md) — do not open a public issue for a vulnerability.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or https://www.apache.org/licenses/LICENSE-2.0)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or https://opensource.org/licenses/MIT)

at your option.
