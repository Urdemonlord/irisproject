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
Not yet: message purge after expiry, extend/delete endpoints, Bcc delivery (To: header only).

## License

TBD
