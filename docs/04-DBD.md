# IRIS Design Brief Documentation (DBD)

**Product:** IRIS by Meow Labs\
**Primary URL:** `mail.meowlabs.id`\
**Status:** Draft v1.0

## 1. Design objective

Create a calm, fast, trustworthy email interface that feels native to
the Meow Labs ecosystem. The UI should support two distinct jobs:
dependable permanent email and deliberately short-lived temporary email.

## 2. Brand direction

-   Product name: **IRIS**
-   Parent brand: **Meow Labs**
-   Primary tagline candidate: **Every message finds its way.**
-   Personality: precise, quiet, technical, human, trustworthy.
-   Avoid: excessive gradients, noisy dashboards, excessive rounded
    cards, playful temporary-mail aesthetics that undermine trust.

## 3. Visual direction

Use the Meow Labs brand system as the source of truth. Proposed starting
palette: - Paper / background: `#F7F7F2` - Surface: `#FFFFFF` - Primary
text: `#171717` - Secondary text: `#707070` - Border: `#D9D9D2` - Brand
accent: use the approved Meow Labs red token once confirmed. - Success:
muted green. - Warning: muted amber. - Critical: muted red.

These are provisional product tokens and should be reconciled with the
official Meow Labs design system before production.

Typography: - Use a readable modern sans-serif for interface text. - Use
strong typographic hierarchy instead of decorative effects. - Keep
message body typography comfortable for long reading.

## 4. Layout principles

### Desktop

-   Narrow left navigation rail or sidebar.
-   Message list as the primary working area.
-   Message detail pane when width allows.
-   Search and primary actions near the top.
-   Avoid wasting vertical space on oversized controls.

### Mobile

-   Inbox list is the default view.
-   Message detail becomes a dedicated screen.
-   Navigation is compact and accessible.
-   Compose action remains easy to reach.
-   Temporary address has a prominent copy control and visible expiry
    time.

## 5. Information architecture

Permanent mail: - Inbox - Sent - Drafts - Archive - Trash - Search -
Settings

Temporary inbox: - Create inbox - Current address - Copy address -
Message list - Expiry countdown - Refresh status - Delete inbox / create
another

Admin: - Overview - Mailboxes - Temporary inboxes - Delivery and queue
health - Storage and quotas - Audit events - System settings

## 6. Core components

-   Message row with sender, subject, preview, date, unread indicator,
    and attachment indicator.
-   Message detail header with sender, recipient, timestamp, and
    actions.
-   Safe email content frame.
-   Attachment list with filename, size, and download action.
-   Search field with loading and empty states.
-   Toast/inline feedback for send, copy, delete, and error states.
-   Expiry badge and countdown for temporary inboxes.
-   Confirmation dialog for destructive actions.
-   Accessible loading skeletons.

## 7. Interaction principles

-   Optimistic UI only where failures can be safely reconciled.
-   Avoid silent message deletion or send failures.
-   Show explicit delivery/send state.
-   Copy-address action must provide confirmation.
-   Temporary inbox expiry must never be hidden.
-   Remote content in emails is blocked by default or requires explicit
    permission.
-   HTML email must not be able to access the application's cookies,
    local storage, or API session.

## 8. Accessibility

-   Semantic landmarks and heading structure.
-   Full keyboard support.
-   Visible focus states.
-   Sufficient color contrast.
-   Status messages announced to assistive technology.
-   Do not communicate read/unread or errors by color alone.
-   Respect reduced-motion preferences.

## 9. Design deliverables

Before implementation is considered complete, create: 1. Wordmark and
approved logo usage. 2. Color and typography tokens. 3. Desktop inbox
wireframe. 4. Mobile inbox wireframe. 5. Message detail states. 6.
Compose/reply flow. 7. Temporary inbox creation and expiry states. 8.
Admin overview. 9. Loading, empty, error, and permission-denied states.
10. A small component inventory.

## 10. Acceptance criteria

-   Permanent and temporary workflows are visibly distinct.
-   Primary actions are understandable without documentation.
-   Email content is visually separated from trusted application chrome.
-   Mobile use is practical at common phone widths.
-   Focus and contrast requirements are met.
-   No unapproved logo, mascot, or color variant is introduced.
