import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { defineConfig, loadEnv, type Plugin } from 'vite'

const api = process.env.IRIS_API_URL ?? 'http://127.0.0.1:8000'

// Public, non-secret defaults. Override with env vars or apps/web/.env.production.local.
const DEFAULTS: Record<string, string> = {
  VITE_SITE_URL: 'https://mail.meowlabs.id',
  VITE_CONTACT_EMAIL: 'admin@meowlabs.id',
  VITE_CONTACT_ADDRESS: '', // postal address shown on Privacy/Terms; empty = hidden
  VITE_ANALYTICS_SRC: 'https://analytics.meowlabs.id/script.js', // Umami
  VITE_ANALYTICS_WEBSITE_ID: '', // Umami website ID — empty = analytics off
}
for (const [k, v] of Object.entries(DEFAULTS)) process.env[k] ??= v

const pages = { main: 'index.html', privacy: 'privacy/index.html', terms: 'terms/index.html', notFound: '404.html' }
const PUBLIC_PATHS = ['/', '/privacy/', '/terms/']

/** Inlines the shared <head> partial, fills in env values and emits robots.txt + sitemap.xml. */
function site(env: Record<string, string>): Plugin {
  const siteUrl = env.VITE_SITE_URL.replace(/\/$/, '')
  const fill = (s: string) => s.replace(/%(VITE_\w+)%/g, (_, k) => env[k] ?? '')
  return {
    name: 'iris-site',
    transformIndexHtml: {
      order: 'pre',
      handler: (html) => fill(html.replace('<!--head-->', readFileSync(resolve(__dirname, 'partials/head.html'), 'utf8'))),
    },
    generateBundle() {
      const today = new Date().toISOString().slice(0, 10)
      this.emitFile({ type: 'asset', fileName: 'robots.txt', source: `User-agent: *\nAllow: /\nDisallow: /api/\nDisallow: /auth/\n\nSitemap: ${siteUrl}/sitemap.xml\n` })
      this.emitFile({
        type: 'asset', fileName: 'sitemap.xml',
        source: `<?xml version="1.0" encoding="UTF-8"?>\n<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">\n${PUBLIC_PATHS.map((p) => `  <url><loc>${siteUrl}${p}</loc><lastmod>${today}</lastmod></url>`).join('\n')}\n</urlset>\n`,
      })
    },
  }
}

export default defineConfig(({ mode }) => {
  const env = { ...loadEnv(mode, __dirname, 'VITE_') }
  return {
    plugins: [site(env)],
    server: {
      host: '127.0.0.1',
      port: 5173,
      proxy: { '/auth': api, '/api': api },
    },
    build: {
      target: 'ES2020',
      minify: 'esbuild',
      rollupOptions: { input: Object.fromEntries(Object.entries(pages).map(([k, v]) => [k, resolve(__dirname, v)])) },
    },
  }
})
