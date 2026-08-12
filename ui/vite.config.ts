import tailwindcss from '@tailwindcss/vite'
import react from '@vitejs/plugin-react'
import { defineConfig } from 'vitest/config'

/**
 * `base: './'` keeps every asset URL relative, and the server injects a
 * `<base href>` at serve time. Together that is what makes the bundle work
 * unchanged at `/`, at `/notebook/<ns>/<name>/proxy/8787/`, or anywhere else a
 * proxy decides to mount it — the UI never learns its own prefix.
 */
export default defineConfig({
  base: './',
  plugins: [react(), tailwindcss()],
  build: {
    outDir: 'dist',
    emptyOutDir: true,
  },
  server: {
    // `npm run dev` against a `mire` already running on its default port.
    proxy: {
      '/api': 'http://127.0.0.1:8787',
      '/healthz': 'http://127.0.0.1:8787',
    },
  },
  test: {
    environment: 'jsdom',
    globals: true,
    setupFiles: ['./src/test-setup.ts'],
  },
})
