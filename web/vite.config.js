import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'

// In development, /v1 goes to the API on :8080, so the browser sees one origin (like production behind Caddy).
export default defineConfig({
  plugins: [vue()],
  server: { port: 5173, proxy: { '/v1': 'http://localhost:8080' } },
  test: { environment: 'jsdom' },
})
