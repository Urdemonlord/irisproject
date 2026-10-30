# IRIS Production Readiness Checklist

Status as of 2026-10-30, staging (`iris-stalwart` + `iris-api`). `[x]` = verified with the proof named; `[ ]` = open.

## 1. Application security

- [x] CSRF: double-submit (`iris_csrf` cookie + `X-CSRF-Token` header) on every POST/PUT/PATCH/DELETE, constant-time compare. Proof: `scripts/security_smoke.py` (missing/mismatched token → 403).
- [x] Session rotation on login: new session token + new CSRF token; previous session from the same browser revoked. Proof: smoke test (old cookie → 401).
- [x] Mail credential persistence: login mints a per-session Stalwart app password (`x:AppPassword`, expires with the session), stored ChaCha20-Poly1305-sealed (key derived from `SESSION_SECRET`). User password never stored; secret destroyed at logout/expiry. Proof: smoke test DB checks; session survives API restart.
- [x] Production refuses to boot with `SESSION_SECRET` < 32 chars or the dev default.
- [x] HTML email isolation: `<iframe sandbox="">` (no scripts, no same-origin), CSP `img-src data: cid:` blocks trackers and remote CSS. Proof: Playwright hostile-HTML probe (script/onerror blocked, 0 remote fetches, parent title untouched).
- [x] Oversize mail rejected at SMTP: Stalwart `maxMessageSize` 26214400 (25 MiB), advertised in EHLO `SIZE`. Proof: smoke test sends 26 MiB with and without `SIZE=` → 5xx.
- [ ] Rotating `SESSION_SECRET` invalidates all sealed credentials: users must log in again. Document in the ops runbook.
- [ ] Separate origin for HTML mail rendering (TRD §6 "where feasible"); currently same-origin `srcdoc` + empty sandbox.

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

| Record | meowlabs.id | inbox.meowlabs.id |
|---|---|---|
| MX | none (apex does not receive) | `10 mx-iris.meowlabs.id` → 129.226.193.34 |
| SPF | `v=spf1 mx -all` ⚠ apex has no MX, so this authorizes nobody | `v=spf1 -all` ✓ (receive-only domain) |
| DKIM | `default._domainkey` RSA-2048 ✓, `resend._domainkey` RSA-1024 | n/a (never sends) |
| DMARC | **missing** | **missing** |

Actions (DNS changes, owner: Arya):

- [ ] `_dmarc.meowlabs.id TXT "v=DMARC1; p=quarantine; rua=mailto:dmarc@meowlabs.id; adkim=s; aspf=s"` (start `p=none` for 2 weeks if anything legitimately sends from the apex).
- [ ] `_dmarc.inbox.meowlabs.id TXT "v=DMARC1; p=reject; adkim=s; aspf=s"` (domain never sends).
- [ ] Fix apex SPF: list the real sender (e.g. `v=spf1 ip4:129.226.193.34 -all`, or `include:` of the provider using `resend._domainkey`), or `v=spf1 -all` if the apex never sends.
- [ ] PTR for 129.226.193.34 → `mx-iris.meowlabs.id` (currently no PTR; required by many receivers if IRIS ever sends).
- [ ] Optional: MTA-STS + TLS-RPT (`_mta-sts`, `_smtp._tls`) for inbound TLS enforcement.

## 5. TLS

- [ ] HTTPS on the web origin via reverse proxy; HTTP → HTTPS redirect; HSTS.
- [ ] `IRIS_ENV=production` so cookies get `Secure`.
- [ ] Stalwart SMTP STARTTLS / IMAPS certificate for `mx-iris.meowlabs.id` (ACME), auto-renew checked.

## 6. Backups

- [ ] SQLite: nightly `sqlite3 iris.db ".backup /backup/iris-$(date +%F).db"` (online-safe), keep 14 days.
- [ ] Stalwart data volume (`/var/lib/stalwart`) + `etc/`: nightly snapshot.
- [ ] Off-host copy; restore drill performed once before launch and timed.
- [ ] Temp-inbox catch-all excluded or short-retained in backups (retention promise).

## 7. Monitoring

- [ ] Uptime check on `/ready` (DB) and `/health`.
- [ ] Disk usage alert at 80 % (mail + SQLite volumes).
- [ ] Log shipping; alert on `cleanup` errors, `mail_server_unavailable` spikes, 5xx rate.
- [ ] Stalwart queue depth / delivery failures.

## 8. Policies

- [ ] Terms of service + acceptable-use (temp inbox abuse: no sign-up fraud at scale).
- [ ] Privacy policy stating retention: temp mail deleted ≤ 1 h after expiry (+ 1 min job cadence); sessions 30 days.
- [ ] Abuse contact (`abuse@`, `postmaster@` mailboxes exist and are monitored).
- [ ] Incident response owner and secret-rotation procedure.
