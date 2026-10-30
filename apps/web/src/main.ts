import { footerLinks, initSite, setPage } from './site'

type Summary = { id: string; from: string; to: string[]; subject: string; preview: string; received_at: string; is_read: boolean }
type Detail = Summary & { text_body: string | null; html_body: string | null }
type TempInbox = { id: string; address: string; access_token: string; expires_at: string }

const app = document.querySelector<HTMLDivElement>('#app')!

// Requests started by the inline script in index.html before this bundle loaded.
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
  mail_server_unavailable: 'Mail server is unavailable. Try again shortly.',
  network: 'Can’t reach IRIS. Check your connection and try again.',
}
const tempErrors: Record<string, string> = {
  rate_limited: 'Too many addresses created from your network. Try again later.',
  quota_exceeded: 'You already have 3 active temporary addresses. Wait for one to expire.',
  network: errorText.network,
}

const brand = (sub: string) => h('a', { href: '#/', className: 'brand', ariaLabel: 'IRIS home' }, h('span', { className: 'brand-mark' }, 'IRIS'), h('span', { className: 'brand-sub' }, sub))

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
  <path class="pupil" d="M83 82.5C81 87 81 97 83 101C85 97 85 87 83 82.5Z" fill="#16130F" style="transform-box:fill-box;transform-origin:center;"/>
  <path d="M104 92Q117 79 130 92Q117 103 104 92Z" fill="#B83232"/>
  <path class="pupil" d="M117 82.5C115 87 115 97 117 101C119 97 119 87 117 82.5Z" fill="#16130F" style="transform-box:fill-box;transform-origin:center;"/>
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
  const email = h('input', { type: 'email', name: 'email', id: 'email', autocomplete: 'username', inputMode: 'email', placeholder: 'you@meowlabs.id', spellcheck: false }) as HTMLInputElement
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

  const left = h('div', { className: 'login-left' },
    h('div', { className: 'topline' }, h('span', { className: 'wordmark' }, 'IRIS'), h('span', {}, 'Meow Labs')),
    h('p', { className: 'login-tagline' }, h('span', {}, 'Every'), h('span', {}, 'message'), h('span', {}, 'finds'), h('span', {}, 'its way.')),
    h('div', { className: 'login-route', 'aria-hidden': 'true' }, h('div', { className: 'route-line' }), h('div', { className: 'route-nodes' }, h('span', { className: 'route-node-sq' }), h('span', { className: 'route-node-sq' }), h('span', { className: 'route-node-rd' })), catWrap),
    h('div', { className: 'login-footer' }, h('span', {}, 'MX → Auth → Inbox'), h('span', {}, location.hostname)))
  const form = h('form', { className: 'login-form', noValidate: true },
    emailF.wrap, pwF.wrap, status, submit,
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
    const restore = busy(submit, 'Signing in…')
    try {
      await api('/auth/login', { method: 'POST', body: JSON.stringify({ email: email.value.trim(), password: password.value }) })
      await renderMailbox()
    } catch (err) {
      const code = (err as HttpError).code
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

function emptyState(ic: string, title: string, text: string) {
  return h('div', { className: 'empty' }, h('span', { className: 'empty-art' }, icon(ic, 'icon icon-xl')), h('p', { className: 'empty-title' }, title), h('p', { className: 'empty-text' }, text))
}
const errorState = (text: string, retry?: () => void) => h('div', { className: 'error-state', role: 'alert' },
  h('p', { className: 'form-error' }, icon('alert'), text), ...(retry ? [btn('btn btn-secondary', 'Try again', 'refresh', { onclick: retry })] : []))

const REMOTE_IMG = /<img\b[^>]*\bsrc\s*=\s*["']?\s*(https?:)?\/\//i
function messageBody(m: Detail) {
  if (!m.html_body) return [h('pre', { className: 'mail-text' }, m.text_body ?? '')]
  const frame = (allowRemote: boolean) => {
    const csp = `<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; img-src data: cid:${allowRemote ? ' https:' : ''}">`
    const f = h('iframe', { className: 'mail-frame', title: 'Message body', srcdoc: csp + m.html_body, loading: 'lazy' })
    f.setAttribute('sandbox', '')
    f.setAttribute('referrerpolicy', 'no-referrer')
    return f
  }
  let body = frame(false)
  if (!REMOTE_IMG.test(m.html_body)) return [body]
  const load = btn('btn btn-secondary btn-sm', 'Load images once')
  const bar = h('div', { className: 'remote-bar' }, icon('eye'), h('span', {}, h('strong', {}, 'Remote images blocked'), ' to protect your privacy.'), load)
  load.onclick = () => { const next = frame(true); body.replaceWith(next); body = next; bar.remove() }
  return [bar, body]
}

// Message bodies don't change: keep them (as promises) for instant re-open and hover prefetch.
const messageCache = new Map<string, Promise<{ message: Detail }>>()
function prefetchMessage(url: string, init?: RequestInit) {
  let p = messageCache.get(url)
  if (!p) {
    p = api<{ message: Detail }>(url, init)
    p.catch(() => messageCache.delete(url))
    if (messageCache.size > 100) messageCache.delete(messageCache.keys().next().value!)
    messageCache.set(url, p)
  }
  return p
}

// Last message list per account, per tab (sessionStorage is cleared when the tab closes).
const LIST_KEY = 'iris_list_cache'
function readListCache(email: string, mb?: string): Summary[] | null {
  try { const c = JSON.parse(sessionStorage.getItem(LIST_KEY) ?? 'null'); return c && c.email === email && c.mb === mb ? c.messages : null } catch { return null }
}
function writeListCache(email: string, mb: string, messages: Summary[]) {
  try { sessionStorage.setItem(LIST_KEY, JSON.stringify({ email, mb, messages })) } catch { /* storage full or blocked */ }
}
function clearMailCache() { messageCache.clear(); try { sessionStorage.removeItem(LIST_KEY) } catch { /* ignore */ } }

async function openMessageAt(url: string, reader: HTMLElement, init: RequestInit | undefined, onAuthFail: () => void, position: string) {
  if (!messageCache.has(url)) reader.replaceChildren(readerSkeleton())
  try {
    const { message: m } = await prefetchMessage(url, init)
    const initial = (senderName(m.from).replace(/[^\p{L}\p{N}]/gu, '')[0] ?? '?').toUpperCase()
    reader.replaceChildren(
      h('p', { className: 'reader-pos' }, position),
      h('h1', { className: 'reader-subject' }, m.subject || '(no subject)'),
      h('div', { className: 'reader-head' }, h('span', { className: 'avatar', 'aria-hidden': 'true' }, initial),
        h('dl', { className: 'reader-meta' },
          h('dt', {}, 'From'), h('dd', { className: 'reader-from' }, m.from),
          h('dt', {}, 'To'), h('dd', {}, m.to.join(', ')),
          h('dt', {}, 'Date'), h('dd', {}, h('time', { dateTime: m.received_at }, fmtDate(m.received_at))))),
      ...messageBody(m))
    reader.focus({ preventScroll: true })
  } catch (err) {
    if ([401, 410].includes((err as HttpError).status)) return onAuthFail()
    messageCache.delete(url)
    reader.replaceChildren(errorState('Could not open this message.', () => openMessageAt(url, reader, init, onAuthFail, position)))
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
  return { list, readerBody, show: () => { layout.dataset.view = 'reader'; reader.scrollTop = 0 } }
}

/** Render message rows. Rows are buttons; an optional OTP copy button sits beside each row (never nested). */
function fillList(list: HTMLElement, messages: Summary[], o: { current: string; empty: HTMLElement; compact?: boolean; onOpen: (m: Summary, index: number) => void; onIntent?: (m: Summary) => void }) {
  if (!messages.length) return list.replaceChildren(o.empty)
  const prevList = list.querySelector('.message-list')
  const seen = new Set([...list.querySelectorAll<HTMLElement>('.msg-item')].map((li) => li.dataset.id))
  list.replaceChildren(h('ul', { className: `message-list${prevList ? '' : ' stagger'}`, ariaLabel: 'Messages' }, ...messages.map((m, i) => {
    const mark = m.is_read ? [] : [h('span', { className: 'iris-mark', title: 'Unread' }, h('span', { className: 'sr-only' }, 'Unread: '))]
    const row = h('button', { type: 'button', className: `msg${m.is_read ? '' : ' unread'}${o.compact ? ' compact' : ''}` },
      h('span', { className: 'msg-from' }, ...mark, h('span', { className: 'msg-from-name' }, senderName(m.from))),
      h('time', { className: 'msg-date', dateTime: m.received_at, title: fmtDate(m.received_at) }, fmtShort(m.received_at)),
      h('span', { className: 'msg-subject' }, m.subject || '(no subject)'),
      ...(m.preview ? [h('span', { className: 'msg-preview' }, m.preview)] : []))
    if (m.id === o.current) row.setAttribute('aria-current', 'true')
    if (o.onIntent) { const intent = () => o.onIntent!(m); row.addEventListener('pointerenter', intent, { once: true }); row.addEventListener('focus', intent, { once: true }); row.addEventListener('touchstart', intent, { once: true, passive: true }) }
    row.onclick = () => {
      list.querySelectorAll('.msg').forEach((b) => b.removeAttribute('aria-current'))
      row.setAttribute('aria-current', 'true')
      row.classList.remove('unread')
      row.querySelector('.iris-mark')?.remove()
      m.is_read = true
      o.onOpen(m, i)
    }
    const li = h('li', { className: `msg-item${prevList && !seen.has(m.id) ? ' is-new' : ''}` }, row)
    li.dataset.id = m.id
    li.style.setProperty('--i', String(Math.min(i, 12)))
    const code = o.compact ? `${m.subject} ${m.preview}`.match(/\b(\d{4,8})\b/)?.[1] : undefined
    if (code) li.append(copyButton(code, `Copy code ${code}`, 'otp-chip', code))
    return li
  })))
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
  const input = h('input', { type: 'search', placeholder: 'Search this list', ariaLabel: 'Search messages', autocomplete: 'off' }) as HTMLInputElement
  input.addEventListener('input', () => onSearch(input.value.trim().toLowerCase()))
  return h('label', { className: 'topbar-search' }, icon('search'), input)
}
const matches = (m: Summary, q: string) => !q || `${m.from} ${m.subject} ${m.preview}`.toLowerCase().includes(q)

// ---------- Mailbox ----------
async function renderMailbox() {
  const [session, { mailboxes }] = await Promise.all([
    api<{ email: string }>('/api/v1/session'),
    api<{ mailboxes: { id: string }[] }>('/api/v1/mailboxes'),
  ])
  const mb = mailboxes[0]?.id
  const cached = readListCache(session.email, mb)
  setPage({ title: 'Inbox', description: 'Your IRIS inbox.', path: '/', noindex: true })

  let all: Summary[] = cached ?? []
  let query = ''
  let current = ''

  const logout = btn('btn btn-ghost btn-icon-sm', 'Sign out', 'logout')
  logout.onclick = async () => { busy(logout, ''); clearMailCache(); await api('/auth/logout', { method: 'POST' }).catch(() => {}); location.hash = '#/signed-out' }
  const refresh = btn('btn btn-ghost btn-icon-sm', 'Refresh', 'refresh')
  const synced = h('span', { className: 'synced', 'aria-live': 'polite' })
  const unreadChip = h('span', { className: 'pane-chip' })
  const navCount = h('span', { className: 'nav-count' })

  const topbar = h('header', { className: 'topbar' },
    brand(location.hostname),
    searchBox((q) => { query = q; draw() }),
    h('div', { className: 'topbar-right' }, synced,
      h('span', { className: 'account-chip', title: session.email }, icon('account'), h('span', { className: 'account-email' }, session.email)),
      logout))

  const ui = mailShell({
    topbar,
    nav: [
      navSection('Mailbox'),
      navItem('#/', 'inbox', 'Inbox', true, navCount),
      navSection('Temporary'),
      navItem('#/temp', 'timer', 'Temporary inbox'),
      footerLinks('sidebar-footer'),
    ],
    listTop: [h('header', { className: 'pane-head' }, h('h2', { className: 'pane-title' }, 'Inbox'), unreadChip, refresh)],
    idle: emptyState('mail', 'No message selected', 'Choose a message from the list to read it here.'),
  })

  const draw = () => {
    const shown = all.filter((m) => matches(m, query))
    const unread = all.filter((m) => !m.is_read).length
    unreadChip.textContent = `${unread} unread`
    unreadChip.hidden = !unread
    navCount.textContent = unread ? String(unread) : ''
    fillList(ui.list, shown, {
      current,
      empty: query ? emptyState('search', 'No matches', `Nothing in this list matches “${query}”.`) : emptyState('inbox', 'Your inbox is clear', 'New mail for this address will appear here.'),
      onOpen: (m, i) => {
        current = m.id
        draw()
        ui.show()
        openMessageAt(`/api/v1/mailboxes/${mb}/messages/${encodeURIComponent(m.id)}`, ui.readerBody, undefined, renderLogin, `${i + 1} of ${shown.length}`)
      },
      onIntent: (m) => prefetchMessage(`/api/v1/mailboxes/${mb}/messages/${encodeURIComponent(m.id)}`),
    })
  }

  const load = async () => {
    if (!mb) { ui.list.replaceChildren(emptyState('inbox', 'No mailbox yet', 'Your account has no mailbox. Contact your administrator.')); return }
    if (!all.length) ui.list.replaceChildren(skeleton())
    const restore = busy(refresh, '')
    refresh.classList.add('is-spinning')
    try {
      const fresh = (await api<{ messages: Summary[] }>(`/api/v1/mailboxes/${mb}/messages?limit=50`)).messages
      // keep read state set locally while the request was in flight
      const read = new Set(all.filter((m) => m.is_read).map((m) => m.id))
      all = fresh.map((m) => ({ ...m, is_read: m.is_read || read.has(m.id) }))
      writeListCache(session.email, mb, all)
      synced.textContent = `Synced ${fmtTime(new Date())}`
      draw()
    } catch (err) {
      if ((err as HttpError).status === 401) { clearMailCache(); return renderLogin() }
      if (all.length) { synced.textContent = 'Offline · showing saved list'; return }
      ui.list.replaceChildren(errorState(errorText[(err as HttpError).code] ?? 'Could not load messages.', load))
    } finally { restore(); refresh.classList.remove('is-spinning') }
  }
  refresh.onclick = load
  if (all.length) { draw(); load() } else await load()
}

// ---------- Temp inbox ----------
let tempTimers: number[] = []
let onVisible: (() => void) | null = null
document.addEventListener('visibilitychange', () => onVisible?.())
const stopTemp = () => { tempTimers.forEach(clearInterval); tempTimers = []; onVisible = null }
const TEMP_KEY = 'iris_temp_inbox'
const savedTemp = (): TempInbox | null => { try { return JSON.parse(sessionStorage.getItem(TEMP_KEY) ?? 'null') } catch { return null } }
const saveTemp = (t: TempInbox | null) => { try { t ? sessionStorage.setItem(TEMP_KEY, JSON.stringify(t)) : sessionStorage.removeItem(TEMP_KEY) } catch { /* storage blocked */ } }

function simplePage(cls: string, ...kids: HTMLElement[]) {
  app.className = 'auth-shell'
  app.replaceChildren(h('header', { className: 'simple-top' }, brand('Meow Labs')), h('main', { className: cls }, ...kids), footerLinks())
}

function renderTempStart(note = '') {
  stopTemp()
  setPage({ title: 'Free temporary email', description: 'Get a free disposable email address in one click. Receive-only, no sign-up, deleted automatically after 1 hour.', path: '/#/temp' })
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
      h('p', { className: 'auth-alt' }, 'Have an account? ', h('a', { href: '#/', className: 'link' }, 'Sign in →'))))
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

function renderTempInbox(inbox: TempInbox, justCreated = false) {
  stopTemp()
  setPage({ title: 'Your temporary inbox', description: 'Your disposable IRIS address.', path: '/#/temp', noindex: true })
  const auth = { headers: { authorization: `Bearer ${inbox.access_token}` } }
  const base = `/api/v1/temp-inboxes/${inbox.id}`
  const expired = () => renderTempExpired(inbox)
  const discard = () => { saveTemp(null); renderTempStart() }

  // countdown (rail on wide screens, sticky bar below that)
  const countdownEl = h('span', { className: 'countdown', role: 'timer' })
  const stickyCount = h('span', { className: 'sticky-count', role: 'timer' })
  const expiryChip = h('span', { className: 'expiry-chip' })
  const tickBar = h('div', { className: 'tick-bar', 'aria-hidden': 'true' }, ...Array.from({ length: 60 }, () => h('span', { className: 'tick' })))
  const tick = () => {
    const s = Math.max(0, Math.floor((Date.parse(inbox.expires_at) - Date.now()) / 1000))
    const t = [Math.floor(s / 3600), Math.floor((s % 3600) / 60), s % 60].map((n) => String(n).padStart(2, '0')).join(':')
    countdownEl.textContent = t
    stickyCount.textContent = `Expires in ${t.replace(/^00:/, '')}`
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
  const rail = h('aside', { className: 'expiry-rail', ariaLabel: 'Expiry' },
    h('div', { className: 'expiry-label' }, 'Expires in'), countdownEl,
    h('div', { className: 'expiry-time' }, `at ${fmtTime(new Date(inbox.expires_at))}`), tickBar,
    newBtn(),
    h('p', { className: 'rail-note' }, 'Starting a new address forgets this one in this browser. Mail still arrives until it expires.'))
  const stickyBar = h('div', { className: 'temp-sticky' }, stickyCount, copyButton(inbox.address, 'Copy address', 'btn btn-primary btn-sm', 'Copy'))

  const topbar = h('header', { className: 'topbar' },
    brand('Temporary'),
    searchBox((q) => { query = q; draw() }),
    h('div', { className: 'topbar-right' }, h('a', { href: '#/', className: 'btn btn-ghost btn-sm' }, icon('account'), h('span', { className: 'btn-label' }, 'Sign in')),
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

function renderNotFound() {
  setPage({ title: 'Page not found', description: 'This page does not exist.', path: '/404', noindex: true })
  simplePage('auth-main', h('section', { className: 'auth-card center' },
    h('p', { className: 'big-404', 'aria-hidden': 'true' }, '404'),
    h('h1', {}, 'This message got lost.'),
    h('p', { className: 'muted' }, 'The page you’re looking for doesn’t exist or has moved.'),
    h('a', { href: '#/', className: 'btn btn-ink btn-block' }, h('span', { className: 'btn-label' }, 'Go to inbox')),
    h('a', { href: '#/temp', className: 'link' }, 'Get a temporary inbox →')))
}

// ---------- Router ----------
function route() {
  stopTemp()
  window.scrollTo(0, 0)
  const r = location.hash.replace(/^#/, '') || '/'
  if (r === '/temp') {
    const saved = savedTemp()
    return saved ? renderTempInbox(saved) : renderTempStart()
  }
  if (r === '/signed-out') return renderSignedOut()
  if (r !== '/') return renderNotFound()
  renderMailbox().catch((err) => {
    if ((err as HttpError).status === 401) return renderLogin()
    setPage({ title: 'Unavailable', description: 'IRIS is temporarily unavailable.', noindex: true })
    simplePage('auth-main', h('section', { className: 'auth-card center' },
      h('h1', {}, 'IRIS is unavailable.'),
      errorState(errorText[(err as HttpError).code] ?? 'The service did not respond.', route)))
  })
}
window.addEventListener('hashchange', route)
initSite()
route()
