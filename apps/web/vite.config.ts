import { defineConfig } from 'vite'

const api = process.env.IRIS_API_URL ?? 'http://127.0.0.1:8000'

export default defineConfig({
  server: {
    host: '127.0.0.1',
    port: 5173,
    proxy: { '/auth': api, '/api': api },
  },
  build: {
    target: 'ES2020',
    minify: 'esbuild',
  },
})
