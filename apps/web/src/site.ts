// Site-wide concerns shared by the app and the static pages:
// per-page meta tags, cookie consent, opt-in analytics and the footer.

const env = import.meta.env
export const SITE = {
  url: (env.VITE_SITE_URL || location.origin).replace(/\/$/, ''),
  contactEmail: env.VITE_CONTACT_EMAIL || '',
  contactAddress: env.VITE_CONTACT_ADDRESS || '',
  // Umami: script URL + website ID. Analytics stays off unless both are set.
  analyticsSrc: env.VITE_ANALYTICS_SRC || '',
  analyticsWebsiteId: env.VITE_ANALYTICS_WEBSITE_ID || '',
}
const analyticsConfigured = !!(SITE.analyticsSrc && SITE.analyticsWebsiteId)

// ---------- meta ----------
function meta(selector: string, attr: string, value: string) {
  let el = document.head.querySelector<HTMLElement>(selector)
  if (!el) {
    const [, tag, key, name] = selector.match(/^(\w+)\[(\w+)="([^"]+)"\]$/)!
    el = document.createElement(tag)
    el.setAttribute(key, name)
    document.head.append(el)
  }
  el.setAttribute(attr, value)
}

let currentPath = location.pathname
export function setPage(p: { title: string; description: string; path?: string; noindex?: boolean }) {
  const title = p.title === 'IRIS' ? 'IRIS — Every message finds its way' : `${p.title} · IRIS`
  const url = SITE.url + (p.path ?? '/')
  document.title = title
  meta('meta[name="description"]', 'content', p.description)
  meta('meta[name="robots"]', 'content', p.noindex ? 'noindex, nofollow' : 'index, follow')
  meta('link[rel="canonical"]', 'href', url)
  meta('meta[property="og:title"]', 'content', title)
  meta('meta[property="og:description"]', 'content', p.description)
  meta('meta[property="og:url"]', 'content', url)
  meta('meta[name="twitter:title"]', 'content', title)
  meta('meta[name="twitter:description"]', 'content', p.description)
  currentPath = p.path ?? '/'
  trackPageview(currentPath)
}

// ---------- consent + analytics ----------
type Consent = 'granted' | 'denied'
const CONSENT_KEY = 'iris_consent'
const readConsent = (): Consent | null => { try { return localStorage.getItem(CONSENT_KEY) as Consent | null } catch { return null } }
const writeConsent = (c: Consent) => { try { localStorage.setItem(CONSENT_KEY, c) } catch { /* private mode */ } }
const analyticsOn = () => analyticsConfigured && readConsent() === 'granted'

type UmamiPayload = Record<string, unknown>
declare global { interface Window { umami?: { track: (p?: (props: UmamiPayload) => UmamiPayload) => void } } }

let analyticsLoaded = false
let pending: string | null = null
const send = (path: string) => window.umami?.track((props) => ({ ...props, url: path, title: document.title }))

function loadAnalytics() {
  if (analyticsLoaded || !analyticsOn()) return
  analyticsLoaded = true
  const s = document.createElement('script')
  s.defer = true
  s.src = SITE.analyticsSrc
  s.dataset.websiteId = SITE.analyticsWebsiteId
  s.dataset.autoTrack = 'false' // we send page views ourselves, with the route only
  s.dataset.domains = new URL(SITE.url).hostname // ignore dev/preview hosts
  s.onload = () => { if (pending) send(pending); pending = null }
  document.head.append(s)
}

let lastTracked = ''
export function trackPageview(path: string) {
  // Only the route is sent — never message ids, addresses, tokens or the query string.
  if (!analyticsOn() || path === lastTracked) return
  lastTracked = path
  loadAnalytics()
  if (window.umami) send(path)
  else pending = path
}

export function showCookieBanner(force = false) {
  document.querySelector('.cookie-banner')?.remove()
  if (!force && readConsent()) return
  const hasAnalytics = analyticsConfigured
  const bar = document.createElement('section')
  bar.className = 'cookie-banner'
  bar.setAttribute('role', 'region')
  bar.setAttribute('aria-label', 'Cookie notice')
  bar.innerHTML = hasAnalytics
    ? `<p>IRIS uses essential cookies to keep you signed in. With your OK we also count anonymous page views — no message data, ever. <a href="/privacy/#cookies">Details</a></p>
       <div class="cookie-actions"><button type="button" class="btn btn-secondary" data-c="denied">Essential only</button><button type="button" class="btn btn-ink" data-c="granted">Allow analytics</button></div>`
    : `<p>IRIS only uses essential cookies to keep you signed in and protect forms. No tracking. <a href="/privacy/#cookies">Details</a></p>
       <div class="cookie-actions"><button type="button" class="btn btn-ink" data-c="denied">Got it</button></div>`
  bar.addEventListener('click', (e) => {
    const c = (e.target as HTMLElement).closest<HTMLButtonElement>('button[data-c]')?.dataset.c as Consent | undefined
    if (!c) return
    writeConsent(c)
    bar.remove()
    if (c === 'granted') { loadAnalytics(); trackPageview(currentPath) }
  })
  document.body.append(bar)
}

export function initSite() {
  loadAnalytics()
  showCookieBanner()
  document.addEventListener('click', (e) => {
    if ((e.target as HTMLElement).closest('[data-cookie-settings]')) { e.preventDefault(); showCookieBanner(true) }
  })
}

// ---------- footer ----------
export function footerLinks(cls = 'site-footer') {
  const f = document.createElement('footer')
  f.className = cls
  const year = new Date().getFullYear()
  f.innerHTML = `<nav aria-label="Legal"><a href="/privacy/">Privacy</a><a href="/terms/">Terms</a>${SITE.contactEmail ? `<a href="mailto:${SITE.contactEmail}">Contact</a>` : ''}<button type="button" class="linklike" data-cookie-settings>Cookies</button></nav><span>© ${year} Meow Labs</span>`
  return f
}
