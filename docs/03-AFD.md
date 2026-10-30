# IRIS Application Flow Documentation (AFD)

**Product:** IRIS by Meow Labs\
**Primary URL:** `mail.meowlabs.id`\
**Status:** Draft v1.0

## 1. Application map

``` text
IRIS
├── Authentication
│   ├── Sign in
│   ├── Session validation
│   └── Sign out
├── Permanent Mail
│   ├── Inbox
│   ├── Sent
│   ├── Drafts
│   ├── Archive
│   ├── Search
│   ├── Message detail
│   ├── Compose / Reply / Forward
│   └── Settings
├── Temporary Inbox
│   ├── Create inbox
│   ├── Copy address
│   ├── Inbox message list
│   ├── Message detail
│   ├── Extend expiry (if allowed)
│   └── Expired state
└── Admin
    ├── Service health
    ├── Mailbox management
    ├── Temporary inbox overview
    ├── Quotas and abuse controls
    ├── Audit events
    └── Storage / delivery diagnostics
```

## 2. First visit

1.  User opens `mail.meowlabs.id`.
2.  Frontend loads public shell and asks the API for session state.
3.  If signed out, show sign-in and a separate entry point for temporary
    inbox.
4.  If signed in, show the permanent mailbox dashboard.
5.  Temporary inbox access must not automatically grant access to
    permanent mail.

## 3. Permanent mailbox sign-in flow

``` text
Open app
  -> Enter credentials
  -> Backend validates identity
  -> Rotate/create session
  -> Redirect to Inbox
  -> Fetch paginated message list
```

Failure paths: - Invalid credentials: generic error message, rate limit,
no account enumeration. - Mail server unavailable: show a retryable
service error, not an empty inbox. - Expired/revoked session: clear
local session state and return to sign-in.

## 4. Read email flow

1.  User selects a message from the list.
2.  Frontend requests the message by identifier.
3.  Backend verifies mailbox ownership before querying the mail server.
4.  Backend returns safe headers, plain-text content, and a sanitized or
    isolated HTML representation.
5.  Remote images and external resources are blocked by default or
    require explicit user action.
6.  Attachments are listed separately and downloaded through an
    authorized endpoint.
7.  Mark-as-read operation occurs only after access authorization.

## 5. Compose and send flow

1.  User opens Compose.
2.  Frontend validates required fields for usability, but backend
    performs authoritative validation.
3.  Backend verifies sender identity and recipient policy.
4.  Backend submits the message through the selected mail-server API.
5.  UI displays sending, sent, or failure state.
6.  Failure details must not expose credentials or internal
    infrastructure.
7.  Temporary inboxes cannot send mail in MVP.

## 6. Temporary inbox creation flow

``` text
Open Temporary Inbox
  -> Apply IP/session rate limit
  -> Generate cryptographically random address
  -> Create temporary-inbox record and expiry
  -> Return address + opaque access token
  -> Show inbox screen
  -> Poll/subscribe for messages
```

Requirements: - The local part must have sufficient entropy and be
collision-checked. - The access token must be unguessable and stored
hashed where feasible. - The address itself must not be treated as the
only authorization secret if inbox contents are sensitive. - Apply
limits on active inboxes per IP/session and total system capacity. - The
UI must clearly state the expiry time and that temporary email is not
suitable for sensitive or long-term correspondence.

## 7. Incoming temporary email flow

1.  External sender resolves the MX record for `inbox.meowlabs.id`.
2.  Mail server receives and validates the message.
3.  The server routes it to the configured temporary-address handling
    mechanism.
4.  The application maps the recipient to an active temporary inbox.
5.  Message metadata becomes available in that inbox.
6.  UI refreshes through polling or an event mechanism.
7.  If the address is expired or unknown, apply the configured
    rejection, discard, or quarantine policy. Do not route it into a
    permanent mailbox.

The exact routing integration must be proven with the selected Stalwart
version before implementation.

## 8. Temporary inbox expiry flow

1.  A scheduled job finds inboxes whose expiry time has passed.
2.  Application immediately denies further reads and message access.
3.  The job requests deletion of associated messages and mailbox
    resources through supported server APIs.
4.  The record transitions to `deletion_pending`, then `deleted`.
5.  Failures are retried idempotently and surfaced to administrators.
6.  A cleanup audit event is recorded without message contents.

## 9. Admin flow

-   Admin authenticates with stronger controls than ordinary users.
-   Admin can disable an address, change quotas, inspect delivery
    health, and view operational metadata.
-   Access to message contents is not a routine admin function and must
    be restricted and audited if ever supported.
-   Destructive operations require confirmation and should be reversible
    where practical.

## 10. Error and empty states

-   Empty inbox: explain there are no messages yet.
-   Loading: show skeleton or progress state.
-   Mail server unavailable: show service unavailable and retry option.
-   Expired temporary inbox: show expiry and offer creation of a new
    inbox.
-   Attachment too large or blocked: explain the restriction.
-   Rate limited: display when retry is possible without exposing
    internal thresholds.
-   Unauthorized message: return a generic not-found/forbidden response
    that does not leak another mailbox's existence.

## 11. Navigation principles

-   Keep permanent mail and temporary inbox visually distinct.
-   Do not show temporary messages inside permanent inboxes.
-   Make expiry visible in the temporary inbox header.
-   Provide keyboard navigation and clear focus states.
-   Use responsive layouts for mobile and desktop.
