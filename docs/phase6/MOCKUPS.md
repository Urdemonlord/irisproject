# IRIS Phase 6: screen mockups (text spec)

Status: visual lock-in. No code yet. All values reference `iris-tokens.css` in this folder.
Scope: Login, Desktop Inbox (3-pane), Temporary Inbox. Mobile, compose, and empty states are deferred.

## System in one paragraph

Cream paper (`--iris-cream` #F7F7F2, light grain) is the ground. Ink (#16130F) does the structure:
1px rules between columns, 2px bars under section heads. Vermilion (#B83232) is used rarely and with
intent: the primary action, the selected edge, the unread iris mark, and the one brand field on login.
Shadows are flat offset plates only. No gradients, glass, or soft blur. Radius is 0 for panes and rows,
2px for controls. Type does the hierarchy, and the eye/iris is the recurring mark.

## Type roles

| Role | Face | Use |
|---|---|---|
| Display | Archivo, 800–900, width 112–125%, tracking −0.025em | Wordmark, pane titles, login headline, subject line. One display moment per pane. |
| UI | IBM Plex Sans 400/500/600 | Everything interactive, message body |
| Data | IBM Plex Mono 500, 11px caps, +0.08em | Labels, timestamps, addresses, counts, countdown |

Scale (px): 11 label / 12 / 13 meta / 15 UI / 17 reading (lh 1.6) / 20 / 24 / 32 pane title / 48 auth headline / 72 / 120 brand field.

---

## 1. Login (`/login`), 1440×900

### Layout
Asymmetric 2-column split, 7/5 on a 12-col grid with no gutter. A full-height 1px ink rule separates the columns.

```
┌──────────────────────────────────────┬─────────────────────────┐
│ VERMILION FIELD (cols 1–7)           │ CREAM (cols 8–12)       │
│ ◉ IRIS                  MEOW LABS    │                         │
│                                      │  Sign in                │
│   Every                              │  ─────────────── (2px)  │
│   message                            │  EMAIL                  │
│   finds                              │  [ arya@meowlabs.id   ] │
│   its way.                           │  PASSWORD               │
│                     ┌──cat sits──┐   │  [ ••••••••      Show ] │
│   ─ ─ ─ ─ ─ ─ ─ ─ ─ ┤ eyes track ├   │  [ Sign in →  (ink)   ] │
│   dashed route line └────────────┘   │                         │
│ MX → AUTH → INBOX       mail.meow…   │  Need a throwaway?      │
│                                      │  Temporary inbox →      │
└──────────────────────────────────────┴─────────────────────────┘
```

### Color placement
- Left field is solid `--iris-vermilion` with grain at 0.22 opacity. All text on it is `--iris-cream` (5.52:1). Ink text on red is not allowed (3.12:1).
- Mascot is ink-black. Its irises are cream with ink slit pupils, so they still read against the red.
- The right panel is `--iris-cream`, and inputs are `--iris-cream-raised` (#FFF) with a 1.5px `--iris-stone` border.
- The submit button is ink-filled, not red. The red field already carries the brand, and a second red mass would compete with it.

### Typography hierarchy
1. Tagline, 120px Archivo 900 / 125% width / lh 0.9. One word per line, left-aligned to col 1 + 48px.
2. "Sign in", 32px Archivo 800, with a 2px ink bar beneath.
3. Field labels use mono 11px caps. Inputs use Plex 15px at 48px height.
4. Footer route caption is mono 11px caps in cream at 80% opacity: `MX → AUTH → INBOX`.

### Mascot purpose
The mascot is a guard rather than decoration. The cat sits on the route line.
- Idle: pupils follow the caret in the email field (pupil translates horizontally ±6px).
- Password field focused: eyes close (`cat-sit-shut`). The cat does not watch you type the password. This is the single delight moment, and it encodes privacy.
- Error: pupils dilate to round. Copy appears in an inline `--iris-danger` row with an alert icon: "Email or password is incorrect."
- Reduced motion: no tracking, static open eyes.

### States
Loading: button label becomes "Signing in…" with mono dots, and the button is disabled. Width stays the same.

---

## 2. Desktop inbox, 3-pane (`/mail`), 1440×900

### Layout
CSS grid columns `232px | 384px | 1fr`, with a 56px top bar whose dividers line up with the pane columns.

```
┌ ◉ IRIS  meowlabs.id ┬ ⌕ Search mail      [/] ┬ ⟳ SYNCED 19:42     arya@… [A] ┐
╞═════════════════════╪═══════════════════════════╪══════════════════════════════════╡ 2px ink
│[ ✎ Compose     C ]  │ Inbox          [3 UNREAD] │ [Reply] Forward ▢ 🗑   1 OF 128 ⋯│
│ MAILBOX             │ ══════════════════════════│──────────────────────────────────│
│▌Inbox            3  │ TODAY          NEWEST FIRST│ INBOX · FRI 30 OCT 09:12         │
│ Sent                │▌◉ Rina Hartono      09:12 │ Q4 roadmap:              │ ATTACH │
│ Drafts           1  │   Q4 roadmap: review notes│ review notes             │ ▛PDF▜ Q4-ro… ↓
│ Archive             │   Three items before…     │══════════════════════════│ ▛PDF▜ temp… ↓
│ Trash               │   ⌎ [PDF][2]              │ [R] Rina Hartono  SPF✓   │ [Download all]
│ TEMPORARY           │ ◉ GitHub            08:47 │──────────────────────────│ ROUTE  │
│ ◷ kurir-7f3k2    2  │   [iris] CI passed…       │ Hi Arya, …  (17/1.6,68ch)│ ■ mx.meowlabs.id
│ + New address       │   Budi Santoso  Yesterday │                          │ ┊      │
│─────────────────────│   Invoice #2026-104       │ ┌ Remote images blocked ┐│ ■ auth pass
│ STORAGE ▬▬▭▭▭ 1.2/5G│                           │ └──────────── Load once ┘│ ● Delivered
└─────────────────────┴───────────────────────────┴──────────────────────────────────┘
```

### Color placement
- The cream background with grain covers the top bar, sidebar, and list. The reader is flat white (`--iris-cream-raised`), and message text never sits on grain.
- Pane dividers are 1px ink. Under pane titles and the top bar there is a 2px ink bar. Row separators are 1px `--iris-rule`.
- Red appears in four places only:
  - the Compose button (cream text)
  - the 3px left edge on the selected row and the active nav item
  - the unread iris mark
  - the PDF file tile
- Selected row and active nav get a `--iris-cream-sunk` fill plus the red edge. The treatment is identical in both places.
- The "3 unread" chip and avatar are ink-filled with cream text. The sender avatar in the reader is red with cream initial.

### Typography hierarchy
1. Pane title "Inbox", 32px Archivo 800 / 118% width.
2. Reader subject, 36px Archivo 800 / 110% width, max 18ch, wraps to 2 lines.
3. List row: sender 14px is 600 if unread and 400 if read. Subject is 14px, preview 13px graphite, time is mono 12px graphite right-aligned. All of these are single-line with ellipsis.
4. Message body is Plex 17/1.6 with max 68ch.
5. Labels (MAILBOX, TODAY, ATTACHMENTS, ROUTE) are mono 11px caps graphite.

### Components
- **Unread mark**: 10px vermilion disc with a 1.3×3.6px ink slit pupil, a tiny iris. It is paired with the semibold weight, so state is never signalled by color alone.
- **Attachment rail**: a 248px column inside the reader, separated by a 1px ink rule. Each file tile is 44×52 with a 1.5px ink border, a dog-eared top-right corner, and the type in mono caps at the bottom-left (PDF tiles are red-filled). Next to the tile: name 13/500 and a mono meta line ("2.9 MB · 14 pages"), then a download icon button. Below: a full-width "Download all (.zip)" secondary button. Files are download-only until the sandbox origin is live.
- **Route trace**: the message-journey metaphor shown inside the product. It is a vertical dashed ink line with square nodes: ink = passed hop, red circle = delivered. Each node has a mono timestamp. It is built only from real `Received`, SPF, and DKIM headers and never invented.
- **Remote content bar**: 1px ink box on cream, eye icon, bold first clause, and a "Load once" secondary button.
- **Buttons**: 36px tall, 1.5px ink border, 2px radius, Plex 600 14px. There are three types:
  - Primary: red fill with cream text.
  - Secondary: transparent.
  - Ink: ink fill with cream text.
- **Focus**: 2px ink ring with 2px cream offset on all controls.

### Mascot
None in the working inbox. Content-dense screens stay quiet, and the eye mark in the wordmark is enough.

---

## 3. Temporary inbox (`/temp/:id`), 1440×900

Same shell and top bar. The sidebar shows "TEMPORARY · kurir-7f3k2" as active. The list and reader merge into a 2-column body: `1fr | 360px`.

### Layout
```
┌ sidebar ┬────────────────────────────────────────────────┬───────────────────────┐
│         │ TEMPORARY ADDRESS                    (cat peeks│ EXPIRES               │
│         │ ┌──────────────────────────────────────over──┐ │ 00:42:17  (Archivo 72,│
│         │ │ kurir-7f3k2@inbox.meowlabs.id   [⧉ Copy]   │ │  tabular)             │
│         │ └────────────────────────────────────────────┘ │ ▬▬▬▬▬▬▬▬▭▭▭▭ ticks   │
│         │ RECEIVE-ONLY · NOT FOR SENSITIVE MAIL          │ at 20:24 WIB          │
│         │════════════════════════════════════════════════│───────────────────────│
│         │ Messages 2                  ● Listening · 5s   │ [ Extend? no. ]       │
│         │◉ Shopee      Kode verifikasi kamu: 482913  1m  │ [ New address ]       │
│         │  noreply@x   Confirm your email            9m  │ [ Delete now ] danger │
└─────────┴────────────────────────────────────────────────┴───────────────────────┘
```
(Extend is not in the MVP. The right rail only shows **New address** and **Delete now**.)

### Address card (the hero)
- The card sits on a `--iris-cream-raised` slab with a 2px ink border and an `--iris-shadow-plate` offset (8px 8px 0 ink). This is the only plate shadow on the screen.
- The address is mono 600 at 28px, ink, and wraps at `@` if it lacks room. It is never truncated.
- The Copy button is primary (red fill, cream text) and 44px tall. After copying, the label becomes "Copied" with a check icon for 2s, and `aria-live="polite"` announces "Address copied".
- `cat-peek` (ink cat head and paws) hangs over the top edge of the card at right-third, eyes looking down at the address. This is the only mascot placement in the app shell, used because a temp inbox is a "watched" mailbox.
- Caption line: mono 11px caps graphite, "RECEIVE-ONLY · NOT FOR SENSITIVE MAIL".

### Expiry badge and countdown
- Rail label "EXPIRES", then the countdown `HH:MM:SS` in Archivo 900 at 72px with tabular figures (`font-variant-numeric: tabular-nums`).
- A tick bar sits below it: 60 1px ticks, with elapsed ticks in `--iris-rule` and remaining ticks in ink.
- An absolute time is always shown with the countdown: "at 20:24 WIB" in mono 13px.
- Thresholds, each pairing text with a visual change so color is not the only signal:

| Remaining | Treatment |
|---|---|
| > 10 min | ink digits |
| ≤ 10 min | red digits; chip "EXPIRING" with timer icon |
| ≤ 60 s | red digits; chip text "LESS THAN A MINUTE"; seconds digits get a red underline |
| expired | screen switches to the expired state |

- The compact badge (sidebar and list contexts) is a 22px chip, mono 11px caps: `◷ 42M LEFT`, with an ink border. It becomes red-filled with cream text at ≤ 10 min.

### Message list
- Rows are 64px and use a single-line grid: `14px mark | sender 200px | subject 1fr | time mono`.
- Unread rows get the iris mark and semibold weight, the same as the permanent inbox.
- If the subject contains a numeric code of 4–8 digits, show it as a mono chip with its own copy icon (OTP is the main temp-mail use case). ponytail: regex detection only; no parsing of the body.
- Live status sits at the top-right of the list: a pulsing 6px ink dot (static under reduced motion) and the label "LISTENING · 5S".
- Clicking a row opens the reader in place of the list, with a back arrow and the same reader spec as the desktop inbox, minus Reply/Forward.

### Expired state
- The address card stays, but the address gets an ink strikethrough and the plate shadow is removed.
- `cat-sleep` illustration at 240px replaces the list.
- Headline: "This address has expired." 32px Archivo 800.
- Body: "Messages were deleted at 20:24 WIB and can't be recovered."
- The primary button reads "Create new address". No stale message content is rendered.

### Typography hierarchy
1. Countdown, 72px Archivo 900 (the one display moment on this screen).
2. Address, mono 28px/600.
3. "Messages", 24px Archivo 800, plus a count in mono.
4. Rows: Plex 14 for sender and subject, mono 12 for time.

---

## Accessibility invariants (all screens)
- AA contrast. Verified ratios are listed in `iris-tokens.css` comments.
- No status is conveyed by color alone: unread uses shape + weight, expiry uses text + chip, errors use icon + text.
- Touch targets are ≥ 44px for copy and primary actions.
- Visible 2px ink focus ring.
- Every mascot SVG is `aria-hidden="true"`, and its meaning is always stated in text as well.
- `prefers-reduced-motion` disables eye tracking, pulse, and countdown tick animation (digits still update).
