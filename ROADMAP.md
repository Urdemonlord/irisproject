# IRIS Roadmap

No dates — this is a direction, not a promise. "Built" means the feature
exists in the code on `main`; "Planned" means it is a documented gap, not a
commitment to ship it.

## Built

- **Permanent mailboxes via JMAP** — sign in with a mail-server account;
  login mints a per-session app password, sealed (ChaCha20-Poly1305) in
  SQLite and destroyed on logout. The user's real password is never stored.
- **Five fixed folders** — Inbox, Sent, Drafts, Archive, Trash, with
  move / mark-seen actions.
- **Server-side search + load-more** — message list search and pagination
  on the server, not just in the browser.
- **Compose** — plain-text compose, autosave drafts, reply/forward,
  per-hour send limits (100/hour).
- **Attachments & raw view** — MIME parsing, attachment download and
  preview, raw message view, DMARC XML report preview (zip/gzip).
- **Stars, labels, custom folders, filters** — star and label messages,
  make your own folders, and filter new mail (from / to / subject contains
  → move, label, star, mark read). Filters compile to a Sieve script on
  the mail server, so they run at delivery.
- **Threading** — messages group into conversations by the mail
  server's JMAP thread ids; the list shows one row per thread
  (subject, participants, count, latest snippet) and expanding a
  thread shows its messages oldest first.
- **Contacts** — address book with autocomplete in compose; managed on
  the Organize page (add, edit, delete; one list per user, shared across
  their mailboxes).
- **DMARC dashboard** — aggregate DMARC reports across domains: scan the
  inbox for report attachments (XML, gzip, zip), parse them, store
  per-report totals and per-source rows, and show totals per domain,
  per-domain detail, and the worst failing sources.
- **Admin dashboard** — read-only users, quotas, and delivery status
  in one place (`#/admin`), gated by the `IRIS_ADMIN_EMAILS`
  allow-list (empty = nobody is an admin).
- **Temporary inboxes** — anonymous, receive-only, 1-hour addresses;
  10 creates/hour per IP, 3 active per browser. Extendable by 1 hour,
  up to 5 times and 24 hours total life, or deleted early from the
  temp view (the address stops at once; the retention job purges its
  mail and flips the row to `deleted`).
- **Real-time notifications** — new mail arrives over a server-sent
  event stream: one shared watcher per mailbox polls the inbox
  snapshot (size + newest message) every 15 s and pushes a notice
  when mail arrives; the 30 s list poll stays as a fallback while
  the stream is down.
- **Transactional email API** — scoped API keys for companion apps
  (per-template scopes, daily limits, CLI key management). The app
  registry is configuration: set `IRIS_INTEGRATION_APPS` (JSON) to
  register your own apps; see `apps/api/src/integration_apps.rs`.
- **Separate HTML-mail origin** — HTML bodies render in an isolated
  origin behind one-time tokens (120 s TTL), with a same-origin srcdoc
  fallback; see `docs/separate-mail-origin.md`.
- **Security basics** — CSRF protection, login/send/read rate limits,
  in-process retention cleanup every 60 s plus a `cleanup-once`
  systemd timer as a safety net.
- **CI** — `cargo fmt`, clippy (`-D warnings`), `cargo test`, web
  typecheck + build, and an advisory dependency audit on every push/PR.
- **Audit trail** — who did what, when, for logins, sends, and admin
  actions (v0.4.0).
- **HTML compose + signatures** — rich-text mail and per-mailbox
  signatures (v0.4.0).
- **Multi-mailbox UI** — manage several permanent mailboxes from one
  login (v0.4.0).
- **Main-page CSP** — a strict Content-Security-Policy for the app
  shell (v0.4.0).
- **One-line installer** — `curl -fsSL .../install.sh | sudo sh`
  installs the .deb on Debian 12 / Ubuntu 24.04 (amd64) (v0.3.0+).
- **One-package installs** — Linux .deb + tarball, macOS tarballs
  (arm64/amd64, launchd), and a Windows zip (x86_64, NSSM services +
  install.ps1) bundling IRIS + Stalwart + Caddy (Windows: PR #42,
  v0.4.1).
- **Release automation** — Linux, macOS, and Windows assets are built
  and uploaded to the GitHub Release automatically on `v*` tag pushes
  (v0.4.1). The Homebrew formula in the repo tracks the release
  (sha256 filled for 0.4.1); it is still a draft file, not a
  published tap.
- **Threading/drafts hardening** — get_thread fetches bodies in
  batches of 20 with a 10 MB body budget, and drafts are guarded
  server-side so autosave/send/discard can never destroy a
  non-draft original (PR #45, on main, unreleased).

## Planned

- **Homebrew tap** — publish the formula as a tap so `brew install`
  works (formula exists in packaging/homebrew/, sha256 filled
  for 0.4.1).
- **Linux install smoke test in CI** — install the .deb on a runner
  and check the API responds (macOS and Windows already have one).
- **Production hardening** — nightly SQLite backups (14-day
  retention) + Stalwart snapshots with an off-host copy and a restore
  drill; uptime/disk/log alerting; Terms of Service + Privacy Policy;
  abuse@ and postmaster@ mailboxes.
- **Real-machine install verification** — the Linux, macOS, and
  Windows packages have only been smoke-tested in CI runners so far,
  never on a real user machine.
