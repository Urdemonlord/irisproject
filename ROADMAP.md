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
- **Temporary inboxes** — anonymous, receive-only, 1-hour addresses;
  10 creates/hour per IP, 3 active per browser.
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

## Planned

- **Admin dashboard** — users, quotas, and delivery status in one place.
- **Audit trail** — who did what, when, for logins, sends, and key use.
- **Threading** — group messages into conversations.
- **Labels, stars, custom folders, filters** — beyond the five fixed folders.
- **Contacts** — address book with autocomplete in compose.
- **Real-time notifications** — new mail without polling.
- **DMARC dashboard** — aggregate DMARC reports across domains.
- **Temp inbox extend/delete** — extend an address past 1 hour, or delete
  it early.
- **Attachments + HTML compose + signatures** — compose with attachments,
  rich-text mail, and per-mailbox signatures.
- **Multi-mailbox UI** — manage several permanent mailboxes from one login.
- **Main-page CSP** — a strict Content-Security-Policy for the app shell
  (blocked on the inline-script refactor in `index.html`).
