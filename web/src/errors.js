// Reports what goes wrong in the browser to the platform's error log, so staff can see it without
// asking the customer to describe it. Kept small and polite: duplicates are dropped, volume is capped,
// and reporting can never throw or loop.
import { config, ApiError } from './api'

const seen = new Map()
let sent = 0
const MAX_PER_PAGE = 12
const QUIET_MS = 60_000

export function report(kind, err, extra = {}) {
  try {
    const cfg = config()
    if (!cfg.tenant || sent >= MAX_PER_PAGE) return
    const message = String((err && err.message) || err || 'Unknown error').slice(0, 500)
    const key = kind + '|' + message
    const now = Date.now()
    if (now - (seen.get(key) || 0) < QUIET_MS) return
    seen.set(key, now)
    sent++
    // Fire and forget. keepalive lets it finish even if the page is closing.
    fetch(`/v1/t/${cfg.tenant}/client-errors`, {
      method: 'POST', keepalive: true, headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ message, kind, stack: err && err.stack ? String(err.stack).slice(0, 3000) : undefined, url: location.pathname + location.search.replace(/(token|code)=[^&]*/g, '$1=…'), ...extra }),
    }).catch(() => {})
  } catch { /* never let reporting break the app */ }
}

/** Hook up every place the browser can tell us about a failure. */
export function installReporter(app) {
  window.addEventListener('error', (e) => report('js', e.error || e.message, { extra: { file: e.filename, line: e.lineno } }))
  window.addEventListener('unhandledrejection', (e) => {
    // Failed calls shown to the person (wrong password, no network) are normal, not bugs.
    if (e.reason instanceof ApiError) return
    report('js', e.reason)
  })
  app.config.errorHandler = (err, instance, info) => {
    report('render', err, { component: instance && instance.$options && (instance.$options.__name || instance.$options.name), extra: { info } })
    console.error(err)
  }
}
