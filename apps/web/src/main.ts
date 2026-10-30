import { footerLinks, initSite, setPage } from './site'

type Summary = { id: string; from: string; to: string[]; subject: string; preview: string; received_at: string; is_read: boolean; is_draft?: boolean; has_attachment?: boolean }
type Detail = Summary & { text_body: string | null; html_body: string | null; cc?: string[]; message_id?: string | null; references?: string[] }
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
function readListCache(email: string, mb: string, folder: string): Summary[] | null {
  try { const c = JSON.parse(sessionStorage.getItem(LIST_KEY) ?? 'null'); return c && c.email === email && c.mb === mb ? c.folders?.[folder] ?? null : null } catch { return null }
}
function writeListCache(email: string, mb: string, folder: string, messages: Summary[]) {
  try {
    const c = JSON.parse(sessionStorage.getItem(LIST_KEY) ?? 'null')
    const folders = c && c.email === email && c.mb === mb ? c.folders ?? {} : {}
    folders[folder] = messages
    sessionStorage.setItem(LIST_KEY, JSON.stringify({ email, mb, folders }))
  } catch { /* storage full or blocked */ }
}
function clearMailCache() { messageCache.clear(); try { sessionStorage.removeItem(LIST_KEY) } catch { /* ignore */ } }

async function openMessageAt(url: string, reader: HTMLElement, init: RequestInit | undefined, onAuthFail: () => void, position: string, toolbar?: (d: Detail) => HTMLElement) {
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
      ...messageBody(m))
    reader.focus({ preventScroll: true })
  } catch (err) {
    if ([401, 410].includes((err as HttpError).status)) return onAuthFail()
    messageCache.delete(url)
    reader.replaceChildren(errorState('Could not open this message.', () => openMessageAt(url, reader, init, onAuthFail, position, toolbar)))
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
function fillList(list: HTMLElement, messages: Summary[], o: { current: string; empty: HTMLElement; compact?: boolean; showTo?: boolean; onOpen: (m: Summary, index: number, wasUnread: boolean) => void; onIntent?: (m: Summary) => void }) {
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
      ...(m.preview ? [h('span', { className: 'msg-preview' }, m.preview)] : []))
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
type FolderRole = 'inbox' | 'sent' | 'drafts' | 'archive' | 'trash'
type FolderInfo = { id: string; role: FolderRole; name: string; total: number; unread: number }
const FOLDERS: { role: FolderRole; label: string; icon: string; empty: [string, string] }[] = [
  { role: 'inbox', label: 'Inbox', icon: 'inbox', empty: ['Your inbox is clear', 'New mail for this address will appear here.'] },
  { role: 'sent', label: 'Sent', icon: 'send', empty: ['Nothing sent yet', 'Messages you send appear here.'] },
  { role: 'drafts', label: 'Drafts', icon: 'draft', empty: ['No drafts', 'Messages you start but don’t send are saved here.'] },
  { role: 'archive', label: 'Archive', icon: 'archive', empty: ['Archive is empty', 'Archive mail to clear your inbox without deleting it.'] },
  { role: 'trash', label: 'Trash', icon: 'trash', empty: ['Trash is empty', 'Deleted messages stay here until you delete them forever.'] },
]
const folderHash = (r: FolderRole) => (r === 'inbox' ? '#/' : `#/${r}`)
const folderMeta = (r: FolderRole) => FOLDERS.find((f) => f.role === r)!

/** Signed-in account, kept while switching folders so each switch skips two requests. */
let account: { email: string; mb: string } | null = null

const sendErrors: Record<string, string> = {
  external_sending_disabled: 'IRIS can only send to @meowlabs.id addresses for now. Sending to other domains isn’t set up yet.',
  too_many_sends: 'You’ve reached the sending limit (100 per hour). Try again later.',
  invalid_recipient: 'One of the addresses isn’t valid.',
  no_recipients: 'Add at least one recipient.',
  too_many_recipients: 'Too many recipients (50 max).',
  send_rejected: 'The mail server refused this message.',
  network: errorText.network,
}

// ----- toast + confirm -----
function toast(text: string, action?: { label: string; run: () => void }) {
  document.querySelector('.toast')?.remove()
  const t = h('div', { className: 'toast', role: 'status' }, h('span', {}, text))
  if (action) t.append(h('button', { type: 'button', className: 'toast-action', onclick: () => { t.remove(); action.run() } }, action.label))
  document.body.append(t)
  setTimeout(() => t.remove(), action ? 7000 : 4000)
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

// ----- composer -----
type ComposeDraft = { draftId?: string; to?: string[]; cc?: string[]; bcc?: string[]; subject?: string; text?: string; inReplyTo?: string | null; references?: string[]; title?: string }
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

async function openComposer(base: string, d: ComposeDraft, onChange: () => void) {
  if (closeComposer) await closeComposer()
  let draftId = d.draftId
  let dirty = false
  let saving: Promise<void> | null = null
  let timer = 0
  let closed = false

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
  const body = h('textarea', { className: 'compose-body', ariaLabel: 'Message', value: d.text ?? '', placeholder: 'Write your message…' }) as HTMLTextAreaElement
  const showCc = !!(d.cc?.length || d.bcc?.length)
  cc.row.hidden = bcc.row.hidden = !showCc
  const ccToggle = h('button', { type: 'button', className: 'linklike cc-toggle', hidden: showCc }, 'Cc / Bcc')
  ccToggle.onclick = () => { cc.row.hidden = bcc.row.hidden = false; ccToggle.hidden = true; cc.el.focus() }
  to.row.append(ccToggle)

  const status = h('span', { className: 'compose-status', 'aria-live': 'polite' })
  const error = h('div', { className: 'form-error', role: 'alert' })
  const send = h('button', { type: 'button', className: 'btn btn-primary' }, icon('send'), h('span', { className: 'btn-label' }, 'Send')) as HTMLButtonElement
  const discard = btn('btn btn-ghost btn-icon-sm', 'Discard draft', 'trash')
  const closeBtn = btn('btn btn-ghost btn-icon-sm', 'Close (saves draft)', 'close')
  const panel = h('section', { className: 'composer', role: 'dialog', ariaLabel: d.title ?? 'New message' },
    h('header', { className: 'composer-head' }, h('h2', {}, d.title ?? 'New message'), status, closeBtn),
    h('div', { className: 'composer-fields' }, to.row, to.err, cc.row, cc.err, bcc.row, bcc.err, subject.row),
    body,
    error,
    h('footer', { className: 'composer-foot' }, send, h('span', { className: 'compose-hint' }, 'Ctrl + Enter to send'), discard))

  const payload = () => ({
    to: splitAddrs(to.el.value), cc: splitAddrs(cc.el.value), bcc: splitAddrs(bcc.el.value),
    subject: subject.el.value.replace(/[\r\n]+/g, ' '), text: body.value,
    in_reply_to: d.inReplyTo ?? null, references: d.references ?? [], draft_id: draftId ?? null,
  })
  const isEmpty = () => !to.el.value.trim() && !cc.el.value.trim() && !bcc.el.value.trim() && !subject.el.value.trim() && !body.value.trim()

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
    if (id) { await api(`${base}/messages/${encodeURIComponent(id)}`, { method: 'DELETE' }).catch(() => {}); onChange() }
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
      await api(`${base}/send`, { method: 'POST', body: JSON.stringify({ ...p, draft_id: draftId ?? null }) })
      closed = true; closeComposer = null
      document.removeEventListener('keydown', onKey)
      panel.classList.add('is-closing')
      setTimeout(() => panel.remove(), 160)
      toast('Message sent')
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
  ;(d.to?.length ? (d.subject ? body : subject.el) : to.el).focus()
  if (d.to?.length && d.subject) body.setSelectionRange(0, 0)
}

// ----- mailbox view -----
async function renderMailbox(folder: FolderRole = 'inbox') {
  if (!account) {
    const [session, { mailboxes }] = await Promise.all([
      api<{ email: string }>('/api/v1/session'),
      api<{ mailboxes: { id: string }[] }>('/api/v1/mailboxes'),
    ])
    account = { email: session.email, mb: mailboxes[0]?.id ?? '' }
  }
  const { email, mb } = account
  const meta = folderMeta(folder)
  const base = `/api/v1/mailboxes/${mb}`
  setPage({ title: meta.label, description: `Your IRIS ${meta.label.toLowerCase()}.`, path: '/', noindex: true })

  let all: Summary[] = (mb && readListCache(email, mb, folder)) || []
  let query = ''
  let current = ''

  const signOut = () => { clearMailCache(); account = null; closeComposer?.() }
  const logout = btn('btn btn-ghost btn-icon-sm', 'Sign out', 'logout')
  logout.onclick = async () => { busy(logout, ''); signOut(); await api('/auth/logout', { method: 'POST' }).catch(() => {}); location.hash = '#/signed-out' }
  const refresh = btn('btn btn-ghost btn-icon-sm', 'Refresh', 'refresh')
  const synced = h('span', { className: 'synced', 'aria-live': 'polite' })
  const unreadChip = h('span', { className: 'pane-chip', hidden: true })
  const counts = new Map(FOLDERS.map((f) => [f.role, h('span', { className: 'nav-count' })]))
  const tabCounts = new Map(FOLDERS.map((f) => [f.role, h('span', { className: 'tab-count' })]))

  const compose = (d: ComposeDraft = {}) => { if (mb) openComposer(base, d, onMutate) }
  const composeBtn = btn('btn btn-primary compose-btn', 'Compose', 'pen', { onclick: () => compose() })
  const fab = btn('btn btn-primary fab', 'Compose', 'pen', { onclick: () => compose() })

  const topbar = h('header', { className: 'topbar' },
    brand(location.hostname),
    searchBox((q) => { query = q; draw() }),
    h('div', { className: 'topbar-right' }, synced,
      h('span', { className: 'account-chip', title: email }, icon('account'), h('span', { className: 'account-email' }, email)),
      logout))

  const folderTabs = h('nav', { className: 'folder-tabs', ariaLabel: 'Folders' }, ...FOLDERS.map((f) =>
    h('a', { href: folderHash(f.role), className: 'folder-tab', ...(f.role === folder ? { 'aria-current': 'page' } : {}) }, f.label, tabCounts.get(f.role)!)))

  const idle = () => emptyState('mail', 'No message selected', 'Choose a message from the list to read it here.')
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
      h('a', { href: folderHash(folder), className: 'nav-item nav-mail', 'aria-current': 'page' }, icon('mail'), h('span', { className: 'nav-label' }, 'Mail')),
      navSection('Temporary'),
      navItem('#/temp', 'timer', 'Temporary inbox'),
      footerLinks('sidebar-footer'),
    ],
    listTop: [h('header', { className: 'pane-head' }, h('h2', { className: 'pane-title' }, meta.label), unreadChip, refresh), folderTabs],
    listBottom: fab,
    idle: idle(),
  })

  const url = (id: string) => `${base}/messages/${encodeURIComponent(id)}`

  const loadFolders = async () => {
    if (!mb) return
    try {
      const { folders } = await api<{ folders: FolderInfo[] }>(`${base}/folders`)
      for (const f of folders) {
        const n = f.role === 'inbox' ? f.unread : f.role === 'drafts' ? f.total : 0
        counts.get(f.role)!.textContent = n ? String(n) : ''
        tabCounts.get(f.role)!.textContent = n ? String(n) : ''
      }
    } catch { /* counts are optional */ }
  }

  const removeLocal = (id: string) => {
    all = all.filter((m) => m.id !== id)
    writeListCache(email, mb, folder, all)
    if (current === id) { current = ''; ui.hide(idle()) }
    draw()
  }

  const move = async (m: Summary, target: FolderRole) => {
    removeLocal(m.id)
    try {
      await api(`${url(m.id)}/move`, { method: 'POST', body: JSON.stringify({ folder: target }) })
      toast(target === 'trash' ? 'Moved to Trash' : `Moved to ${folderMeta(target).label}`, {
        label: 'Undo',
        run: async () => {
          await api(`${url(m.id)}/move`, { method: 'POST', body: JSON.stringify({ folder }) }).catch(() => toast('Could not undo.'))
          load(); loadFolders()
        },
      })
      loadFolders()
    } catch {
      toast('Could not move the message.'); load()
    }
  }

  const deleteForever = async (m: Summary) => {
    if (!(await confirmDialog('Delete forever?', 'This message will be permanently deleted. This can’t be undone.', 'Delete forever'))) return
    removeLocal(m.id)
    try { await api(url(m.id), { method: 'DELETE' }); messageCache.delete(url(m.id)); toast('Message deleted forever'); loadFolders() }
    catch { toast('Could not delete the message.'); load() }
  }

  const markUnread = async (m: Summary) => {
    m.is_read = false
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
    if (folder === 'inbox') icons.push(ib('Archive', 'archive', () => move(m, 'archive')))
    if (folder === 'archive') icons.push(ib('Move to Inbox', 'inbox', () => move(m, 'inbox')))
    if (folder === 'trash') {
      actions.push(b('Restore to Inbox', 'inbox', () => move(m, 'inbox')))
      actions.push(b('Delete forever', 'trash', () => deleteForever(m), 'btn btn-danger btn-sm'))
    } else {
      icons.push(ib('Delete', 'trash', () => move(m, 'trash')))
    }
    if (folder === 'inbox' || folder === 'archive') icons.push(ib('Mark as unread', 'mail', () => markUnread(m)))
    return h('div', { className: 'reader-toolbar' }, h('div', { className: 'reader-actions' }, ...actions), h('div', { className: 'reader-icons' }, ...icons))
  }

  const openDraft = async (m: Summary) => {
    try {
      const { message: d } = await api<{ message: Detail }>(url(m.id))
      compose({ title: 'Draft', draftId: m.id, to: d.to.map(emailOf), cc: (d.cc ?? []).map(emailOf), subject: d.subject, text: d.text_body ?? '' })
    } catch { toast('Could not open the draft.') }
  }

  const draw = () => {
    const shown = all.filter((m) => matches(m, query))
    const unread = all.filter((m) => !m.is_read).length
    unreadChip.textContent = `${unread} unread`
    unreadChip.hidden = folder !== 'inbox' || !unread
    const [title, text] = meta.empty
    fillList(ui.list, shown, {
      current,
      showTo: folder === 'sent' || folder === 'drafts',
      empty: query ? emptyState('search', 'No matches', `Nothing in ${meta.label} matches “${query}”.`) : emptyState(meta.icon, title, text),
      onOpen: (m, i, wasUnread) => {
        if (folder === 'drafts') return void openDraft(m)
        if (wasUnread) {
          writeListCache(email, mb, folder, all)
          api(`${url(m.id)}/seen`, { method: 'POST', body: JSON.stringify({ seen: true }) }).then(loadFolders, () => {})
        }
        current = m.id
        draw()
        ui.show()
        openMessageAt(url(m.id), ui.readerBody, undefined, () => { account = null; renderLogin() }, `${i + 1} of ${shown.length}`, toolbar(m))
      },
      onIntent: folder === 'drafts' ? undefined : (m) => prefetchMessage(url(m.id)),
    })
  }

  const load = async () => {
    if (!mb) { ui.list.replaceChildren(emptyState('inbox', 'No mailbox yet', 'Your account has no mailbox. Contact your administrator.')); return }
    if (!all.length) ui.list.replaceChildren(skeleton())
    const restore = busy(refresh, '')
    refresh.classList.add('is-spinning')
    try {
      const fresh = (await api<{ messages: Summary[] }>(`${base}/messages?folder=${folder}&limit=50`)).messages
      const read = new Set(all.filter((m) => m.is_read).map((m) => m.id))
      all = fresh.map((m) => ({ ...m, is_read: m.is_read || read.has(m.id) }))
      writeListCache(email, mb, folder, all)
      synced.textContent = `Synced ${fmtTime(new Date())}`
      draw()
    } catch (err) {
      if ((err as HttpError).status === 401) { signOut(); return renderLogin() }
      if (all.length) { synced.textContent = 'Offline · showing saved list'; return }
      ui.list.replaceChildren(errorState(errorText[(err as HttpError).code] ?? 'Could not load messages.', load))
    } finally { restore(); refresh.classList.remove('is-spinning') }
  }
  // After sending or saving: refresh counts, and the list if it's the folder that changed.
  function onMutate() {
    loadFolders()
    if (folder === 'drafts' || folder === 'sent') load()
    if (folder === 'inbox') setTimeout(load, 1500) // a message sent to yourself arrives shortly
  }

  refresh.onclick = () => { load(); loadFolders() }
  loadFolders()
  // New mail shows up on its own while the tab is visible.
  tempTimers.push(window.setInterval(() => { if (!document.hidden) { load(); loadFolders() } }, 30_000))
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
  const folder = r === '/' ? 'inbox' : FOLDERS.find((f) => `/${f.role}` === r)?.role
  if (!folder) return renderNotFound()
  renderMailbox(folder).catch((err) => {
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
