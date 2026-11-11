// Fails the build when a built page still carries an inline script, an
// inline event handler or an inline style: the app shell must stay
// CSP-clean, because the shipped policy has no 'unsafe-inline' anywhere
// (see CSP_HEADER_VALUE in apps/api/src/main.rs and the header block in
// packaging/src/usr/share/iris/Caddyfile.template).
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { join } from 'node:path'

const dist = fileURLToPath(new URL('../dist', import.meta.url))
const pages = ['index.html', 'privacy/index.html', 'terms/index.html', '404.html']
let failed = false
for (const page of pages) {
  const html = readFileSync(join(dist, page), 'utf8')
  const problems = []
  for (const m of html.matchAll(/<script(?![^>]*\bsrc=)[^>]*>/g)) {
    problems.push(`inline <script>: ${m[0].slice(0, 80)}`)
  }
  for (const m of html.matchAll(/\son\w+\s*=/g)) {
    problems.push(`inline handler: ${m[0].trim()}`)
  }
  for (const m of html.matchAll(/\sstyle\s*=/g)) {
    problems.push(`inline style: ${m[0].trim()}`)
  }
  if (problems.length) {
    failed = true
    console.error(`${page}:\n  ${problems.join('\n  ')}`)
  }
}
if (failed) {
  console.error('check-csp: built pages must not contain inline scripts, handlers or styles')
  process.exit(1)
}
console.log('check-csp: built pages are CSP-clean')
