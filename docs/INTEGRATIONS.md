# Integrating with IRIS transactional email

IRIS offers a small API that lets your own apps send auth mail through
IRIS: email verification, password reset, and sign-in links. That is
all it sends. It is not a bulk mailer, and it does not send mail on
behalf of anyone except a registered companion app.

Delivery goes through the configured outbound relays, in order:
Resend first, then Brevo when it is configured (see section 7).
Every message is sent from one fixed sender address,
`IRIS_INTEGRATION_EMAIL_FROM`. Recipients are never stored in plain
text, only as hashes, and the request body a caller sends is at most
8 KiB.

## 1. Register your app

An app must be registered before it can send anything. Register it
from the admin dashboard: open the Admin page, scroll to the Delivery
section, and use the "Integration apps" form. Fill in the app id
(lowercase letters, digits and hyphens), the exact HTTPS host its
links must point at, and at least one template: an id, a subject, an
intro, and a closing. The app can send and take keys at once, no
restart needed. Edit an app from the same form (the id itself never
changes); delete it when it is no longer needed. Deleting is refused
while any active key exists for the app, so revoke the keys first.

Operators can also seed the registry from configuration instead:
set `IRIS_INTEGRATION_APPS` to a JSON array of
`{id, host, templates: [{id, subject, intro, closing}]}` before the
first start. The tables are seeded from it only when they are empty;
after that, the database is the source of truth and the variable is
ignored until the tables are cleared by hand. When the variable is
unset, IRIS seeds a built-in example registry so a fresh checkout
works out of the box. The example apps are `alpha`
(`alpha.example.com`, templates `auth.verify` and `auth.recovery`),
`beta` (`beta.example.com`, template `auth.login_link`), and `gamma`
(`gamma.example.com`, templates `auth.recovery` and
`auth.verification`). Nothing in the example registry belongs to any
real deployment.

Each app has one exact HTTPS host its links must point at, and the
templates it owns. An app may own at most 4 templates: a key holds
at most 4 scopes, so a fifth template could never be sent with.

A template renders as plain text:

```
{intro}

{action_url}

{closing}
```

The subject comes from the template. There is no HTML body and no
custom subject: the template fixes the wording, the caller only
supplies the link.

## 2. Create an API key

### From the admin dashboard

Open the Admin page, scroll to the Delivery section, and use the
"Create integration key" form:

1. Pick the app from the dropdown. The form lists the scopes the app
   may hold, one per template it owns plus `email:status`, all
   checked by default.
2. Set the expiry in days (1 to 365) and the daily send limit
   (1 to 10000).
3. Press "Create integration key".

The secret is shown once, in a copyable field. Copy it right away:
IRIS stores only its hash and never shows it again. If you lose it,
revoke the key and create a new one. An app may hold several active
keys at once, for example one per environment.

### From the command line

On the host, run:

```bash
iris-api integration-key create <app> <expires-days> <daily-limit> <comma-separated-scopes>
```

For example, a key for `alpha` that can send verification mail and
check send status, valid 365 days, 100 sends per day:

```bash
iris-api integration-key create alpha 365 100 email:template:auth.verify,email:status
```

The command prints `key_id` and the secret token once. The same
limits apply as in the dashboard: 1 to 365 days, 1 to 10000 sends
per day, at most 4 scopes, and every scope must be allowed for the
app.

### Revoking a key

Revoke from the Admin page (Delivery section, Revoke next to the key)
or from the command line:

```bash
iris-api integration-key revoke <key-id>
```

A revoked key stops authenticating at once.

## 3. Send an email

### Authentication

Send the key as a Bearer token:

```
Authorization: Bearer iris1_<key_id>_<secret>
```

The key must hold the scope for the template being sent,
`email:template:<template-id>`. A key without that scope is refused
with 403.

### Endpoint

```
POST /api/v1/integrations/email
Content-Type: application/json
Authorization: Bearer <key>
Idempotency-Key: <unique key per send>
```

Request body (unknown fields are rejected):

```json
{
  "template_id": "auth.verify",
  "recipient": "user@example.com",
  "action_url": "https://alpha.example.com/verify?token=abc"
}
```

| Field | Rule |
|---|---|
| `template_id` | Must be a template the app owns. |
| `recipient` | Must be a valid email address. |
| `action_url` | Must be `https` on the app's registered host, port 443, with no userinfo. Any other host, scheme, port, or embedded credentials are rejected. |

### Idempotency

`Idempotency-Key` is required: 1 to 128 printable ASCII characters.
It makes retries safe. Sending the same key with the same body
returns the original result. Sending the same key with a different
body answers 409 `idempotency_conflict`. Use a fresh random key per
send, for example a UUID.

### Responses

The body is always `{"request_id": "<id>", "status": "<status>"}`.

| Code | Meaning |
|---|---|
| 202 | `accepted`. The relay took the message. `request_id` identifies the send. |
| 422 | `failed`. The relay refused the message. See troubleshooting below. |
| 409 | `idempotency_conflict`. The key was reused with a different body. |
| 429 | `daily_limit_reached`. The key hit its daily send limit. |
| 503 | `delivery_disabled`. Integration email is off, no relay is configured, or the sender address is invalid. |
| 503 | `unknown`. The relay answered in a way that proves nothing about delivery. Check the status endpoint. |
| 400 | `invalid_request` (bad recipient, action URL, or template) or `invalid_idempotency_key`. |
| 401 | `unauthorized`. Missing or wrong key, or an expired or revoked key. |
| 403 | `forbidden`. The key does not hold the scope for this template. |

A `failed` or `unknown` answer still returns a `request_id`, so the
send can be traced in the dashboard.

### Full example

```bash
curl -X POST https://mail.example.com/api/v1/integrations/email \
  -H "Authorization: Bearer iris1_0123456789abcdef0123456789abcdef_0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef" \
  -H "Idempotency-Key: 550e8400-e29b-41d4-a716-446655440000" \
  -H "Content-Type: application/json" \
  -d '{"template_id":"auth.verify","recipient":"user@example.com","action_url":"https://alpha.example.com/verify?token=abc"}'
```

This example uses the built-in example registry (`alpha` on
`alpha.example.com`). Replace the host, key, recipient, and link with
your own app's values.

## 4. Check a send

```
GET /api/v1/integrations/email-requests/<request_id>
Authorization: Bearer <key>
```

The key must hold the `email:status` scope. The answer is
`{"request_id": "<id>", "status": "<status>"}` with status `sending`,
`accepted`, `failed`, or `unknown`. An unknown id, or an id belonging
to another key, answers 404 `not_found`.

## 5. Daily limits

Each key has its own daily limit, set when the key is created
(1 to 10000 sends per day). The count is per UTC day and covers
accepted sends plus sends still in flight for that key. When the
limit is reached, further sends answer 429 until the next UTC day.
The limit cannot be raised on an existing key: revoke it and create
a new one.

## 6. Troubleshooting

### A send answers 422 `failed`

The relay refused the message. The reason is shown in the Admin page,
Delivery section, recent requests table, Error column, and in the
server log. Common causes are a recipient the relay rejects, for
example a malformed or undeliverable address, or a sender address
the relay does not accept. Fix the request and send again with a
fresh idempotency key.

### A send answers 503 `delivery_disabled`

Integration email is not enabled on this IRIS host. The operator must
set `IRIS_INTEGRATION_EMAIL_ENABLED=true`, a valid
`IRIS_INTEGRATION_EMAIL_FROM`, and at least one relay key
(`RESEND_API_KEY` or `BREVO_API_KEY`), then restart the API.

### A send answers 409 `idempotency_conflict`

The `Idempotency-Key` was already used with a different body. Generate
a new key per send and retry.

### A send answers 429 `daily_limit_reached`

The key hit its daily limit. Wait for the next UTC day, or create a
new key with a higher limit and revoke the old one.

### A send answers 401 or 403

401 means the key is missing, wrong, expired, or revoked. 403 means
the key is valid but does not hold `email:template:<template-id>`
for the template in the request. Create a key with the right scopes.

### The dashboard shows a `failed` request with no error text

Requests that failed before this release carry no reason, because no
reason was stored then. New failures store the relay reason. If a
recent failure still shows no reason, it failed before the relay
was reached or the relay returned no message.

## 7. Relay failover

IRIS sends through a chain of outbound relays, in order: Resend
first, then Brevo. Configure either or both with `RESEND_API_KEY`
and `BREVO_API_KEY`; the chain exists when at least one key is set,
and a Brevo-only setup sends through Brevo alone. In production the
endpoints are pinned to the official ones (`api.resend.com` and
`api.brevo.com`); outside production, `IRIS_RESEND_API_URL` and
`IRIS_BREVO_API_URL` accept a local test endpoint, the same way.

Failover happens only when a relay definitively refuses with an HTTP
error (bad credentials, rate limit, server error). It never happens
when the outcome is unknown: a transport error with no HTTP response
means the message may already be accepted, so retrying elsewhere
could send it twice. It also never happens on a permanent refusal
(a bad recipient fails on every provider), which answers 422
`failed` at once. A 2xx answer with no provider id is recorded as
`unknown`, not retried.

The Delivery section of the admin dashboard shows which relay
delivered each request (Provider column) and which relays are
configured (Quotas section). The Brevo account must verify the
sender address or domain, the same as Resend: an unverified sender
is refused with 400. Brevo's free tier allows 300 sends per day.
