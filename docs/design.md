# IRIS Design System

**Product:** IRIS by Meow Labs  
**Primary application:** `mail.meowlabs.id`  
**Version:** 1.0  
**Direction:** Minimal Editorial + Swiss Design + Modern Developer Tool

## 1. Design intent

IRIS combines permanent mailboxes and temporary inboxes in one calm, precise, trustworthy email workspace. It should feel like a focused productivity tool, not a generic email template or a clone of Gmail.

Principles:
1. Content first: sender, subject, timestamp, and message body take priority.
2. Quiet interface: neutral surfaces, thin borders, restrained accent color.
3. Precise layout: consistent grid, spacing, and alignment.
4. Clear separation: permanent mail and temporary inbox workflows must remain distinct.
5. Safe by design: untrusted email content must never behave like trusted application UI.
6. Lightweight: semantic HTML, Vanilla TypeScript, and CSS, without an unnecessary UI framework.
7. Responsive: three panes on wide screens, one primary pane at a time on mobile.

## 2. Brand

- Product: **IRIS**
- Parent brand: **Meow Labs**
- Tagline: **Every message finds its way.**
- Dashboard: `https://mail.meowlabs.id`
- Permanent email domain: `meowlabs.id`
- Proposed temporary email domain: `inbox.meowlabs.id`

Use the official Meow Labs logo and brand red when available. The colors below are provisional and must be reconciled with the official brand palette. Do not invent a new mascot or modify the parent logo without approval.

## 3. Visual direction

**Use:** Minimal Editorial, Swiss grid, modern SaaS, typography-led layouts, calm high information density.

**Avoid:** excessive gradients, glassmorphism, huge rounded cards, pill controls everywhere, heavy shadows, emoji as interface icons, bouncy animations, decorative illustrations in the inbox, or status communicated by color alone.

## 4. Color tokens

```css
:root {
  color-scheme: light;
  --iris-bg: #F7F7F2;
  --iris-surface: #FFFFFF;
  --iris-surface-subtle: #F1F1EB;
  --iris-text: #171717;
  --iris-text-secondary: #5E5E58; /* AA on all surfaces */
  --iris-text-tertiary: #6B6B64;
  --iris-border: #E2E2DA;
  --iris-border-strong: #8C8C84; /* inputs: >=3:1 */

  /* Replace with official Meow Labs red after confirmation. */
  --iris-accent: #B83232;
  --iris-accent-hover: #982828;
  --iris-accent-subtle: #F8EAEA;

  --iris-success: #2F6B3B;
  --iris-success-subtle: #EAF3EB;
  --iris-warning: #8A4A14;
  --iris-warning-subtle: #FAF0E4;
  --iris-danger: #9E2F2F;
  --iris-danger-subtle: #F8EAEA;
  --iris-focus: #245CC5;
}
```

Usage:
- Background: `--iris-bg`
- Main working surfaces: `--iris-surface`
- Primary text: `--iris-text`
- Metadata: `--iris-text-secondary`
- Dividers: `--iris-border`
- Primary actions and selected navigation: `--iris-accent`
- Semantic states: success, warning, and danger tokens
- Keyboard focus: `--iris-focus`

Keep the UI predominantly neutral. Check text and control contrast before release.

## 5. Typography

- UI font: Inter, with system sans-serif fallbacks.
- Optional technical metadata: IBM Plex Mono.
- Avoid relying on remote font loading for basic functionality.

```css
:root {
  --iris-font-ui: Inter, ui-sans-serif, system-ui, -apple-system,
    BlinkMacSystemFont, "Segoe UI", sans-serif;
  --iris-font-mono: "IBM Plex Mono", ui-monospace, SFMono-Regular,
    Menlo, Consolas, monospace;
  --iris-text-xs: 0.75rem;
  --iris-text-sm: 0.8125rem;
  --iris-text-base: 0.9375rem;
  --iris-text-lg: 1.0625rem;
  --iris-heading-sm: 1.25rem;
  --iris-heading-md: 1.5rem;
  --iris-heading-lg: 1.875rem;
}
```

Guidelines:
- Body UI: 14–15px.
- Message body: 15–16px with comfortable line height.
- Metadata: 12–13px.
- Page headings: 22–30px.
- Use semibold for unread sender/subject and key actions.
- Do not bold every label.
- Treat message-body typography separately from application chrome.

## 6. Spacing and shapes

Use a 4px base scale.

```css
:root {
  --iris-space-1: 0.25rem;
  --iris-space-2: 0.5rem;
  --iris-space-3: 0.75rem;
  --iris-space-4: 1rem;
  --iris-space-5: 1.25rem;
  --iris-space-6: 1.5rem;
  --iris-space-8: 2rem;
  --iris-space-10: 2.5rem;
  --iris-space-12: 3rem;

  --iris-radius-sm: 4px;
  --iris-radius-md: 7px;
  --iris-radius-lg: 10px;

  --iris-sidebar-width: 220px;
  --iris-list-width: 360px;
  --iris-topbar-height: 60px;
  --iris-control-height: 36px;
}
```

- Prefer 4–8px corner radius for controls and small surfaces.
- Avoid oversized rounded cards.
- Standard desktop controls should be around 36px high.
- Important mobile touch targets should generally be at least 44px high.

## 7. Layout and navigation

### Desktop

Use a three-pane layout:
1. Sidebar for navigation and product modes.
2. Message list for searchable email summaries.
3. Reading pane for the selected message.

Suggested starting widths at 1440px:
- Sidebar: 220px
- Message list: 360px
- Reading pane: remaining space
- Top bar: 60px

Use CSS Grid. Adapt gracefully at narrower widths; collapse the sidebar or switch to a separate message detail view as necessary.

### Navigation

Permanent mail:
- Inbox
- Sent
- Drafts
- Archive
- Trash

Other actions:
- Global search in the top bar.
- Compose as a primary action.
- Settings under the account menu.

Temporary inbox:
- Create address
- Current address
- Message list
- Expiry indicator

Admin navigation must not be visible to ordinary users.

### Top bar

Show the IRIS wordmark, global search when useful, and account controls. Keep it compact and avoid marketing copy inside the working application.

## 8. Permanent mailbox screens

### Inbox
- Show sender, subject, short preview, date/time, and attachment indicator.
- Unread messages use stronger text weight and a small indicator.
- Selected message uses a subtle neutral background.
- Paginate or incrementally load messages. Do not load the entire mailbox at once.
- Empty state: “Your inbox is clear” plus a short explanation.
- Search, refresh, and compose actions should be easy to find.

### Message detail
- Subject first, then sender/recipient metadata and timestamp.
- Group Reply, Forward, and More actions consistently.
- Display attachments separately with filename, type, size, and download action.
- Offer plain-text view when available.
- Never execute scripts from email or allow message content to access application storage/session.
- Prefer rendering HTML in a sandboxed frame on a separate origin with restrictive CSP. Block remote content by default or require explicit user action.

### Compose
- Use a dedicated pane or sufficiently large modal.
- Fields: From, To, Subject, Body; Cc/Bcc only when implemented.
- Send is the clear primary action.
- Show saving, sending, sent, and failure states explicitly.
- Prevent accidental duplicate sends after retries.
- Warn before discarding a non-empty draft.

### Search
- Support only fields the backend can actually search.
- Provide loading, no-results, and error states.
- Do not imply search coverage that is not implemented.

## 9. Temporary inbox screens

Use the same design system, with a simpler information hierarchy.

### Create address
- Explain that the address is temporary.
- Primary action: “Create temporary address”.
- Display the expiry duration before creation.
- Do not request unnecessary personal data.
- Enforce rate limits on the server, not only in the UI.

### Active inbox
- Show the address in a prominent copyable field.
- Provide a clear Copy action with confirmation.
- Display exact expiry time and a countdown.
- Show refresh or live-update status.
- List sender, subject, received time, and attachment indicator.
- Explain that temporary inboxes are not suitable for sensitive or long-term correspondence.
- Disable outbound email for temporary inboxes in the MVP.

### Expired state
- State clearly that the address has expired.
- Deny message access immediately after expiry.
- Offer “Create new address”.
- Never display stale message content after expiry.

## 10. Responsive behavior

### Desktop: 1024px and above
- Three-pane layout where width allows.
- Sidebar may collapse.
- Keep message list and reader side by side on wide screens.

### Tablet: 768px–1023px
- Sidebar can collapse to an icon rail or drawer.
- List and reader may switch between views.
- Avoid horizontal scrolling.

### Mobile: below 768px
- Show one primary pane at a time: navigation, list, or reader.
- Compact top bar.
- Compose uses a full-screen view.
- Address and copy action must fit narrow screens; allow the address to wrap safely.
- Important touch targets should be at least 44px high.
- Do not use fixed-width desktop panels.

## 11. Interaction and motion

- Use restrained transitions of roughly 120–180ms.
- Avoid spring/bounce effects.
- Respect `prefers-reduced-motion`.
- Copy action shows concise confirmation.
- Destructive actions require confirmation when irreversible.
- Do not silently swallow network errors.
- Preserve layout during loading to reduce visual jumps.
- Keep keyboard focus visible.

## 12. Icons and assets

- Use one consistent lightweight line-icon set or inline SVGs.
- Keep stroke weight consistent.
- Icon-only buttons need accessible names.
- Do not use emoji as interface icons.
- Use official Meow Labs brand assets.
- Do not add a mascot to core mail workflows unless explicitly approved.

## 13. Accessibility

- Semantic HTML landmarks and headings.
- Keyboard-operable navigation, message list, menus, and dialogs.
- Visible focus ring.
- WCAG AA contrast where applicable.
- Do not communicate status through color alone.
- Use `aria-live` for important asynchronous status messages.
- Manage dialog focus correctly.
- Respect reduced-motion preferences.

## 14. Component inventory

Implement with Vanilla TypeScript and CSS:
- `AppShell`
- `TopBar`
- `Sidebar`
- `NavItem`
- `SearchInput`
- `PrimaryButton`
- `IconButton`
- `MessageList`
- `MessageRow`
- `MessageReader`
- `MessageHeader`
- `AttachmentList`
- `ComposeView`
- `EmptyState`
- `LoadingState`
- `ErrorState`
- `Toast`
- `ConfirmDialog`
- `TemporaryAddressCard`
- `ExpiryBadge`
- `AdminStatusCard`

Keep components small and semantic. Do not add a heavy UI framework.

## 15. Frontend implementation rules

- Use Vanilla TypeScript + Vite.
- Do not introduce React, Next.js, or another UI framework.
- Keep application state explicit; avoid a global state framework until complexity requires it.
- Use CSS custom properties for design tokens.
- Prefer CSS Grid and Flexbox.
- Keep API access in a small typed client module.
- Separate routing, view rendering, and data access.
- Never place mail-server administrator credentials in frontend code.
- Never insert raw email HTML directly into the main application DOM.
- Implement loading, empty, error, and unauthorized states for major screens.

## 16. CSS foundation

```css
*,
*::before,
*::after {
  box-sizing: border-box;
}

html {
  color-scheme: light;
  background: var(--iris-bg);
  color: var(--iris-text);
  font-family: var(--iris-font-ui);
}

body {
  margin: 0;
  min-width: 320px;
  min-height: 100vh;
}

button,
input,
textarea,
select {
  font: inherit;
}

button {
  cursor: pointer;
}

:focus-visible {
  outline: 2px solid var(--iris-focus);
  outline-offset: 2px;
}

.app-shell {
  min-height: 100vh;
  display: grid;
  grid-template-rows: var(--iris-topbar-height) minmax(0, 1fr);
}

.mail-layout {
  min-height: 0;
  display: grid;
  grid-template-columns:
    var(--iris-sidebar-width)
    var(--iris-list-width)
    minmax(0, 1fr);
}

@media (max-width: 1023px) {
  .mail-layout {
    grid-template-columns: 64px minmax(260px, 40%) minmax(0, 1fr);
  }
}

@media (max-width: 767px) {
  .app-shell {
    grid-template-rows: 56px minmax(0, 1fr);
  }

  .mail-layout {
    display: block;
  }

  .mail-sidebar,
  .message-list,
  .message-reader {
    min-width: 0;
    width: 100%;
  }

  /* Implement view-state behavior in the app router. */
  .mail-layout[data-view="list"] .message-reader,
  .mail-layout[data-view="reader"] .mail-sidebar,
  .mail-layout[data-view="reader"] .message-list {
    display: none;
  }
}
```

Treat the CSS as a starting point. Verify the responsive view-state logic during implementation.

## 17. Design acceptance checklist

- [ ] Official Meow Labs logo and brand red confirmed.
- [ ] Desktop three-pane inbox implemented.
- [ ] Mobile navigation and message detail work without horizontal scrolling.
- [ ] Permanent mail and temporary inbox are clearly separated.
- [ ] Loading, empty, error, and expired states implemented.
- [ ] Keyboard focus is visible throughout.
- [ ] Contrast checked.
- [ ] Email HTML isolated from the app origin and scripts cannot execute in app context.
- [ ] Remote content is disabled by default or explicitly controlled.
- [ ] No emoji used as interface icons.
- [ ] No unnecessary framework or animation dependency.
- [ ] Visual QA completed at desktop, tablet, and mobile widths.
