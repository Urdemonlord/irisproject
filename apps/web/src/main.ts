type Summary = {
  id: string; from: string; to: string[]; subject: string; preview: string;
  received_at: string; is_read: boolean
}
type Detail = Summary & { text_body: string | null; html_body: string | null }

const app = document.querySelector<HTMLDivElement>('#app')!

class HttpError extends Error { constructor(public status: number, public code: string) { super(code) } }

const csrfCookie = () => document.cookie.split('; ').find((c) => c.startsWith('iris_csrf='))?.slice(10)

async function api<T>(path: string, init: RequestInit = {}): Promise<T> {
  const method = (init.method ?? 'GET').toUpperCase()
  const headers: Record<string, string> = { 'content-type': 'application/json', ...(init.headers as Record<string, string>) }
  if (method !== 'GET' && method !== 'HEAD') {
    // Double-submit CSRF token: server compares this header to the iris_csrf cookie.
    if (!csrfCookie()) await fetch('/auth/csrf', { credentials: 'same-origin' })
    headers['x-csrf-token'] = csrfCookie() ?? ''
  }
  const res = await fetch(path, { credentials: 'same-origin', ...init, headers })
  if (!res.ok) {
    const body = await res.json().catch(() => ({}))
    throw new HttpError(res.status, body.error ?? 'request_failed')
  }
  return res.status === 204 ? (undefined as T) : res.json()
}

function h<K extends keyof HTMLElementTagNameMap>(tag: K, props: Record<string, unknown> = {}, ...kids: (Node | string)[]) {
  const el = Object.assign(document.createElement(tag), props)
  el.append(...kids)
  return el
}

const fmtDate = (iso: string) => new Date(iso).toLocaleString(undefined, { dateStyle: 'medium', timeStyle: 'short' })

// ---- Icons: inline 24px line set (stroke via CSS), no external deps ----
const ICONS: Record<string, string> = {
  inbox: '<path d="M22 12h-6l-2 3h-4l-2-3H2"/><path d="M5.45 5.11 2 12v6a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2v-6l-3.45-6.89A2 2 0 0 0 16.76 4H7.24a2 2 0 0 0-1.79 1.11z"/>',
  mail: '<rect x="2" y="4" width="20" height="16" rx="2"/><path d="m22 7-10 6L2 7"/>',
  send: '<path d="m22 2-7 20-4-9-9-4Z"/><path d="M22 2 11 13"/>',
  user: '<circle cx="12" cy="8" r="4"/><path d="M4 21a8 8 0 0 1 16 0"/>',
  copy: '<rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/>',
  check: '<path d="M20 6 9 17l-5-5"/>',
  close: '<path d="M18 6 6 18M6 6l12 12"/>',
  clock: '<circle cx="12" cy="12" r="10"/><path d="M12 6v6l4 2"/>',
  refresh: '<path d="M21 12a9 9 0 1 1-3-6.7L21 8"/><path d="M21 3v5h-5"/>',
  logout: '<path d="M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4"/><path d="m16 17 5-5-5-5"/><path d="M21 12H9"/>',
  lock: '<rect x="3" y="11" width="18" height="11" rx="2"/><path d="M7 11V7a5 5 0 0 1 10 0v4"/>',
  alert: '<circle cx="12" cy="12" r="10"/><path d="M12 8v4M12 16h.01"/>',
  back: '<path d="M19 12H5M12 19l-7-7 7-7"/>',
}
function icon(name: keyof typeof ICONS, cls = 'icon') {
  const s = document.createElementNS('http://www.w3.org/2000/svg', 'svg')
  s.setAttribute('viewBox', '0 0 24 24')
  s.setAttribute('class', cls)
  s.setAttribute('aria-hidden', 'true')
  s.innerHTML = ICONS[name] // static strings above, never user data
  return s
}
const btn = (cls: string, label: string, ic?: keyof typeof ICONS, props: Record<string, unknown> = {}) =>
  h('button', { type: 'button', className: cls, ...props }, ...(ic ? [icon(ic)] : []), h('span', { className: 'btn-label' }, label))

const emptyState = (ic: keyof typeof ICONS, title: string, text: string) =>
  h('div', { className: 'empty' }, h('span', { className: 'empty-art' }, icon(ic, 'icon icon-xl')),
    h('p', { className: 'empty-title' }, title), h('p', { className: 'empty-text' }, text))
const errorState = (text: string) => h('p', { className: 'form-error', role: 'alert' }, icon('alert'), text)
const skeleton = () => h('ul', { className: 'message-list', ariaBusy: 'true', ariaLabel: 'Loading messages' },
  ...Array.from({ length: 7 }, () => h('li', { className: 'skel-row' },
    h('span', { className: 'skel skel-from' }), h('span', { className: 'skel skel-date' }),
    h('span', { className: 'skel skel-subject' }), h('span', { className: 'skel skel-preview' }))))

const fmtShort = (iso: string) => {
  const d = new Date(iso)
  return d.toDateString() === new Date().toDateString()
    ? d.toLocaleTimeString(undefined, { timeStyle: 'short' })
    : d.toLocaleDateString(undefined, { month: 'short', day: 'numeric' })
}

const errorText: Record<string, string> = {
  invalid_credentials: 'Email or password is incorrect.',
  too_many_attempts: 'Too many attempts. Try again in 15 minutes.',
  mail_server_unavailable: 'Mail server is unavailable. Try again shortly.',
}

const field = (label: string, ic: keyof typeof ICONS, input: HTMLInputElement) =>
  h('div', { className: 'field' }, h('label', { htmlFor: input.id }, label), h('div', { className: 'input-wrap' }, icon(ic), input))

const brand = (sub?: string) => h('div', { className: 'brand' }, h('span', { className: 'brand-mark' }, 'IRIS'), ...(sub ? [h('span', { className: 'brand-sub' }, sub)] : []))

function renderLogin() {
  const status = h('p', { className: 'form-error', role: 'alert' })
  const email = h('input', { type: 'email', name: 'email', id: 'email', required: true, autocomplete: 'username', placeholder: 'you@meowlabs.id' })
  const password = h('input', { type: 'password', name: 'password', id: 'password', required: true, autocomplete: 'current-password' })
  const submit = h('button', { type: 'submit', className: 'btn btn-primary btn-block' }, 'Sign in')
  const form = h('form', { className: 'auth-card' },
    h('header', { className: 'auth-head' }, brand('by Meow Labs'), h('h1', {}, 'Sign in to your mailbox'), h('p', { className: 'muted' }, 'Every message finds its way.')),
    field('Email', 'mail', email), field('Password', 'lock', password),
    status, submit,
    h('a', { href: '#/temp', className: 'link' }, icon('clock'), 'Need a disposable address? Get a temporary inbox'))
  const clearErr = () => { status.replaceChildren(); [email, password].forEach((i) => i.removeAttribute('aria-invalid')) }
  email.oninput = password.oninput = clearErr
  form.addEventListener('submit', async (e) => {
    e.preventDefault()
    submit.disabled = true
    submit.textContent = 'Signing in…'
    clearErr()
    try {
      await api('/auth/login', { method: 'POST', body: JSON.stringify({ email: email.value, password: password.value }) })
      await renderMailbox()
    } catch (err) {
      const code = (err as HttpError).code
      if (code === 'invalid_credentials') [email, password].forEach((i) => i.setAttribute('aria-invalid', 'true'))
      status.replaceChildren(icon('alert'), errorText[code] ?? 'Sign-in failed.')
      submit.disabled = false
      submit.textContent = 'Sign in'
    }
  })
  app.className = 'auth-shell'
  app.replaceChildren(h('main', {}, form))
  email.focus()
}

// Shared three-pane shell. Mobile shows one pane at a time via data-view (list | reader).
function mailShell(opts: { topbar: HTMLElement; nav: HTMLElement[]; listHead: HTMLElement[]; idle: HTMLElement }) {
  const list = h('div', { className: 'list-body' }, skeleton())
  const readerBody = h('div', { className: 'reader-body', ariaLive: 'polite' }, opts.idle)
  const back = btn('btn btn-ghost reader-back', 'Back to list', 'back')
  const layout = h('main', { className: 'mail-layout' },
    h('nav', { className: 'sidebar', ariaLabel: 'Mail' }, ...opts.nav),
    h('section', { className: 'list-pane', ariaLabel: 'Message list' }, ...opts.listHead, list),
    h('section', { className: 'reader' }, back, readerBody))
  layout.dataset.view = 'list'
  back.onclick = () => { layout.dataset.view = 'list'; list.querySelector<HTMLElement>('[aria-current="true"]')?.focus() }
  app.className = 'app-shell'
  app.replaceChildren(opts.topbar, layout)
  return { list, readerBody, show: () => { layout.dataset.view = 'reader'; back.focus({ preventScroll: true }) } }
}

const navItem = (href: string, ic: keyof typeof ICONS, label: string, current = false, count = '') =>
  h('a', { href, className: 'nav-item', ...(current ? { ariaCurrent: 'page' } : {}) }, icon(ic), h('span', { className: 'nav-label' }, label),
    ...(count ? [h('span', { className: 'nav-count' }, count)] : []))

function fillList(list: HTMLElement, messages: Summary[], current: string, empty: HTMLElement, onOpen: (m: Summary, row: HTMLElement) => void) {
  if (!messages.length) return list.replaceChildren(empty)
  list.replaceChildren(h('ul', { className: 'message-list', ariaLabel: 'Messages' }, ...messages.map((m) => {
    const row = h('button', { type: 'button', className: m.is_read ? 'msg' : 'msg unread' },
      h('span', { className: 'msg-from' }, m.is_read ? '' : h('span', { className: 'unread-dot', title: 'Unread' }, h('span', { className: 'sr-only' }, 'Unread: ')), m.from.replace(/\s*<[^>]*>$/, '') || m.from || '(unknown sender)'),
      h('time', { className: 'msg-date', dateTime: m.received_at, title: fmtDate(m.received_at) }, fmtShort(m.received_at)),
      h('span', { className: 'msg-subject' }, m.subject || '(no subject)'),
      h('span', { className: 'msg-preview' }, m.preview))
    if (m.id === current) row.setAttribute('aria-current', 'true')
    row.onclick = () => {
      list.querySelectorAll('.msg').forEach((b) => b.removeAttribute('aria-current'))
      row.setAttribute('aria-current', 'true')
      row.classList.remove('unread')
      row.querySelector('.unread-dot')?.remove()
      onOpen(m, row)
    }
    return h('li', {}, row)
  })))
}

async function renderMailbox() {
  const session = await api<{ email: string }>('/api/v1/session')
  const { mailboxes } = await api<{ mailboxes: { id: string }[] }>('/api/v1/mailboxes')
  const mb = mailboxes[0].id

  const logout = btn('btn btn-ghost btn-icon-sm', 'Sign out', 'logout')
  logout.onclick = async () => { await api('/auth/logout', { method: 'POST' }).catch(() => {}); renderLogin() }
  const refresh = btn('btn btn-ghost btn-icon', 'Refresh', 'refresh', { title: 'Refresh' })
  const count = h('span', { className: 'pane-count' })
  const inboxNav = navItem('#/', 'inbox', 'Inbox', true)
  const ui = mailShell({
    topbar: h('header', { className: 'topbar' }, brand(),
      h('div', { className: 'account' }, h('span', { className: 'account-chip' }, icon('user'), h('span', { className: 'account-email' }, session.email)), logout)),
    nav: [inboxNav, navItem('#/temp', 'clock', 'Temporary inbox')],
    listHead: [h('header', { className: 'pane-head' }, h('h2', { className: 'pane-title' }, 'Inbox'), count, refresh)],
    idle: emptyState('mail', 'No message selected', 'Choose a message from the list to read it here.'),
  })
  let current = ''
  const load = async () => {
    ui.list.replaceChildren(skeleton())
    refresh.disabled = true
    try {
      const { messages } = await api<{ messages: Summary[] }>(`/api/v1/mailboxes/${mb}/messages?limit=50`)
      const unread = messages.filter((m) => !m.is_read).length
      count.textContent = unread ? `${unread} unread` : ''
      inboxNav.querySelector('.nav-count')?.remove()
      if (unread) inboxNav.append(h('span', { className: 'nav-count' }, String(unread)))
      fillList(ui.list, messages, current,
        emptyState('inbox', 'Your inbox is clear', 'New mail for this address will appear here.'),
        (m) => { current = m.id; ui.show(); openMessage(mb, m.id, ui.readerBody) })
    } catch (err) {
      ui.list.replaceChildren(h('div', { className: 'pad' }, errorState(errorText[(err as HttpError).code] ?? 'Could not load messages.')))
    } finally { refresh.disabled = false }
  }
  refresh.onclick = load
  await load()
}

async function openMessage(mb: string, id: string, reader: HTMLElement) {
  return openMessageAt(`/api/v1/mailboxes/${mb}/messages/${encodeURIComponent(id)}`, reader, undefined, renderLogin)
}

async function openMessageAt(url: string, reader: HTMLElement, init: RequestInit | undefined, onAuthFail: () => void) {
  reader.replaceChildren(h('div', { className: 'reader-skel', ariaBusy: 'true' },
    h('span', { className: 'skel skel-title' }), h('span', { className: 'skel skel-subject' }), h('span', { className: 'skel skel-preview' })))
  try {
    const { message: m } = await api<{ message: Detail }>(url, init)
    let body: HTMLElement
    if (m.html_body) {
      // Sandboxed (no scripts, opaque origin) + CSP blocking remote content: email HTML
      // can't touch app cookies/storage or load trackers (DBD §7).
      const csp = `<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; img-src data: cid:">`
      body = h('iframe', { className: 'mail-frame', title: 'Message body', srcdoc: csp + m.html_body })
      body.setAttribute('sandbox', '')
    } else {
      body = h('pre', { className: 'mail-text' }, m.text_body ?? '')
    }
    const initial = (m.from.replace(/[^\p{L}\p{N}]/gu, '')[0] ?? '?').toUpperCase()
    reader.replaceChildren(
      h('h1', { className: 'reader-subject' }, m.subject || '(no subject)'),
      h('div', { className: 'reader-head' }, h('span', { className: 'avatar', ariaHidden: 'true' }, initial),
        h('dl', { className: 'reader-meta' },
          h('dt', {}, 'From'), h('dd', { className: 'reader-from' }, m.from),
          h('dt', {}, 'To'), h('dd', {}, m.to.join(', ')),
          h('dt', {}, 'Date'), h('dd', {}, h('time', { dateTime: m.received_at }, fmtDate(m.received_at))))),
      body)
  } catch (err) {
    if ([401, 410].includes((err as HttpError).status)) return onAuthFail()
    reader.replaceChildren(errorState('Could not open message.'))
  }
}

// ---- Temporary inbox (anonymous; capability token lives in this tab's sessionStorage) ----
type TempInbox = { id: string; address: string; access_token: string; expires_at: string }
const TEMP_KEY = 'iris_temp_inbox'
let tempTimers: number[] = []
const stopTemp = () => { tempTimers.forEach(clearInterval); tempTimers = [] }

const tempErrors: Record<string, string> = {
  rate_limited: 'Too many addresses created from your network. Try again later.',
  quota_exceeded: 'You already have 3 active temporary addresses. Wait for one to expire.',
}

function renderTempStart(note = '') {
  stopTemp()
  const status = h('p', { className: 'form-error', role: 'alert' }, ...(note ? [icon('alert'), note] : []))
  const create = h('button', { type: 'button', className: 'btn btn-primary btn-block' }, 'Create temporary address')
  create.onclick = async () => {
    create.disabled = true
    create.textContent = 'Creating…'
    try {
      const inbox = await api<TempInbox>('/api/v1/temp-inboxes', { method: 'POST' })
      sessionStorage.setItem(TEMP_KEY, JSON.stringify(inbox))
      renderTempInbox(inbox)
    } catch (err) {
      status.replaceChildren(icon('alert'), tempErrors[(err as HttpError).code] ?? 'Could not create an address.')
      create.disabled = false
      create.textContent = 'Create temporary address'
    }
  }
  app.className = 'auth-shell'
  app.replaceChildren(h('main', {}, h('section', { className: 'auth-card' },
    h('header', { className: 'auth-head' }, brand('Temporary inbox'), h('h1', {}, 'Get a disposable address'),
      h('p', { className: 'muted' }, 'Receive-only. No sign-up needed.')),
    h('ul', { className: 'facts' },
      h('li', {}, icon('clock'), 'Expires after 1 hour, messages are deleted with it.'),
      h('li', {}, icon('inbox'), 'Receive only. You cannot send from it.'),
      h('li', {}, icon('lock'), 'Not for sensitive or long-term mail.')),
    status, create,
    h('a', { href: '#/', className: 'link' }, icon('user'), 'Sign in to your mailbox'))))
  create.focus()
}

function renderTempInbox(inbox: TempInbox) {
  stopTemp()
  const auth = { headers: { authorization: `Bearer ${inbox.access_token}` } }
  const base = `/api/v1/temp-inboxes/${inbox.id}`
  const expired = () => { sessionStorage.removeItem(TEMP_KEY); renderTempStart('This temporary address has expired.') }

  const copy = btn('btn btn-secondary', 'Copy', 'copy')
  const copyLabel = copy.querySelector('.btn-label')!
  copy.onclick = async () => {
    const ok = await navigator.clipboard.writeText(inbox.address).then(() => true, () => false)
    copy.replaceChildren(icon(ok ? 'check' : 'alert'), copyLabel)
    copyLabel.textContent = ok ? 'Copied' : 'Copy failed'
    copy.classList.toggle('is-done', ok)
    setTimeout(() => { copy.replaceChildren(icon('copy'), copyLabel); copyLabel.textContent = 'Copy'; copy.classList.remove('is-done') }, 1500)
  }
  const countdown = h('span', { className: 'countdown' })
  const badge = h('span', { className: 'expiry-badge', title: `Expires ${fmtDate(inbox.expires_at)}` }, icon('clock'), countdown)
  const tick = () => {
    const s = Math.max(0, Math.floor((Date.parse(inbox.expires_at) - Date.now()) / 1000))
    countdown.textContent = `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')} left`
    badge.classList.toggle('is-urgent', s < 300)
    if (s === 0) expired()
  }
  const discard = btn('btn btn-ghost btn-icon-sm', 'Discard address', 'close')
  discard.onclick = () => { sessionStorage.removeItem(TEMP_KEY); renderTempStart() }
  const syncNote = h('p', { className: 'sync-note', ariaLive: 'polite' }, h('span', { className: 'live-dot' }), 'Checking every 10 seconds')

  const ui = mailShell({
    topbar: h('header', { className: 'topbar' }, brand('Temporary'), h('div', { className: 'account' }, discard)),
    nav: [navItem('#/', 'inbox', 'Mailbox'), navItem('#/temp', 'clock', 'Temporary inbox', true)],
    listHead: [h('div', { className: 'temp-card' },
      h('span', { className: 'temp-label' }, 'Your temporary address'),
      h('code', { className: 'temp-address' }, inbox.address),
      h('div', { className: 'temp-actions' }, copy, badge), syncNote)],
    idle: emptyState('mail', 'No message selected', 'Messages sent to your temporary address open here.'),
  })
  let current = ''
  const load = async () => {
    try {
      const { messages } = await api<{ messages: Summary[] }>(`${base}/messages`, auth)
      fillList(ui.list, messages, current,
        emptyState('send', 'Waiting for mail', 'Share the address above. New messages show up here automatically.'),
        (m) => { current = m.id; ui.show(); openMessageAt(`${base}/messages/${encodeURIComponent(m.id)}`, ui.readerBody, auth, expired) })
    } catch (err) {
      if ([401, 404, 410].includes((err as HttpError).status)) return expired()
      ui.list.replaceChildren(h('div', { className: 'pad' }, errorState('Could not load messages.')))
    }
  }
  tick(); load()
  tempTimers = [setInterval(tick, 1000), setInterval(load, 10_000)]
}

function route() {
  stopTemp()
  if (location.hash === '#/temp') {
    const saved = sessionStorage.getItem(TEMP_KEY)
    return saved ? renderTempInbox(JSON.parse(saved)) : renderTempStart()
  }
  renderMailbox().catch((err) => {
    if ((err as HttpError).status === 401) renderLogin()
    else app.replaceChildren(h('div', { className: 'pad' }, errorState('Service unavailable. Reload to retry.')))
  })
}
window.addEventListener('hashchange', route)
route()
