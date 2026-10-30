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

const errorText: Record<string, string> = {
  invalid_credentials: 'Email or password is incorrect.',
  too_many_attempts: 'Too many attempts. Try again in 15 minutes.',
  mail_server_unavailable: 'Mail server is unavailable. Try again shortly.',
}

function renderLogin() {
  const status = h('p', { className: 'form-error', role: 'alert' })
  const email = h('input', { type: 'email', name: 'email', id: 'email', required: true, autocomplete: 'username' })
  const password = h('input', { type: 'password', name: 'password', id: 'password', required: true, autocomplete: 'current-password' })
  const submit = h('button', { type: 'submit', className: 'btn-primary' }, 'Sign in')
  const form = h('form', { className: 'login-card' },
    h('h1', {}, 'IRIS'),
    h('p', { className: 'muted' }, 'Every message finds its way.'),
    h('label', { htmlFor: 'email' }, 'Email'), email,
    h('label', { htmlFor: 'password' }, 'Password'), password,
    status, submit,
    h('a', { href: '#/temp', className: 'temp-link' }, 'Need a disposable address? Get a temporary inbox'))
  form.addEventListener('submit', async (e) => {
    e.preventDefault()
    submit.disabled = true
    status.textContent = ''
    try {
      await api('/auth/login', { method: 'POST', body: JSON.stringify({ email: email.value, password: password.value }) })
      await renderMailbox()
    } catch (err) {
      status.textContent = errorText[(err as HttpError).code] ?? 'Sign-in failed.'
      submit.disabled = false
    }
  })
  app.className = 'login-shell'
  app.replaceChildren(h('main', {}, form))
  email.focus()
}

async function renderMailbox() {
  const session = await api<{ email: string }>('/api/v1/session')
  const { mailboxes } = await api<{ mailboxes: { id: string }[] }>('/api/v1/mailboxes')
  const mb = mailboxes[0].id

  const logout = h('button', { type: 'button', className: 'btn-ghost' }, 'Sign out')
  logout.onclick = async () => { await api('/auth/logout', { method: 'POST' }).catch(() => {}); renderLogin() }
  const list = h('ul', { className: 'message-list', ariaLabel: 'Messages' })
  const reader = h('section', { className: 'reader', ariaLive: 'polite' }, h('p', { className: 'muted' }, 'Select a message.'))
  const listPane = h('section', { className: 'list-pane' }, h('h2', { className: 'pane-title' }, 'Inbox'), list)

  app.className = 'app-shell'
  app.replaceChildren(
    h('header', { className: 'topbar' }, h('strong', {}, 'IRIS'), h('span', { className: 'muted' }, session.email), logout),
    h('main', { className: 'mail-layout' }, listPane, reader))

  list.replaceChildren(h('li', { className: 'muted pad' }, 'Loading…'))
  try {
    const { messages } = await api<{ messages: Summary[] }>(`/api/v1/mailboxes/${mb}/messages?limit=50`)
    list.replaceChildren(...(messages.length ? messages.map((m) => {
      const btn = h('button', { type: 'button', className: m.is_read ? 'msg' : 'msg unread' },
        h('span', { className: 'msg-from' }, m.from || '(unknown sender)'),
        h('span', { className: 'msg-date' }, fmtDate(m.received_at)),
        h('span', { className: 'msg-subject' }, m.subject || '(no subject)'),
        h('span', { className: 'msg-preview' }, m.preview))
      btn.onclick = () => {
        list.querySelectorAll('.msg').forEach((b) => b.removeAttribute('aria-current'))
        btn.setAttribute('aria-current', 'true')
        btn.classList.remove('unread')
        openMessage(mb, m.id, reader)
      }
      return h('li', {}, btn)
    }) : [h('li', { className: 'muted pad' }, 'No messages yet.')]))
  } catch (err) {
    list.replaceChildren(h('li', { className: 'form-error pad', role: 'alert' }, errorText[(err as HttpError).code] ?? 'Could not load messages.'))
  }
}

async function openMessage(mb: string, id: string, reader: HTMLElement) {
  return openMessageAt(`/api/v1/mailboxes/${mb}/messages/${encodeURIComponent(id)}`, reader, undefined, renderLogin)
}

async function openMessageAt(url: string, reader: HTMLElement, init: RequestInit | undefined, onAuthFail: () => void) {
  reader.replaceChildren(h('p', { className: 'muted' }, 'Loading…'))
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
    reader.replaceChildren(
      h('h1', { className: 'reader-subject' }, m.subject || '(no subject)'),
      h('dl', { className: 'reader-meta' },
        h('dt', {}, 'From'), h('dd', {}, m.from),
        h('dt', {}, 'To'), h('dd', {}, m.to.join(', ')),
        h('dt', {}, 'Date'), h('dd', {}, fmtDate(m.received_at))),
      body)
  } catch (err) {
    if ([401, 410].includes((err as HttpError).status)) return onAuthFail()
    reader.replaceChildren(h('p', { className: 'form-error', role: 'alert' }, 'Could not open message.'))
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
  const status = h('p', { className: 'form-error', role: 'alert' }, note)
  const create = h('button', { type: 'button', className: 'btn-primary' }, 'Create temporary address')
  create.onclick = async () => {
    create.disabled = true
    try {
      const inbox = await api<TempInbox>('/api/v1/temp-inboxes', { method: 'POST' })
      sessionStorage.setItem(TEMP_KEY, JSON.stringify(inbox))
      renderTempInbox(inbox)
    } catch (err) {
      status.textContent = tempErrors[(err as HttpError).code] ?? 'Could not create an address.'
      create.disabled = false
    }
  }
  app.className = 'login-shell'
  app.replaceChildren(h('main', {}, h('section', { className: 'login-card' },
    h('h1', {}, 'IRIS'),
    h('p', { className: 'muted' }, 'Receive-only address that expires after 1 hour. Messages are deleted with it; you cannot send from it.'),
    status, create,
    h('a', { href: '#/', className: 'temp-link' }, 'Sign in to your mailbox'))))
  create.focus()
}

function renderTempInbox(inbox: TempInbox) {
  stopTemp()
  const auth = { headers: { authorization: `Bearer ${inbox.access_token}` } }
  const base = `/api/v1/temp-inboxes/${inbox.id}`
  const expired = () => { sessionStorage.removeItem(TEMP_KEY); renderTempStart('This temporary address has expired.') }

  const copy = h('button', { type: 'button', className: 'btn-ghost' }, 'Copy')
  copy.onclick = async () => {
    await navigator.clipboard.writeText(inbox.address).then(() => { copy.textContent = 'Copied' }, () => { copy.textContent = 'Copy failed' })
    setTimeout(() => { copy.textContent = 'Copy' }, 1500)
  }
  const countdown = h('span', { className: 'temp-countdown', ariaLive: 'off' })
  const tick = () => {
    const s = Math.max(0, Math.floor((Date.parse(inbox.expires_at) - Date.now()) / 1000))
    countdown.textContent = `Expires in ${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`
    if (s === 0) expired()
  }
  const discard = h('button', { type: 'button', className: 'btn-ghost' }, 'New address')
  discard.onclick = () => { sessionStorage.removeItem(TEMP_KEY); renderTempStart() }

  const list = h('ul', { className: 'message-list', ariaLabel: 'Messages' })
  const reader = h('section', { className: 'reader', ariaLive: 'polite' }, h('p', { className: 'muted' }, 'Waiting for mail. This list refreshes every 10 seconds.'))
  let current = ''
  const load = async () => {
    try {
      const { messages } = await api<{ messages: Summary[] }>(`${base}/messages`, auth)
      list.replaceChildren(...(messages.length ? messages.map((m) => {
        const btn = h('button', { type: 'button', className: 'msg' },
          h('span', { className: 'msg-from' }, m.from || '(unknown sender)'),
          h('span', { className: 'msg-date' }, fmtDate(m.received_at)),
          h('span', { className: 'msg-subject' }, m.subject || '(no subject)'),
          h('span', { className: 'msg-preview' }, m.preview))
        if (m.id === current) btn.setAttribute('aria-current', 'true')
        btn.onclick = () => {
          current = m.id
          list.querySelectorAll('.msg').forEach((b) => b.removeAttribute('aria-current'))
          btn.setAttribute('aria-current', 'true')
          openMessageAt(`${base}/messages/${encodeURIComponent(m.id)}`, reader, auth, expired)
        }
        return h('li', {}, btn)
      }) : [h('li', { className: 'muted pad' }, 'No messages yet.')]))
    } catch (err) {
      if ([401, 404, 410].includes((err as HttpError).status)) return expired()
      list.replaceChildren(h('li', { className: 'form-error pad', role: 'alert' }, 'Could not load messages.'))
    }
  }

  app.className = 'app-shell'
  app.replaceChildren(
    h('header', { className: 'topbar' }, h('strong', {}, 'IRIS'),
      h('code', { className: 'temp-address' }, inbox.address), copy, countdown, discard),
    h('main', { className: 'mail-layout' },
      h('section', { className: 'list-pane' }, h('h2', { className: 'pane-title' }, 'Temporary inbox'), list), reader))
  list.replaceChildren(h('li', { className: 'muted pad' }, 'Loading…'))
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
    else app.replaceChildren(h('p', { className: 'form-error pad', role: 'alert' }, 'Service unavailable. Reload to retry.'))
  })
}
window.addEventListener('hashchange', route)
route()
