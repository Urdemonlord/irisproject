type Summary = { id: string; from: string; to: string[]; subject: string; preview: string; received_at: string; is_read: boolean }
type Detail = Summary & { text_body: string | null; html_body: string | null }
type TempInbox = { id: string; address: string; access_token: string; expires_at: string }

const app = document.querySelector<HTMLDivElement>('#app')!

class HttpError extends Error { constructor(public status: number, public code: string) { super(code) } }

const csrfCookie = () => document.cookie.split('; ').find((c) => c.startsWith('iris_csrf='))?.slice(10)
async function api<T>(path: string, init: RequestInit = {}): Promise<T> {
  const method = (init.method ?? 'GET').toUpperCase()
  const headers: Record<string, string> = { 'content-type': 'application/json', ...(init.headers as Record<string, string>) }
  if (method !== 'GET' && method !== 'HEAD') {
    if (!csrfCookie()) await fetch('/auth/csrf', { credentials: 'same-origin' })
    headers['x-csrf-token'] = csrfCookie() ?? ''
  }
  const res = await fetch(path, { credentials: 'same-origin', ...init, headers })
  if (!res.ok) { const body = await res.json().catch(() => ({})); throw new HttpError(res.status, body.error ?? 'request_failed') }
  return res.status === 204 ? (undefined as T) : res.json()
}

function h<K extends keyof HTMLElementTagNameMap>(tag: K, props: Record<string, unknown> = {}, ...kids: (Node | string)[]) {
  const el = Object.assign(document.createElement(tag), props)
  el.append(...kids)
  return el
}

function svgUse(id: string, viewBox = '0 0 20 20', cls = 'icon') {
  const s = document.createElementNS('http://www.w3.org/2000/svg', 'svg')
  s.setAttribute('viewBox', viewBox)
  s.setAttribute('class', cls)
  s.setAttribute('aria-hidden', 'true')
  const u = document.createElementNS('http://www.w3.org/2000/svg', 'use')
  u.setAttribute('href', `/mascot-sprite.svg#${id}`)
  s.append(u)
  return s
}
const icon = (id: string, cls = 'icon') => svgUse(id.startsWith('i-') ? id : `i-${id}`, '0 0 20 20', cls)
const mascot = (id: string, cls = 'mascot') => svgUse(id, id === 'cat-peek' ? '0 -4 160 82' : id === 'cat-sleep' ? '0 0 260 150' : id === 'cat-walk' ? '0 0 300 170' : '0 0 200 250', cls)

const btn = (cls: string, label: string, ic?: string, props: Record<string, unknown> = {}) => {
  const isIconOnly = cls.includes('btn-icon')
  const el = h('button', { type: 'button', className: cls, ...(isIconOnly ? { ariaLabel: label } : {}), ...props },
    ...(ic ? [icon(ic)] : []),
    ...(!isIconOnly ? [h('span', { className: 'btn-label' }, label)] : []))
  return el
}

const fmtDate = (iso: string) => new Date(iso).toLocaleString(undefined, { dateStyle: 'medium', timeStyle: 'short' })
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
const tempErrors: Record<string, string> = {
  rate_limited: 'Too many addresses created from your network. Try again later.',
  quota_exceeded: 'You already have 3 active temporary addresses. Wait for one to expire.',
}

// ---------- cat with trackable eyes ----------
function catSitSVG(shut = false): SVGSVGElement {
  const ns = 'http://www.w3.org/2000/svg'
  const svg = document.createElementNS(ns, 'svg')
  svg.setAttribute('viewBox', '0 0 200 250')
  svg.setAttribute('class', 'mascot-svg')
  svg.setAttribute('aria-hidden', 'true')
  if (shut) {
    svg.innerHTML = `
      <defs><filter id="ink" x="-5%" y="-5%" width="110%" height="110%"><feTurbulence type="fractalNoise" baseFrequency=".045" numOctaves="2" seed="7" result="n"/><feDisplacementMap in="SourceGraphic" in2="n" scale="3.2" xChannelSelector="R" yChannelSelector="G"/></filter></defs>
      <g filter="url(#ink)" fill="#16130F">
        <path d="M58 244C34 214 38 160 66 132C55 116 52 98 56 82L50 34L82 60C93 56 109 56 120 60L152 34L146 82C150 98 147 116 136 132C160 158 166 212 142 244Z"/>
        <path d="M140 238C178 240 196 214 186 184C180 166 162 168 166 184" fill="none" stroke="#16130F" stroke-width="13" stroke-linecap="round"/>
      </g>
      <path d="M71 92Q83 99 95 92M105 92Q117 99 129 92" stroke="#B83232" stroke-width="3" fill="none" stroke-linecap="round"/>
      <path d="M96 112l4 3 4-3" stroke="#F7F7F2" stroke-width="1.6" fill="none" stroke-linecap="round" opacity=".7"/>
    `
  } else {
    svg.innerHTML = `
      <defs><filter id="ink" x="-5%" y="-5%" width="110%" height="110%"><feTurbulence type="fractalNoise" baseFrequency=".045" numOctaves="2" seed="7" result="n"/><feDisplacementMap in="SourceGraphic" in2="n" scale="3.2" xChannelSelector="R" yChannelSelector="G"/></filter></defs>
      <g filter="url(#ink)" fill="#16130F">
        <path d="M58 244C34 214 38 160 66 132C55 116 52 98 56 82L50 34L82 60C93 56 109 56 120 60L152 34L146 82C150 98 147 116 136 132C160 158 166 212 142 244Z"/>
        <path d="M140 238C178 240 196 214 186 184C180 166 162 168 166 184" fill="none" stroke="#16130F" stroke-width="13" stroke-linecap="round"/>
      </g>
      <path d="M84 216C86 228 84 238 80 244M116 216C114 228 116 238 120 244" stroke="#F7F7F2" stroke-width="2" fill="none" stroke-linecap="round" opacity=".55"/>
      <g>
        <path class="iris" d="M70 92Q83 79 96 92Q83 103 70 92Z" fill="#B83232"/>
        <path class="pupil" d="M83 82.5C81 87 81 97 83 101C85 97 85 87 83 82.5Z" fill="#16130F" style="transform-box:fill-box;transform-origin:center;"/>
        <path class="iris" d="M104 92Q117 79 130 92Q117 103 104 92Z" fill="#B83232"/>
        <path class="pupil" d="M117 82.5C115 87 115 97 117 101C119 97 119 87 117 82.5Z" fill="#16130F" style="transform-box:fill-box;transform-origin:center;"/>
      </g>
      <path d="M96 112l4 3 4-3" stroke="#F7F7F2" stroke-width="1.6" fill="none" stroke-linecap="round" opacity=".7"/>
    `
  }
  return svg
}

function trackEyes(email: HTMLInputElement, wrap: HTMLElement) {
  const update = () => {
    const v = email.value
    const pos = email.selectionStart ?? v.length
    const ratio = v.length ? pos / v.length : 0.5
    const x = Math.max(-6, Math.min(6, ratio * 12 - 6))
    wrap.querySelectorAll<SVGPathElement>('.pupil').forEach((p) => { p.style.transform = `translateX(${x}px)` })
  }
  email.addEventListener('input', update)
  email.addEventListener('keyup', update)
  email.addEventListener('click', update)
}

// ---------- Login ----------
function renderLogin() {
  const status = h('p', { className: 'form-error', role: 'alert' })
  const email = h('input', { type: 'email', name: 'email', id: 'email', required: true, autocomplete: 'username', placeholder: 'you@meowlabs.id' }) as HTMLInputElement
  const password = h('input', { type: 'password', name: 'password', id: 'password', required: true, autocomplete: 'current-password' }) as HTMLInputElement
  const submit = h('button', { type: 'submit', className: 'btn btn-ink btn-block' }, 'Sign in') as HTMLButtonElement
  const showPw = h('button', { type: 'button', className: 'show-pw' }, 'Show') as HTMLButtonElement
  showPw.onclick = () => { password.type = password.type === 'password' ? 'text' : 'password'; showPw.textContent = password.type === 'password' ? 'Show' : 'Hide' }

  const catWrap = h('div', { className: 'mascot-wrap' })
  const catSvg = catSitSVG(false)
  catWrap.append(catSvg)
  trackEyes(email, catWrap)

  const left = h('div', { className: 'login-left' },
    h('div', { className: 'topline' }, h('span', { className: 'wordmark' }, 'IRIS'), h('span', {}, 'MEOW LABS')),
    h('div', {}, h('div', { className: 'login-tagline' }, h('span', {}, 'Every'), h('span', {}, 'message'), h('span', {}, 'finds'), h('span', {}, 'its way.'))),
    h('div', { className: 'login-route' }, h('div', { className: 'route-line' }), h('div', { className: 'route-nodes' }, h('span', { className: 'route-node-sq' }), h('span', { className: 'route-node-sq' }), h('span', { className: 'route-node-rd' })), catWrap),
    h('div', { className: 'login-footer' }, h('span', {}, 'MX → AUTH → INBOX'), h('span', {}, 'mail.meowlabs.id')))
  const right = h('div', { className: 'login-right' },
    h('h1', {}, 'Sign in'),
    h('form', { className: 'login-form' },
      h('div', { className: 'field' }, h('label', { htmlFor: 'email' }, 'Email'), h('div', { className: 'input-wrap' }, icon('mail'), email)),
      h('div', { className: 'field' }, h('label', { htmlFor: 'password' }, 'Password'), h('div', { className: 'input-wrap' }, icon('lock'), password, showPw)),
      status,
      submit,
      h('div', { className: 'login-foot' }, h('span', {}, 'Need a throwaway?'), h('a', { href: '#/temp', className: 'link' }, 'Temporary inbox →'))))
  const form = right.querySelector('form')!
  const clearErr = () => { status.replaceChildren(); [email, password].forEach((i) => i.removeAttribute('aria-invalid')); status.classList.remove('login-error') }
  email.oninput = password.oninput = clearErr
  password.addEventListener('focus', () => { catWrap.replaceChildren(catSitSVG(true)) })
  password.addEventListener('blur', () => { catWrap.replaceChildren(catSitSVG(false)); trackEyes(email, catWrap) })
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
      status.classList.add('login-error')
      status.replaceChildren(icon('alert'), h('span', {}, errorText[code] ?? 'Sign-in failed.'))
      submit.disabled = false
      submit.textContent = 'Sign in'
      catWrap.replaceChildren(catSitSVG(false))
      // dilated eyes: swap pupils to round by reusing cat with a class? keep simple
    }
  })
  app.className = 'login-shell'
  app.replaceChildren(left, right)
  email.focus()
}

// ---------- Shared shell ----------
function mailShell(opts: { topbar: HTMLElement; nav: HTMLElement[]; listHead: HTMLElement[]; idle: HTMLElement }) {
  const list = h('div', { className: 'list-body' }, skeleton())
  const readerBody = h('div', { className: 'reader-body', ariaLive: 'polite' }, opts.idle)
  const back = btn('btn btn-ghost reader-back', 'Back to list', 'back')
  const reader = h('div', { className: 'reader' }, back, readerBody)
  const attachRail = h('aside', { className: 'attach-rail' }, attachmentRail())
  const readerWrap = h('div', { className: 'reader-wrap' }, reader, attachRail)
  const layout = h('main', { className: 'mail-layout' },
    h('nav', { className: 'sidebar', ariaLabel: 'Mail' }, ...opts.nav),
    h('section', { className: 'list-pane', ariaLabel: 'Message list' }, ...opts.listHead, list),
    readerWrap)
  layout.dataset.view = 'list'
  back.onclick = () => { layout.dataset.view = 'list'; list.querySelector<HTMLElement>('[aria-current="true"]')?.focus() }
  const topbarWrap = opts.topbar
  app.className = 'app-shell'
  app.replaceChildren(topbarWrap, layout)
  return { list, readerBody, readerWrap, attachRail, show: () => { layout.dataset.view = 'reader'; back.focus({ preventScroll: true }) } }
}

function skeleton() {
  return h('div', { ariaBusy: 'true', ariaLabel: 'Loading messages' }, ...Array.from({ length: 7 }, () => {
    const li = h('div', { className: 'skel-row' }, h('span', { className: 'skel skel-from' }), h('span', { className: 'skel skel-date' }), h('span', { className: 'skel skel-subject' }), h('span', { className: 'skel skel-preview' }))
    return li
  }))
}

const navItem = (href: string, ic: string, label: string, current = false, count = '') =>
  h('a', { href, className: 'nav-item', ariaLabel: label, ...(current ? { ariaCurrent: 'page' } : {}) }, icon(ic), h('span', { className: 'nav-label' }, label),
    ...(count ? [h('span', { className: 'nav-count' }, count)] : []))

const navSection = (label: string) => h('div', { className: 'nav-section' }, label)

function fillList(list: HTMLElement, messages: Summary[], current: string, empty: HTMLElement, onOpen: (m: Summary, row: HTMLElement) => void) {
  if (!messages.length) return list.replaceChildren(empty)
  list.replaceChildren(h('ul', { className: 'message-list', ariaLabel: 'Messages' }, ...messages.map((m) => {
    const row = h('button', { type: 'button', className: m.is_read ? 'msg' : 'msg unread' },
      h('span', { className: 'msg-from' }, ...(m.is_read ? [] : [h('span', { className: 'iris-mark', title: 'Unread' })]), (m.from.replace(/\s*<[^>]*>$/, '') || m.from || '(unknown sender)')),
      h('time', { className: 'msg-date', dateTime: m.received_at, title: fmtDate(m.received_at) }, fmtShort(m.received_at)),
      h('span', { className: 'msg-subject' }, m.subject || '(no subject)'),
      h('span', { className: 'msg-preview' }, m.preview),
      ...(m.preview.includes('PDF') ? [h('span', { className: 'msg-attach' }, icon('clip'), '2')] : []))
    if (m.id === current) row.setAttribute('aria-current', 'true')
    row.onclick = () => {
      list.querySelectorAll('.msg').forEach((b) => b.removeAttribute('aria-current'))
      row.setAttribute('aria-current', 'true')
      row.classList.remove('unread')
      row.querySelector('.iris-mark')?.remove()
      onOpen(m, row)
    }
    return h('li', {}, row)
  })))
}

function emptyState(ic: string, title: string, text: string) {
  return h('div', { className: 'empty' }, h('span', { className: 'empty-art' }, icon(ic, 'icon icon-xl')), h('p', { className: 'empty-title' }, title), h('p', { className: 'empty-text' }, text))
}
const errorState = (text: string) => h('p', { className: 'form-error', role: 'alert' }, icon('alert'), text)

function attachmentRail() {
  const attachments = [
    { name: 'Q4-roadmap.pdf', meta: '2.9 MB · 14 pages' },
    { name: 'temp-inbox-notes.pdf', meta: '184 KB · 2 pages' },
  ]
  const frag: HTMLElement[] = [h('div', { className: 'rail-label' }, 'Attachments')]
  for (const a of attachments) {
    frag.push(h('div', { className: 'file-tile' },
      h('div', { className: 'tile pdf' }, 'PDF'),
      h('div', { className: 'file-meta' }, h('div', { className: 'name' }, a.name), h('div', { className: 'meta' }, a.meta)),
      btn('btn btn-ghost btn-icon-sm', 'Download', 'download')))
  }
  frag.push(btn('btn btn-secondary btn-block download-all', 'Download all (.zip)', 'download'))
  frag.push(h('div', { className: 'rail-label', style: 'margin-top:16px' }, 'Route'))
  const nodes = [
    { label: 'mx.meowlabs.id', sub: 'Received 09:12:04', delivered: false },
    { label: 'auth pass', sub: 'SPF ✓ · DKIM ✓', delivered: false },
    { label: 'Delivered', sub: 'inbox', delivered: true },
  ]
  for (const n of nodes) {
    frag.push(h('div', { className: 'route-node' },
      h('span', { className: `route-dot${n.delivered ? ' delivered' : ''}` }),
      h('div', { className: 'route-info' }, h('strong', {}, n.label), h('span', {}, n.sub))))
  }
  return h('div', {}, ...frag)
}

async function renderMailbox() {
  const session = await api<{ email: string }>('/api/v1/session')
  const { mailboxes } = await api<{ mailboxes: { id: string }[] }>('/api/v1/mailboxes')
  const mb = mailboxes[0].id

  const logout = btn('btn btn-ghost btn-icon-sm', 'Sign out', 'logout')
  logout.onclick = async () => { await api('/auth/logout', { method: 'POST' }).catch(() => {}); renderLogin() }
  const refresh = btn('btn btn-ghost btn-icon', 'Refresh', 'refresh', { title: 'Refresh' })
  const count = h('span', { className: 'pane-count' }, 'NEWEST FIRST')
  const inboxNav = navItem('#/', 'inbox', 'Inbox', true)
  const unreadChip = h('span', { className: 'pane-chip' }, '0 UNREAD')

  const topbar = h('header', { className: 'topbar' },
    h('div', { className: 'brand' }, h('span', { className: 'brand-mark' }, 'IRIS'), h('span', { className: 'brand-sub' }, 'meowlabs.id')),
    h('div', { className: 'topbar-search' }, icon('search'), h('input', { placeholder: 'Search mail', 'aria-label': 'Search mail' })),
    h('div', { className: 'topbar-right' },
      h('span', {}, '⟳ SYNCED 19:42'),
      h('span', { className: 'account-chip' }, icon('account'), h('span', { className: 'account-email' }, session.email)),
      logout))

  const ui = mailShell({
    topbar,
    nav: [
      navSection('Mailbox'),
      navItem('#/', 'inbox', 'Inbox', true),
      navItem('#/', 'send', 'Sent'),
      navItem('#/', 'draft', 'Drafts', false, '1'),
      navItem('#/', 'archive', 'Archive'),
      navItem('#/', 'trash', 'Trash'),
      navSection('Temporary'),
      navItem('#/temp', 'timer', 'Temporary inbox'),
      navItem('#/temp', 'pen', 'New address'),
      h('div', { className: 'storage' }, 'STORAGE', h('div', { className: 'storage-bar' }, h('span', { style: 'width:24%' })), ' 1.2 / 5 GB'),
    ],
    listHead: [h('header', { className: 'pane-head' }, h('h2', { className: 'pane-title' }, 'Inbox'), unreadChip, count, refresh)],
    idle: emptyState('mail', 'No message selected', 'Choose a message from the list to read it here.'),
  })
  // fix nav: use inboxNav reference for count updates? We'll update chip instead.
  let current = ''
  const load = async () => {
    ui.list.replaceChildren(skeleton())
    refresh.disabled = true
    try {
      const { messages } = await api<{ messages: Summary[] }>(`/api/v1/mailboxes/${mb}/messages?limit=50`)
      const unread = messages.filter((m) => !m.is_read).length
      unreadChip.textContent = `${unread} UNREAD`
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
  reader.replaceChildren(h('div', { className: 'reader-skel', ariaBusy: 'true' }, h('span', { className: 'skel skel-title' }), h('span', { className: 'skel skel-subject' }), h('span', { className: 'skel skel-preview' })))
  try {
    const { message: m } = await api<{ message: Detail }>(url, init)
    let body: HTMLElement
    if (m.html_body) {
      const csp = `<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; img-src data: cid:">`
      body = h('iframe', { className: 'mail-frame', title: 'Message body', srcdoc: csp + m.html_body })
      body.setAttribute('sandbox', '')
    } else {
      body = h('pre', { className: 'mail-text' }, m.text_body ?? '')
    }
    const initial = (m.from.replace(/[^\p{L}\p{N}]/gu, '')[0] ?? '?').toUpperCase()
    const remote = h('div', { className: 'remote-bar' }, icon('eye'), h('span', {}, h('strong', {}, 'Remote images blocked'), ' · '), h('button', { className: 'btn btn-secondary', type: 'button' }, 'Load once'))
    reader.replaceChildren(
      h('h1', { className: 'reader-subject' }, m.subject || '(no subject)'),
      h('div', { className: 'reader-actions' }, btn('btn btn-secondary', 'Reply', 'reply'), btn('btn btn-secondary', 'Forward', 'forward'), btn('btn btn-ghost btn-icon', 'More', 'more'), h('span', { className: 'pane-count', style: 'margin-left:auto' }, '1 OF 128')),
      h('div', { className: 'reader-head' }, h('span', { className: 'avatar', ariaHidden: 'true' }, initial),
        h('dl', { className: 'reader-meta' },
          h('dt', {}, 'From'), h('dd', { className: 'reader-from' }, m.from),
          h('dt', {}, 'To'), h('dd', {}, m.to.join(', ')),
          h('dt', {}, 'Date'), h('dd', {}, h('time', { dateTime: m.received_at }, fmtDate(m.received_at))))),
      remote,
      body)
  } catch (err) {
    if ([401, 410].includes((err as HttpError).status)) return onAuthFail()
    reader.replaceChildren(errorState('Could not open message.'))
  }
}

// ---------- Temp inbox ----------
let tempTimers: number[] = []
const stopTemp = () => { tempTimers.forEach(clearInterval); tempTimers = [] }

function renderTempStart(note = '') {
  stopTemp()
  const status = h('p', { className: 'form-error', role: 'alert' }, ...(note ? [icon('alert'), note] : []))
  const create = btn('btn btn-primary btn-block', 'Create temporary address', undefined)
  create.onclick = async () => {
    create.disabled = true
    create.textContent = 'Creating…'
    try {
      const inbox = await api<TempInbox>('/api/v1/temp-inboxes', { method: 'POST' })
      sessionStorage.setItem('iris_temp_inbox', JSON.stringify(inbox))
      renderTempInbox(inbox)
    } catch (err) {
      status.replaceChildren(icon('alert'), tempErrors[(err as HttpError).code] ?? 'Could not create an address.')
      create.disabled = false
      create.textContent = 'Create temporary address'
    }
  }
  app.className = 'auth-shell'
  app.replaceChildren(h('main', {}, h('section', { className: 'auth-card' },
    h('header', { className: 'auth-head' }, h('div', { className: 'brand' }, h('span', { className: 'brand-mark' }, 'IRIS'), h('span', { className: 'brand-sub' }, 'Temporary')), h('h1', {}, 'Get a disposable address'), h('p', { className: 'muted' }, 'Receive-only. No sign-up needed.')),
    h('ul', { className: 'facts' },
      h('li', {}, icon('clock'), 'Expires after 1 hour, messages are deleted with it.'),
      h('li', {}, icon('inbox'), 'Receive only. You cannot send from it.'),
      h('li', {}, icon('shield'), 'Not for sensitive or long-term mail.')),
    status, create,
    h('a', { href: '#/', className: 'link' }, icon('account'), 'Sign in to your mailbox'))))
  create.focus()
}

function renderTempExpired(inbox: TempInbox) {
  stopTemp()
  const create = btn('btn btn-primary btn-block', 'Create new address')
  create.onclick = () => renderTempStart()
  const card = h('div', { className: 'temp-card', style: 'box-shadow:none' },
    h('div', { className: 'cat-peek' }, mascot('cat-sleep', 'mascot')),
    h('span', { className: 'temp-label' }, 'Your temporary address'),
    h('code', { className: 'temp-address strikethrough' }, inbox.address),
    h('div', { className: 'temp-caption' }, 'RECEIVE-ONLY · NOT FOR SENSITIVE MAIL'))
  app.className = 'app-shell'
  const topbar = h('header', { className: 'topbar' },
    h('div', { className: 'brand' }, h('span', { className: 'brand-mark' }, 'IRIS'), h('span', { className: 'brand-sub' }, 'Temporary')),
    h('div', { className: 'topbar-search' }, icon('search'), h('input', { placeholder: 'Search mail' })),
    h('div', { className: 'topbar-right' }, h('span', { className: 'account-chip' }, icon('account'), 'temp')))
  const expiredView = h('div', { className: 'expired-wrap' },
    mascot('cat-sleep', 'mascot'),
    h('h2', {}, 'This address has expired.'),
    h('p', { className: 'empty-text' }, `Messages were deleted at ${fmtDate(inbox.expires_at).split(',').pop()?.trim() ?? ''} and can't be recovered.`),
    create)
  const layout = h('main', { className: 'mail-layout' },
    h('nav', { className: 'sidebar', ariaLabel: 'Mail' }, navItem('#/', 'inbox', 'Mailbox'), navItem('#/temp', 'timer', 'Temporary inbox', true)),
    h('section', { className: 'list-pane', ariaLabel: 'Messages' }, card, expiredView),
    h('aside', { className: 'expiry-rail' }, h('div', { className: 'expiry-label' }, 'Expired')))
  app.replaceChildren(topbar, layout)
}

function renderTempInbox(inbox: TempInbox) {
  stopTemp()
  const auth = { headers: { authorization: `Bearer ${inbox.access_token}` } }
  const base = `/api/v1/temp-inboxes/${inbox.id}`
  const expired = () => { sessionStorage.removeItem('iris_temp_inbox'); renderTempExpired(inbox) }

  const copy = btn('btn btn-primary copy-btn', 'Copy', 'copy')
  const copyLabel = copy.querySelector('.btn-label')!
  copy.onclick = async () => {
    const ok = await navigator.clipboard.writeText(inbox.address).then(() => true, () => false)
    copy.replaceChildren(icon(ok ? 'check' : 'alert'), copyLabel)
    copyLabel.textContent = ok ? 'Copied' : 'Copy failed'
    copy.classList.toggle('is-done', ok)
    setTimeout(() => { copy.replaceChildren(icon('copy'), copyLabel); copyLabel.textContent = 'Copy'; copy.classList.remove('is-done') }, 2000)
  }

  // countdown
  const countdownEl = h('span', { className: 'countdown' })
  const expiryChip = h('span', { className: 'expiry-chip' }, icon('timer'), ' ')
  const tickBar = h('div', { className: 'tick-bar' })
  const expiryTime = h('div', { className: 'expiry-time' }, `at ${new Date(inbox.expires_at).toLocaleTimeString(undefined, { timeStyle: 'short' })}`)
  const tick = () => {
    const s = Math.max(0, Math.floor((Date.parse(inbox.expires_at) - Date.now()) / 1000))
    const hh = String(Math.floor(s / 3600)).padStart(2, '0')
    const mm = String(Math.floor((s % 3600) / 60)).padStart(2, '0')
    const ss = String(s % 60).padStart(2, '0')
    countdownEl.textContent = `${hh}:${mm}:${ss}`
    countdownEl.classList.toggle('urgent', s <= 600 && s > 60)
    countdownEl.classList.toggle('danger', s <= 60)
    expiryChip.classList.toggle('urgent', s <= 600)
    expiryChip.textContent = ''
    expiryChip.append(icon('timer'), document.createTextNode(s <= 60 ? ' LESS THAN A MINUTE' : s <= 600 ? ' EXPIRING' : ` ${Math.ceil(s/60)}M LEFT`))
    // tick bar
    const total = 3600
    const elapsed = Math.min(60, Math.max(0, Math.floor(((total - s) / total) * 60)))
    tickBar.replaceChildren(...Array.from({ length: 60 }, (_, i) => h('span', { className: `tick${i < elapsed ? ' elapsed' : ''}` })))
    if (s === 0) expired()
  }

  const discard = btn('btn btn-ghost btn-icon-sm', 'Discard', 'close')
  discard.onclick = () => { sessionStorage.removeItem('iris_temp_inbox'); renderTempStart() }

  const topbar = h('header', { className: 'topbar' },
    h('div', { className: 'brand' }, h('span', { className: 'brand-mark' }, 'IRIS'), h('span', { className: 'brand-sub' }, 'Temporary')),
    h('div', { className: 'topbar-search' }, icon('search'), h('input', { placeholder: 'Search mail' })),
    h('div', { className: 'topbar-right' }, h('span', { className: 'account-chip' }, icon('account'), 'temp'), discard))

  const catPeek = h('div', { className: 'cat-peek' }, mascot('cat-peek', 'mascot'))
  const addressCard = h('div', { className: 'temp-card' }, catPeek,
    h('span', { className: 'temp-label' }, 'Temporary address'),
    h('code', { className: 'temp-address' }, inbox.address),
    h('div', { className: 'temp-caption' }, 'RECEIVE-ONLY · NOT FOR SENSITIVE MAIL'),
    h('div', { className: 'temp-actions' }, copy, expiryChip))

  const liveStatus = h('span', { className: 'live-status' }, h('span', { className: 'live-dot' }), 'LISTENING · 5S')
  const listHead = h('header', { className: 'temp-list-head' }, h('h2', { className: 'temp-list-title' }, 'Messages'), h('span', { className: 'pane-count' }, '2'), liveStatus)

  const listBody = h('div', { className: 'list-body' }, skeleton())
  const readerBody = h('div', { className: 'reader-body', ariaLive: 'polite' }, emptyState('mail', 'No message selected', 'Messages sent to your temporary address open here.'))
  const back = btn('btn btn-ghost reader-back', 'Back to list', 'back')

  const listPane = h('section', { className: 'list-pane', ariaLabel: 'Messages' }, addressCard, listHead, listBody)
  const reader = h('div', { className: 'reader' }, back, readerBody)
  const expiryRail = h('aside', { className: 'expiry-rail' },
    h('div', { className: 'expiry-label' }, 'Expires'),
    countdownEl,
    expiryTime,
    tickBar,
    h('div', { style: 'margin-top:16px' }, btn('btn btn-secondary btn-block', 'New address', 'pen'), h('div', { style: 'height:8px' }), btn('btn btn-ghost btn-block', 'Delete now', 'trash')))

  const sidebar = h('nav', { className: 'sidebar', ariaLabel: 'Mail' },
    navItem('#/', 'inbox', 'Mailbox'),
    navItem('#/temp', 'timer', 'Temporary inbox', true))

  const layout = h('main', { className: 'mail-layout' }, sidebar, listPane, expiryRail)
  layout.dataset.view = 'list'
  app.className = 'app-shell'
  app.replaceChildren(topbar, layout)

  const showReader = () => { listPane.replaceChildren(back, reader); readerBody.parentElement && null }
  back.onclick = () => { listPane.replaceChildren(addressCard, listHead, listBody) }
  // patch show to replace listPane content
  const originalShow = () => { listPane.replaceChildren(back, reader); back.focus({ preventScroll: true }) }

  let current = ''
  const load = async () => {
    try {
      const { messages } = await api<{ messages: Summary[] }>(`${base}/messages`, auth)
      listBody.replaceChildren(...[h('ul', { className: 'message-list', ariaLabel: 'Messages' })])
      const ul = listBody.querySelector('ul')!
      if (!messages.length) { listBody.replaceChildren(emptyState('send', 'Waiting for mail', 'Share the address above. New messages show up here automatically.')); return }
      ul.replaceChildren(...messages.map((m) => {
        const row = h('button', { type: 'button', className: `temp-row${m.is_read ? '' : ' unread'}` },
          ...(m.is_read ? [h('span', {})] : [h('span', { className: 'iris-mark' })]),
          h('span', { className: 'temp-sender' }, m.from.replace(/\s*<[^>]*>$/, '') || m.from),
          h('span', {}, m.subject || '(no subject)'),
          h('time', { className: 'msg-date' }, fmtShort(m.received_at)))
        // OTP chip detection
        const codeMatch = (m.subject || '').match(/\b(\d{4,8})\b/)
        if (codeMatch) {
          const chip = h('span', { className: 'otp-chip' }, codeMatch[1], btn('btn btn-ghost btn-icon-sm', 'Copy', 'copy'))
          row.append(chip)
        }
        if (m.id === current) row.setAttribute('aria-current', 'true')
        row.onclick = () => {
          listBody.querySelectorAll('.temp-row').forEach((b) => b.removeAttribute('aria-current'))
          row.setAttribute('aria-current', 'true')
          row.classList.remove('unread')
          row.querySelector('.iris-mark')?.remove()
          current = m.id
          originalShow()
          openMessageAt(`${base}/messages/${encodeURIComponent(m.id)}`, readerBody, auth, expired)
        }
        return row
      }))
    } catch (err) {
      if ([401, 404, 410].includes((err as HttpError).status)) return expired()
      listBody.replaceChildren(h('div', { className: 'pad' }, errorState('Could not load messages.')))
    }
  }
  tick(); load()
  tempTimers = [setInterval(tick, 1000), setInterval(load, 10_000)]
}

function route() {
  stopTemp()
  if (location.hash === '#/temp') {
    const saved = sessionStorage.getItem('iris_temp_inbox')
    return saved ? renderTempInbox(JSON.parse(saved)) : renderTempStart()
  }
  renderMailbox().catch((err) => {
    if ((err as HttpError).status === 401) renderLogin()
    else app.replaceChildren(h('div', { className: 'pad' }, errorState('Service unavailable. Reload to retry.')))
  })
}
window.addEventListener('hashchange', route)
route()
