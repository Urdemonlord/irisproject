type Summary = {
  id: string; from: string; to: string[]; subject: string; preview: string;
  received_at: string; is_read: boolean
}
type Detail = Summary & { text_body: string | null; html_body: string | null }

const app = document.querySelector<HTMLDivElement>('#app')!

class HttpError extends Error { constructor(public status: number, public code: string) { super(code) } }

async function api<T>(path: string, init?: RequestInit): Promise<T> {
  const res = await fetch(path, {
    credentials: 'same-origin',
    headers: { 'content-type': 'application/json' },
    ...init,
  })
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
    status, submit)
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
  reader.replaceChildren(h('p', { className: 'muted' }, 'Loading…'))
  try {
    const { message: m } = await api<{ message: Detail }>(`/api/v1/mailboxes/${mb}/messages/${encodeURIComponent(id)}`)
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
    if ((err as HttpError).status === 401) return renderLogin()
    reader.replaceChildren(h('p', { className: 'form-error', role: 'alert' }, 'Could not open message.'))
  }
}

renderMailbox().catch((err) => {
  if ((err as HttpError).status === 401) renderLogin()
  else app.replaceChildren(h('p', { className: 'form-error pad', role: 'alert' }, 'Service unavailable. Reload to retry.'))
})
