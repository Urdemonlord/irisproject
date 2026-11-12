import { SITE, footerLinks, initSite, setPage } from './site'
import { INTEGRATION_DOCS_HTML } from './docs-integrations'

type Summary = { id: string; from: string; to: string[]; subject: string; preview: string; received_at: string; is_read: boolean; is_draft?: boolean; has_attachment?: boolean; is_starred?: boolean; labels?: string[]; thread_id?: string | null }
type Attachment = { part_id: string; blob_id: string; name: string; content_type: string; size: number; disposition?: string | null }
type MimePart = { part_id: string; content_type: string; name?: string | null; disposition?: string | null; size: number }
type Detail = Summary & { text_body: string | null; html_body: string | null; cc?: string[]; message_id?: string | null; references?: string[]; attachments?: Attachment[]; mime_parts?: MimePart[]; body_source?: string }
type TempInbox = { id: string; address: string; access_token: string; expires_at: string; extensions_left?: number }
type Contact = { id: string; name: string; email: string }

const app = document.querySelector<HTMLDivElement>('#app')!

// Requests started by /early.js before this bundle loaded.
declare global { interface Window { __early?: Record<string, Promise<Response>> } }
function takeEarly(path: string) {
  const p = window.__early?.[path]
  if (p) delete window.__early![path]
  return p
}

class HttpError extends Error { constructor(public status: number, public code: string) { super(code) } }

const csrfCookie = () => document.cookie.split('; ').find((c) => c.startsWith('iris_csrf='))?.slice(10)
async function api<T>(path: string, init: RequestInit = {}): Promise<T> {
  const method = (init.method ?? 'GET').toUpperCase()
  const headers: Record<string, string> = { 'content-type': 'application/json', ...(init.headers as Record<string, string>) }
  if (method !== 'GET' && method !== 'HEAD') {
    if (!csrfCookie()) await fetch('/auth/csrf', { credentials: 'same-origin' }).catch(() => {})
    headers['x-csrf-token'] = csrfCookie() ?? ''
  }
  let res: Response
  const early = method === 'GET' ? takeEarly(path) : undefined
  try { res = await (early ?? fetch(path, { credentials: 'same-origin', ...init, headers })) } catch { throw new HttpError(0, 'network') }
  if (!res.ok) { const body = await res.json().catch(() => ({})); throw new HttpError(res.status, body.error ?? 'request_failed') }
  return res.status === 204 ? (undefined as T) : res.json()
}

// ---------- DOM helpers ----------
type Props = Record<string, unknown>
function h<K extends keyof HTMLElementTagNameMap>(tag: K, props: Props = {}, ...kids: (Node | string)[]) {
  const el = document.createElement(tag)
  for (const [k, v] of Object.entries(props)) {
    if (v == null || v === false) continue
    if (k.includes('-')) el.setAttribute(k, String(v))
    else (el as unknown as Props)[k] = v
  }
  el.append(...kids)
  return el
}

function svgUse(id: string, viewBox: string, cls: string) {
  const s = document.createElementNS('http://www.w3.org/2000/svg', 'svg')
  s.setAttribute('viewBox', viewBox)
  s.setAttribute('class', cls)
  s.setAttribute('aria-hidden', 'true')
  s.setAttribute('focusable', 'false')
  const u = document.createElementNS('http://www.w3.org/2000/svg', 'use')
  u.setAttribute('href', `/mascot-sprite.svg#${id}`)
  s.append(u)
  return s
}
const icon = (id: string, cls = 'icon') => svgUse(`i-${id}`, '0 0 20 20', cls)
const eyeMark = (cls = 'eye-mark') => {
  const s = document.createElementNS('http://www.w3.org/2000/svg', 'svg')
  s.setAttribute('viewBox', '0 0 48 28')
  s.setAttribute('class', cls)
  s.setAttribute('aria-hidden', 'true')
  s.setAttribute('focusable', 'false')
  s.innerHTML = '<path d="M2 14 C8 3 40 3 46 14 C40 25 8 25 2 14 Z" fill="var(--iris-vermilion)" stroke="var(--iris-cream)" stroke-width="1.5" stroke-linejoin="round" paint-order="stroke fill"/><path d="M24 8 C21 11 21 17 24 20 C27 17 27 11 24 8 Z" fill="var(--iris-ink)"/>'
  return s
}
const mascot = (id: 'cat-peek' | 'cat-sleep', cls = 'mascot') => svgUse(id, id === 'cat-peek' ? '0 -4 160 82' : '0 0 260 150', cls)

/** Text button with optional icon. Icon-only variants keep the label for screen readers. */
const btn = (cls: string, label: string, ic?: string, props: Props = {}) => {
  const iconOnly = /\bbtn-icon(-sm)?\b/.test(cls)
  return h('button', { type: 'button', className: cls, ...(iconOnly ? { ariaLabel: label, title: label } : {}), ...props },
    ...(ic ? [icon(ic)] : []),
    ...(iconOnly ? [] : [h('span', { className: 'btn-label' }, label)]))
}
/** Put a button into a busy state (spinner + label) and return a restore function. */
function busy(b: HTMLButtonElement, label: string) {
  const before = [...b.childNodes]
  b.disabled = true
  b.setAttribute('aria-busy', 'true')
  b.replaceChildren(h('span', { className: 'spinner', 'aria-hidden': 'true' }), h('span', { className: 'btn-label' }, label))
  return () => { b.disabled = false; b.removeAttribute('aria-busy'); b.replaceChildren(...before) }
}

const fmtDate = (iso: string) => new Date(iso).toLocaleString(undefined, { dateStyle: 'medium', timeStyle: 'short' })
const fmtTime = (d: Date) => d.toLocaleTimeString(undefined, { timeStyle: 'short' })
const fmtShort = (iso: string) => {
  const d = new Date(iso)
  return d.toDateString() === new Date().toDateString() ? fmtTime(d) : d.toLocaleDateString(undefined, { month: 'short', day: 'numeric' })
}
const senderName = (from: string) => from.replace(/\s*<[^>]*>$/, '').replace(/^"|"$/g, '') || from || '(unknown sender)'

const errorText: Record<string, string> = {
  invalid_credentials: 'Email or password is incorrect.',
  too_many_attempts: 'Too many attempts. Try again in 15 minutes.',
  too_many_reads: 'Too many reads. Wait a moment and try again.',
  mail_server_unavailable: 'Mail server is unavailable. Try again shortly.',
  mail_server_error: 'The mail server returned an unexpected response. Try again shortly.',
  database_unavailable: 'IRIS is temporarily unavailable. Try again shortly.',
  turnstile_failed: 'Verification failed. Please try again.',
  turnstile_unavailable: 'Verification is temporarily unavailable. Please try again shortly.',
  network: 'Can’t reach IRIS. Check your connection and try again.',
}
const tempErrors: Record<string, string> = {
  rate_limited: 'Too many addresses created from your network. Try again later.',
  quota_exceeded: 'You already have 3 active temporary addresses. Wait for one to expire.',
  extension_limit_reached: 'This address cannot be extended any more.',
  network: errorText.network,
}

const brand = (sub: string) => h('a', { href: '#/', className: 'brand', ariaLabel: 'IRIS home' }, h('span', { className: 'brand-mark' }, eyeMark(), 'IRIS'), h('span', { className: 'brand-sub' }, sub))

// ---------- cat with trackable eyes ----------
const CAT_BODY = `
  <defs><filter id="ink" x="-5%" y="-5%" width="110%" height="110%"><feTurbulence type="fractalNoise" baseFrequency=".045" numOctaves="2" seed="7" result="n"/><feDisplacementMap in="SourceGraphic" in2="n" scale="3.2" xChannelSelector="R" yChannelSelector="G"/></filter></defs>
  <g filter="url(#ink)" fill="#16130F">
    <path d="M58 244C34 214 38 160 66 132C55 116 52 98 56 82L50 34L82 60C93 56 109 56 120 60L152 34L146 82C150 98 147 116 136 132C160 158 166 212 142 244Z"/>
    <path class="cat-tail" d="M140 238C178 240 196 214 186 184C180 166 162 168 166 184" fill="none" stroke="#16130F" stroke-width="13" stroke-linecap="round"/>
  </g>
  <path d="M96 112l4 3 4-3" stroke="#F7F7F2" stroke-width="1.6" fill="none" stroke-linecap="round" opacity=".7"/>`
const CAT_EYES_OPEN = `
  <path d="M84 216C86 228 84 238 80 244M116 216C114 228 116 238 120 244" stroke="#F7F7F2" stroke-width="2" fill="none" stroke-linecap="round" opacity=".55"/>
  <g class="cat-eyes">
  <path d="M70 92Q83 79 96 92Q83 103 70 92Z" fill="#B83232"/>
  <path class="pupil" d="M83 82.5C81 87 81 97 83 101C85 97 85 87 83 82.5Z" fill="#16130F"/>
  <path d="M104 92Q117 79 130 92Q117 103 104 92Z" fill="#B83232"/>
  <path class="pupil" d="M117 82.5C115 87 115 97 117 101C119 97 119 87 117 82.5Z" fill="#16130F"/>
  </g>`
const CAT_EYES_SHUT = `<path d="M71 92Q83 99 95 92M105 92Q117 99 129 92" stroke="#B83232" stroke-width="3" fill="none" stroke-linecap="round"/>`

function catSitSVG(shut = false): SVGSVGElement {
  const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg')
  svg.setAttribute('viewBox', '0 0 200 250')
  svg.setAttribute('aria-hidden', 'true')
  svg.innerHTML = CAT_BODY + (shut ? CAT_EYES_SHUT : CAT_EYES_OPEN)
  return svg
}

function trackEyes(email: HTMLInputElement, wrap: HTMLElement) {
  const update = () => {
    const v = email.value
    const ratio = v.length ? (email.selectionStart ?? v.length) / v.length : 0.5
    const x = Math.max(-6, Math.min(6, ratio * 12 - 6))
    wrap.querySelectorAll<SVGPathElement>('.pupil').forEach((p) => { p.style.transform = `translateX(${x}px)` })
  }
  ;['input', 'keyup', 'click'].forEach((ev) => email.addEventListener(ev, update))
}

// ---------- Turnstile (login bot gate) ----------
// Explicit rendering (the app is an SPA, so the form does not exist at
// page load). Only used when a site key is baked into the build
// (SITE.turnstileSiteKey); without one the login form works as before.
declare global {
  interface Window {
    turnstile?: {
      render: (
        container: HTMLElement,
        params: {
          sitekey: string
          // The action the token is minted for; the server requires
          // it to be "login" (see apps/api/src/turnstile.rs).
          action?: string
          theme?: 'light' | 'dark' | 'auto'
          callback?: (token: string) => void
          'expired-callback'?: () => void
          'error-callback'?: (code: string) => void
        },
      ) => string
      reset: (widgetId?: string) => void
    }
  }
}

const TURNSTILE_SCRIPT = 'https://challenges.cloudflare.com/turnstile/v0/api.js?render=explicit'
let turnstileScript: Promise<void> | null = null
function loadTurnstileScript(): Promise<void> {
  if (window.turnstile) return Promise.resolve()
  if (!turnstileScript) {
    turnstileScript = new Promise<void>((resolve, reject) => {
      const s = document.createElement('script')
      s.src = TURNSTILE_SCRIPT
      s.defer = true
      s.onload = () => resolve()
      // Clear the cache so a later visit to the login route retries
      // the load instead of reusing this rejected promise forever.
      s.onerror = () => { turnstileScript = null; reject(new Error('turnstile script failed to load')) }
      document.head.append(s)
    })
  }
  return turnstileScript
}

// ---------- Login ----------
function field(id: string, label: string, ic: string, input: HTMLInputElement, extra?: HTMLElement) {
  const err = h('p', { className: 'field-error', id: `${id}-error`, 'aria-live': 'polite' })
  input.setAttribute('aria-describedby', err.id)
  const wrap = h('div', { className: 'field' }, h('label', { htmlFor: id }, label), h('div', { className: 'input-wrap' }, icon(ic), input, ...(extra ? [extra] : [])), err)
  const set = (msg: string) => {
    err.replaceChildren(...(msg ? [icon('alert', 'icon icon-sm'), msg] : []))
    msg ? input.setAttribute('aria-invalid', 'true') : input.removeAttribute('aria-invalid')
  }
  return { wrap, set }
}

function renderLogin() {
  setPage({ title: 'Sign in', description: 'Sign in to IRIS, the self-hosted mailbox from Meow Labs — or grab a 1-hour temporary inbox, no sign-up needed.', path: '/' })
  const status = h('div', { className: 'form-error', role: 'alert' })
  const email = h('input', { type: 'email', name: 'email', id: 'email', autocomplete: 'username', inputMode: 'email', placeholder: 'you@example.com', spellcheck: false }) as HTMLInputElement
  const password = h('input', { type: 'password', name: 'password', id: 'password', autocomplete: 'current-password' }) as HTMLInputElement
  const submit = h('button', { type: 'submit', className: 'btn btn-ink btn-block' }, h('span', { className: 'btn-label' }, 'Sign in')) as HTMLButtonElement
  const showPw = h('button', { type: 'button', className: 'show-pw', ariaLabel: 'Show password', 'aria-pressed': 'false' }, 'Show') as HTMLButtonElement
  showPw.onclick = () => {
    const hidden = password.type === 'password'
    password.type = hidden ? 'text' : 'password'
    showPw.textContent = hidden ? 'Hide' : 'Show'
    showPw.setAttribute('aria-pressed', String(hidden))
  }
  const emailF = field('email', 'Email', 'mail', email)
  const pwF = field('password', 'Password', 'lock', password, showPw)

  const catWrap = h('div', { className: 'mascot-wrap' }, catSitSVG(false))
  trackEyes(email, catWrap)

  // Turnstile widget, only when a site key is baked into the build.
  // Explicit rendering: the login form mounts after page load (SPA),
  // so implicit rendering could miss it. The token is single-use and
  // expires after 5 minutes: it is cleared (and the widget reset)
  // after every completed login request and when it expires, so the
  // next attempt always mints a fresh token. The widget mints tokens
  // with action "login", which the server requires.
  let turnstileToken: string | null = null
  let turnstileWidgetId: string | null = null
  let turnstileFailed = false
  const turnstileBox = h('div', { className: 'turnstile-box' })
  const resetTurnstile = () => {
    turnstileToken = null
    if (turnstileWidgetId && window.turnstile) window.turnstile.reset(turnstileWidgetId)
  }
  if (SITE.turnstileSiteKey) {
    loadTurnstileScript()
      .then(() => {
        if (!window.turnstile) return
        turnstileWidgetId = window.turnstile.render(turnstileBox, {
          sitekey: SITE.turnstileSiteKey,
          action: 'login',
          theme: 'light',
          callback: (token) => { turnstileToken = token; turnstileFailed = false },
          'expired-callback': resetTurnstile,
          'error-callback': () => {
            turnstileToken = null
            // Mark the failure so a submit shows the actionable
            // refresh message instead of "wait for verification".
            turnstileFailed = true
            status.replaceChildren(icon('alert'), h('span', {}, 'Verification failed to load. Refresh the page and try again.'))
          },
        })
      })
      .catch(() => {
        turnstileFailed = true
        turnstileBox.replaceChildren(h('p', { className: 'field-error' }, 'Verification could not load. Check your connection and refresh.'))
      })
  }

  const left = h('div', { className: 'login-left' },
    h('div', { className: 'topline' }, h('span', { className: 'wordmark' }, eyeMark('eye-mark wordmark-eye'), 'IRIS'), h('span', {}, 'Meow Labs')),
    h('p', { className: 'login-tagline' }, h('span', {}, 'Every'), h('span', {}, 'message'), h('span', {}, 'finds'), h('span', {}, 'its way.')),
    h('div', { className: 'login-route', 'aria-hidden': 'true' }, h('div', { className: 'route-line' }), h('div', { className: 'route-nodes' }, h('span', { className: 'route-node-sq' }), h('span', { className: 'route-node-sq' }), h('span', { className: 'route-node-rd' })), catWrap),
    h('div', { className: 'login-footer' }, h('span', {}, 'MX → Auth → Inbox'), h('span', {}, location.hostname)))
  const form = h('form', { className: 'login-form', noValidate: true },
    emailF.wrap, pwF.wrap, ...(SITE.turnstileSiteKey ? [turnstileBox] : []), status, submit,
    h('p', { className: 'login-foot' }, 'Need a throwaway? ', h('a', { href: '#/temp', className: 'link' }, 'Temporary inbox →')))
  const right = h('main', { className: 'login-right' },
    h('div', { className: 'login-panel' }, h('h1', {}, 'Sign in'), form),
    footerLinks())

  const clearStatus = () => status.replaceChildren()
  email.addEventListener('input', () => { emailF.set(''); clearStatus() })
  password.addEventListener('input', () => { pwF.set(''); clearStatus() })
  password.addEventListener('focus', () => catWrap.replaceChildren(catSitSVG(true)))
  password.addEventListener('blur', () => catWrap.replaceChildren(catSitSVG(false)))

  const validate = () => {
    const e = email.value.trim()
    const eMsg = !e ? 'Enter your email address.' : !/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(e) ? 'That doesn’t look like an email address.' : ''
    const pMsg = password.value ? '' : 'Enter your password.'
    emailF.set(eMsg); pwF.set(pMsg)
    ;(eMsg ? email : pMsg ? password : null)?.focus()
    return !eMsg && !pMsg
  }

  form.addEventListener('submit', async (e) => {
    e.preventDefault()
    clearStatus()
    if (!validate()) return
    // With the widget on, a submit without a token means the challenge
    // has not finished (or the script failed to load): say so instead
    // of spending a login attempt the server would reject.
    if (SITE.turnstileSiteKey && !turnstileToken) {
      status.replaceChildren(icon('alert'), h('span', {}, turnstileFailed ? 'Verification could not load. Check your connection and refresh.' : 'Please wait for the verification to finish.'))
      return
    }
    const restore = busy(submit, 'Signing in…')
    try {
      await api('/auth/login', { method: 'POST', body: JSON.stringify({ email: email.value.trim(), password: password.value, turnstile_token: turnstileToken ?? undefined }) })
      // The token was spent by the request: reset so the widget mints
      // a fresh one (single-use tokens cannot be replayed).
      resetTurnstile()
      await renderMailbox()
    } catch (err) {
      const code = (err as HttpError).code
      // The token was already spent on the failed attempt (or never
      // minted one): reset so the next attempt gets a fresh token.
      // No-op when the widget is off.
      resetTurnstile()
      if (code === 'invalid_credentials') { [email, password].forEach((i) => i.setAttribute('aria-invalid', 'true')); password.select() }
      status.replaceChildren(icon('alert'), h('span', {}, errorText[code] ?? 'Sign-in failed. Please try again.'))
      restore()
    }
  })
  app.className = 'login-shell'
  app.replaceChildren(left, right)
  email.focus({ preventScroll: true })
}

// ---------- Shared pieces ----------
function skeleton(rows = 6) {
  return h('div', { className: 'skel-list', 'aria-busy': 'true', ariaLabel: 'Loading messages', role: 'status' },
    ...Array.from({ length: rows }, () => h('div', { className: 'skel-row' },
      h('span', { className: 'skel skel-from' }), h('span', { className: 'skel skel-date' }), h('span', { className: 'skel skel-subject' }), h('span', { className: 'skel skel-preview' }))))
}
const readerSkeleton = () => h('div', { className: 'reader-skel', 'aria-busy': 'true', role: 'status', ariaLabel: 'Loading message' },
  h('span', { className: 'skel skel-title' }), h('span', { className: 'skel skel-from' }), h('span', { className: 'skel skel-subject' }), h('span', { className: 'skel skel-preview' }), h('span', { className: 'skel skel-preview' }))

const navItem = (href: string, ic: string, label: string, current = false, count?: HTMLElement) =>
  h('a', { href, className: 'nav-item', ...(current ? { 'aria-current': 'page' } : {}) }, icon(ic), h('span', { className: 'nav-label' }, label), ...(count ? [count] : []))
const navSection = (label: string) => h('div', { className: 'nav-section' }, label)

/** Account chip: opens the Mailboxes dialog. A button everywhere (not a
 *  plain label) so the multi-mailbox UI is reachable from every page,
 *  including on phones where the chip is icon-only (see styles.css). */
const accountChip = (email: string) =>
  h('button', { type: 'button', className: 'account-chip', title: email, onclick: () => { manageMailboxes().catch(() => toast('Could not load mailboxes. Try again.')) } }, icon('account'), h('span', { className: 'account-email' }, email))

/** Bottom navigation for pages without the mail sidebar (Organize, DMARC,
 *  Admin). Same destinations as the mailbox bottom bar; only shown on
 *  phones (see styles.css), where those pages otherwise have no nav. */
const mobileNav = (current: string, isAdmin: boolean) =>
  h('nav', { className: 'mobile-nav', ariaLabel: 'Sections' },
    navItem('#/', 'inbox', 'Mailbox', current === 'mail'),
    navItem('#/temp', 'timer', 'Temp', current === 'temp'),
    navItem('#/organize', 'filter', 'Organize', current === 'organize'),
    navItem('#/dmarc', 'shield', 'DMARC', current === 'dmarc'),
    ...(isAdmin ? [navItem('#/admin', 'account', 'Admin', current === 'admin')] : []))

function emptyState(ic: string, title: string, text: string) {
  return h('div', { className: 'empty' }, h('span', { className: 'empty-art' }, icon(ic, 'icon icon-xl')), h('p', { className: 'empty-title' }, title), h('p', { className: 'empty-text' }, text))
}
const errorState = (text: string, retry?: () => void) => h('div', { className: 'error-state', role: 'alert' },
  h('p', { className: 'form-error' }, icon('alert'), text), ...(retry ? [btn('btn btn-secondary', 'Try again', 'refresh', { onclick: retry })] : []))

const REMOTE_IMG = /<img\b[^>]*\bsrc\s*=\s*["']?\s*(https?:)?\/\//i
const fmtBytes = (n: number) => n < 1024 ? `${n} B` : n < 1024 * 1024 ? `${(n / 1024).toFixed(1)} KB` : `${(n / 1024 / 1024).toFixed(1)} MB`
const previewable = (a: Attachment) => /\.gz$|\.zip$|\.xml$/i.test(a.name) || /gzip|zip|xml/i.test(a.content_type)
/** Mint a one-time render URL for `src` mode, or null when the separate
 *  content origin is off / minting failed. Supplied by the caller
 *  (permanent mailboxes only); temp inboxes pass nothing and stay on srcdoc. */
type HtmlBodyMinter = (allowRemote: boolean) => Promise<string | null>

/** Build the minter for a permanent mailbox message, or `undefined` when
 *  the separate content origin is off — then messageBody stays on the
 *  srcdoc frame, exactly as today. A mint failure resolves to null and
 *  also falls back to srcdoc, so the user still sees the message. */
const htmlBodyMinter = (messageUrl: string): HtmlBodyMinter | undefined =>
  SITE.mailContentOrigin
    ? async (allowRemote) => {
        try {
          const { url } = await api<{ url: string }>(`${messageUrl}/html-body-token`, {
            method: 'POST',
            body: JSON.stringify({ allow_remote: allowRemote }),
          })
          return url
        } catch {
          return null
        }
      }
    : undefined

async function messageBody(m: Detail, mintUrl?: HtmlBodyMinter) {
  if (!m.html_body) return m.text_body ? [h('pre', { className: 'mail-text' }, m.text_body)] : []
  const frame = (allowRemote: boolean) => {
    // The srcdoc frame is sandboxed (no scripts, no same-origin, no top
    // navigation) and this meta CSP is its only policy. The directives
    // below only restate the 'none' that default-src already enforces
    // (each falls back to default-src), so the effective policy is
    // unchanged — they stay 'none' even if default-src is ever loosened.
    // style-src 'unsafe-inline' must stay: real HTML mail relies on
    // inline styles, and dropping it would break rendering of most HTML
    // messages. frame-ancestors/sandbox cannot be set via <meta>.
    const csp = `<meta http-equiv="Content-Security-Policy" content="default-src 'none'; script-src 'none'; object-src 'none'; media-src 'none'; font-src 'none'; connect-src 'none'; worker-src 'none'; manifest-src 'none'; frame-src 'none'; base-uri 'none'; form-action 'none'; style-src 'unsafe-inline'; img-src data: cid:${allowRemote ? ' https:' : ''}">`
    const f = h('iframe', { className: 'mail-frame', title: 'Message body', srcdoc: csp + m.html_body, loading: 'lazy' })
    f.setAttribute('sandbox', '')
    f.setAttribute('referrerpolicy', 'no-referrer')
    return f
  }
  // Separate-origin mode: the HTML is served from MAIL_CONTENT_ORIGIN
  // under a one-time token URL, so it never executes in the app origin.
  // Any mint failure falls back to the srcdoc frame below.
  const srcFrame = async (allowRemote: boolean): Promise<HTMLElement | null> => {
    if (!mintUrl) return null
    const src = await mintUrl(allowRemote).catch(() => null)
    if (!src) return null
    // sandbox without allow-same-origin / allow-scripts: the frame stays
    // an opaque origin even if the content origin is ever misconfigured.
    const f = h('iframe', { className: 'mail-frame', title: 'Message body', src, loading: 'lazy' })
    f.setAttribute('sandbox', '')
    f.setAttribute('referrerpolicy', 'no-referrer')
    return f
  }
  let body: HTMLElement = (await srcFrame(false)) ?? frame(false)
  if (!REMOTE_IMG.test(m.html_body)) return [body]
  const load = btn('btn btn-secondary btn-sm', 'Load images once')
  const bar = h('div', { className: 'remote-bar' }, icon('eye'), h('span', {}, h('strong', {}, 'Remote images blocked'), ' to protect your privacy.'), load)
  load.onclick = async () => { const next = (await srcFrame(true)) ?? frame(true); body.replaceWith(next); body = next; bar.remove() }
  return [bar, body]
}

/** Attachments, the no-body fallback note, and View Original. Rendered
 *  after the body for both permanent mailboxes and temp inboxes, since
 *  both go through openMessageAt with their own detail URL + auth init.
 *  Attachment downloads are fetched (not plain links) so temp-inbox
 *  bearer auth travels with the request too. */
function messageExtras(url: string, init: RequestInit | undefined, m: Detail) {
  const nodes: HTMLElement[] = []
  const hasBody = !!(m.html_body || m.text_body)
  if (!hasBody) {
    const parts = [...new Set((m.mime_parts ?? []).map((p) => p.content_type))].join(', ')
    nodes.push(h('div', { className: 'mail-empty-note', role: 'note' },
      h('p', {}, 'This message has no readable text body.'),
      h('p', {}, m.attachments?.length
        ? `It carries ${m.attachments.length} attachment${m.attachments.length > 1 ? 's' : ''} instead — open one below, or check View original to see the raw message.`
        : 'It has no text part and no attachment, so there is nothing to render.'),
      ...(parts ? [h('p', { className: 'mail-parts' }, `MIME parts: ${parts}`)] : [])))
  }
  if (m.attachments?.length) {
    const rows = m.attachments.map((a) => {
      const box = h('div', { className: 'attach-preview' })
      const dl = btn('btn btn-secondary btn-sm', 'Download')
      dl.onclick = async () => {
        const restore = busy(dl, 'Downloading…')
        try {
          const res = await fetch(`${url}/attachments/${encodeURIComponent(a.part_id)}`, { credentials: 'same-origin', ...init })
          if (!res.ok) throw new Error(`download failed (${res.status})`)
          const blob = await res.blob()
          const href = URL.createObjectURL(blob)
          const link = h('a', { href, download: a.name })
          document.body.append(link); link.click(); link.remove()
          setTimeout(() => URL.revokeObjectURL(href), 5000)
        } catch { box.replaceChildren(h('p', { className: 'form-error' }, 'Could not download this attachment.')) }
        restore()
      }
      const actions = [dl]
      if (previewable(a)) {
        const pv = btn('btn btn-secondary btn-sm', 'Preview')
        pv.onclick = async () => {
          if (box.childNodes.length) { box.replaceChildren(); return }
          const restore = busy(pv, 'Loading…')
          try {
            const p = await api<{ inner_name: string; text: string; truncated: boolean }>(`${url}/attachments/${encodeURIComponent(a.part_id)}/preview`, init)
            box.replaceChildren(
              h('p', { className: 'attach-inner' }, p.inner_name),
              h('pre', { className: 'attach-text' }, p.text),
              ...(p.truncated ? [h('p', { className: 'mail-parts' }, 'Preview truncated — download the attachment for the full file.')] : []))
          } catch (err) {
            box.replaceChildren(h('p', { className: 'form-error' },
              (err as HttpError).code === 'not_previewable'
                ? 'Preview is not available for this file type — download it instead.'
                : 'Could not preview this attachment. Try downloading it instead.'))
          }
          restore()
        }
        actions.push(pv)
      }
      return h('div', { className: 'attach-row' },
        h('div', { className: 'attach-meta' }, h('strong', {}, a.name), h('span', {}, `${a.content_type} · ${fmtBytes(a.size)}`)),
        h('div', { className: 'attach-actions' }, ...actions), box)
    })
    nodes.push(h('section', { className: 'attach-section' }, h('h2', {}, 'Attachments'), ...rows))
  }
  const rawBox = h('div', { className: 'raw-box' })
  const rawBtn = btn('btn btn-secondary btn-sm', 'View original')
  rawBtn.onclick = async () => {
    if (rawBox.childNodes.length) { rawBox.replaceChildren(); rawBtn.replaceChildren(h('span', { className: 'btn-label' }, 'View original')); return }
    const restore = busy(rawBtn, 'Loading…')
    try {
      const res = await fetch(`${url}/raw`, { credentials: 'same-origin', ...init })
      if (!res.ok) throw new Error(`raw failed (${res.status})`)
      const text = await res.text()
      const copy = btn('btn btn-secondary btn-sm', 'Copy')
      copy.onclick = async () => { try { await navigator.clipboard.writeText(text) } catch { /* clipboard unavailable */ } copy.replaceChildren(h('span', { className: 'btn-label' }, 'Copied')) }
      rawBox.replaceChildren(copy, h('pre', { className: 'mail-raw' }, text))
      restore()
      rawBtn.replaceChildren(h('span', { className: 'btn-label' }, 'Hide original'))
    } catch {
      restore()
      rawBox.replaceChildren(h('p', { className: 'form-error' }, 'Could not load the original message.'))
    }
  }
  nodes.push(h('div', { className: 'raw-section' }, rawBtn, rawBox))
  return nodes
}

// Message bodies don't change: keep them (as promises) for instant re-open and hover prefetch.
//
// Eviction rules: bodies are immutable, so an entry is never invalidated by
// content -- only by age, size, or a mutation that removes the message:
// - TTL: entries older than MESSAGE_CACHE_TTL_MS are refetched on next use.
//   Bodies never change server-side, so this is only a memory-hygiene bound
//   for entries that outlive their tab's activity (e.g. prefetched bodies
//   in a tab left idle).
// - Size: at most MESSAGE_CACHE_MAX entries, oldest (first inserted)
//   evicted first. The previous `size > 100` check ran before `set`, so the
//   map could grow to 101 entries.
// - Mutation: move()/deleteForever() drop the entry; logout -- and a 401
//   while reading -- drop the whole cache via clearMailCache().
// Folder switches deliberately do NOT clear this cache: keys are
// mailbox+message ids and bodies don't depend on the folder.
const MESSAGE_CACHE_MAX = 100
const MESSAGE_CACHE_TTL_MS = 30 * 60 * 1000
const messageCache = new Map<string, { at: number; p: Promise<{ message: Detail }> }>()
function prefetchMessage(url: string, init?: RequestInit) {
  const hit = messageCache.get(url)
  if (hit) {
    if (Date.now() - hit.at < MESSAGE_CACHE_TTL_MS) return hit.p
    messageCache.delete(url) // expired: refetch below
  }
  const p = api<{ message: Detail }>(url, init)
  p.catch(() => { if (messageCache.get(url)?.p === p) messageCache.delete(url) })
  while (messageCache.size >= MESSAGE_CACHE_MAX) messageCache.delete(messageCache.keys().next().value!)
  messageCache.set(url, { at: Date.now(), p })
  return p
}

// Last message list per account, per tab (sessionStorage is cleared when the tab closes).
//
// Eviction rules: the stored list is only an initial paint -- load()
// refetches it on render, on a stream notice, and every 30 s while the
// live stream is down -- so the stored copy is bounded on both axes:
// - TTL: entries older than LIST_CACHE_TTL_MS are ignored, so a reloaded
//   tab shows a skeleton and fetches fresh instead of serving a stale list.
//   A live tab never hits this: every successful poll rewrites the entry.
// - Size: at most LIST_CACHE_MAX_MESSAGES summaries per folder are stored.
//   Load-more can grow the in-memory list beyond that, but only the first
//   pages are persisted, so the stored object stays far under the
//   sessionStorage quota instead of growing with every load-more click.
const LIST_KEY = 'iris_list_cache'
const LIST_CACHE_TTL_MS = 10 * 60 * 1000
const LIST_CACHE_MAX_MESSAGES = 200
type ListCache = { email: string; mb: string; savedAt: number; folders: Record<string, Summary[]> }
function readListCache(email: string, mb: string, folder: string): Summary[] | null {
  try {
    const c = JSON.parse(sessionStorage.getItem(LIST_KEY) ?? 'null') as ListCache | null
    if (!c || c.email !== email || c.mb !== mb) return null
    if (!c.savedAt || Date.now() - c.savedAt > LIST_CACHE_TTL_MS) return null
    return c.folders?.[folder] ?? null
  } catch { return null }
}
function writeListCache(email: string, mb: string, folder: string, messages: Summary[]) {
  try {
    const c = JSON.parse(sessionStorage.getItem(LIST_KEY) ?? 'null') as ListCache | null
    const folders = c && c.email === email && c.mb === mb ? c.folders ?? {} : {}
    folders[folder] = messages.slice(0, LIST_CACHE_MAX_MESSAGES)
    sessionStorage.setItem(LIST_KEY, JSON.stringify({ email, mb, savedAt: Date.now(), folders }))
  } catch { /* storage full or blocked */ }
}
function clearMailCache() { messageCache.clear(); try { sessionStorage.removeItem(LIST_KEY) } catch { /* ignore */ } }

async function openMessageAt(url: string, reader: HTMLElement, init: RequestInit | undefined, onAuthFail: () => void, position: string, toolbar?: (d: Detail) => HTMLElement, htmlBodyToken?: HtmlBodyMinter) {
  if (!messageCache.has(url)) reader.replaceChildren(readerSkeleton())
  try {
    const { message: m } = await prefetchMessage(url, init)
    const initial = (senderName(m.from).replace(/[^\p{L}\p{N}]/gu, '')[0] ?? '?').toUpperCase()
    reader.replaceChildren(
      ...(toolbar ? [toolbar(m)] : []),
      h('p', { className: 'reader-pos' }, position),
      h('h1', { className: 'reader-subject' }, m.subject || '(no subject)'),
      h('div', { className: 'reader-head' }, h('span', { className: 'avatar', 'aria-hidden': 'true' }, initial),
        h('dl', { className: 'reader-meta' },
          h('dt', {}, 'From'), h('dd', { className: 'reader-from' }, m.from),
          h('dt', {}, 'To'), h('dd', {}, m.to.join(', ') || '—'),
          ...(m.cc?.length ? [h('dt', {}, 'Cc'), h('dd', {}, m.cc.join(', '))] : []),
          h('dt', {}, 'Date'), h('dd', {}, h('time', { dateTime: m.received_at }, fmtDate(m.received_at))))),
      ...(await messageBody(m, htmlBodyToken)),
      ...messageExtras(url, init, m))
    reader.focus({ preventScroll: true })
  } catch (err) {
    if ([401, 410].includes((err as HttpError).status)) return onAuthFail()
    messageCache.delete(url)
    reader.replaceChildren(errorState('Could not open this message.', () => openMessageAt(url, reader, init, onAuthFail, position, toolbar, htmlBodyToken)))
  }
}

/** Three-pane layout: sidebar · list · reader (+ optional right rail). Mobile shows one pane at a time. */
function mailShell(o: { topbar: HTMLElement; nav: HTMLElement[]; listTop: HTMLElement[]; idle: HTMLElement; rail?: HTMLElement; listBottom?: HTMLElement }) {
  const list = h('div', { className: 'list-body' }, skeleton())
  const readerBody = h('div', { className: 'reader-body', tabIndex: -1, 'aria-live': 'polite' }, o.idle)
  const back = btn('btn btn-ghost reader-back', 'Back to list', 'back')
  const reader = h('section', { className: 'reader', ariaLabel: 'Message' }, back, readerBody)
  const layout = h('div', { className: `mail-layout${o.rail ? ' has-rail' : ''}` },
    h('nav', { className: 'sidebar', ariaLabel: 'Mail' }, ...o.nav),
    h('section', { className: 'list-pane', ariaLabel: 'Message list' }, ...o.listTop, list, ...(o.listBottom ? [o.listBottom] : [])),
    reader, ...(o.rail ? [o.rail] : []))
  layout.dataset.view = 'list'
  back.onclick = () => { layout.dataset.view = 'list'; list.querySelector<HTMLElement>('[aria-current="true"]')?.focus() }
  app.className = 'app-shell'
  app.replaceChildren(o.topbar, h('main', { className: 'app-main' }, layout))
  return { list, readerBody, show: () => { layout.dataset.view = 'reader'; reader.scrollTop = 0 }, hide: (idle: HTMLElement) => { layout.dataset.view = 'list'; readerBody.replaceChildren(idle) } }
}

/** Render message rows. Rows are buttons; an optional OTP copy button sits beside each row (never nested). */
function fillList(list: HTMLElement, messages: Summary[], o: { current: string; empty: HTMLElement; compact?: boolean; showTo?: boolean; footer?: HTMLElement; onOpen: (m: Summary, index: number, wasUnread: boolean) => void; onIntent?: (m: Summary) => void; onStar?: (m: Summary) => void }) {
  if (!messages.length) return list.replaceChildren(o.empty)
  const prevList = list.querySelector('.message-list')
  const seen = new Set([...list.querySelectorAll<HTMLElement>('.msg-item')].map((li) => li.dataset.id))
  list.replaceChildren(h('ul', { className: `message-list${prevList ? '' : ' stagger'}`, ariaLabel: 'Messages' }, ...messages.map((m, i) => {
    const mark = m.is_read ? [] : [h('span', { className: 'iris-mark', title: 'Unread' }, h('span', { className: 'sr-only' }, 'Unread: '))]
    const row = h('button', { type: 'button', className: `msg${m.is_read ? '' : ' unread'}${o.compact ? ' compact' : ''}` },
      h('span', { className: 'msg-from' }, ...mark,
        ...(m.is_draft ? [h('span', { className: 'draft-tag' }, 'Draft')] : []),
        h('span', { className: 'msg-from-name' }, o.showTo ? (m.to.length ? `To: ${m.to.map(senderName).join(', ')}` : '(no recipients)') : senderName(m.from)),
        ...(m.has_attachment ? [icon('clip', 'icon icon-sm msg-clip')] : [])),
      h('time', { className: 'msg-date', dateTime: m.received_at, title: fmtDate(m.received_at) }, fmtShort(m.received_at)),
      h('span', { className: 'msg-subject' }, m.subject || '(no subject)'),
      ...(m.preview ? [h('span', { className: 'msg-preview' }, m.preview)] : []),
      ...(m.labels?.length ? [h('span', { className: 'msg-labels' }, ...m.labels.map((l) => h('span', { className: 'label-chip' }, icon('tag', 'icon icon-xs'), l)))] : []))
    if (m.id === o.current) row.setAttribute('aria-current', 'true')
    if (o.onIntent) { const intent = () => o.onIntent!(m); row.addEventListener('pointerenter', intent, { once: true }); row.addEventListener('focus', intent, { once: true }); row.addEventListener('touchstart', intent, { once: true, passive: true }) }
    row.onclick = () => {
      list.querySelectorAll('.msg').forEach((b) => b.removeAttribute('aria-current'))
      row.setAttribute('aria-current', 'true')
      row.classList.remove('unread')
      row.querySelector('.iris-mark')?.remove()
      const wasUnread = !m.is_read
      m.is_read = true
      o.onOpen(m, i, wasUnread)
    }
    const li = h('li', { className: `msg-item${prevList && !seen.has(m.id) ? ' is-new' : ''}` }, row)
    li.dataset.id = m.id
    li.style.setProperty('--i', String(Math.min(i, 12)))
    const code = o.compact ? `${m.subject} ${m.preview}`.match(/\b(\d{4,8})\b/)?.[1] : undefined
    if (code) li.append(copyButton(code, `Copy code ${code}`, 'otp-chip', code))
    if (o.onStar) {
      const label = m.is_starred ? 'Unstar' : 'Star'
      li.append(h('button', { type: 'button', className: `star-btn${m.is_starred ? ' is-on' : ''}`, ariaLabel: `${label}: ${m.subject || '(no subject)'}`, title: label, 'aria-pressed': String(!!m.is_starred), onclick: () => o.onStar!(m) },
        icon(m.is_starred ? 'star-fill' : 'star', 'icon icon-sm')))
    }
    return li
  })), ...(o.footer ? [o.footer] : []))
}

/** One conversation in the list: all loaded messages sharing a threadId. */
type ThreadGroup = { id: string; subject: string; participants: string[]; count: number; latest: Summary; unread: boolean; messages: Summary[] }
type ThreadDetail = { thread_id: string; subject: string; messages: Detail[] }

/** Group loaded summaries into conversations, newest thread first.
 * Messages without a threadId are a thread of their own. Pure, so the
 * grouping rule is the same everywhere the list is drawn. With `showTo`
 * (Sent, Drafts) participants are the recipients, as in the flat list. */
function groupThreads(messages: Summary[], showTo = false): ThreadGroup[] {
  const byId = new Map<string, Summary[]>()
  for (const m of messages) {
    const key = m.thread_id ?? `msg:${m.id}`
    const g = byId.get(key)
    if (g) g.push(m)
    else byId.set(key, [m])
  }
  const groups: ThreadGroup[] = []
  for (const [id, ms] of byId) {
    const sorted = [...ms].sort((a, b) => b.received_at.localeCompare(a.received_at))
    const latest = sorted[0]
    groups.push({
      id,
      subject: sorted.find((m) => m.subject.trim())?.subject ?? '',
      participants: uniq(ms.flatMap((m) => showTo ? m.to.map(senderName) : [senderName(m.from)])),
      count: ms.length,
      latest,
      unread: ms.some((m) => !m.is_read),
      messages: ms,
    })
  }
  return groups.sort((a, b) => b.latest.received_at.localeCompare(a.latest.received_at))
}

/** Render one row per conversation. Expanding a row shows its messages
 * oldest first (the full thread when fetched, else the loaded page's
 * slice) using the same rows as the flat list. */
function fillThreadList(list: HTMLElement, groups: ThreadGroup[], o: {
  current: string
  empty: HTMLElement
  footer?: HTMLElement
  showTo?: boolean
  expanded: Set<string>
  threads: Map<string, ThreadDetail>
  onToggle: (g: ThreadGroup) => void
  onOpen: (m: Summary, index: number, wasUnread: boolean, total: number) => void
  onIntent?: (m: Summary) => void
  onStar?: (m: Summary) => void
}) {
  if (!groups.length) return list.replaceChildren(o.empty)
  list.replaceChildren(h('ul', { className: 'message-list thread-list', ariaLabel: 'Conversations' }, ...groups.map((g) => {
    const isOpen = o.expanded.has(g.id)
    const countLabel = g.count === 1 ? '1 message' : `${g.count} messages`
    const row = h('button', { type: 'button', className: `thread-row${g.unread ? ' unread' : ''}`, 'aria-expanded': String(isOpen), title: countLabel },
      h('span', { className: 'msg-from' },
        ...(g.unread ? [h('span', { className: 'iris-mark', title: 'Unread' }, h('span', { className: 'sr-only' }, 'Unread: '))] : []),
        h('span', { className: 'msg-from-name' }, g.participants.join(', ') || '(unknown sender)'),
        ...(g.count > 1 ? [h('span', { className: 'thread-count' }, String(g.count))] : [])),
      h('time', { className: 'msg-date', dateTime: g.latest.received_at, title: fmtDate(g.latest.received_at) }, fmtShort(g.latest.received_at)),
      h('span', { className: 'msg-subject' }, g.subject || '(no subject)'),
      ...(g.latest.preview ? [h('span', { className: 'msg-preview' }, g.latest.preview)] : []))
    row.onclick = () => o.onToggle(g)
    const head = h('div', { className: 'thread-head' }, row)
    if (o.onStar) {
      const latest = g.latest
      head.append(h('button', {
        type: 'button', className: `star-btn${latest.is_starred ? ' is-on' : ''}`,
        ariaLabel: `${latest.is_starred ? 'Unstar' : 'Star'}: ${g.subject || '(no subject)'}`, title: latest.is_starred ? 'Unstar' : 'Star',
        'aria-pressed': String(!!latest.is_starred), onclick: () => o.onStar!(latest),
      }, icon(latest.is_starred ? 'star-fill' : 'star', 'icon icon-sm')))
    }
    const li = h('li', { className: 'thread-item' }, head)
    if (isOpen) {
      const cached = o.threads.get(g.id)
      const msgs = [...(cached?.messages ?? g.messages)].sort((a, b) => a.received_at.localeCompare(b.received_at))
      const box = h('div', { className: 'thread-messages' })
      fillList(box, msgs, {
        current: o.current,
        showTo: o.showTo,
        empty: h('div'),
        onOpen: (m, i, wasUnread) => o.onOpen(m, i, wasUnread, msgs.length),
        ...(o.onIntent ? { onIntent: o.onIntent } : {}),
        ...(o.onStar ? { onStar: o.onStar } : {}),
      })
      li.append(box)
    }
    return li
  })), ...(o.footer ? [o.footer] : []))
}

function copyButton(text: string, label: string, cls: string, visible?: string) {
  const b = h('button', { type: 'button', className: cls, ariaLabel: label, title: label }, ...(visible ? [h('span', {}, visible)] : []), icon('copy', 'icon icon-sm')) as HTMLButtonElement
  b.onclick = async () => {
    const ok = await navigator.clipboard?.writeText(text).then(() => true, () => false)
    b.classList.toggle('is-done', !!ok)
    b.lastElementChild!.replaceWith(icon(ok ? 'check' : 'alert', 'icon icon-sm'))
    b.setAttribute('aria-label', ok ? 'Copied' : 'Copy failed')
    setTimeout(() => { b.classList.remove('is-done'); b.lastElementChild!.replaceWith(icon('copy', 'icon icon-sm')); b.setAttribute('aria-label', label) }, 2000)
  }
  return b
}

function searchBox(onSearch: (q: string) => void) {
  const input = h('input', { type: 'search', placeholder: 'Search messages', ariaLabel: 'Search messages', autocomplete: 'off' }) as HTMLInputElement
  input.addEventListener('input', () => onSearch(input.value.trim().toLowerCase()))
  return h('label', { className: 'topbar-search' }, icon('search'), input)
}
const matches = (m: Summary, q: string) => !q || `${m.from} ${m.subject} ${m.preview}`.toLowerCase().includes(q)

// ---------- Mailbox ----------
type FolderRole = 'inbox' | 'sent' | 'drafts' | 'archive' | 'trash'
type FolderInfo = { id: string; role: FolderRole | 'custom'; name: string; total: number; unread: number }
/** What a mailbox page lists: a standard folder, a custom folder, Starred, or one label. */
type MailView = { kind: 'role'; role: FolderRole } | { kind: 'folder'; id: string } | { kind: 'starred' } | { kind: 'label'; name: string }
type MoveTarget = { folder: FolderRole } | { folder_id: string }
type FilterRule = { id: string; field: 'from' | 'to' | 'subject'; value: string; action: 'move' | 'label' | 'star' | 'read'; target: string | null }
const FOLDERS: { role: FolderRole; label: string; icon: string; empty: [string, string] }[] = [
  { role: 'inbox', label: 'Inbox', icon: 'inbox', empty: ['Your inbox is clear', 'New mail for this address will appear here.'] },
  { role: 'sent', label: 'Sent', icon: 'send', empty: ['Nothing sent yet', 'Messages you send appear here.'] },
  { role: 'drafts', label: 'Drafts', icon: 'draft', empty: ['No drafts', 'Messages you start but don’t send are saved here.'] },
  { role: 'archive', label: 'Archive', icon: 'archive', empty: ['Archive is empty', 'Archive mail to clear your inbox without deleting it.'] },
  { role: 'trash', label: 'Trash', icon: 'trash', empty: ['Trash is empty', 'Deleted messages stay here until you delete them forever.'] },
]
const folderHash = (r: FolderRole) => (r === 'inbox' ? '#/' : `#/${r}`)
const folderMeta = (r: FolderRole) => FOLDERS.find((f) => f.role === r)!

/** Custom folders and labels, kept across navigation so the sidebar draws at once. */
let org: { folders: FolderInfo[]; labels: string[] } = { folders: [], labels: [] }
const customFolders = () => org.folders.filter((f) => f.role === 'custom')
const viewHash = (v: MailView) => v.kind === 'role' ? folderHash(v.role) : v.kind === 'starred' ? '#/starred'
  : v.kind === 'folder' ? `#/folder/${encodeURIComponent(v.id)}` : `#/label/${encodeURIComponent(v.name)}`
/** Key for the list cache and for marking the current nav item. */
const viewKey = (v: MailView) => v.kind === 'role' ? v.role : v.kind === 'starred' ? 'starred' : v.kind === 'folder' ? `folder:${v.id}` : `label:${v.name}`
const viewParams = (v: MailView): Record<string, string> => v.kind === 'role' ? { folder: v.role } : v.kind === 'starred' ? { view: 'starred' }
  : v.kind === 'folder' ? { folder_id: v.id } : { label: v.name }
function viewMeta(v: MailView): { label: string; icon: string; empty: [string, string] } {
  switch (v.kind) {
    case 'role': return folderMeta(v.role)
    case 'starred': return { label: 'Starred', icon: 'star', empty: ['No starred mail', 'Star a message to find it here quickly.'] }
    case 'folder': return { label: org.folders.find((f) => f.id === v.id)?.name ?? 'Folder', icon: 'folder', empty: ['This folder is empty', 'Move messages here to keep them together.'] }
    case 'label': return { label: v.name, icon: 'tag', empty: ['No messages with this label', 'Open a message and choose Labels to add one.'] }
  }
}

/** A mailbox linked to this login, as the list endpoint returns it. */
type LinkedMailbox = { id: string; email_address: string; is_primary: boolean; unlocked: boolean }

/** Signed-in account, kept while switching folders so each switch skips two requests. `is_admin` only decides whether the Admin nav link shows; the server gates the admin endpoints. `boxes` is every mailbox linked to this login, primary first; `mb` is the active one. */
let account: { email: string; mb: string; is_admin: boolean; boxes: LinkedMailbox[] } | null = null

const sendErrors: Record<string, string> = {
  external_sending_disabled: 'IRIS can only send to local addresses for now. Sending to other domains isn’t set up yet.',
  too_many_sends: 'You’ve reached the sending limit (100 per hour). Try again later.',
  invalid_recipient: 'One of the addresses isn’t valid.',
  no_recipients: 'Add at least one recipient.',
  too_many_recipients: 'Too many recipients (50 max).',
  invalid_attachment: 'One of the attachments isn’t valid — too large, too many, or a bad file name.',
  send_rejected: 'The mail server refused this message.',
  mail_server_error: 'The mail server returned an unexpected response. Try again shortly.',
  database_unavailable: 'IRIS is temporarily unavailable. Try again shortly.',
  network: errorText.network,
}

// ----- toast + confirm -----
function toast(text: string, action?: { label: string; run: () => void }, ms?: number) {
  document.querySelector('.toast')?.remove()
  const t = h('div', { className: 'toast', role: 'status' }, h('span', {}, text))
  if (action) t.append(h('button', { type: 'button', className: 'toast-action', onclick: () => { t.remove(); action.run() } }, action.label))
  document.body.append(t)
  setTimeout(() => t.remove(), ms ?? (action ? 7000 : 4000))
}

function confirmDialog(title: string, text: string, confirmLabel: string): Promise<boolean> {
  return new Promise((resolve) => {
    const ok = h('button', { type: 'button', className: 'btn btn-primary' }, h('span', { className: 'btn-label' }, confirmLabel))
    const cancel = h('button', { type: 'button', className: 'btn btn-secondary' }, h('span', { className: 'btn-label' }, 'Cancel'))
    const d = h('dialog', { className: 'confirm' }, h('h2', {}, title), h('p', { className: 'muted' }, text), h('div', { className: 'confirm-actions' }, cancel, ok)) as HTMLDialogElement
    const done = (v: boolean) => { d.close(); d.remove(); resolve(v) }
    ok.onclick = () => done(true)
    cancel.onclick = () => done(false)
    d.addEventListener('cancel', (e) => { e.preventDefault(); done(false) })
    document.body.append(d)
    d.showModal()
    cancel.focus()
  })
}

/** Text-input dialog. `submit` returns an error to show, or null to close. Resolves true when submitted. */
function promptDialog(o: { title: string; label: string; value?: string; confirm: string; hint?: string; submit: (v: string) => Promise<string | null> }): Promise<boolean> {
  return new Promise((resolve) => {
    const input = h('input', { id: 'prompt-input', type: 'text', value: o.value ?? '', autocomplete: 'off', spellcheck: false, 'aria-describedby': 'prompt-error' }) as HTMLInputElement
    const err = h('p', { className: 'field-error', id: 'prompt-error', role: 'alert' })
    const ok = h('button', { type: 'submit', className: 'btn btn-primary' }, h('span', { className: 'btn-label' }, o.confirm)) as HTMLButtonElement
    const cancel = h('button', { type: 'button', className: 'btn btn-secondary' }, h('span', { className: 'btn-label' }, 'Cancel'))
    const form = h('form', { className: 'prompt-form' },
      h('div', { className: 'field' }, h('label', { htmlFor: 'prompt-input' }, o.label), input, ...(o.hint ? [h('p', { className: 'prompt-hint muted' }, o.hint)] : []), err),
      h('div', { className: 'confirm-actions' }, cancel, ok))
    const d = h('dialog', { className: 'confirm' }, h('h2', {}, o.title), form) as HTMLDialogElement
    const done = (v: boolean) => { d.close(); d.remove(); resolve(v) }
    form.onsubmit = async (e) => {
      e.preventDefault()
      const restore = busy(ok, 'Saving…')
      const problem = await o.submit(input.value)
      if (!problem) return done(true)
      restore()
      err.textContent = problem
      input.setAttribute('aria-invalid', 'true')
      input.focus()
    }
    input.oninput = () => { err.textContent = ''; input.removeAttribute('aria-invalid') }
    cancel.onclick = () => done(false)
    d.addEventListener('cancel', (e) => { e.preventDefault(); done(false) })
    document.body.append(d)
    d.showModal()
    input.focus()
    input.select()
  })
}

/** List of one-click choices (e.g. Move to). */
function choiceDialog(title: string, items: { label: string; icon: string; run: () => void }[], extra?: { label: string; run: () => void }) {
  const d = h('dialog', { className: 'confirm' }) as HTMLDialogElement
  const close = () => { if (d.open) d.close() }
  const cancel = btn('btn btn-secondary', 'Cancel', undefined, { onclick: close })
  d.append(h('h2', {}, title),
    h('div', { className: 'choice-list' }, ...items.map((i) => h('button', { type: 'button', className: 'choice-item', onclick: () => { close(); i.run() } }, icon(i.icon, 'icon icon-sm'), h('span', {}, i.label)))),
    h('div', { className: 'confirm-actions' }, ...(extra ? [btn('btn btn-ghost', extra.label, 'plus', { onclick: () => { close(); extra.run() } })] : []), cancel))
  d.addEventListener('close', () => d.remove())
  document.body.append(d)
  d.showModal()
  ;(d.querySelector('.choice-item') as HTMLElement | null ?? cancel).focus()
}

// ----- composer -----
type ComposeAttachmentIn = { name: string; content_type: string; data: string; size: number }
type ComposeDraft = { draftId?: string; to?: string[]; cc?: string[]; bcc?: string[]; subject?: string; text?: string; html?: string; inReplyTo?: string | null; references?: string[]; title?: string; attachments?: ComposeAttachmentIn[] }
const MAX_COMPOSE_FILES = 5
const MAX_COMPOSE_FILE_BYTES = 5_000_000
const MAX_COMPOSE_TOTAL_BYTES = 10_000_000

/// ArrayBuffer to base64, chunked so a 5 MB file never blows the stack
/// (String.fromCharCode.apply on the whole buffer would).
function bufToBase64(buf: ArrayBuffer): string {
  const bytes = new Uint8Array(buf)
  let s = ''
  const CHUNK = 0x8000
  for (let i = 0; i < bytes.length; i += CHUNK) s += String.fromCharCode(...bytes.subarray(i, i + CHUNK))
  return btoa(s)
}
const emailOf = (a: string) => (a.match(/<([^>]+)>/)?.[1] ?? a).trim().toLowerCase()
const ADDRESS = /^[^\s@<>(),;:"[\]\\]+@[^\s@<>(),;:"[\]\\]+\.[^\s@<>(),;:"[\]\\]+$/
const splitAddrs = (v: string) => v.split(/[,;\s]+/).map((x) => x.trim()).filter(Boolean)
const uniq = (xs: string[]) => [...new Set(xs)]

function htmlToText(html: string) {
  // Parsing into an inert document never runs scripts; only text is read back.
  const doc = new DOMParser().parseFromString(html, 'text/html')
  doc.querySelectorAll('br').forEach((b) => b.replaceWith('\n'))
  doc.querySelectorAll('p, div, li, tr, h1, h2, h3').forEach((b) => b.append('\n'))
  return (doc.body.textContent ?? '').replace(/\n{3,}/g, '\n\n').trim()
}
const plainBody = (d: Detail) => (d.text_body ?? htmlToText(d.html_body ?? '')).trim()

/** Plain text as simple HTML: escaped, one `<br>` per line break. */
function textToHtml(text: string) {
  return text.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/\n/g, '<br>')
}

type Signature = { text: string; html: string }
const signatureCache = new Map<string, Signature>()
async function loadSignature(base: string): Promise<Signature> {
  const hit = signatureCache.get(base)
  if (hit) return hit
  try {
    const sig = await api<Signature>(`${base}/signature`)
    signatureCache.set(base, sig)
    return sig
  } catch { return { text: '', html: '' } }
}

const normWs = (s: string) => s.replace(/\s+/g, ' ').trim()
/** Insert a signature into a fresh body, once: after the leading blank
 * lines (the typing area) when there are any, else at the end behind a
 * standard `-- ` divider. A body that already contains the signature
 * (typed by hand, or reopened from a draft) is left alone. */
function withSignature(text: string, sig: string): string {
  const s = sig.trim()
  if (!s || normWs(text).includes(normWs(s))) return text
  const block = `-- \n${s}`
  if (!text.trim()) return `\n\n${block}`
  const lines = text.split('\n')
  let lead = 0
  while (lead < lines.length && lines[lead].trim() === '') lead++
  if (lead === 0) return `${text}\n\n${block}`
  return `${lines.slice(0, lead).join('\n')}\n${block}\n\n${lines.slice(lead).join('\n')}`
}

/** A small dependency-free rich-text editor: a contenteditable area with
 * a formatting toolbar (bold, italic, underline, link, lists).
 * Formatting runs through document.execCommand, which every browser
 * still supports for these commands; the server sanitizes the HTML
 * before it is stored or sent. Pastes land as plain text, so formatted
 * content from elsewhere cannot smuggle in styles or tracking pixels. */
function makeRichEditor(initialHtml: string, onInput: () => void) {
  const ed = h('div', { className: 'compose-rich', contentEditable: 'true', ariaLabel: 'Message', spellcheck: true }) as HTMLDivElement
  ed.innerHTML = initialHtml
  ed.addEventListener('input', onInput)
  ed.addEventListener('paste', (e) => {
    e.preventDefault()
    document.execCommand('insertText', false, e.clipboardData?.getData('text/plain') ?? '')
  })
  const tool = (label: string, cmd: string, arg?: string) => {
    const b = h('button', { type: 'button', className: 'btn btn-ghost btn-sm rich-tool' }, label) as HTMLButtonElement
    b.onclick = (e) => { e.preventDefault(); ed.focus(); document.execCommand(cmd, false, arg); onInput() }
    return b
  }
  const link = h('button', { type: 'button', className: 'btn btn-ghost btn-sm rich-tool' }, 'Link') as HTMLButtonElement
  link.onclick = async () => {
    ed.focus()
    await promptDialog({
      title: 'Add link', label: 'Link address', confirm: 'Add link', hint: 'A web address or email address.',
      submit: async (v) => {
        const url = v.trim()
        if (!/^(https?:\/\/|mailto:)/i.test(url)) return 'Use a web address starting with http:// or https://, or a mailto: address.'
        document.execCommand('createLink', false, url)
        onInput()
        return null
      },
    })
  }
  const toolbar = h('div', { className: 'rich-toolbar', role: 'toolbar', ariaLabel: 'Formatting' },
    tool('Bold', 'bold'), tool('Italic', 'italic'), tool('Underline', 'underline'), link,
    tool('Bullets', 'insertUnorderedList'), tool('Numbered', 'insertOrderedList'))
  const wrap = h('div', { className: 'rich-wrap' }, toolbar, ed)
  return { wrap, el: ed, getHtml: () => ed.innerHTML, setHtml: (html: string) => { ed.innerHTML = html } }
}

function replyDraft(d: Detail, kind: 'reply' | 'all' | 'forward', me: string): ComposeDraft {
  const subject = d.subject || ''
  if (kind === 'forward') {
    return {
      title: 'Forward', subject: /^fwd?:/i.test(subject) ? subject : `Fwd: ${subject}`,
      text: `\n\n---------- Forwarded message ----------\nFrom: ${d.from}\nDate: ${fmtDate(d.received_at)}\nSubject: ${subject}\nTo: ${d.to.join(', ')}\n\n${plainBody(d)}`,
    }
  }
  const fromMe = emailOf(d.from) === me
  let to = fromMe ? d.to.map(emailOf) : [emailOf(d.from)]
  let cc: string[] = []
  if (kind === 'all') {
    to = uniq([...to, ...d.to.map(emailOf)]).filter((a) => a !== me)
    cc = uniq((d.cc ?? []).map(emailOf)).filter((a) => a !== me && !to.includes(a))
  }
  const quoted = plainBody(d).split('\n').map((l) => `> ${l}`).join('\n')
  return {
    title: kind === 'all' ? 'Reply all' : 'Reply', to, cc,
    subject: /^re:/i.test(subject) ? subject : `Re: ${subject}`,
    text: `\n\nOn ${fmtDate(d.received_at)}, ${d.from} wrote:\n${quoted}`,
    inReplyTo: d.message_id, references: [...(d.references ?? []), ...(d.message_id ? [d.message_id] : [])],
  }
}

let closeComposer: (() => Promise<void>) | null = null

/** Contact autocomplete for one recipient field. Typing suggests matching
 * contacts below the field; Arrow keys move, Enter or Tab picks, Escape
 * closes. Picking replaces the address being typed, never the whole field. */
function attachContactSuggest(f: { el: HTMLInputElement; row: HTMLDivElement }, onPick: () => void) {
  // The browser's own email completion would fight this dropdown.
  f.el.setAttribute('autocomplete', 'off')
  const box = h('div', { className: 'compose-suggest', role: 'listbox', hidden: true })
  f.row.append(box)
  let items: Contact[] = []
  let active = -1
  let seq = 0
  let timer = 0
  const close = () => { box.hidden = true; items = []; active = -1 }
  // The address being typed: the comma/semicolon-separated token around the caret.
  const token = () => {
    const v = f.el.value
    const pos = f.el.selectionStart ?? v.length
    const before = v.slice(0, pos)
    const raw = Math.max(before.lastIndexOf(','), before.lastIndexOf(';')) + 1
    const leading = v.slice(raw, pos).match(/^\s*/)?.[0].length ?? 0
    const start = raw + leading
    const after = v.slice(pos)
    const next = after.search(/[,;]/)
    const end = next === -1 ? v.length : pos + next
    return { text: v.slice(start, end).trim(), start, end }
  }
  const render = () => {
    box.replaceChildren(...items.map((c, i) => {
      const b = h('button', { type: 'button', className: 'suggest-item', role: 'option', 'aria-selected': String(i === active) },
        h('span', { className: 'suggest-name' }, c.name),
        h('span', { className: 'suggest-email' }, c.email))
      // mousedown (not click) so the input never blurs before the pick lands.
      b.addEventListener('mousedown', (e) => { e.preventDefault(); pick(i) })
      return b
    }))
    box.hidden = !items.length
  }
  const pick = (i: number) => {
    const c = items[i]
    if (!c) return
    const { start, end } = token()
    const v = f.el.value
    const rest = v.slice(end).replace(/^[,;\s]*/, '')
    f.el.value = v.slice(0, start) + c.email + ', ' + rest
    const caret = start + c.email.length + 2
    f.el.setSelectionRange(caret, caret)
    close()
    f.el.focus()
    onPick()
  }
  f.el.addEventListener('input', () => {
    clearTimeout(timer)
    if (token().text.length < 2) { close(); return }
    const my = ++seq
    timer = window.setTimeout(async () => {
      try {
        const { contacts } = await api<{ contacts: Contact[] }>(`/api/v1/contacts?q=${encodeURIComponent(token().text)}&limit=8`)
        if (my !== seq) return
        items = contacts
        active = -1
        render()
      } catch { if (my === seq) close() }
    }, 200)
  })
  f.el.addEventListener('keydown', (e) => {
    if (box.hidden) return
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault()
      active = e.key === 'ArrowDown' ? (active + 1) % items.length : (active - 1 + items.length) % items.length
      render()
    } else if (e.key === 'Enter' || e.key === 'Tab') {
      if (active >= 0) { e.preventDefault(); pick(active) }
    } else if (e.key === 'Escape') {
      // Stop here so the composer's own Escape handler does not close the panel.
      e.preventDefault(); e.stopPropagation(); close()
    }
  })
  f.el.addEventListener('blur', close)
}

async function openComposer(base: string, d: ComposeDraft, onChange: () => void, onDiscard?: (id: string) => void) {
  if (closeComposer) await closeComposer()
  let draftId = d.draftId
  let dirty = false
  let saving: Promise<void> | null = null
  let timer = 0
  let closed = false

  // The mailbox signature is appended once, at open, behind the typing
  // area. The fetch races a short timeout so a slow server never blocks
  // opening the composer; the cache makes later opens instant.
  const sig = await Promise.race([
    loadSignature(base),
    new Promise<Signature>((r) => setTimeout(() => r({ text: '', html: '' }), 1500)),
  ])
  // Plain text stays the default: HTML mode opens only for a draft that
  // already holds HTML. Switching modes converts the body in place.
  let htmlMode = !!d.html
  const rawText = d.text ?? ''
  const initialText = withSignature(rawText, sig.text)
  const sigInserted = initialText !== rawText
  let initialHtml = d.html ?? textToHtml(initialText)
  if (sigInserted) initialHtml = initialHtml.replace('-- <br>', '<hr>')

  const input = (id: string, label: string, value: string, type = 'text') => {
    const el = h('input', { id, type, value, autocomplete: type === 'email' ? 'email' : 'off', spellcheck: type !== 'email' }) as HTMLInputElement
    if (type === 'email') { el.type = 'text'; el.inputMode = 'email'; el.setAttribute('autocapitalize', 'off') }
    const err = h('p', { className: 'field-error', id: `${id}-error` })
    el.setAttribute('aria-describedby', err.id)
    const row = h('div', { className: 'compose-row' }, h('label', { htmlFor: id }, label), el)
    return { el, row, err, set: (m: string) => { err.textContent = m; m ? el.setAttribute('aria-invalid', 'true') : el.removeAttribute('aria-invalid') } }
  }
  const to = input('c-to', 'To', (d.to ?? []).join(', '), 'email')
  const cc = input('c-cc', 'Cc', (d.cc ?? []).join(', '), 'email')
  const bcc = input('c-bcc', 'Bcc', (d.bcc ?? []).join(', '), 'email')
  const subject = input('c-subject', 'Subject', d.subject ?? '')
  const body = h('textarea', { className: 'compose-body', ariaLabel: 'Message', value: initialText, placeholder: 'Write your message…' }) as HTMLTextAreaElement
  const rich = makeRichEditor(initialHtml, () => changed())
  const plainBtn = h('button', { type: 'button', className: 'btn btn-ghost btn-sm', 'aria-pressed': String(!htmlMode) }, 'Plain') as HTMLButtonElement
  const richBtn = h('button', { type: 'button', className: 'btn btn-ghost btn-sm', 'aria-pressed': String(htmlMode) }, 'Rich') as HTMLButtonElement
  const setMode = (richMode: boolean) => {
    if (richMode === htmlMode) return
    htmlMode = richMode
    if (richMode) {
      let html = textToHtml(body.value)
      if (sig.text.trim() && normWs(body.value).includes(normWs(sig.text))) html = html.replace('-- <br>', '<hr>')
      rich.setHtml(html)
    } else {
      body.value = htmlToText(rich.getHtml())
    }
    body.hidden = richMode
    rich.wrap.hidden = !richMode
    plainBtn.setAttribute('aria-pressed', String(!richMode))
    richBtn.setAttribute('aria-pressed', String(richMode))
    changed()
  }
  plainBtn.onclick = () => setMode(false)
  richBtn.onclick = () => setMode(true)
  const modeRow = h('div', { className: 'compose-mode', role: 'group', ariaLabel: 'Message format' }, plainBtn, richBtn)
  body.hidden = htmlMode
  rich.wrap.hidden = !htmlMode
  const showCc = !!(d.cc?.length || d.bcc?.length)
  cc.row.hidden = bcc.row.hidden = !showCc
  const ccToggle = h('button', { type: 'button', className: 'linklike cc-toggle', hidden: showCc }, 'Cc / Bcc')
  ccToggle.onclick = () => { cc.row.hidden = bcc.row.hidden = false; ccToggle.hidden = true; cc.el.focus() }
  to.row.append(ccToggle)

  // Attachments: kept in memory as base64 (same shape the API validates),
  // listed with name + size and a remove button. The file input is hidden
  // inside its label so the whole button opens the picker.
  let atts: ComposeAttachmentIn[] = [...(d.attachments ?? [])]
  const attachList = h('div', { className: 'compose-attach-list' })
  const fileInput = h('input', { type: 'file', multiple: true, ariaLabel: 'Choose files to attach' }) as HTMLInputElement
  const attachBtn = h('label', { className: 'btn btn-secondary btn-sm' }, 'Attach files', fileInput)
  const attachBox = h('div', { className: 'compose-attach' }, attachBtn, attachList)
  const renderAtts = () => {
    attachList.replaceChildren(...atts.map((a, i) => h('div', { className: 'attach-row' },
      h('div', { className: 'attach-meta' }, h('strong', {}, a.name), h('span', {}, `${a.content_type} · ${fmtBytes(a.size)}`)),
      h('div', { className: 'attach-actions' },
        btn('btn btn-ghost btn-sm', 'Remove', undefined, { onclick: () => { atts.splice(i, 1); renderAtts(); changed() } })))))
  }
  renderAtts()
  fileInput.onchange = async () => {
    const files = [...(fileInput.files ?? [])]
    fileInput.value = ''
    if (!files.length) return
    error.replaceChildren()
    let used = atts.reduce((n, a) => n + a.size, 0)
    for (const f of files) {
      if (atts.length >= MAX_COMPOSE_FILES) {
        error.replaceChildren(icon('alert'), h('span', {}, `At most ${MAX_COMPOSE_FILES} attachments.`))
        break
      }
      if (f.size > MAX_COMPOSE_FILE_BYTES) {
        error.replaceChildren(icon('alert'), h('span', {}, `“${f.name}” is over the 5 MB limit.`))
        continue
      }
      if (used + f.size > MAX_COMPOSE_TOTAL_BYTES) {
        error.replaceChildren(icon('alert'), h('span', {}, 'Attachments total over the 10 MB limit.'))
        continue
      }
      const data = await new Promise<string>((resolve) => {
        const r = new FileReader()
        r.onload = () => resolve(String(r.result).split(',')[1] ?? '')
        r.onerror = () => resolve('')
        r.readAsDataURL(f)
      })
      if (!data) {
        error.replaceChildren(icon('alert'), h('span', {}, `Could not read “${f.name}”.`))
        continue
      }
      atts.push({ name: f.name, content_type: f.type || 'application/octet-stream', data, size: f.size })
      used += f.size
    }
    renderAtts()
    changed()
  }

  const status = h('span', { className: 'compose-status', 'aria-live': 'polite' })
  const error = h('div', { className: 'form-error', role: 'alert' })
  // With several mailboxes linked, say which one this sends from. With
  // one mailbox the composer reads exactly as before.
  const fromLine = (account?.boxes.length ?? 0) > 1
    ? h('p', { className: 'compose-from' }, `From: ${activeBox()?.email_address ?? account?.email ?? ''}`)
    : null
  const send = h('button', { type: 'button', className: 'btn btn-primary' }, icon('send'), h('span', { className: 'btn-label' }, 'Send')) as HTMLButtonElement
  const discard = btn('btn btn-ghost btn-icon-sm', 'Discard draft', 'trash')
  const closeBtn = btn('btn btn-ghost btn-icon-sm', 'Close (saves draft)', 'close')
  const panel = h('section', { className: 'composer', role: 'dialog', ariaLabel: d.title ?? 'New message' },
    h('header', { className: 'composer-head' }, h('h2', {}, d.title ?? 'New message'), status, closeBtn),
    h('div', { className: 'composer-fields' }, ...(fromLine ? [fromLine] : []), to.row, to.err, cc.row, cc.err, bcc.row, bcc.err, subject.row),
    attachBox,
    modeRow,
    rich.wrap,
    body,
    error,
    h('footer', { className: 'composer-foot' }, send, h('span', { className: 'compose-hint' }, 'Ctrl + Enter to send'), discard))

  /** The body text in the current mode: the editor HTML rendered as
   * plain text, or the textarea value. */
  const currentText = () => (htmlMode ? htmlToText(rich.getHtml()) : body.value)
  const payload = () => {
    const p = {
      to: splitAddrs(to.el.value), cc: splitAddrs(cc.el.value), bcc: splitAddrs(bcc.el.value),
      subject: subject.el.value.replace(/[\r\n]+/g, ' '), text: currentText(),
      in_reply_to: d.inReplyTo ?? null, references: d.references ?? [], draft_id: draftId ?? null,
      attachments: atts.map(({ name, content_type, data }) => ({ name, content_type, data })),
    }
    return htmlMode ? { ...p, html: rich.getHtml() } : p
  }
  const isEmpty = () => !to.el.value.trim() && !cc.el.value.trim() && !bcc.el.value.trim() && !subject.el.value.trim() && !currentText().trim() && !atts.length

  const save = async () => {
    clearTimeout(timer)
    if (saving) await saving
    if (!dirty || closed && isEmpty()) return
    // Drafts may hold half-typed addresses; keep only valid ones so the save never fails.
    const p = payload()
    for (const k of ['to', 'cc', 'bcc'] as const) p[k] = p[k].filter((a) => ADDRESS.test(a))
    dirty = false
    status.textContent = 'Saving…'
    saving = api<{ id: string }>(`${base}/drafts`, { method: 'POST', body: JSON.stringify(p) })
      .then((r) => { draftId = r.id; status.textContent = `Saved ${fmtTime(new Date())}`; onChange() })
      .catch(() => { dirty = true; status.textContent = 'Not saved' })
      .finally(() => { saving = null })
    await saving
  }
  const changed = () => { dirty = true; status.textContent = ''; clearTimeout(timer); timer = window.setTimeout(save, 2500) }
  ;[to.el, cc.el, bcc.el, subject.el, body].forEach((el) => el.addEventListener('input', changed))
  ;[to, cc, bcc].forEach((f) => f.el.addEventListener('input', () => f.set('')))
  ;[to, cc, bcc].forEach((f) => attachContactSuggest(f, changed))

  const close = async () => {
    if (closed) return
    closed = true
    closeComposer = null
    document.removeEventListener('keydown', onKey)
    panel.classList.add('is-closing')
    if (dirty && !isEmpty()) { await save(); toast('Draft saved') }
    setTimeout(() => panel.remove(), 160)
  }
  closeComposer = close
  closeBtn.onclick = () => close()

  discard.onclick = async () => {
    clearTimeout(timer)
    if (saving) await saving
    const id = draftId
    closed = true; closeComposer = null
    document.removeEventListener('keydown', onKey)
    panel.remove()
    if (id) { await api(`${base}/messages/${encodeURIComponent(id)}`, { method: 'DELETE' }).catch(() => {}); onDiscard?.(id); onChange() }
    toast('Draft discarded')
  }

  send.onclick = async () => {
    error.replaceChildren()
    const p = payload()
    let bad = false
    for (const [f, list] of [[to, p.to], [cc, p.cc], [bcc, p.bcc]] as const) {
      const wrong = list.find((a) => !ADDRESS.test(a))
      f.set(wrong ? `“${wrong}” isn’t a valid email address.` : '')
      bad ||= !!wrong
    }
    if (!bad && !p.to.length && !p.cc.length && !p.bcc.length) { to.set('Add at least one recipient.'); bad = true }
    if (bad) { panel.querySelector<HTMLInputElement>('[aria-invalid="true"]')?.focus(); return }
    clearTimeout(timer)
    if (saving) await saving
    const restore = busy(send, 'Sending…')
    try {
      const r = await api<{ pending: boolean; id: string; send_at: string }>(`${base}/send`, { method: 'POST', body: JSON.stringify({ ...p, draft_id: draftId ?? null }) })
      closed = true; closeComposer = null
      document.removeEventListener('keydown', onKey)
      panel.classList.add('is-closing')
      setTimeout(() => panel.remove(), 160)
      // The send waits out a 10 second undo window on the server; the
      // toast lives exactly that long. Undo cancels the queued send, and
      // the draft stays in Drafts. Past the window the message is already
      // on its way, so Undo reports that instead.
      toast('Sending message…', { label: 'Undo', run: async () => {
        try {
          await api(`${base}/pending-sends/${encodeURIComponent(r.id)}`, { method: 'DELETE' })
          toast('Send cancelled')
        } catch { toast('Already sent') }
      } }, 10000)
      onChange()
    } catch (err) {
      restore()
      const code = (err as HttpError).code
      error.replaceChildren(icon('alert'), h('span', {}, sendErrors[code] ?? 'Could not send. Your draft is kept.'))
      if ((err as HttpError).status === 401) { account = null; renderLogin() }
    }
  }

  const onKey = (e: KeyboardEvent) => {
    if (e.key === 'Escape' && !document.querySelector('dialog[open]')) { e.preventDefault(); close() }
    if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) { e.preventDefault(); send.click() }
  }
  document.addEventListener('keydown', onKey)
  document.body.append(panel)
  const focusEl = d.to?.length ? (d.subject ? (htmlMode ? rich.el : body) : subject.el) : to.el
  focusEl.focus()
  // The typing area is the blank lines at the top (a reply, or a fresh
  // message with a signature below it): start the caret there. Setting
  // the selection now persists until the user clicks or tabs in.
  if (initialText.startsWith('\n')) {
    if (htmlMode) {
      const range = document.createRange()
      range.selectNodeContents(rich.el)
      range.collapse(true)
      const sel = window.getSelection()
      sel?.removeAllRanges()
      sel?.addRange(range)
    } else {
      body.setSelectionRange(0, 0)
    }
  }
}

// ----- mailbox view -----
async function ensureAccount() {
  if (!account) {
    const [session, { mailboxes }] = await Promise.all([
      api<{ email: string; is_admin?: boolean }>('/api/v1/session'),
      api<{ mailboxes: LinkedMailbox[] }>('/api/v1/mailboxes'),
    ])
    account = { email: session.email, mb: mailboxes[0]?.id ?? '', is_admin: session.is_admin === true, boxes: mailboxes }
  }
  return account
}

/** Re-reads the linked mailboxes from the server (after link/unlink/unlock). If the active mailbox is gone, falls back to the first one. */
async function refreshBoxes() {
  if (!account) return
  const { mailboxes } = await api<{ mailboxes: LinkedMailbox[] }>('/api/v1/mailboxes')
  account.boxes = mailboxes
  if (!mailboxes.some((b) => b.id === account!.mb)) account.mb = mailboxes[0]?.id ?? ''
}

const activeBox = () => account?.boxes.find((b) => b.id === account!.mb)

/** Makes `id` the active mailbox everywhere: closes the live stream and the composer, drops the cached lists and folders, and re-renders the current view. */
async function switchMailbox(id: string) {
  if (!account || account.mb === id) return
  account.mb = id
  await reswitch()
}

/** Drops the live stream, the composer and the cached lists, then re-renders the current view for the mailbox `account.mb` already points at. */
async function reswitch() {
  mailStream?.close(); mailStream = null
  clearMailCache()
  org = { folders: [], labels: [] }
  await closeComposer?.()
  route()
}

const mailboxErrors: Record<string, string> = {
  invalid_credentials: 'The email or password is wrong. Check them and try again.',
  mailbox_already_linked: 'That mailbox is already linked to an IRIS account.',
  invalid_request: 'Enter a valid email address and a password.',
  already_unlocked: 'That mailbox is already unlocked.',
  cannot_unlink_primary: 'The primary mailbox cannot be unlinked.',
  not_found: 'It no longer exists. Reload the page and try again.',
  mail_server_error: errorText.mail_server_error,
  mail_server_unavailable: errorText.mail_server_unavailable,
  network: errorText.network,
}
const mailboxError = (err: unknown) => mailboxErrors[(err as HttpError).code] ?? 'Something went wrong. Try again.'

/** Mailbox switcher dialog: switch, unlock, unlink, and add a mailbox. A locked mailbox asks for its password here; a wrong password shows inline and never signs the user out. */
async function manageMailboxes() {
  if (!account) return
  await refreshBoxes()
  const list = h('div', { className: 'mailbox-list' })
  const err = h('p', { className: 'field-error', role: 'alert' })
  const emailInput = h('input', { type: 'email', autocomplete: 'username', spellcheck: false, placeholder: 'name@example.com', 'aria-label': 'Mailbox email' }) as HTMLInputElement
  const passInput = h('input', { type: 'password', autocomplete: 'current-password', placeholder: 'Mailbox password', 'aria-label': 'Mailbox password' }) as HTMLInputElement
  const addBtn = h('button', { type: 'button', className: 'btn btn-primary' }, h('span', { className: 'btn-label' }, 'Add mailbox')) as HTMLButtonElement
  const closeBtn = h('button', { type: 'button', className: 'btn btn-secondary' }, h('span', { className: 'btn-label' }, 'Close'))
  const d = h('dialog', { className: 'confirm mailbox-dialog' },
    h('h2', {}, 'Mailboxes'),
    h('p', { className: 'muted' }, 'Switch between your linked mailboxes, or add another one.'),
    list,
    h('div', { className: 'mailbox-add' },
      h('div', { className: 'field' }, h('label', {}, 'Add a mailbox'), emailInput, passInput, err),
      h('div', { className: 'confirm-actions' }, addBtn)),
    h('div', { className: 'confirm-actions' }, closeBtn)) as HTMLDialogElement
  const done = () => { d.close(); d.remove() }
  closeBtn.onclick = done
  d.addEventListener('cancel', (e) => { e.preventDefault(); done() })
  const draw = () => {
    const boxes = account!.boxes
    list.replaceChildren(...boxes.map((b) => {
      const active = b.id === account!.mb
      const tagText = active ? 'Active' : b.is_primary ? 'Primary' : b.unlocked ? '' : 'Locked'
      const row = h('div', { className: 'mailbox-row' },
        icon('mail', 'icon'), h('span', { className: 'mailbox-email', title: b.email_address }, b.email_address),
        ...(tagText ? [h('span', { className: 'mailbox-tag' }, tagText)] : []))
      const unlockErr = h('p', { className: 'field-error', role: 'alert' })
      if (!b.unlocked) {
        const pw = h('input', { type: 'password', autocomplete: 'current-password', placeholder: 'Password for this mailbox', 'aria-label': `Password for ${b.email_address}` }) as HTMLInputElement
        const unlockBtn = h('button', { type: 'button', className: 'btn btn-secondary btn-sm' }, h('span', { className: 'btn-label' }, 'Unlock')) as HTMLButtonElement
        unlockBtn.onclick = async () => {
          const restore = busy(unlockBtn, 'Unlocking…')
          try {
            await api(`/api/v1/mailboxes/${encodeURIComponent(b.id)}/unlock`, { method: 'POST', body: JSON.stringify({ password: pw.value }) })
            await refreshBoxes(); draw()
          } catch (e) { restore(); unlockErr.textContent = mailboxError(e); pw.setAttribute('aria-invalid', 'true'); pw.focus() }
        }
        pw.oninput = () => { unlockErr.textContent = ''; pw.removeAttribute('aria-invalid') }
        row.append(h('div', { className: 'mailbox-unlock' }, pw, unlockBtn, unlockErr))
      } else if (!active) {
        const switchBtn = h('button', { type: 'button', className: 'btn btn-secondary btn-sm' }, h('span', { className: 'btn-label' }, 'Switch')) as HTMLButtonElement
        switchBtn.onclick = async () => { done(); await switchMailbox(b.id) }
        row.append(switchBtn)
      }
      if (!b.is_primary) {
        const unlinkBtn = h('button', { type: 'button', className: 'btn btn-ghost btn-sm' }, h('span', { className: 'btn-label' }, 'Unlink')) as HTMLButtonElement
        unlinkBtn.onclick = async () => {
          const ok = await confirmDialog(`Unlink ${b.email_address}?`, 'Only the IRIS-side data for this mailbox is removed. Mail on the server stays untouched.', 'Unlink')
          if (!ok) return
          try {
            await api(`/api/v1/mailboxes/${encodeURIComponent(b.id)}/unlink`, { method: 'POST' })
            const wasActive = b.id === account!.mb
            await refreshBoxes()
            // refreshBoxes already moved `mb` to the first box; the
            // stream, composer and caches still point at the old one.
            if (wasActive) { done(); await reswitch(); return }
            draw()
          } catch (e) { err.textContent = mailboxError(e) }
        }
        row.append(unlinkBtn)
      }
      return row
    }))
  }
  addBtn.onclick = async () => {
    const restore = busy(addBtn, 'Adding…')
    try {
      await api('/api/v1/mailboxes/link', { method: 'POST', body: JSON.stringify({ email: emailInput.value, password: passInput.value }) })
      restore()
      await refreshBoxes(); draw()
      emailInput.value = ''; passInput.value = ''; err.textContent = ''
      toast('Mailbox added')
    } catch (e) { restore(); err.textContent = mailboxError(e); emailInput.setAttribute('aria-invalid', 'true'); emailInput.focus() }
  }
  draw()
  document.body.append(d)
  d.showModal()
}

/** Refreshes `org` (folders with counts + labels). Returns the folder list. */
async function loadOrg(base: string) {
  const [{ folders }, { labels }] = await Promise.all([
    api<{ folders: FolderInfo[] }>(`${base}/folders`),
    api<{ labels: { name: string }[] }>(`${base}/labels`),
  ])
  org = { folders, labels: labels.map((l) => l.name) }
  return folders
}

/** Sidebar links (and mobile tabs) for Starred, custom folders, labels and Organize. */
function orgNav(current: string) {
  const link = (href: string, ic: string, label: string, key: string, count?: number) => {
    const a = navItem(href, ic, label, key === current, count ? h('span', { className: 'nav-count' }, String(count)) : undefined)
    a.classList.add('nav-folder')
    return a
  }
  const tab = (href: string, label: string, key: string) =>
    h('a', { href, className: 'folder-tab', ...(key === current ? { 'aria-current': 'page' } : {}) }, label)
  const sidebar = h('div', { className: 'nav-group' })
  const tabs = h('div', { className: 'nav-group' })
  let drawn = ''
  const draw = () => {
    const custom = customFolders()
    const sig = JSON.stringify([custom.map((f) => [f.id, f.name, f.unread]), org.labels])
    if (sig === drawn) return
    drawn = sig
    sidebar.replaceChildren(
      ...(custom.length ? [navSection('Folders')] : []),
      ...custom.map((f) => link(viewHash({ kind: 'folder', id: f.id }), 'folder', f.name, `folder:${f.id}`, f.unread)),
      ...(org.labels.length ? [navSection('Labels')] : []),
      ...org.labels.map((l) => link(viewHash({ kind: 'label', name: l }), 'tag', l, `label:${l}`)),
      link('#/organize', 'filter', 'Organize', 'organize'))
    tabs.replaceChildren(
      tab('#/starred', 'Starred', 'starred'),
      ...custom.map((f) => tab(viewHash({ kind: 'folder', id: f.id }), f.name, `folder:${f.id}`)),
      ...org.labels.map((l) => tab(viewHash({ kind: 'label', name: l }), `# ${l}`, `label:${l}`)),
      tab('#/organize', 'Organize', 'organize'))
  }
  draw()
  return { starred: link('#/starred', 'star', 'Starred', 'starred'), sidebar, tabs, draw }
}

const orgErrors: Record<string, string> = {
  invalid_label: 'Use 1–32 lowercase letters, numbers, “-” or “_”.',
  too_many_labels: 'You’ve reached the limit of 50 labels.',
  invalid_folder_name: 'Use 1–64 characters, no “/”, and not a built-in folder name.',
  folder_exists: 'A folder with this name already exists.',
  too_many_folders: 'You’ve reached the limit of 50 folders.',
  folder_not_empty: 'This folder still has messages. Move or delete them first.',
  folder_in_use_by_filter: 'A filter moves mail into this folder. Delete that filter first.',
  label_in_use_by_filter: 'A filter adds this label. Delete that filter first.',
  invalid_filter: 'Choose what to match and enter the text to look for.',
  too_many_filters: 'You’ve reached the limit of 50 filters.',
  unknown_label: 'That label no longer exists. Reload the page and try again.',
  invalid_folder: 'That folder can’t be used here.',
  invalid_contact_name: 'Use a name of 1–100 characters.',
  invalid_contact_email: 'Enter a valid email address.',
  contact_exists: 'A contact with this email already exists.',
  too_many_contacts: 'You’ve reached the limit of 500 contacts.',
  not_found: 'It no longer exists. Reload the page and try again.',
  mail_server_unavailable: errorText.mail_server_unavailable,
  mail_server_error: errorText.mail_server_error,
  network: errorText.network,
}
const orgError = (err: unknown) => orgErrors[(err as HttpError).code] ?? 'Something went wrong. Try again.'
const labelName = (v: string) => v.trim().toLowerCase().replace(/\s+/g, '-')

const newLabelDialog = (base: string) => promptDialog({
  title: 'New label', label: 'Label name', confirm: 'Create label', hint: 'Lowercase letters, numbers, “-” and “_”. Up to 32 characters.',
  submit: async (v) => {
    try {
      const { label } = await api<{ label: { name: string } }>(`${base}/labels`, { method: 'POST', body: JSON.stringify({ name: labelName(v) }) })
      org.labels = uniq([...org.labels, label.name]).sort()
      return null
    } catch (err) { return orgError(err) }
  },
})
const newFolderDialog = (base: string) => promptDialog({
  title: 'New folder', label: 'Folder name', confirm: 'Create folder',
  submit: async (v) => {
    try {
      const { folder } = await api<{ folder: FolderInfo }>(`${base}/folders`, { method: 'POST', body: JSON.stringify({ name: v }) })
      org.folders = [...org.folders, folder]
      return null
    } catch (err) { return orgError(err) }
  },
})

async function renderMailbox(view: MailView = { kind: 'role', role: 'inbox' }) {
  const { email, mb, is_admin } = await ensureAccount()
  // The chip shows the active mailbox, and opens the switcher. With one
  // mailbox linked it reads exactly as before.
  const activeEmail = activeBox()?.email_address ?? email
  const folder = view.kind === 'role' ? view.role : null
  const key = viewKey(view)
  let meta = viewMeta(view)
  const base = `/api/v1/mailboxes/${mb}`
  setPage({ title: meta.label, description: `Your IRIS ${meta.label.toLowerCase()}.`, path: '/', noindex: true })

  let all: Summary[] = (mb && readListCache(email, mb, key)) || []
  let query = ''
  let current = ''
  /** Server-side total for the current view/search; `all` is only page 1 until load-more appends. */
  let total = 0
  /** The query `all` was last fetched with. While it differs from `query`, draw() falls back to the local matches() filter (cached list / search in flight). */
  let loadedQuery = ''
  let loadingMore = false
  let searchTimer: number | undefined
  /** True while the live stream is connected: the 30 s poll stands down. */
  let streamLive = false
  /** New arrivals the stream has reported since the last list load. */
  let pendingNew = 0

  const signOut = () => { mailStream?.close(); mailStream = null; clearMailCache(); account = null; org = { folders: [], labels: [] }; closeComposer?.() }
  const logout = btn('btn btn-ghost btn-icon-sm', 'Sign out', 'logout')
  logout.onclick = async () => { busy(logout, ''); signOut(); await api('/auth/logout', { method: 'POST' }).catch(() => {}); location.hash = '#/signed-out' }
  const refresh = btn('btn btn-ghost btn-icon-sm', 'Refresh', 'refresh')
  const synced = h('span', { className: 'synced', 'aria-live': 'polite' })
  const unreadChip = h('span', { className: 'pane-chip', hidden: true })
  const counts = new Map(FOLDERS.map((f) => [f.role, h('span', { className: 'nav-count' })]))
  const tabCounts = new Map(FOLDERS.map((f) => [f.role, h('span', { className: 'tab-count' })]))
  const title = h('h2', { className: 'pane-title' }, meta.label)
  const nav = orgNav(key)

  const compose = (d: ComposeDraft = {}) => { if (mb) openComposer(base, d, onMutate, (id) => dropThreadsWith(id)) }
  const composeBtn = btn('btn btn-primary compose-btn', 'Compose', 'pen', { onclick: () => compose() })
  const fab = btn('btn btn-primary fab', 'Compose', 'pen', { onclick: () => compose() })

  const topbar = h('header', { className: 'topbar' },
    brand(location.hostname),
    searchBox((q) => {
      query = q
      draw() // instant local filter on the loaded list; the server search below replaces it
      window.clearTimeout(searchTimer)
      searchTimer = window.setTimeout(() => { void load() }, 300)
    }),
    h('div', { className: 'topbar-right' }, synced,
      accountChip(activeEmail),
      logout))

  const folderTabs = h('nav', { className: 'folder-tabs', ariaLabel: 'Folders' }, ...FOLDERS.map((f) =>
    h('a', { href: folderHash(f.role), className: 'folder-tab', ...(f.role === folder ? { 'aria-current': 'page' } : {}) }, f.label, tabCounts.get(f.role)!)), nav.tabs)

  const idle = () => emptyState('mail', 'No message selected', 'Choose a message from the list to read it here.')
  // Phone bottom-bar items: the sidebar becomes a fixed bottom bar on
  // phones, and these classes order it Mail, Temp, Organize, DMARC, Admin
  // (same as mobileNav). The Organize item is phone-only.
  const dmarcNav = navItem('#/dmarc', 'shield', 'DMARC reports'); dmarcNav.classList.add('nav-dmarc')
  const tempNav = navItem('#/temp', 'timer', 'Temporary inbox'); tempNav.classList.add('nav-temp')
  const orgNavItem = navItem('#/organize', 'filter', 'Organize'); orgNavItem.classList.add('nav-organize')
  const adminNav = navItem('#/admin', 'account', 'Admin'); adminNav.classList.add('nav-admin')
  const ui = mailShell({
    topbar,
    nav: [
      composeBtn,
      navSection('Mailbox'),
      ...FOLDERS.map((f) => {
        const a = navItem(folderHash(f.role), f.icon, f.label, f.role === folder, counts.get(f.role))
        a.classList.add('nav-folder')
        return a
      }),
      nav.starred,
      nav.sidebar,
      dmarcNav,
      h('a', { href: viewHash(view), className: 'nav-item nav-mail', 'aria-current': 'page' }, icon('mail'), h('span', { className: 'nav-label' }, 'Mail')),
      navSection('Temporary'),
      tempNav,
      orgNavItem,
      ...(is_admin ? [navSection('Admin'), adminNav] : []),
      footerLinks('sidebar-footer'),
    ],
    listTop: [h('header', { className: 'pane-head' }, title, unreadChip, refresh), folderTabs],
    listBottom: fab,
    idle: idle(),
  })

  const url = (id: string) => `${base}/messages/${encodeURIComponent(id)}`

  const loadFolders = async () => {
    if (!mb) return
    try {
      const folders = await loadOrg(base)
      for (const f of folders) {
        const n = f.role === 'inbox' ? f.unread : f.role === 'drafts' ? f.total : 0
        const c = counts.get(f.role as FolderRole)
        if (!c) continue
        c.textContent = n ? String(n) : ''
        tabCounts.get(f.role as FolderRole)!.textContent = n ? String(n) : ''
      }
      nav.draw()
      if (view.kind === 'folder') { meta = viewMeta(view); title.textContent = meta.label; setPage({ title: meta.label, description: 'Your IRIS folder.', path: '/', noindex: true }) }
    } catch { /* counts are optional */ }
  }

  const removeLocal = (id: string) => {
    all = all.filter((m) => m.id !== id)
    writeListCache(email, mb, key, all)
    if (current === id) { current = ''; ui.hide(idle()) }
    draw()
  }
  const keepLocal = () => { writeListCache(email, mb, key, all); draw() }

  /** Mirror a mutation onto the Summary in `all`. A message opened
   * from a fetched thread is a Detail, not the Summary the thread row
   * is drawn from, so mutating it alone leaves the row (unread dot,
   * star) and the unread count stale until the next reload. */
  const syncSummary = (id: string, patch: Partial<Summary>) => {
    const s = all.find((x) => x.id === id)
    if (s) Object.assign(s, patch)
  }

  /** Where messages in this view live, for Undo. Starred/label views span folders, so they have none. */
  const here: MoveTarget | null = view.kind === 'role' ? { folder: view.role } : view.kind === 'folder' ? { folder_id: view.id } : null
  const targetName = (t: MoveTarget) => 'folder' in t ? folderMeta(t.folder).label : org.folders.find((f) => f.id === t.folder_id)?.name ?? 'folder'

  const move = async (m: Summary, target: MoveTarget) => {
    const toTrash = 'folder' in target && target.folder === 'trash'
    // Starred and label views show mail from every folder except Trash.
    if (here || toTrash) removeLocal(m.id)
    messageCache.delete(url(m.id)) // the body is no longer in this folder; refetch if it comes back (undo)
    try {
      await api(`${url(m.id)}/move`, { method: 'POST', body: JSON.stringify(target) })
      toast(toTrash ? 'Moved to Trash' : `Moved to ${targetName(target)}`, here ? {
        label: 'Undo',
        run: async () => {
          await api(`${url(m.id)}/move`, { method: 'POST', body: JSON.stringify(here) }).catch(() => toast('Could not undo.'))
          load(); loadFolders()
        },
      } : undefined)
      loadFolders()
    } catch {
      toast('Could not move the message.'); load()
    }
  }

  const moveDialog = (m: Summary) => {
    type Item = { label: string; icon: string; target: MoveTarget }
    const fixed: Item[] = [
      { label: 'Inbox', icon: 'inbox', target: { folder: 'inbox' } },
      { label: 'Archive', icon: 'archive', target: { folder: 'archive' } },
    ]
    const items = [
      ...fixed,
      ...customFolders().map((f): Item => ({ label: f.name, icon: 'folder', target: { folder_id: f.id } })),
    ].filter((i) => JSON.stringify(i.target) !== JSON.stringify(here))
    choiceDialog('Move to', items.map((i) => ({ label: i.label, icon: i.icon, run: () => move(m, i.target) })),
      { label: 'New folder', run: async () => { if (await newFolderDialog(base)) { nav.draw(); moveDialog(m) } } })
  }

  const toggleStar = async (m: Summary) => {
    const on = !m.is_starred
    m.is_starred = on
    syncSummary(m.id, { is_starred: on })
    if (view.kind === 'starred' && !on) removeLocal(m.id)
    else keepLocal()
    try {
      await api(`${url(m.id)}/star`, { method: 'POST', body: JSON.stringify({ starred: on }) })
    } catch {
      m.is_starred = !on
      toast('Could not update the star.')
      load()
    }
  }

  const setLabel = async (m: Summary, label: string, on: boolean) => {
    try {
      await api(`${url(m.id)}/labels`, { method: 'POST', body: JSON.stringify({ label, on }) })
    } catch (err) { toast(orgError(err)); return false }
    m.labels = on ? uniq([...(m.labels ?? []), label]).sort() : (m.labels ?? []).filter((l) => l !== label)
    if (view.kind === 'label' && view.name === label && !on) removeLocal(m.id)
    else keepLocal()
    return true
  }

  const labelsDialog = (m: Summary) => {
    const list = h('div', { className: 'choice-list' })
    const render = () => list.replaceChildren(...(org.labels.length
      ? org.labels.map((l) => {
        const cb = h('input', { type: 'checkbox', checked: !!m.labels?.includes(l) }) as HTMLInputElement
        cb.onchange = async () => {
          cb.disabled = true
          if (!(await setLabel(m, l, cb.checked))) cb.checked = !cb.checked
          cb.disabled = false
        }
        return h('label', { className: 'choice-item choice-check' }, cb, icon('tag', 'icon icon-sm'), h('span', {}, l))
      })
      : [h('p', { className: 'muted' }, 'No labels yet. Create one to start sorting mail.')]))
    render()
    const done = btn('btn btn-primary', 'Done')
    const create = btn('btn btn-secondary', 'New label', 'plus')
    const d = h('dialog', { className: 'confirm' }, h('h2', {}, 'Labels'), list, h('div', { className: 'confirm-actions' }, create, done)) as HTMLDialogElement
    create.onclick = async () => { if (await newLabelDialog(base)) { render(); nav.draw() } }
    done.onclick = () => d.close()
    d.addEventListener('close', () => d.remove())
    document.body.append(d)
    d.showModal()
    done.focus()
  }

  const deleteForever = async (m: Summary) => {
    if (!(await confirmDialog('Delete forever?', 'This message will be permanently deleted. This can’t be undone.', 'Delete forever'))) return
    removeLocal(m.id)
    dropThreadsWith(m.id)
    try { await api(url(m.id), { method: 'DELETE' }); messageCache.delete(url(m.id)); toast('Message deleted forever'); loadFolders() }
    catch { toast('Could not delete the message.'); load() }
  }

  const markUnread = async (m: Summary) => {
    m.is_read = false
    syncSummary(m.id, { is_read: false })
    current = ''
    ui.hide(idle())
    draw()
    await api(`${url(m.id)}/seen`, { method: 'POST', body: JSON.stringify({ seen: false }) }).catch(() => {})
    loadFolders()
  }

  const toolbar = (m: Summary) => (d: Detail) => {
    const b = (label: string, ic: string, run: () => void, cls = 'btn btn-secondary btn-sm') => btn(cls, label, ic, { onclick: run })
    const many = d.to.length + (d.cc?.length ?? 0) > 1
    const actions: HTMLElement[] = []
    if (folder !== 'trash') {
      actions.push(b('Reply', 'reply', () => compose(replyDraft(d, 'reply', email))))
      if (many) actions.push(b('Reply all', 'reply', () => compose(replyDraft(d, 'all', email))))
      actions.push(b('Forward', 'forward', () => compose(replyDraft(d, 'forward', email))))
    }
    const icons: HTMLElement[] = []
    const ib = (label: string, ic: string, run: () => void) => btn('btn btn-ghost btn-icon-sm', label, ic, { onclick: run })
    const star = (): HTMLElement => {
      const s = ib(m.is_starred ? 'Unstar' : 'Star', m.is_starred ? 'star-fill' : 'star', () => { void toggleStar(m); s.replaceWith(star()) })
      s.classList.toggle('is-starred', !!m.is_starred)
      s.setAttribute('aria-pressed', String(!!m.is_starred))
      return s
    }
    icons.push(star(), ib('Labels', 'tag', () => labelsDialog(m)))
    if (folder === 'inbox') icons.push(ib('Archive', 'archive', () => move(m, { folder: 'archive' })))
    if (folder === 'archive' || view.kind === 'folder') icons.push(ib('Move to Inbox', 'inbox', () => move(m, { folder: 'inbox' })))
    if (folder !== 'trash' && folder !== 'drafts') icons.push(ib('Move to…', 'folder', () => moveDialog(m)))
    if (folder === 'trash') {
      actions.push(b('Restore to Inbox', 'inbox', () => move(m, { folder: 'inbox' })))
      actions.push(b('Delete forever', 'trash', () => deleteForever(m), 'btn btn-danger btn-sm'))
    } else {
      icons.push(ib('Delete', 'trash', () => move(m, { folder: 'trash' })))
    }
    if (folder !== 'trash' && folder !== 'sent' && folder !== 'drafts') icons.push(ib('Mark as unread', 'mail', () => markUnread(m)))
    return h('div', { className: 'reader-toolbar' }, h('div', { className: 'reader-actions' }, ...actions), h('div', { className: 'reader-icons' }, ...icons))
  }

  const openDraft = async (m: Summary) => {
    try {
      const { message: d } = await api<{ message: Detail }>(url(m.id))
      // A draft's attachments must come along: the next autosave replaces
      // the draft, so opening it without them would silently drop them.
      // If a download fails, don't open the draft at all — the files stay
      // safe in Drafts until the user retries.
      const attachments: ComposeAttachmentIn[] = []
      for (const a of d.attachments ?? []) {
        const res = await fetch(`${url(m.id)}/attachments/${encodeURIComponent(a.part_id)}`, { credentials: 'same-origin' })
        if (!res.ok) throw new Error('attachment download failed')
        const buf = await res.arrayBuffer()
        attachments.push({ name: a.name, content_type: a.content_type, data: bufToBase64(buf), size: buf.byteLength })
      }
      compose({ title: 'Draft', draftId: m.id, to: d.to.map(emailOf), cc: (d.cc ?? []).map(emailOf), subject: d.subject, text: d.text_body ?? '', html: d.html_body ?? undefined, attachments })
    } catch { toast('Could not open the draft.') }
  }

  const loadMoreFooter = () => {
    if (loadedQuery !== query || all.length === 0 || all.length >= total) return undefined
    return h('div', { className: 'load-more' },
      h('span', { className: 'load-more-count' }, `Showing ${all.length} of ${total}`),
      btn('btn btn-secondary btn-sm', loadingMore ? 'Loading…' : 'Load more', undefined, { disabled: loadingMore, onclick: () => { void loadMore() } }))
  }

  /** Opened threads and their fetched details, kept across redraws. */
  const expandedThreads = new Set<string>()
  const threadDetails = new Map<string, ThreadDetail>()
  /** In-flight thread fetches, so collapsing and re-expanding a row
   * while its fetch is still running does not start a second one. */
  const threadFetches = new Map<string, Promise<{ thread: ThreadDetail }>>()
  /** Drop every cached thread holding `id`: after a delete (or a
   * discarded draft) the cached detail would otherwise keep showing
   * a message that no longer exists. */
  const dropThreadsWith = (id: string) => {
    for (const [tid, t] of threadDetails) {
      if (t.messages.some((d) => d.id === id)) threadDetails.delete(tid)
    }
  }

  const openFromList = (m: Summary, i: number, wasUnread: boolean, total: number) => {
    // A draft opens in the composer only when it really is one: the
    // $draft keyword, or listed in this Drafts folder. A thread fetch
    // is account-wide, so an expanded Drafts conversation can also
    // hold the original message — opening that in the composer would
    // let an autosave destroy it (the server refuses non-draft ids
    // too, but the composer must never offer the edit).
    const isDraft = m.is_draft || (folder === 'drafts' && all.some((s) => s.id === m.id))
    if (isDraft) return void openDraft(m)
    if (wasUnread) {
      api(`${url(m.id)}/seen`, { method: 'POST', body: JSON.stringify({ seen: true }) }).then(() => {
        syncSummary(m.id, { is_read: true })
        writeListCache(email, mb, key, all)
        loadFolders()
        draw()
      }, () => {})
    }
    current = m.id
    draw()
    ui.show()
    openMessageAt(url(m.id), ui.readerBody, undefined, () => { signOut(); renderLogin() }, `${i + 1} of ${total}`, toolbar(m), htmlBodyMinter(url(m.id)))
  }

  const toggleThread = (g: ThreadGroup) => {
    if (expandedThreads.has(g.id)) { expandedThreads.delete(g.id); draw(); return }
    expandedThreads.add(g.id)
    draw()
    // The loaded page's count is only a lower bound: a thread's other
    // messages may sit on later pages or in other folders, so a count
    // of one on this page does not prove the thread is a singleton.
    // Expand always fetches a real thread id once and caches it.
    if (!threadDetails.has(g.id) && !g.id.startsWith('msg:') && !threadFetches.has(g.id)) {
      const p = api<{ thread: ThreadDetail }>(`${base}/threads/${encodeURIComponent(g.id)}`)
      threadFetches.set(g.id, p)
      p.then(({ thread }) => {
        threadDetails.set(g.id, thread)
        // Seed the message cache so opening a message in the thread
        // needs no second fetch.
        for (const d of thread.messages) {
          const u = url(d.id)
          if (!messageCache.has(u)) {
            while (messageCache.size >= MESSAGE_CACHE_MAX) messageCache.delete(messageCache.keys().next().value!)
            messageCache.set(u, { at: Date.now(), p: Promise.resolve({ message: d }) })
          }
        }
        draw()
      }).catch((err) => {
        if ((err as HttpError).status === 401) { signOut(); renderLogin(); return }
        toast('Could not load this conversation.')
      }).finally(() => { threadFetches.delete(g.id) })
    }
  }

  const draw = () => {
    // When the loaded list already matches the query (server-side search),
    // show it as-is: a body-only hit would fail the local from/subject/preview filter.
    const shown = loadedQuery === query ? all : all.filter((m) => matches(m, query))
    const unread = all.filter((m) => !m.is_read).length
    unreadChip.textContent = `${unread} unread`
    unreadChip.hidden = folder !== 'inbox' || !unread
    const [emptyTitle, emptyText] = meta.empty
    fillThreadList(ui.list, groupThreads(shown, folder === 'sent' || folder === 'drafts'), {
      current,
      showTo: folder === 'sent' || folder === 'drafts',
      empty: query ? emptyState('search', 'No matches', `Nothing in ${meta.label} matches “${query}”.`) : emptyState(meta.icon, emptyTitle, emptyText),
      footer: loadMoreFooter(),
      expanded: expandedThreads,
      threads: threadDetails,
      onToggle: (g) => { toggleThread(g) },
      onOpen: (m, i, wasUnread, total) => openFromList(m, i, wasUnread, total),
      onStar: folder === 'drafts' ? undefined : (m) => { void toggleStar(m) },
      onIntent: folder === 'drafts' ? undefined : (m) => prefetchMessage(url(m.id)),
    })
  }

  const params = (extra: Record<string, string> = {}) => {
    const p = new URLSearchParams({ ...viewParams(view), limit: '50', ...extra })
    if (query) p.set('q', query)
    return p
  }

  const load = async () => {
    if (!mb) { ui.list.replaceChildren(emptyState('inbox', 'No mailbox yet', 'Your account has no mailbox. Contact your administrator.')); return }
    if (!all.length) ui.list.replaceChildren(skeleton())
    const restore = busy(refresh, '')
    refresh.classList.add('is-spinning')
    try {
      const { messages: fresh, total: t } = await api<{ messages: Summary[]; total: number }>(`${base}/messages?${params()}`)
      const read = new Set(all.filter((m) => m.is_read).map((m) => m.id))
      all = fresh.map((m) => ({ ...m, is_read: m.is_read || read.has(m.id) }))
      total = t
      loadedQuery = query
      writeListCache(email, mb, key, all)
      pendingNew = 0 // the list is fresh, so nothing is pending anymore
      synced.textContent = `Synced ${fmtTime(new Date())}`
      draw()
    } catch (err) {
      if ((err as HttpError).status === 401) { signOut(); return renderLogin() }
      if ((err as HttpError).status === 404 && view.kind === 'folder') {
        ui.list.replaceChildren(emptyState('folder', 'Folder not found', 'This folder was deleted or renamed elsewhere.'))
        return
      }
      if (all.length) { synced.textContent = 'Offline · showing saved list'; return }
      ui.list.replaceChildren(errorState(errorText[(err as HttpError).code] ?? 'Could not load messages.', load))
    } finally { restore(); refresh.classList.remove('is-spinning') }
  }
  const loadMore = async () => {
    if (loadingMore || loadedQuery !== query || all.length >= total) return
    loadingMore = true
    draw()
    try {
      const { messages, total: t } = await api<{ messages: Summary[]; total: number }>(`${base}/messages?${params({ position: String(all.length) })}`)
      total = t
      const seen = new Set(all.map((m) => m.id))
      const read = new Set(all.filter((m) => m.is_read).map((m) => m.id))
      all = [
        ...all,
        ...messages.filter((m) => !seen.has(m.id)).map((m) => ({ ...m, is_read: m.is_read || read.has(m.id) })),
      ]
      loadedQuery = query
      writeListCache(email, mb, key, all)
    } catch {
      toast('Could not load more messages.')
    } finally {
      loadingMore = false
      draw()
    }
  }

  // After sending or saving: refresh counts, and the list if it's the folder that changed.
  function onMutate() {
    loadFolders()
    if (folder === 'drafts' || folder === 'sent') load()
    if (folder === 'inbox') setTimeout(load, 1500) // a message sent to yourself arrives shortly
  }

  refresh.onclick = () => { load(); loadFolders() }
  loadFolders()
  // Live new-mail stream: one EventSource per mailbox view, shared
  // server-side with every other tab on the same mailbox. While it is
  // live the 30 s poll below stands down; when it drops (error, or a
  // reconnect after a network blip) the poll resumes and the stream
  // reconnects on its own. New arrivals surface as a notice with a
  // Show action instead of yanking the list mid-read.
  const openStream = () => {
    mailStream?.close()
    const es = new EventSource(`${base}/events`)
    mailStream = es
    es.addEventListener('hello', () => { streamLive = true })
    es.addEventListener('new-mail', (ev) => {
      const data = JSON.parse((ev as MessageEvent).data) as { new: number; total: number }
      pendingNew += data.new
      toast(pendingNew === 1 ? '1 new message' : `${pendingNew} new messages`, { label: 'Show', run: () => {
        pendingNew = 0
        if (folder === 'inbox') { load(); loadFolders() } else { location.hash = '#/' }
      } })
    })
    es.onerror = () => { streamLive = false }
  }
  openStream()
  // Fallback while the stream is down: the old 30 s poll.
  tempTimers.push(window.setInterval(() => { if (!document.hidden && !streamLive) { load(); loadFolders() } }, 30_000))
  if (all.length) { draw(); load() } else await load()
}

// ----- folders, labels & filters page -----
const FILTER_FIELDS: [FilterRule['field'], string][] = [['from', 'From'], ['to', 'To or Cc'], ['subject', 'Subject']]
const FILTER_ACTIONS: [FilterRule['action'], string][] = [['move', 'Move to folder'], ['label', 'Add label'], ['star', 'Star it'], ['read', 'Mark as read']]

function describeFilter(f: FilterRule) {
  const field = FILTER_FIELDS.find(([k]) => k === f.field)?.[1] ?? f.field
  const then = f.action === 'move' ? `move to ${org.folders.find((x) => x.id === f.target)?.name ?? 'a deleted folder'}`
    : f.action === 'label' ? `add label “${f.target}”` : f.action === 'star' ? 'star it' : 'mark as read'
  return `If ${field} contains “${f.value}”, ${then}`
}

async function renderOrganize() {
  const { email, mb } = await ensureAccount()
  const base = `/api/v1/mailboxes/${mb}`
  setPage({ title: 'Organize', description: 'Manage your IRIS folders, labels, filters and contacts.', path: '/', noindex: true })
  let filters: FilterRule[] = []

  const back = h('a', { href: '#/', className: 'btn btn-ghost btn-sm' }, icon('back'), h('span', { className: 'btn-label' }, 'Back to mail'))
  const topbar = h('header', { className: 'topbar organize-top' }, brand(location.hostname), back,
    h('div', { className: 'topbar-right' }, accountChip(email)))
  const folderList = h('ul', { className: 'org-list' })
  const labelList = h('ul', { className: 'org-list' })
  const filterList = h('ul', { className: 'org-list' })
  const row = (ic: string, text: string, ...actions: HTMLElement[]) =>
    h('li', { className: 'org-row' }, icon(ic, 'icon icon-sm'), h('span', { className: 'org-name' }, text), h('span', { className: 'org-actions' }, ...actions))
  const empty = (text: string) => h('li', { className: 'org-empty muted' }, text)

  const drawFolders = () => {
    const custom = customFolders()
    folderList.replaceChildren(...(custom.length ? custom.map((f) => row('folder', `${f.name}${f.total ? ` · ${f.total}` : ''}`,
      btn('btn btn-ghost btn-sm', 'Rename', 'pen', { onclick: () => renameFolder(f) }),
      btn('btn btn-ghost btn-sm', 'Delete', 'trash', { onclick: () => deleteFolder(f) })))
      : [empty('No folders yet. Folders hold messages you move into them.')]))
  }
  const drawLabels = () => {
    labelList.replaceChildren(...(org.labels.length ? org.labels.map((l) => row('tag', l,
      btn('btn btn-ghost btn-sm', 'Delete', 'trash', { onclick: () => deleteLabel(l) })))
      : [empty('No labels yet. A message can have several labels.')]))
  }
  const drawFilters = () => {
    filterList.replaceChildren(...(filters.length ? filters.map((f) => row('filter', describeFilter(f),
      btn('btn btn-ghost btn-sm', 'Delete', 'trash', { onclick: () => deleteFilter(f) })))
      : [empty('No filters yet. Add one below to sort new mail automatically.')]))
    drawForm()
  }

  // Contacts: the address book behind compose autocomplete. The add form
  // doubles as the edit form: Edit fills it and switches the button.
  const contactList = h('ul', { className: 'org-list' })
  let contacts: Contact[] = []
  let editing: Contact | null = null
  const drawContacts = () => {
    const sorted = [...contacts].sort((a, b) => a.name.localeCompare(b.name) || a.email.localeCompare(b.email))
    contactList.replaceChildren(...(sorted.length ? sorted.map((c) => row('account', `${c.name} · ${c.email}`,
      btn('btn btn-ghost btn-sm', 'Edit', 'pen', { onclick: () => editContact(c) }),
      btn('btn btn-ghost btn-sm', 'Delete', 'trash', { onclick: () => deleteContact(c) })))
      : [empty('No contacts yet. Add people you email often and they will show up as suggestions in To, Cc and Bcc.')]))
    if (contacts.length >= 100) contactList.append(h('li', { className: 'org-empty muted' }, 'Showing the first 100 contacts.'))
  }
  const contactName = h('input', { id: 'ct-name', type: 'text', autocomplete: 'off', spellcheck: false, placeholder: 'Ada Lovelace' }) as HTMLInputElement
  const contactEmail = h('input', { id: 'ct-email', type: 'text', autocomplete: 'off', spellcheck: false, placeholder: 'ada@example.com' }) as HTMLInputElement
  const contactError = h('div', { className: 'form-error', role: 'alert' })
  const contactSave = h('button', { type: 'submit', className: 'btn btn-primary' }, icon('plus'), h('span', { className: 'btn-label' }, 'Add contact')) as HTMLButtonElement
  const contactCancel = h('button', { type: 'button', className: 'btn btn-ghost btn-sm', hidden: true }, 'Cancel editing')
  const contactForm = h('form', { className: 'org-form' },
    h('div', { className: 'field' }, h('label', { htmlFor: 'ct-name' }, 'Name'), contactName),
    h('div', { className: 'field org-grow org-span-2' }, h('label', { htmlFor: 'ct-email' }, 'Email'), contactEmail),
    h('div', { className: 'org-submit' }, contactSave, contactCancel),
    contactError)
  const resetContactForm = () => {
    editing = null
    contactName.value = ''
    contactEmail.value = ''
    ;(contactSave.querySelector('.btn-label') as HTMLElement).textContent = 'Add contact'
    contactCancel.hidden = true
    contactError.replaceChildren()
  }
  const editContact = (c: Contact) => {
    editing = c
    contactName.value = c.name
    contactEmail.value = c.email
    ;(contactSave.querySelector('.btn-label') as HTMLElement).textContent = 'Save changes'
    contactCancel.hidden = false
    contactError.replaceChildren()
    contactName.focus()
  }
  contactCancel.onclick = resetContactForm
  contactForm.onsubmit = async (e) => {
    e.preventDefault()
    contactError.replaceChildren()
    const name = contactName.value.trim()
    const email = contactEmail.value.trim()
    if (!name || !email) { contactError.append(h('span', {}, 'Enter a name and an email address.')); return }
    const restore = busy(contactSave, 'Saving…')
    try {
      if (editing) {
        const { contact } = await api<{ contact: Contact }>(`/api/v1/contacts/${encodeURIComponent(editing.id)}`, { method: 'PATCH', body: JSON.stringify({ name, email }) })
        contacts = contacts.map((x) => x.id === contact.id ? contact : x)
        toast('Contact saved')
      } else {
        const { contact } = await api<{ contact: Contact }>('/api/v1/contacts', { method: 'POST', body: JSON.stringify({ name, email }) })
        contacts = [...contacts, contact]
        toast('Contact added')
      }
      restore()
      resetContactForm()
      drawContacts()
    } catch (err) {
      restore()
      if ((err as HttpError).status === 401) { account = null; return renderLogin() }
      contactError.replaceChildren(icon('alert'), h('span', {}, orgError(err)))
    }
  }
  const deleteContact = async (c: Contact) => {
    if (!(await confirmDialog(`Delete “${c.name}”?`, 'The contact is removed from your address book.', 'Delete contact'))) return
    try {
      await api(`/api/v1/contacts/${encodeURIComponent(c.id)}`, { method: 'DELETE' })
      contacts = contacts.filter((x) => x.id !== c.id)
      if (editing?.id === c.id) resetContactForm()
      drawContacts(); toast('Contact deleted')
    } catch (err) { toast(orgError(err)) }
  }

  const renameFolder = (f: FolderInfo) => promptDialog({
    title: 'Rename folder', label: 'Folder name', value: f.name, confirm: 'Rename',
    submit: async (v) => {
      try {
        await api(`${base}/folders/${encodeURIComponent(f.id)}`, { method: 'PATCH', body: JSON.stringify({ name: v }) })
        f.name = v.trim()
        drawFolders(); drawFilters()
        return null
      } catch (err) { return orgError(err) }
    },
  })
  const deleteFolder = async (f: FolderInfo) => {
    if (!(await confirmDialog(`Delete “${f.name}”?`, 'Only empty folders can be deleted. Messages are never deleted with a folder.', 'Delete folder'))) return
    try {
      await api(`${base}/folders/${encodeURIComponent(f.id)}`, { method: 'DELETE' })
      org.folders = org.folders.filter((x) => x.id !== f.id)
      drawFolders(); toast('Folder deleted')
    } catch (err) { toast(orgError(err)) }
  }
  const deleteLabel = async (l: string) => {
    if (!(await confirmDialog(`Delete label “${l}”?`, 'The label is removed from every message. The messages themselves stay.', 'Delete label'))) return
    try {
      await api(`${base}/labels/${encodeURIComponent(l)}`, { method: 'DELETE' })
      org.labels = org.labels.filter((x) => x !== l)
      drawLabels(); drawForm(); toast('Label deleted')
    } catch (err) { toast(orgError(err)) }
  }
  const deleteFilter = async (f: FilterRule) => {
    try {
      await api(`${base}/filters/${encodeURIComponent(f.id)}`, { method: 'DELETE' })
      filters = filters.filter((x) => x.id !== f.id)
      drawFilters(); toast('Filter deleted')
    } catch (err) { toast(orgError(err)) }
  }

  // Add-filter form.
  const select = (id: string, options: [string, string][]) =>
    h('select', { id, className: 'org-select' }, ...options.map(([v, l]) => h('option', { value: v }, l))) as HTMLSelectElement
  const field = select('f-field', FILTER_FIELDS)
  const value = h('input', { id: 'f-value', type: 'text', autocomplete: 'off', spellcheck: false, placeholder: 'e.g. newsletter@shop.com' }) as HTMLInputElement
  const action = select('f-action', FILTER_ACTIONS)
  const target = h('select', { id: 'f-target', className: 'org-select' }) as HTMLSelectElement
  const targetRow = h('div', { className: 'field' }, h('label', { htmlFor: 'f-target' }, 'Target'), target)
  const formError = h('div', { className: 'form-error', role: 'alert' })
  const add = h('button', { type: 'submit', className: 'btn btn-primary' }, icon('plus'), h('span', { className: 'btn-label' }, 'Add filter')) as HTMLButtonElement
  const drawForm = () => {
    const kind = action.value
    const opts: [string, string][] = kind === 'move'
      ? org.folders.filter((f) => f.role === 'archive' || f.role === 'trash' || f.role === 'custom').map((f) => [f.id, f.name])
      : kind === 'label' ? org.labels.map((l) => [l, l]) : []
    const keep = target.value
    target.replaceChildren(...opts.map(([v, l]) => h('option', { value: v }, l)))
    if (opts.some(([v]) => v === keep)) target.value = keep
    targetRow.hidden = kind !== 'move' && kind !== 'label'
    ;(targetRow.querySelector('label') as HTMLLabelElement).textContent = kind === 'move' ? 'Folder' : 'Label'
    add.disabled = !targetRow.hidden && !opts.length
    formError.replaceChildren(...(!targetRow.hidden && !opts.length ? [h('span', {}, kind === 'label' ? 'Create a label first.' : 'Create a folder first.')] : []))
  }
  action.onchange = drawForm
  value.oninput = () => value.removeAttribute('aria-invalid')
  const form = h('form', { className: 'org-form' },
    h('div', { className: 'field' }, h('label', { htmlFor: 'f-field' }, 'If'), field),
    h('div', { className: 'field org-grow' }, h('label', { htmlFor: 'f-value' }, 'Contains'), value),
    h('div', { className: 'field' }, h('label', { htmlFor: 'f-action' }, 'Then'), action),
    targetRow,
    h('div', { className: 'org-submit' }, add),
    formError)
  form.onsubmit = async (e) => {
    e.preventDefault()
    formError.replaceChildren()
    if (!value.value.trim()) { value.setAttribute('aria-invalid', 'true'); value.focus(); formError.append(h('span', {}, 'Enter the text to look for.')); return }
    const restore = busy(add, 'Saving…')
    try {
      const body = { field: field.value, value: value.value, action: action.value, target: targetRow.hidden ? null : target.value }
      const { filter } = await api<{ filter: FilterRule }>(`${base}/filters`, { method: 'POST', body: JSON.stringify(body) })
      filters = [...filters, filter]
      value.value = ''
      restore()
      drawFilters()
      toast('Filter added')
    } catch (err) {
      restore()
      if ((err as HttpError).status === 401) { account = null; return renderLogin() }
      formError.replaceChildren(icon('alert'), h('span', {}, orgError(err)))
    }
  }

  const section = (heading: string, text: string, list: HTMLElement, ...extra: HTMLElement[]) =>
    h('section', { className: 'org-section' }, h('h2', {}, heading), h('p', { className: 'muted' }, text), list, ...extra)

  // Signature: the sign-off the composer appends to new messages. The
  // editor has the same plain/rich modes as the composer; saving one
  // form derives the other on the server.
  const sigText = h('textarea', { className: 'sig-text', ariaLabel: 'Signature (plain text)', placeholder: 'Best regards,\nAda' }) as HTMLTextAreaElement
  const sigRich = makeRichEditor('', () => { sigStatus.textContent = '' })
  let sigHtmlMode = false
  const sigPlainBtn = h('button', { type: 'button', className: 'btn btn-ghost btn-sm', 'aria-pressed': 'true' }, 'Plain') as HTMLButtonElement
  const sigRichBtn = h('button', { type: 'button', className: 'btn btn-ghost btn-sm', 'aria-pressed': 'false' }, 'Rich') as HTMLButtonElement
  const setSigMode = (richMode: boolean) => {
    if (richMode === sigHtmlMode) return
    sigHtmlMode = richMode
    if (richMode) sigRich.setHtml(textToHtml(sigText.value))
    else sigText.value = htmlToText(sigRich.getHtml())
    sigText.hidden = richMode
    sigRich.wrap.hidden = !richMode
    sigPlainBtn.setAttribute('aria-pressed', String(!richMode))
    sigRichBtn.setAttribute('aria-pressed', String(richMode))
  }
  sigPlainBtn.onclick = () => setSigMode(false)
  sigRichBtn.onclick = () => setSigMode(true)
  sigRich.wrap.hidden = true
  const sigStatus = h('span', { className: 'compose-status' })
  const sigError = h('div', { className: 'form-error', role: 'alert' })
  const sigSave = h('button', { type: 'button', className: 'btn btn-primary' }, icon('plus'), h('span', { className: 'btn-label' }, 'Save signature')) as HTMLButtonElement
  sigSave.onclick = async () => {
    sigError.replaceChildren()
    const restore = busy(sigSave, 'Saving…')
    try {
      const saved = await api<Signature>(`${base}/signature`, {
        method: 'PUT',
        body: JSON.stringify(sigHtmlMode ? { text: '', html: sigRich.getHtml() } : { text: sigText.value, html: '' }),
      })
      signatureCache.set(base, saved)
      sigText.value = saved.text
      sigRich.setHtml(saved.html)
      restore()
      sigStatus.textContent = 'Saved'
      toast('Signature saved')
    } catch (err) {
      restore()
      if ((err as HttpError).status === 401) { account = null; return renderLogin() }
      sigError.replaceChildren(icon('alert'), h('span', {}, orgError(err)))
    }
  }
  const sigModeRow = h('div', { className: 'compose-mode', role: 'group', ariaLabel: 'Signature format' }, sigPlainBtn, sigRichBtn)
  const sigBox = h('div', { className: 'sig-editor' }, sigModeRow, sigRich.wrap, sigText, sigError)
  const sigSection = section('Signature', 'Added automatically to new messages. You can still edit or delete it in the composer.',
    sigBox, h('div', { className: 'org-submit' }, sigSave, sigStatus))

  app.className = 'app-shell'
  app.replaceChildren(topbar, h('main', { className: 'app-main organize' }, h('div', { className: 'organize-inner' },
    h('h1', { className: 'pane-title' }, 'Organize'),
    section('Folders', 'Your own folders, next to Inbox, Archive and Trash.', folderList,
      btn('btn btn-secondary btn-sm', 'New folder', 'plus', { onclick: async () => { if (await newFolderDialog(base)) drawFolders() } })),
    section('Labels', 'Tag messages without moving them. Starred is always available.', labelList,
      btn('btn btn-secondary btn-sm', 'New label', 'plus', { onclick: async () => { if (await newLabelDialog(base)) { drawLabels(); drawForm() } } })),
    section('Filters', 'Filters run on the mail server as new mail arrives, even when IRIS is closed. Moves run last; the first matching move wins.', filterList, form),
    section('Contacts', 'People you email often. They show up as suggestions when you type in To, Cc or Bcc.', contactList, contactForm),
    sigSection)), mobileNav('organize', account?.is_admin === true))
  folderList.replaceChildren(skeleton(2))
  try {
    const [, f, ct, sg] = await Promise.all([loadOrg(base), api<{ filters: FilterRule[] }>(`${base}/filters`), api<{ contacts: Contact[] }>('/api/v1/contacts?limit=100'),
      api<Signature>(`${base}/signature`).catch(() => ({ text: '', html: '' }))])
    filters = f.filters
    contacts = ct.contacts
    sigText.value = sg.text
    sigRich.setHtml(sg.html)
    drawFolders(); drawLabels(); drawFilters(); drawContacts()
  } catch (err) {
    if ((err as HttpError).status === 401) { account = null; return renderLogin() }
    folderList.replaceChildren(errorState(orgError(err), () => { void renderOrganize() }))
  }
}

// ---------- DMARC dashboard ----------
type DmarcDomainRow = { domain: string; reports: number; messages: number; passed: number; failed: number; last_end: number }
type DmarcOffender = { source_ip: string; messages: number; passed: number; failed: number }
type DmarcSummary = { reports: number; domains: number; messages: number; passed: number; failed: number; first_begin: number | null; last_end: number | null; per_domain: DmarcDomainRow[]; offenders: DmarcOffender[] }
type DmarcReportRow = { id: string; org_name: string; report_id: string; date_begin: number; date_end: number; total_messages: number; passed_messages: number; failed_messages: number }
type DmarcSourceRow = { source_ip: string; messages: number; passed: number; failed: number }
type DmarcDomain = { domain: string; reports: number; messages: number; passed: number; failed: number; report_list: DmarcReportRow[]; sources: DmarcSourceRow[] }

const fmtDay = (unix: number) => new Date(unix * 1000).toLocaleDateString(undefined, { month: 'short', day: 'numeric', year: 'numeric' })
const pct = (part: number, total: number) => total > 0 ? `${Math.round((part / total) * 100)}%` : 'n/a'

async function renderDmarc() {
  const { email, mb } = await ensureAccount()
  const base = `/api/v1/mailboxes/${mb}`
  setPage({ title: 'DMARC reports', description: 'Aggregate DMARC reports for your domains.', path: '/#/dmarc', noindex: true })

  const back = h('a', { href: '#/', className: 'btn btn-ghost btn-sm' }, icon('back'), h('span', { className: 'btn-label' }, 'Back to mail'))
  const topbar = h('header', { className: 'topbar organize-top' }, brand(location.hostname), back,
    h('div', { className: 'topbar-right' }, accountChip(email)))
  const scanBtn = btn('btn btn-primary btn-sm', 'Scan inbox for reports', 'refresh') as HTMLButtonElement
  const stats = h('div', { className: 'dmarc-stats' })
  const domainList = h('ul', { className: 'org-list' })
  const offenderList = h('ul', { className: 'org-list' })
  const section = (heading: string, text: string, list: HTMLElement, ...extra: HTMLElement[]) =>
    h('section', { className: 'org-section' }, h('h2', {}, heading), h('p', { className: 'muted' }, text), list, ...extra)

  let summary: DmarcSummary | null = null
  let expanded: string | null = null
  const details = new Map<string, DmarcDomain>()

  const stat = (value: string, label: string) =>
    h('div', { className: 'dmarc-stat' }, h('b', {}, value), h('span', {}, label))

  const drawStats = () => {
    if (!summary) return
    stats.replaceChildren(
      stat(String(summary.reports), 'Reports'),
      stat(String(summary.domains), 'Domains'),
      stat(String(summary.messages), 'Messages checked'),
      stat(pct(summary.passed, summary.messages), 'DMARC pass'))
  }

  const drawOffenders = () => {
    if (!summary) return
    offenderList.replaceChildren(...(summary.offenders.length ? summary.offenders.map((o) =>
      h('li', { className: 'org-row' }, icon('shield', 'icon icon-sm'),
        h('span', { className: 'org-name mono' }, o.source_ip),
        h('span', { className: 'org-actions muted' }, `${o.messages} messages, ${o.failed} failed`)))
      : [h('li', { className: 'org-empty muted' }, 'No failing sources. Every checked message passed DMARC.')]))
  }

  const drawDomains = () => {
    if (!summary) return
    if (!summary.per_domain.length) {
      domainList.replaceChildren(h('li', { className: 'org-empty muted' }, 'No reports yet. Receiving servers send aggregate reports as attachments to your inbox; scan to import them.'))
      return
    }
    domainList.replaceChildren(...summary.per_domain.map((d) => {
      const li = h('li', { className: 'dmarc-domain' })
      const isOpen = expanded === d.domain
      const row = h('button', { type: 'button', className: 'dmarc-row', 'aria-expanded': String(isOpen) },
        h('span', { className: 'dmarc-domain-name' }, d.domain),
        h('span', { className: 'muted' }, `${d.reports} report${d.reports === 1 ? '' : 's'}`),
        h('span', { className: 'muted' }, `${d.messages} messages`),
        h('span', { className: d.failed > 0 ? 'dmarc-fail' : '' }, `${pct(d.passed, d.messages)} pass`),
        h('span', { className: 'muted' }, `last ${fmtDay(d.last_end)}`))
      row.onclick = () => { void toggleDomain(d.domain) }
      li.append(row)
      if (isOpen) {
        const detail = details.get(d.domain)
        const box = h('div', { className: 'dmarc-detail' })
        if (!detail) {
          box.replaceChildren(h('p', { className: 'muted' }, 'Loading…'))
        } else {
          const reports = h('ul', { className: 'org-list' }, ...detail.report_list.map((r) =>
            h('li', { className: 'org-row' }, icon('mail', 'icon icon-sm'),
              h('span', { className: 'org-name' }, `${r.org_name} · ${fmtDay(r.date_begin)} to ${fmtDay(r.date_end)}`),
              h('span', { className: 'org-actions muted' }, `${r.total_messages} messages, ${r.failed_messages} failed`))))
          const sources = h('ul', { className: 'org-list' }, ...(detail.sources.length ? detail.sources.map((s) =>
            h('li', { className: 'org-row' }, icon('shield', 'icon icon-sm'),
              h('span', { className: 'org-name mono' }, s.source_ip),
              h('span', { className: 'org-actions muted' }, `${s.messages} messages, ${s.failed} failed`)))
            : [h('li', { className: 'org-empty muted' }, 'No sources in these reports.')]))
          box.replaceChildren(
            h('h3', {}, 'Reports'),
            reports,
            h('h3', {}, 'Sources'),
            sources)
        }
        li.append(box)
      }
      return li
    }))
  }

  const toggleDomain = async (domain: string) => {
    if (expanded === domain) { expanded = null; drawDomains(); return }
    expanded = domain
    drawDomains()
    if (!details.has(domain)) {
      try {
        const { domain: d } = await api<{ domain: DmarcDomain }>(`${base}/dmarc/domains/${encodeURIComponent(domain)}`)
        details.set(domain, d)
        drawDomains()
      } catch (err) {
        if ((err as HttpError).status === 401) { account = null; return renderLogin() }
        toast('Could not load this domain.')
        expanded = null
        drawDomains()
      }
    }
  }

  const load = async () => {
    try {
      const { summary: s } = await api<{ summary: DmarcSummary }>(`${base}/dmarc/summary`)
      summary = s
      drawStats(); drawDomains(); drawOffenders()
    } catch (err) {
      if ((err as HttpError).status === 401) { account = null; return renderLogin() }
      domainList.replaceChildren(errorState('Could not load DMARC reports.', () => { void load() }))
    }
  }

  scanBtn.onclick = async () => {
    const restore = busy(scanBtn, 'Scanning…')
    try {
      const r = await api<{ scanned: number; imported: number; skipped: number }>(`${base}/dmarc/scan`, { method: 'POST' })
      restore()
      toast(r.imported > 0
        ? `Imported ${r.imported} new report${r.imported === 1 ? '' : 's'} from ${r.scanned} message${r.scanned === 1 ? '' : 's'}.`
        : 'No new reports found in your inbox.')
      await load()
    } catch (err) {
      restore()
      if ((err as HttpError).status === 401) { account = null; return renderLogin() }
      toast('Scan failed. Try again.')
    }
  }

  app.className = 'app-shell'
  app.replaceChildren(topbar, h('main', { className: 'app-main organize' }, h('div', { className: 'organize-inner' },
    h('h1', { className: 'pane-title' }, 'DMARC reports'),
    h('p', { className: 'muted' }, 'Aggregate reports show how receiving servers treat mail from your domains: how much passed DMARC, and which sources failed.'),
    h('div', { className: 'org-submit' }, scanBtn),
    section('Totals', 'Across all imported reports.', stats),
    section('By domain', 'Open a domain to see its reports and the sources that sent mail for it.', domainList),
    section('Worst offenders', 'Sources with the most failed messages, across all domains.', offenderList))), mobileNav('dmarc', account?.is_admin === true))
  stats.replaceChildren()
  await load()
}

// ---------- Temp inbox ----------
let tempTimers: number[] = []
let onVisible: (() => void) | null = null
// Live mailbox stream for the current view; closed on every route
// change (stopTemp) and on sign-out, so a dead tab never holds a
// stream open.
let mailStream: EventSource | null = null
document.addEventListener('visibilitychange', () => onVisible?.())
const stopTemp = () => { tempTimers.forEach(clearInterval); tempTimers = []; onVisible = null; mailStream?.close(); mailStream = null }
const TEMP_KEY = 'iris_temp_inbox'
const savedTemp = (): TempInbox | null => { try { return JSON.parse(sessionStorage.getItem(TEMP_KEY) ?? 'null') } catch { return null } }
const saveTemp = (t: TempInbox | null) => { try { t ? sessionStorage.setItem(TEMP_KEY, JSON.stringify(t)) : sessionStorage.removeItem(TEMP_KEY) } catch { /* storage blocked */ } }

function simplePage(cls: string, ...kids: HTMLElement[]) {
  app.className = 'auth-shell'
  app.replaceChildren(h('header', { className: 'simple-top' }, brand('Meow Labs')), h('main', { className: cls }, ...kids), footerLinks())
}

async function renderTempStart(note = '') {
  stopTemp()
  setPage({ title: 'Free temporary email', description: 'Get a free disposable email address in one click. Receive-only, no sign-up, deleted automatically after 1 hour.', path: '/#/temp' })
  if (!account) {
    try { const s = await api<{ email: string; is_admin?: boolean }>('/api/v1/session'); account = { email: s.email, mb: '', is_admin: s.is_admin === true, boxes: [] } } catch { account = null }
  }
  const loggedIn = !!account
  const status = h('div', { className: 'form-error', role: 'alert' }, ...(note ? [icon('alert'), note] : []))
  const create = btn('btn btn-primary btn-block', 'Create temporary address', 'pen') as HTMLButtonElement
  create.onclick = async () => {
    status.replaceChildren()
    const restore = busy(create, 'Creating…')
    try {
      const inbox = await api<TempInbox>('/api/v1/temp-inboxes', { method: 'POST' })
      saveTemp(inbox)
      renderTempInbox(inbox, true)
    } catch (err) {
      status.replaceChildren(icon('alert'), h('span', {}, tempErrors[(err as HttpError).code] ?? 'Could not create an address. Please try again.'))
      restore()
    }
  }
  simplePage('auth-main',
    h('section', { className: 'auth-card' },
      h('div', { className: 'cat-peek' }, mascot('cat-peek')),
      h('header', { className: 'auth-head' },
        h('p', { className: 'eyebrow' }, 'Temporary inbox'),
        h('h1', {}, 'Get a disposable address'),
        h('p', { className: 'muted' }, 'One click. Receive-only. No sign-up.')),
      h('ul', { className: 'facts' },
        h('li', {}, icon('clock'), h('span', {}, 'Expires after 1 hour — messages are deleted with it.')),
        h('li', {}, icon('inbox'), h('span', {}, 'Receive only. You can’t send from it.')),
        h('li', {}, icon('shield'), h('span', {}, 'Not for sensitive or long-term mail.'))),
      status,
      h('div', { className: 'sticky-cta' }, create),
      h('p', { className: 'auth-alt' }, loggedIn ? h('a', { href: '#/', className: 'link' }, 'Go to your mailbox →') : h('span', {}, 'Have an account? ', h('a', { href: '#/', className: 'link' }, 'Sign in →')))));
  // Logged-in users get the section nav too: the temp start page is a
  // simple page with no sidebar, so without this the Temp tab is a dead
  // end on phones (only "Go to your mailbox" leads out).
  if (loggedIn) app.append(mobileNav('temp', account?.is_admin === true))
  create.focus({ preventScroll: true })
}

function renderTempExpired(inbox: TempInbox) {
  stopTemp()
  saveTemp(null)
  setPage({ title: 'Address expired', description: 'This temporary address has expired.', path: '/#/temp', noindex: true })
  const create = btn('btn btn-primary btn-block', 'Create new address', 'pen')
  create.onclick = () => renderTempStart()
  simplePage('auth-main',
    h('section', { className: 'auth-card expired' },
      mascot('cat-sleep', 'mascot expired-cat'),
      h('h1', {}, 'This address has expired.'),
      h('code', { className: 'temp-address strikethrough' }, inbox.address),
      h('p', { className: 'muted' }, `Its messages were deleted at ${fmtTime(new Date(inbox.expires_at))} and can’t be recovered.`),
      create))
  create.focus()
}

function renderTempDeleted(inbox: TempInbox) {
  stopTemp()
  saveTemp(null)
  setPage({ title: 'Address deleted', description: 'This temporary address was deleted.', path: '/#/temp', noindex: true })
  const create = btn('btn btn-primary btn-block', 'Create new address', 'pen')
  create.onclick = () => renderTempStart()
  simplePage('auth-main',
    h('section', { className: 'auth-card expired' },
      mascot('cat-sleep', 'mascot expired-cat'),
      h('h1', {}, 'This address is deleted.'),
      h('code', { className: 'temp-address strikethrough' }, inbox.address),
      h('p', { className: 'muted' }, 'Its messages were deleted and can’t be recovered.'),
      create))
  create.focus()
}

async function renderTempInbox(inbox: TempInbox, justCreated = false) {
  stopTemp()
  setPage({ title: 'Your temporary inbox', description: 'Your disposable IRIS address.', path: '/#/temp', noindex: true })
  // Resolve auth state so the topbar reflects a logged-in session instead of
  // always showing "Sign in" (issue: logged-in users saw "Sign in" on /#/temp).
  if (!account) {
    try { const s = await api<{ email: string; is_admin?: boolean }>('/api/v1/session'); account = { email: s.email, mb: '', is_admin: s.is_admin === true, boxes: [] } } catch { account = null }
  }
  const loggedIn = !!account
  const auth = { headers: { authorization: `Bearer ${inbox.access_token}` } }
  const base = `/api/v1/temp-inboxes/${inbox.id}`
  const expired = () => renderTempExpired(inbox)
  const discard = () => { saveTemp(null); renderTempStart() }

  // countdown (rail on wide screens, sticky bar below that)
  const countdownEl = h('span', { className: 'countdown', role: 'timer' })
  const stickyCount = h('span', { className: 'sticky-count', role: 'timer' })
  const expiryChip = h('span', { className: 'expiry-chip' })
  const expiryTime = h('div', { className: 'expiry-time' })
  const tickBar = h('div', { className: 'tick-bar', 'aria-hidden': 'true' }, ...Array.from({ length: 60 }, () => h('span', { className: 'tick' })))
  const tick = () => {
    const s = Math.max(0, Math.floor((Date.parse(inbox.expires_at) - Date.now()) / 1000))
    const t = [Math.floor(s / 3600), Math.floor((s % 3600) / 60), s % 60].map((n) => String(n).padStart(2, '0')).join(':')
    countdownEl.textContent = t
    stickyCount.textContent = `Expires in ${t.replace(/^00:/, '')}`
    expiryTime.textContent = `at ${fmtTime(new Date(inbox.expires_at))}`
    ;[countdownEl, stickyCount].forEach((el) => { el.classList.toggle('urgent', s <= 600); el.classList.toggle('danger', s <= 60) })
    expiryChip.classList.toggle('urgent', s <= 600)
    expiryChip.replaceChildren(icon('timer', 'icon icon-sm'), s <= 60 ? 'Under a minute' : s <= 600 ? `${Math.ceil(s / 60)} min left · expiring` : `${Math.ceil(s / 60)} min left`)
    const remaining = Math.ceil((s / 3600) * 60)
    tickBar.querySelectorAll('.tick').forEach((el, i) => el.classList.toggle('elapsed', i >= remaining))
    if (s === 0) expired()
  }

  const copyMain = copyButton(inbox.address, 'Copy address', 'btn btn-primary copy-btn', 'Copy')
  const addressCard = h('div', { className: 'temp-card' },
    h('div', { className: 'cat-peek' }, mascot('cat-peek')),
    h('span', { className: 'temp-label' }, justCreated ? 'Ready — your temporary address' : 'Your temporary address'),
    h('code', { className: 'temp-address' }, inbox.address),
    h('p', { className: 'temp-caption' }, 'Receive-only · Not for sensitive mail'),
    h('div', { className: 'temp-actions' }, copyMain, expiryChip))

  const countLabel = h('span', { className: 'pane-count' })
  const listHead = h('header', { className: 'temp-list-head' }, h('h2', { className: 'temp-list-title' }, 'Messages'), countLabel,
    h('span', { className: 'live-status' }, h('span', { className: 'live-dot' }), 'Live'))

  const newBtn = () => btn('btn btn-secondary btn-block', 'New address', 'pen', { onclick: discard })
  // Extend / delete: same Bearer token as every other temp call, so one
  // browser can only touch its own inbox.
  let extensionsLeft = inbox.extensions_left
  const extNote = h('p', { className: 'rail-note' })
  const extendBtn = btn('btn btn-secondary btn-block', 'Extend by 1 hour', 'clock', { onclick: () => { void doExtend() } }) as HTMLButtonElement
  const deleteBtn = btn('btn btn-ghost btn-block', 'Delete address', 'trash', { onclick: () => { void doDelete() } }) as HTMLButtonElement
  const updateExt = () => {
    extendBtn.disabled = extensionsLeft !== undefined && extensionsLeft <= 0
    extendBtn.title = extensionsLeft !== undefined && extensionsLeft <= 0 ? 'No extensions left for this address.' : 'Add 1 hour to this address.'
    extNote.textContent = extensionsLeft === undefined ? '' : extensionsLeft <= 0 ? 'No extensions left for this address.' : `${extensionsLeft} extension${extensionsLeft === 1 ? '' : 's'} left, 1 hour each.`
  }
  const doExtend = async () => {
    const restore = busy(extendBtn, 'Extending…')
    try {
      const updated = await api<TempInbox>(`${base}/extend`, { ...auth, method: 'POST' })
      inbox.expires_at = updated.expires_at
      inbox.extensions_left = updated.extensions_left
      extensionsLeft = updated.extensions_left
      saveTemp(inbox)
      updateExt()
      tick()
      toast('Address extended by 1 hour.')
    } catch (err) {
      if ([401, 404, 410].includes((err as HttpError).status)) return expired()
      if ((err as HttpError).code === 'extension_limit_reached') { extensionsLeft = 0; updateExt() }
      toast(tempErrors[(err as HttpError).code] ?? 'Could not extend this address.')
    } finally { restore() }
  }
  const doDelete = async () => {
    const ok = await confirmDialog('Delete this address?', 'The address stops working right away and its messages will be deleted. You cannot undo this.', 'Delete address')
    if (!ok) return
    const restore = busy(deleteBtn, 'Deleting…')
    try {
      await api(`${base}/delete`, { ...auth, method: 'POST' })
      renderTempDeleted(inbox)
    } catch (err) {
      if ([401, 404, 410].includes((err as HttpError).status)) return expired()
      toast('Could not delete this address. Please try again.')
    } finally { restore() }
  }
  updateExt()
  const rail = h('aside', { className: 'expiry-rail', ariaLabel: 'Expiry' },
    h('div', { className: 'expiry-label' }, 'Expires in'), countdownEl,
    expiryTime, tickBar,
    extendBtn, extNote, deleteBtn,
    newBtn(),
    h('p', { className: 'rail-note' }, 'Starting a new address forgets this one in this browser. Mail still arrives until it expires.'))
  const stickyBar = h('div', { className: 'temp-sticky' }, stickyCount, copyButton(inbox.address, 'Copy address', 'btn btn-primary btn-sm', 'Copy'))

  const topbar = h('header', { className: 'topbar' },
    brand('Temporary'),
    searchBox((q) => { query = q; draw() }),
    h('div', { className: 'topbar-right' },
      loggedIn
        ? accountChip(account!.email)
        : h('a', { href: '#/', className: 'btn btn-ghost btn-sm' }, icon('account'), h('span', { className: 'btn-label' }, 'Sign in')),
      btn('btn btn-ghost btn-icon-sm', 'Discard address', 'close', { onclick: discard })))

  const ui = mailShell({
    topbar,
    nav: [navItem('#/', 'inbox', 'Mailbox'), navItem('#/temp', 'timer', 'Temporary inbox', true), footerLinks('sidebar-footer')],
    listTop: [addressCard, listHead],
    listBottom: stickyBar,
    idle: emptyState('mail', 'No message selected', 'Messages sent to your temporary address open here.'),
    rail,
  })

  let all: Summary[] = []
  let query = ''
  let current = ''
  let loaded = false
  const draw = () => {
    const shown = all.filter((m) => matches(m, query))
    countLabel.textContent = all.length ? String(all.length) : ''
    fillList(ui.list, shown, {
      current, compact: true,
      empty: query ? emptyState('search', 'No matches', `Nothing matches “${query}”.`) : emptyState('send', 'Waiting for mail', 'Share the address above. New messages show up here automatically.'),
      onOpen: (m, i) => { current = m.id; ui.show(); openMessageAt(`${base}/messages/${encodeURIComponent(m.id)}`, ui.readerBody, auth, expired, `${i + 1} of ${shown.length}`) },
      onIntent: (m) => prefetchMessage(`${base}/messages/${encodeURIComponent(m.id)}`, auth),
    })
  }
  const load = async () => {
    try {
      const { messages } = await api<{ messages: Summary[] }>(`${base}/messages`, auth)
      // keep read state that was set locally
      const read = new Set(all.filter((m) => m.is_read).map((m) => m.id))
      all = messages.map((m) => ({ ...m, is_read: m.is_read || read.has(m.id) }))
      loaded = true
      draw()
    } catch (err) {
      if ([401, 404, 410].includes((err as HttpError).status)) return expired()
      if (!loaded) ui.list.replaceChildren(errorState(tempErrors[(err as HttpError).code] ?? 'Could not load messages.', load))
    }
  }
  // Sync the expiry and remaining extensions with the server once, so a
  // restored tab shows the truth instead of the saved snapshot.
  api<{ expires_at: string; extensions_left: number }>(base, auth)
    .then((st) => {
      inbox.expires_at = st.expires_at
      inbox.extensions_left = st.extensions_left
      extensionsLeft = st.extensions_left
      saveTemp(inbox)
      updateExt()
      tick()
    })
    .catch((err) => { if ([401, 404, 410].includes((err as HttpError).status)) expired() })
  tick(); load()
  tempTimers = [window.setInterval(tick, 1000), window.setInterval(() => { if (!document.hidden) load() }, 5_000)]
  onVisible = () => { if (!document.hidden) { tick(); load() } }
  if (justCreated) copyMain.focus()
}

// ---------- Thank-you + 404 ----------
function renderSignedOut() {
  setPage({ title: 'Signed out', description: 'You have been signed out of IRIS.', path: '/#/signed-out', noindex: true })
  simplePage('auth-main', h('section', { className: 'auth-card center' },
    mascot('cat-sleep', 'mascot expired-cat'),
    h('h1', {}, 'You’re signed out.'),
    h('p', { className: 'muted' }, 'Thanks for using IRIS. Your session and its mail-server password were removed from this device.'),
    h('a', { href: '#/', className: 'btn btn-ink btn-block' }, h('span', { className: 'btn-label' }, 'Sign in again')),
    h('a', { href: '#/temp', className: 'link' }, 'Or get a temporary inbox →')))
}

// ---------- Integration docs (public page, generated from docs/INTEGRATIONS.md) ----------
function renderIntegrationDocs() {
  setPage({ title: 'Integration docs', description: 'How companion apps send auth mail through the IRIS transactional email API.', path: '/#/docs/integrations', noindex: true })
  const article = document.createElement('article')
  article.innerHTML = INTEGRATION_DOCS_HTML
  simplePage('legal', h('p', {}, h('a', { href: '#/', className: 'link' }, '← Back to IRIS')), article)
}

function renderNotFound() {
  setPage({ title: 'Page not found', description: 'This page does not exist.', path: '/404', noindex: true })
  simplePage('auth-main', h('section', { className: 'auth-card center' },
    h('p', { className: 'big-404', 'aria-hidden': 'true' }, '404'),
    h('h1', {}, 'This message got lost.'),
    h('p', { className: 'muted' }, 'The page you’re looking for doesn’t exist or has moved.'),
    h('a', { href: '#/', className: 'btn btn-ink btn-block' }, h('span', { className: 'btn-label' }, 'Go to inbox')),
    h('a', { href: '#/temp', className: 'link' }, 'Get a temporary inbox →')))
}

// ---------- Admin dashboard ----------
type AdminUser = {
  id: string; email: string; display_name: string; role: string; status: string; created_at: string
  mailboxes: { email_address: string; quota_bytes: number; status: string }[]
  active_sessions: number; last_seen_at: string | null
}
type IntegrationAppFull = {
  id: string
  host: string
  created_at: string
  templates: { id: string; subject: string; intro: string; closing: string }[]
}

type AdminSummary = {
  users: { total: number; active: number; disabled: number; truncated: boolean; list: AdminUser[] }
  quotas: {
    sends_per_hour: number
    reads: { max: number; window_minutes: number }
    temp_inbox: { domain: string; default_expiry_hours: number; creates_per_ip_per_hour: number; active_per_client: number; message_limit: number }
    compose: { max_recipients: number; max_body_bytes: number; max_attachments: number; max_attachment_bytes: number; max_total_attachment_bytes: number }
    organize: { max_labels: number; max_custom_folders: number; max_filters: number }
    integration_email_enabled: boolean
    relay_configured: boolean
    relay_providers: string[]
  }
  delivery: {
    transactional_last_24h: Record<string, number>
    transactional_last_7d: Record<string, number>
    transactional_per_app_last_7d: Record<string, Record<string, number>>
    transactional_recent: { id: string; app_id: string; template_id: string; status: string; provider: string | null; provider_message_id: string | null; error_message: string | null; created_at: string }[]
    integration_keys: { key_id: string; app_id: string; status: string; daily_send_limit: number; last_used_at: string | null; expires_at: string | null }[]
    integration_apps: { id: string; host: string; templates: string[] }[]
    user_sends_last_24h: number
  }
  temp_inboxes: {
    by_status: Record<string, { count: number; messages: number; bytes: number }>
    created_last_24h: number
    active_list: { id: string; address: string; created_at: string; expires_at: string; message_count: number }[]
  }
}

type AuditEvent = {
  id: string; user_id: string | null; actor_email: string; action: string
  target_type: string | null; target_id: string | null; detail: Record<string, unknown>; ip: string | null; created_at: string
}
type AuditPage = { events: AuditEvent[]; next_before: string | null }

/** Admin dashboard: users, quotas, delivery status, and the actions
 * an admin can take on them. The server gates every admin endpoint,
 * so a non-admin only ever sees the "not an admin" notice below,
 * never the data. */
async function renderAdmin() {
  const { email } = await ensureAccount()
  setPage({ title: 'Admin', description: 'IRIS admin dashboard: users, quotas and delivery status.', path: '/', noindex: true })
  const back = h('a', { href: '#/', className: 'btn btn-ghost btn-sm' }, icon('back'), h('span', { className: 'btn-label' }, 'Back to mail'))
  const topbar = h('header', { className: 'topbar organize-top' }, brand(location.hostname), back,
    h('div', { className: 'topbar-right' }, accountChip(email)))
  const title = h('h1', { className: 'pane-title' }, 'Admin')
  const intro = h('p', { className: 'muted' }, 'Users, quotas and delivery status. Disable users, revoke sessions and keys, and delete temp inboxes.')
  const body = h('div', { className: 'admin-inner' })
  app.className = 'app-shell'
  app.replaceChildren(topbar, h('main', { className: 'app-main organize' }, body), mobileNav('admin', account?.is_admin === true))
  body.replaceChildren(title, intro, skeleton(4))

  const stat = (label: string, value: string) =>
    h('div', { className: 'admin-stat' }, h('strong', {}, value), h('span', {}, label))
  const countsText = (m: Record<string, number>) => {
    const parts = Object.entries(m).sort(([a], [b]) => a.localeCompare(b)).map(([k, v]) => `${k} ${v}`)
    return parts.length ? parts.join(' · ') : 'none yet'
  }
  const table = (headers: string[], rows: (string | HTMLElement)[][]) =>
    h('div', { className: 'admin-scroll' }, h('table', { className: 'admin-table' },
      h('thead', {}, h('tr', {}, ...headers.map((x) => h('th', { scope: 'col' }, x)))),
      h('tbody', {}, ...rows.map((r) => h('tr', {}, ...r.map((c) => h('td', {}, c)))))))
  const section = (heading: string, text: string, ...kids: HTMLElement[]) =>
    h('section', { className: 'org-section' }, h('h2', {}, heading), h('p', { className: 'muted' }, text), ...kids)
  /** Friendly text for the error codes the admin actions can answer. */
  const adminError = (err: unknown) => ({
    user_not_active: 'That user is already disabled.',
    user_not_disabled: 'That user is already active.',
    cannot_disable_self: 'You cannot disable your own account.',
    key_not_active: 'That key is already revoked.',
    temp_inbox_not_active: 'That inbox is already gone.',
    not_found: 'That item no longer exists. Reload the page to see the latest state.',
    unknown_app: 'That app no longer exists. Reload the page to see the latest state.',
    invalid_app: 'App names use lowercase letters, digits and hyphens.',
    unsupported_app: 'That app is not registered. Check the app name.',
    invalid_host: 'The host must be a bare hostname, no scheme or port.',
    invalid_templates: 'Each app needs 1 to 4 templates with an id, subject, intro and closing.',
    duplicate_app: 'An app with that id is already registered.',
    duplicate_host: 'Another app already uses that host.',
    app_has_active_keys: 'That app still has active keys. Revoke them first, then delete the app.',
    invalid_limits: 'Expiry must be 1 to 365 days and the daily limit 1 to 10000.',
    invalid_scopes: 'Those scopes are not allowed for this app.',
  }[(err as HttpError).code] ?? 'Something went wrong. Try again.')
  /** Confirm, run one admin action, toast the outcome, reload the page. */
  const adminAction = async (confirm: [string, string, string], path: string, done: string) => {
    if (!(await confirmDialog(confirm[0], confirm[1], confirm[2]))) return
    try { await api(path, { method: 'POST' }); toast(done); void renderAdmin() } catch (err) { toast(adminError(err)) }
  }

  try {
    const s = await api<AdminSummary>('/api/v1/admin/summary')
    const t24 = s.delivery.transactional_last_24h
    const tempActive = s.temp_inboxes.by_status.active?.count ?? 0
    const userRows = s.users.list.map((u) => {
      const actions = u.email === email
        ? h('span', { className: 'muted' }, 'This is you')
        : h('span', { className: 'admin-actions' },
          u.status === 'active'
            ? btn('btn btn-ghost btn-sm', 'Disable', undefined, { onclick: () => void adminAction(
              [`Disable ${u.email}?`, 'The account cannot sign in until you enable it again.', 'Disable user'],
              `/api/v1/admin/users/${u.id}/disable`, `${u.email} disabled`) })
            : btn('btn btn-ghost btn-sm', 'Enable', undefined, { onclick: () => void adminAction(
              [`Enable ${u.email}?`, 'The account can sign in again.', 'Enable user'],
              `/api/v1/admin/users/${u.id}/enable`, `${u.email} enabled`) }),
          btn('btn btn-ghost btn-sm', 'Revoke sessions', undefined, { onclick: () => void adminAction(
            [`Revoke sessions for ${u.email}?`, 'The account is signed out everywhere at once.', 'Revoke sessions'],
            `/api/v1/admin/users/${u.id}/revoke-sessions`, `Sessions revoked for ${u.email}`) }))
      return [
        u.email,
        u.display_name,
        u.role,
        u.status,
        u.mailboxes.length ? u.mailboxes.map((m) => `${m.email_address} (${fmtBytes(m.quota_bytes)})`).join(', ') : '—',
        String(u.active_sessions),
        u.last_seen_at ? fmtDate(u.last_seen_at) : '—',
        fmtDate(u.created_at),
        actions,
      ]
    })
    const perAppRows = Object.entries(s.delivery.transactional_per_app_last_7d)
      .sort(([a], [b]) => a.localeCompare(b))
      .map(([app, c]) => [app, String(c.accepted ?? 0), String(c.failed ?? 0), String(c.sending ?? 0), String(c.unknown ?? 0)])
    // Create-key form. Scopes follow the selected app: one per template
    // it owns, plus email:status. All are checked by default, which is
    // what the API does when scopes are omitted.
    const keyApps = s.delivery.integration_apps
    const keyApp = h('select', { id: 'key-app', className: 'org-select' },
      ...keyApps.map((a) => h('option', { value: a.id }, `${a.id} (${a.host})`))) as HTMLSelectElement
    const keyExpires = h('input', { id: 'key-expires', type: 'number', min: '1', max: '365', value: '365', autocomplete: 'off', spellcheck: false }) as HTMLInputElement
    const keyLimit = h('input', { id: 'key-limit', type: 'number', min: '1', max: '10000', value: '100', autocomplete: 'off', spellcheck: false }) as HTMLInputElement
    const keyScopes = h('div', { className: 'admin-scopes' })
    const keyFormError = h('div', { className: 'form-error', role: 'alert' })
    const keySecret = h('div', { className: 'admin-secret', hidden: true })
    const drawKeyScopes = () => {
      const app = keyApps.find((a) => a.id === keyApp.value)
      const scopes = app ? [...app.templates.map((t) => `email:template:${t}`), 'email:status'] : []
      keyScopes.replaceChildren(...scopes.map((scope) => {
        const cb = h('input', { type: 'checkbox', value: scope, checked: true }) as HTMLInputElement
        return h('label', { className: 'admin-scope' }, cb, ` ${scope}`)
      }))
    }
    keyApp.onchange = drawKeyScopes
    drawKeyScopes()
    const keyForm = h('form', { className: 'org-form' },
      h('div', { className: 'field' }, h('label', { htmlFor: 'key-app' }, 'App'), keyApp),
      h('div', { className: 'field' }, h('label', { htmlFor: 'key-expires' }, 'Expires in (days, 1 to 365)'), keyExpires),
      h('div', { className: 'field' }, h('label', { htmlFor: 'key-limit' }, 'Daily limit (1 to 10000)'), keyLimit),
      h('div', { className: 'field' }, h('label', {}, 'Scopes'), keyScopes),
      keyFormError,
      h('button', { type: 'submit', className: 'btn btn-primary' }, h('span', { className: 'btn-label' }, 'Create integration key')))
    keyForm.onsubmit = async (e) => {
      e.preventDefault()
      keyFormError.replaceChildren()
      const expires = Number(keyExpires.value)
      const limit = Number(keyLimit.value)
      if (!Number.isInteger(expires) || expires < 1 || expires > 365 || !Number.isInteger(limit) || limit < 1 || limit > 10000) {
        keyFormError.replaceChildren(h('span', {}, 'Expiry must be 1 to 365 days and the daily limit 1 to 10000.'))
        return
      }
      const scopes = [...keyScopes.querySelectorAll('input:checked')].map((cb) => (cb as HTMLInputElement).value)
      if (!scopes.length) {
        keyFormError.replaceChildren(h('span', {}, 'Pick at least one scope.'))
        return
      }
      try {
        const created = await api<{ key_id: string; app: string; secret: string; expires_at: string; daily_limit: number; scopes: string[] }>('/api/v1/admin/integration-keys', {
          method: 'POST',
          body: JSON.stringify({ app: keyApp.value, expires_days: expires, daily_limit: limit, scopes }),
        })
        keyForm.hidden = true
        keySecret.hidden = false
        const secretInput = h('input', { type: 'text', readonly: true, value: created.secret, autocomplete: 'off', spellcheck: false, 'aria-label': 'New integration key secret' }) as HTMLInputElement
        secretInput.onclick = () => secretInput.select()
        keySecret.replaceChildren(
          h('p', {}, 'Key created. Copy the secret now, it will never be shown again.'),
          h('div', { className: 'admin-secret-row' }, secretInput,
            btn('btn btn-ghost btn-sm', 'Copy', undefined, { onclick: async () => { try { await navigator.clipboard.writeText(created.secret); toast('Secret copied.') } catch { toast('Copy failed. Select the secret and copy it by hand.') } } })),
          btn('btn btn-ghost btn-sm', 'Done', undefined, { onclick: () => { void renderAdmin() } }))
      } catch (err) { keyFormError.replaceChildren(h('span', {}, adminError(err))) }
    }
    // Integration apps. The table reads the dedicated list endpoint
    // (full template text); the key form above reads the summary's
    // id-only list. Both refresh together when the page reloads.
    const appsList = await api<{ apps: IntegrationAppFull[] }>('/api/v1/admin/integration-apps')
    const appFormError = h('div', { className: 'form-error', role: 'alert' })
    const appId = h('input', { id: 'app-id', type: 'text', autocomplete: 'off', spellcheck: false, placeholder: 'myapp' }) as HTMLInputElement
    const appHost = h('input', { id: 'app-host', type: 'text', autocomplete: 'off', spellcheck: false, placeholder: 'myapp.example.com' }) as HTMLInputElement
    const templateRows = h('div', { className: 'admin-templates' })
    const appSubmit = h('button', { type: 'submit', className: 'btn btn-primary' }, h('span', { className: 'btn-label' }, 'Register app'))
    const appCancel = h('button', { type: 'button', className: 'btn btn-ghost', hidden: true }, h('span', { className: 'btn-label' }, 'Cancel'))
    const appFormTitle = h('p', {}, 'Register a new app. It can send and take keys at once, no restart needed.')
    type TemplateRow = { row: HTMLElement; id: HTMLInputElement; subject: HTMLInputElement; intro: HTMLTextAreaElement; closing: HTMLTextAreaElement }
    let templateRowList: TemplateRow[] = []
    let editingAppId: string | null = null
    const addTemplateRow = (t?: { id: string; subject: string; intro: string; closing: string }) => {
      const id = h('input', { type: 'text', autocomplete: 'off', spellcheck: false, placeholder: 'auth.verify', value: t?.id ?? '' }) as HTMLInputElement
      const subject = h('input', { type: 'text', autocomplete: 'off', spellcheck: false, placeholder: 'Verify your email', value: t?.subject ?? '' }) as HTMLInputElement
      const intro = h('textarea', { rows: '2', spellcheck: false, placeholder: 'Use this link to verify your email address.' }, t?.intro ?? '') as HTMLTextAreaElement
      const closing = h('textarea', { rows: '2', spellcheck: false, placeholder: 'If you did not create this account, ignore this message.' }, t?.closing ?? '') as HTMLTextAreaElement
      const row = h('div', { className: 'admin-template' },
        h('div', { className: 'admin-template-row' },
          h('div', { className: 'field' }, h('label', {}, 'Template id'), id),
          h('div', { className: 'field' }, h('label', {}, 'Subject'), subject)),
        h('div', { className: 'field' }, h('label', {}, 'Intro'), intro),
        h('div', { className: 'field' }, h('label', {}, 'Closing'), closing),
        h('div', {}, btn('btn btn-ghost btn-sm', 'Remove template', undefined, { onclick: () => {
          if (templateRowList.length <= 1) return
          templateRowList = templateRowList.filter((r) => r.row !== row)
          row.remove()
        } })))
      templateRowList.push({ row, id, subject, intro, closing })
      templateRows.append(row)
    }
    const resetAppForm = () => {
      editingAppId = null
      appId.value = ''
      appId.disabled = false
      appHost.value = ''
      templateRowList = []
      templateRows.replaceChildren()
      addTemplateRow()
      appSubmit.replaceChildren(h('span', { className: 'btn-label' }, 'Register app'))
      appCancel.hidden = true
      appFormTitle.replaceChildren('Register a new app. It can send and take keys at once, no restart needed.')
      appFormError.replaceChildren()
    }
    const startEditApp = (a: IntegrationAppFull) => {
      editingAppId = a.id
      appId.value = a.id
      appId.disabled = true
      appHost.value = a.host
      templateRowList = []
      templateRows.replaceChildren()
      a.templates.forEach((t) => addTemplateRow(t))
      appSubmit.replaceChildren(h('span', { className: 'btn-label' }, 'Save changes'))
      appCancel.hidden = false
      appFormTitle.replaceChildren(`Editing ${a.id}. The id cannot change; the host and templates are replaced.`)
      appFormError.replaceChildren()
    }
    const deleteApp = async (a: IntegrationAppFull) => {
      if (!(await confirmDialog(`Delete ${a.id}?`, 'The app and its templates are removed. This cannot be undone.', 'Delete app'))) return
      try { await api(`/api/v1/admin/integration-apps/${encodeURIComponent(a.id)}`, { method: 'DELETE' }); toast(`${a.id} deleted`); void renderAdmin() } catch (err) { toast(adminError(err)) }
    }
    const appForm = h('form', { className: 'admin-app-form' },
      h('div', { className: 'field' }, h('label', { htmlFor: 'app-id' }, 'App id'), appId),
      h('div', { className: 'field' }, h('label', { htmlFor: 'app-host' }, 'Host'), appHost),
      h('div', { className: 'field' }, h('label', {}, 'Templates'), templateRows,
        btn('btn btn-ghost btn-sm', 'Add template', undefined, { onclick: () => { addTemplateRow() } })),
      appFormError,
      h('div', { className: 'admin-actions' }, appSubmit, appCancel))
    appCancel.onclick = () => resetAppForm()
    appForm.onsubmit = async (e) => {
      e.preventDefault()
      appFormError.replaceChildren()
      const id = appId.value.trim().toLowerCase()
      const host = appHost.value.trim().toLowerCase()
      const templates = templateRowList.map((r) => ({
        id: r.id.value.trim(),
        subject: r.subject.value.trim(),
        intro: r.intro.value.trim(),
        closing: r.closing.value.trim(),
      }))
      if (!id || !host) {
        appFormError.replaceChildren(h('span', {}, 'Fill in the app id and host.'))
        return
      }
      if (!templates.length || templates.some((t) => !t.id || !t.subject || !t.intro || !t.closing)) {
        appFormError.replaceChildren(h('span', {}, 'Each template needs an id, subject, intro and closing.'))
        return
      }
      try {
        if (editingAppId) {
          await api(`/api/v1/admin/integration-apps/${encodeURIComponent(editingAppId)}`, {
            method: 'PUT',
            body: JSON.stringify({ host, templates }),
          })
          toast(`${editingAppId} updated`)
        } else {
          await api('/api/v1/admin/integration-apps', {
            method: 'POST',
            body: JSON.stringify({ id, host, templates }),
          })
          toast(`${id} registered`)
        }
        void renderAdmin()
      } catch (err) { appFormError.replaceChildren(h('span', {}, adminError(err))) }
    }
    resetAppForm()
    const appRows = appsList.apps.map((a) => [
      a.id,
      a.host,
      a.templates.map((t) => `${t.id} (${t.subject})`).join(', '),
      fmtDate(a.created_at),
      h('span', { className: 'admin-actions' },
        btn('btn btn-ghost btn-sm', 'Edit', undefined, { onclick: () => startEditApp(a) }),
        btn('btn btn-ghost btn-sm', 'Delete', undefined, { onclick: () => void deleteApp(a) })),
    ])
    // Audit trail. Loads after the dashboard renders, so a failure
    // here never takes the whole page down; the section shows a note
    // instead. Filters are exact action names and actor emails (or
    // user ids); Load more follows the server cursor.
    let auditEvents: AuditEvent[] = []
    let auditNext: string | null = null
    const auditWrap = h('div', {})
    const auditAction = h('input', { type: 'text', autocomplete: 'off', spellcheck: false, placeholder: 'login.success', 'aria-label': 'Filter by action' }) as HTMLInputElement
    const auditUser = h('input', { type: 'text', autocomplete: 'off', spellcheck: false, placeholder: 'actor email or user id', 'aria-label': 'Filter by user' }) as HTMLInputElement
    const auditDetail = (d: Record<string, unknown>) => { const s = JSON.stringify(d); return s === '{}' ? '—' : s }
    const drawAudit = () => {
      auditWrap.replaceChildren(
        auditEvents.length
          ? table(['Time', 'Actor', 'Action', 'Target', 'Detail'], auditEvents.map((e) => [
            fmtDate(e.created_at), e.actor_email, e.action,
            e.target_type && e.target_id ? `${e.target_type} ${e.target_id}` : '—',
            auditDetail(e.detail),
          ]))
          : h('p', { className: 'muted' }, 'No audit events match.'),
        ...(auditNext ? [h('div', { className: 'load-more' },
          h('span', { className: 'load-more-count' }, `Showing ${auditEvents.length}`),
          btn('btn btn-secondary btn-sm', 'Load more', undefined, { onclick: () => { void loadAudit() } }))] : []))
    }
    const loadAudit = async (reset = false) => {
      if (reset) { auditEvents = []; auditNext = null }
      const params = new URLSearchParams({ limit: '50' })
      if (auditNext) params.set('before', auditNext)
      if (auditAction.value.trim()) params.set('action', auditAction.value.trim())
      if (auditUser.value.trim()) params.set('user', auditUser.value.trim())
      try {
        const page = await api<AuditPage>(`/api/v1/admin/audit?${params.toString()}`)
        auditEvents = [...auditEvents, ...page.events]
        auditNext = page.next_before
        drawAudit()
      } catch {
        auditWrap.replaceChildren(h('p', { className: 'muted' }, 'Could not load the audit trail. Try again later.'))
      }
    }
    const auditFilters = h('div', { className: 'org-form' },
      h('div', { className: 'field' }, h('label', {}, 'Action'), auditAction),
      h('div', { className: 'field' }, h('label', {}, 'User'), auditUser),
      btn('btn btn-secondary btn-sm', 'Filter', undefined, { onclick: () => { void loadAudit(true) } }))
    body.replaceChildren(title, intro,
      h('div', { className: 'admin-stats' },
        stat('Users', String(s.users.total)),
        stat('Active users', String(s.users.active)),
        stat('Transactional accepted, 24h', String(t24.accepted ?? 0)),
        stat('Transactional failed, 24h', String(t24.failed ?? 0)),
        stat('User sends, 24h', String(s.delivery.user_sends_last_24h)),
        stat('Active temp inboxes', String(tempActive))),
      section('Users', s.users.truncated ? `Showing the 100 newest of ${s.users.total} users.` : `${s.users.total} users.`,
        table(['Email', 'Name', 'Role', 'Status', 'Mailboxes', 'Sessions', 'Last seen', 'Joined', 'Actions'], userRows)),
      section('Quotas', 'Limits in force for every user. Mailbox quotas are per mailbox, in the users table above.',
        table(['Limit', 'Value'], [
          ['Sends', `${s.quotas.sends_per_hour} per hour`],
          ['Message reads', `${s.quotas.reads.max} per ${s.quotas.reads.window_minutes} minutes`],
          ['Temp inbox domain', s.quotas.temp_inbox.domain],
          ['Temp inbox lifetime', `${s.quotas.temp_inbox.default_expiry_hours} hour`],
          ['Temp inbox creates', `${s.quotas.temp_inbox.creates_per_ip_per_hour} per IP per hour`],
          ['Temp inboxes per browser', String(s.quotas.temp_inbox.active_per_client)],
          ['Temp inbox message list', String(s.quotas.temp_inbox.message_limit)],
          ['Compose recipients', String(s.quotas.compose.max_recipients)],
          ['Compose body', fmtBytes(s.quotas.compose.max_body_bytes)],
          ['Compose attachments', `${s.quotas.compose.max_attachments} files, ${fmtBytes(s.quotas.compose.max_attachment_bytes)} each, ${fmtBytes(s.quotas.compose.max_total_attachment_bytes)} total`],
          ['Labels', String(s.quotas.organize.max_labels)],
          ['Custom folders', String(s.quotas.organize.max_custom_folders)],
          ['Filters', String(s.quotas.organize.max_filters)],
          ['Transactional email', s.quotas.integration_email_enabled ? 'enabled' : 'disabled'],
          ['Outbound relay', s.quotas.relay_providers.length ? s.quotas.relay_providers.join(', ') : 'not configured'],
        ])),
      section('Delivery', 'Transactional email is the API that lets companion apps send auth mail. User sends are counted in aggregate only; individual sends are not attributed.',
        h('p', { className: 'muted' }, `Last 24h: ${countsText(t24)}`),
        h('p', { className: 'muted' }, `Last 7 days: ${countsText(s.delivery.transactional_last_7d)}`),
        table(['App', 'Accepted', 'Failed', 'Sending', 'Unknown'], perAppRows.length ? perAppRows : [['—', '0', '0', '0', '0']]),
        table(['Time', 'App', 'Template', 'Status', 'Provider', 'Provider id', 'Error'], s.delivery.transactional_recent.map((r) => [
          fmtDate(r.created_at), r.app_id, r.template_id, r.status, r.provider ?? '—', r.provider_message_id ?? '—', r.error_message ?? '—',
        ])),
        table(['App', 'Status', 'Daily limit', 'Last used', 'Expires', 'Actions'], s.delivery.integration_keys.map((k) => [
          k.app_id, k.status, String(k.daily_send_limit), k.last_used_at ? fmtDate(k.last_used_at) : '—', k.expires_at ? fmtDate(k.expires_at) : '—',
          k.status === 'active'
            ? btn('btn btn-ghost btn-sm', 'Revoke', undefined, { onclick: () => void adminAction(
              [`Revoke the ${k.app_id} key?`, 'Apps using this key stop authenticating at once.', 'Revoke key'],
              `/api/v1/admin/integration-keys/${k.key_id}/revoke`, `Key revoked for ${k.app_id}`) })
            : h('span', { className: 'muted' }, '—'),
        ])),
        h('h3', {}, 'Integration apps'),
        h('p', { className: 'muted' }, 'Apps registered here can send transactional email and take keys. Changes take effect at once, no restart needed.'),
        table(['App', 'Host', 'Templates', 'Registered', 'Actions'], appRows.length ? appRows : [['—', '—', '—', '—', 'No apps registered yet.']]),
        appFormTitle,
        appForm,
        h('p', { className: 'muted' }, 'Setup guide for companion apps: ', h('a', { href: '#/docs/integrations' }, 'Integration docs')),
        h('p', {}, 'Create an integration key for a companion app. The secret is shown once, copy it before you press Done.'),
        keyForm,
        keySecret),
      section('Temp inboxes', `${s.temp_inboxes.created_last_24h} created in the last 24 hours.`,
        table(['Status', 'Inboxes', 'Messages', 'Bytes'], Object.entries(s.temp_inboxes.by_status)
          .sort(([a], [b]) => a.localeCompare(b))
          .map(([status, c]) => [status, String(c.count), String(c.messages), fmtBytes(c.bytes)])),
        ...(s.temp_inboxes.active_list.length
          ? [table(['Address', 'Created', 'Expires', 'Messages', 'Actions'], s.temp_inboxes.active_list.map((t) => [
            t.address, fmtDate(t.created_at), fmtDate(t.expires_at), String(t.message_count),
            btn('btn btn-ghost btn-sm', 'Delete', undefined, { onclick: () => void adminAction(
              [`Delete ${t.address}?`, 'The address stops working right away and its messages will be deleted. You cannot undo this.', 'Delete address'],
              `/api/v1/admin/temp-inboxes/${t.id}/delete`, `${t.address} deleted`) }),
          ]))]
          : [h('p', { className: 'muted' }, 'No active temp inboxes right now.')])),
      section('Audit trail', 'Who did what, when. Logins, sends, admin actions, key and mailbox changes. Reads are never recorded, and rows are kept forever.',
        auditFilters, auditWrap))
    void loadAudit()

  } catch (err) {
    if ((err as HttpError).status === 401) { account = null; return renderLogin() }
    if ((err as HttpError).status === 403) {
      body.replaceChildren(title, h('p', { className: 'muted' }, 'This page is only for admins. Your account is not on the admin list.'))
      return
    }
    body.replaceChildren(title, errorState('Could not load the dashboard. Try again.', () => { void renderAdmin() }))
  }
}

// ---------- Router ----------
function route() {
  stopTemp()
  window.scrollTo(0, 0)
  const r = location.hash.replace(/^#/, '') || '/'
  if (r === '/temp') {
    const saved = savedTemp()
    if (saved) { renderTempInbox(saved).catch(() => {}); return }
    return renderTempStart()
  }
  if (r === '/signed-out') return renderSignedOut()
  if (r === '/docs/integrations') return renderIntegrationDocs()
  let view: MailView | undefined
  try {
    if (r === '/') view = { kind: 'role', role: 'inbox' }
    else if (r === '/starred') view = { kind: 'starred' }
    else if (r.startsWith('/folder/')) view = { kind: 'folder', id: decodeURIComponent(r.slice(8)) }
    else if (r.startsWith('/label/')) view = { kind: 'label', name: decodeURIComponent(r.slice(7)) }
    else {
      const role = FOLDERS.find((f) => `/${f.role}` === r)?.role
      if (role) view = { kind: 'role', role }
    }
  } catch { /* malformed escape in the hash */ }
  const page = r === '/organize' ? renderOrganize() : r === '/dmarc' ? renderDmarc() : r === '/admin' ? renderAdmin() : view ? renderMailbox(view) : undefined
  if (!page) return renderNotFound()
  page.catch((err) => {
    if ((err as HttpError).status === 401) { account = null; return renderLogin() }
    setPage({ title: 'Unavailable', description: 'IRIS is temporarily unavailable.', noindex: true })
    simplePage('auth-main', h('section', { className: 'auth-card center' },
      h('h1', {}, 'IRIS is unavailable.'),
      errorState(errorText[(err as HttpError).code] ?? 'The service did not respond.', route)))
  })
}
window.addEventListener('hashchange', route)
initSite()
route()
