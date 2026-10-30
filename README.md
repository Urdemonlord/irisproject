# IRIS

A self-hosted email platform combining permanent mailboxes and temporary inboxes.

## Documentation

- [PRD](docs/01-PRD.md) — Product Requirements
- [TRD](docs/02-TRD.md) — Technical Requirements
- [AFD](docs/03-AFD.md) — Application Flow
- [DBD](docs/04-DBD.md) — Design Brief
- [BSD](docs/05-BSD.md) — Backend Schema
- [IPD](docs/06-IPD.md) — Implementation Plan
- [Design System](docs/design.md) — UI/UX Design
- [Production Readiness](docs/07-PRODUCTION-READINESS.md) — launch checklist + DNS status

## Quick start

```bash
cargo build
npm --prefix apps/web install
npm --prefix apps/web run dev
```

See `docs/06-IPD.md` for the phased implementation roadmap.

## Temporary inbox (Phase 3)

Anonymous, receive-only, 1-hour addresses at `TEMP_INBOX_DOMAIN`. Web UI: `/#/temp`.
Mail server routes `*@TEMP_INBOX_DOMAIN` to one catch-all mailbox (`TEMP_INBOX_MAIL_USER`);
each inbox sees only messages whose `To:` is its address and that arrived after creation.

```text
POST /api/v1/temp-inboxes                          -> 201 {id, address, access_token, expires_at, expires_in_seconds}
GET  /api/v1/temp-inboxes/:id                      -> status + countdown   (Authorization: Bearer <access_token>)
GET  /api/v1/temp-inboxes/:id/messages             -> message summaries
GET  /api/v1/temp-inboxes/:id/messages/:message_id -> message detail
```

Wrong/missing token: 404/401. Past `expires_at`: 410 `expired` (row flipped to `expired`).
Limits: 10 creates/hour per IP (`X-Real-IP` trusted only from loopback proxy), 3 active per browser (`iris_temp_client` cookie).
Expired inboxes are purged by the in-process cleanup job (see Deployment). Not yet: extend/delete endpoints, Bcc delivery in the UI (To: header only).

## Deployment

Single host: Stalwart (mail) + `iris-api` (Rust, SQLite) + static web build behind a TLS reverse proxy.

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

| Variable | Production value |
|---|---|
| `IRIS_ENV` | `production` (Secure cookies; refuses weak `SESSION_SECRET`) |
| `IRIS_HOST` / `IRIS_PORT` | `127.0.0.1` / `8000` (proxy in front) |
| `DATABASE_URL` | `sqlite:/var/lib/iris/iris.db` (persistent volume, not container FS) |
| `SESSION_SECRET` | `openssl rand -hex 32`. Also the key for sealed mail credentials: rotating it logs everyone out |
| `STALWART_API_URL` | `http://127.0.0.1:8092` |
| `TEMP_INBOX_DOMAIN`, `TEMP_INBOX_MAIL_USER`, `TEMP_INBOX_MAIL_PASSWORD` | catch-all domain + account |

Migrations run automatically on start. Run under systemd (`Restart=always`) as an unprivileged user.
The cleanup job runs inside the API every 60 s (log line `cleanup marked=… purged=…`); no cron needed.

Login does not keep the user's password: it mints a Stalwart app password per session (expires with the
session, destroyed on logout) and stores it ChaCha20-Poly1305-sealed in SQLite.

### 3. Web + reverse proxy

```bash
npm --prefix apps/web ci && npm --prefix apps/web run build   # -> apps/web/dist
```

Serve `apps/web/dist` and proxy `/api` and `/auth` to the API on the same origin (CSRF cookie + `SameSite`
rely on it). Proxy must set `X-Real-IP` (trusted only from loopback).

Production (`mail.meowlabs.id`, behind Cloudflare) runs **nginx**: use [`deploy/nginx/mail.meowlabs.id`](deploy/nginx/mail.meowlabs.id)
plus the [`iris-headers.conf`](deploy/nginx/iris-headers.conf) snippet (it also needs the host's `snippets/cloudflare-realip.conf`):

```bash
VITE_SITE_URL=https://mail.meowlabs.id npm --prefix apps/web run build
rsync -a --delete apps/web/dist/ /srv/iris/dist/
sudo cp deploy/nginx/iris-headers.conf /etc/nginx/snippets/
sudo cp deploy/nginx/mail.meowlabs.id /etc/nginx/sites-available/
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

Public site settings (build-time, all optional — defaults in `apps/web/vite.config.ts`):

| Variable | Default | Used for |
|---|---|---|
| `VITE_SITE_URL` | `https://mail.meowlabs.id` | canonical URLs, Open Graph image, `robots.txt`, `sitemap.xml` |
| `VITE_CONTACT_EMAIL` | `admin@meowlabs.id` | footer + Privacy/Terms contact |
| `VITE_CONTACT_ADDRESS` | empty (hidden) | postal address on Privacy/Terms |
| `VITE_ANALYTICS_SRC` | `https://analytics.meowlabs.id/script.js` | Umami tracking script |
| `VITE_ANALYTICS_WEBSITE_ID` | empty (off) | Umami website ID. Analytics loads only when set **and** the visitor allows it in the cookie notice; only route names are sent. |

Static pages: `/privacy/`, `/terms/`, `/404.html`. The build also emits `robots.txt` and `sitemap.xml`.

### 4. Verify before go-live

```bash
cargo test --workspace
npm --prefix apps/web run build
python3 scripts/license_audit.py                       # no GPL/LGPL/AGPL/SSPL deps
IRIS_URL=https://mail.meowlabs.id IRIS_USER=... IRIS_PASS=... \
  SMTP_HOST=mx-iris.meowlabs.id python3 scripts/security_smoke.py   # CSRF, rotation, creds, 25 MiB limit
```

Then work through `docs/07-PRODUCTION-READINESS.md` (TLS, backups, monitoring, policies).

## License

TBD
