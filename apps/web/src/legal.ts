// Entry for the static pages (privacy, terms, 404): cookie banner, analytics, contact details.
import { SITE, footerLinks, initSite, trackPageview } from './site'

document.querySelectorAll<HTMLElement>('[data-contact-email]').forEach((el) => {
  if (!SITE.contactEmail) return el.closest<HTMLElement>('[data-optional]')?.remove()
  el.replaceChildren(Object.assign(document.createElement('a'), { href: `mailto:${SITE.contactEmail}`, textContent: SITE.contactEmail }))
})
document.querySelectorAll<HTMLElement>('[data-contact-address]').forEach((el) => {
  if (!SITE.contactAddress) return el.closest<HTMLElement>('[data-optional]')?.remove()
  el.textContent = SITE.contactAddress
})
document.querySelectorAll('[data-year]').forEach((el) => { el.textContent = String(new Date().getFullYear()) })

document.querySelector('.auth-shell')?.append(footerLinks())
initSite()
trackPageview(document.body.dataset.page ?? location.pathname)
