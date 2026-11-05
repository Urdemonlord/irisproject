# IRIS Production Readiness Checklist

Status as of 2026-11-06, production. IRIS is live: systemd `iris-api`
(listening on `127.0.0.1:8000`, `IRIS_ENV=production`) behind nginx
vhosts for the app origin and the separate mail-content origin, with
per-subdomain ECDSA certificates (app origin cert to 2027-01-28,
content origin cert to 2027-02-03); Stalwart JMAP on `127.0.0.1:8092`;
both origins resolve via Cloudflare. `[x]` = verified with the proof
named; `[ ]` = open. Items carried over from the 2026-10-30 staging
review keep their original proof unless re-verified live (noted inline).

## 1. Application security

- [x] CSRF: double-submit (`iris_csrf` cookie + `X-CSRF-Token` header) on every POST/PUT/PATCH/DELETE, constant-time compare. Proof: `scripts/security_smoke.py` (missing/mismatched token → 403).
- [x] Session rotation on login: new session token + new CSRF token; previous session from the same browser revoked. Proof: smoke test (old cookie → 401).
- [x] Mail credential persistence: login mints a per-session Stalwart app password (`x:AppPassword`, expires with the session), stored ChaCha20-Poly1305-sealed (key derived from `SESSION_SECRET`). User password never stored; secret destroyed at logout/expiry. Proof: smoke test DB checks; session survives API restart.
- [x] Production refuses to boot with `SESSION_SECRET` < 32 chars or the dev default.
- [x] HTML email isolation: `<iframe sandbox="">` (no scripts, no same-origin), CSP `img-src data: cid:` blocks trackers and remote CSS. Proof: Playwright hostile-HTML probe (script/onerror blocked, 0 remote fetches, parent title untouched).
- [x] Oversize mail rejected at SMTP: Stalwart `maxMessageSize` 26214400 (25 MiB), advertised in EHLO `SIZE`. Proof: smoke test sends 26 MiB with and without `SIZE=` → 5xx.
- [ ] Rotating `SESSION_SECRET` invalidates all sealed credentials: users must log in again. Document in the ops runbook.
- [x] Separate origin for HTML mail rendering (TRD §6 "where feasible"): infra live since 2026-11-05 — DNS + ECDSA cert + nginx vhost + `MAIL_CONTENT_ORIGIN` set; origin smoke checks pass (`/` → 404, `html-body` without token → 404 with `frame-ancestors`). Not yet end-to-end: the deployed web build has no content-origin flag baked in, so browsers still use the same-origin `srcdoc` fallback until the web is rebuilt — see `docs/separate-mail-origin.md` §5–§6.

## 2. Retention

- [x] Expired temp inboxes are inaccessible immediately (410) and moved to `deletion_pending` → mail purged via JMAP → `deleted` by `cleanup.rs` every 60 s. Idempotent; logs `cleanup marked= inboxes= purged= swept=`. Proof: e2e (delivered message gone from catch-all after expiry).
- [x] Catch-all sweep deletes mail older than max inbox life + 1 h (covers Bcc / never-issued addresses).
- [x] Expired/revoked sessions pruned after 7 days, rate-limit events after 2 days.
- [ ] Alert when the cleanup log line stops appearing or reports errors.

## 3. Dependencies

- [x] License audit: `python3 scripts/license_audit.py` (fails on GPL/LGPL/AGPL/SSPL). Current result: 0 copyleft-denied across cargo + npm.
- [ ] `cargo audit` / `npm audit` in CI.

## 4. DNS / email authentication

Checked against authoritative NS (Cloudflare) on 2026-10-30.

Live deployment (verified 2026-11-06): the app and mail-content origins
resolve via Cloudflare. The record table below is the 2026-10-30 staging
check and has not been re-verified against the live domains — treat the
DMARC/SPF/PTR actions as still open until re-checked.

| Record | example.com | inbox.example.com |
|---|---|---|
| MX | none (apex does not receive) | `10 mx.example.com` → 203.0.113.10 |
| SPF | `v=spf1 mx -all` ⚠ apex has no MX, so this authorizes nobody | `v=spf1 -all` ✓ (receive-only domain) |
| DKIM | `default._domainkey` RSA-2048 ✓, `resend._domainkey` RSA-1024 | n/a (never sends) |
| DMARC | **missing** | **missing** |

Actions (DNS changes, owner: the deployment operator):

- [ ] `_dmarc.example.com TXT "v=DMARC1; p=quarantine; rua=mailto:dmarc@example.com; adkim=s; aspf=s"` (start `p=none` for 2 weeks if anything legitimately sends from the apex).
- [ ] `_dmarc.inbox.example.com TXT "v=DMARC1; p=reject; adkim=s; aspf=s"` (domain never sends).
- [ ] Fix apex SPF: list the real sender (e.g. `v=spf1 ip4:203.0.113.10 -all`, or `include:` of the provider using `resend._domainkey`), or `v=spf1 -all` if the apex never sends.
- [ ] PTR for 203.0.113.10 → `mx.example.com` (currently no PTR; required by many receivers if IRIS ever sends).
- [ ] Optional: MTA-STS + TLS-RPT (`_mta-sts`, `_smtp._tls`) for inbound TLS enforcement.

## 5. TLS

- [x] HTTPS on the web origin via reverse proxy; HTTP → HTTPS redirect; HSTS. Proof: public check 2026-11-06 — `http://` answers 301, `https://` answers 200 with `strict-transport-security: max-age=31536000`; nginx vhosts listen on 443 with per-subdomain ECDSA certs (see header).
- [x] `IRIS_ENV=production` so cookies get `Secure`. Proof: live `.env` value checked 2026-11-06.
- [ ] Stalwart SMTP STARTTLS / IMAPS certificate for `mx.example.com` (ACME), auto-renew checked.

## 6. Backups

- [ ] SQLite: nightly `sqlite3 iris.db ".backup /backup/iris-$(date +%F).db"` (online-safe), keep 14 days.
- [ ] Stalwart data volume (`/var/lib/stalwart`) + `etc/`: nightly snapshot.
- [ ] Off-host copy; restore drill performed once before launch and timed.
- [ ] Temp-inbox catch-all excluded or short-retained in backups (retention promise).

## 7. Monitoring

- [ ] Uptime check on `/ready` (DB) and `/health`.
- [ ] Disk usage alert at 80 % (mail + SQLite volumes).
  Live note (2026-11-06): root disk is at 89% (6.5 GB free) — already
  above the 80% threshold — and no alert is configured. Check disk before
  every deploy; stop if < 3 GB free.
- [ ] Log shipping; alert on `cleanup` errors, `mail_server_unavailable` spikes, 5xx rate.
- [ ] Stalwart queue depth / delivery failures.

## 8. Policies

- [ ] Terms of service + acceptable-use (temp inbox abuse: no sign-up fraud at scale).
- [ ] Privacy policy stating retention: temp mail deleted ≤ 1 h after expiry (+ 1 min job cadence); sessions 30 days.
- [ ] Abuse contact (`abuse@`, `postmaster@` mailboxes exist and are monitored).
- [ ] Incident response owner and secret-rotation procedure.
