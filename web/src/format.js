// Small, pure helpers. Covered by tests in src/format.test.js.

/** Kobo to a naira string: 150000 -> "₦1,500". */
export function naira(kobo) {
  const n = Number(kobo || 0) / 100
  const whole = Number.isInteger(n)
  return '₦' + n.toLocaleString('en-NG', { minimumFractionDigits: whole ? 0 : 2, maximumFractionDigits: 2 })
}

export function bytes(b) {
  b = Number(b || 0)
  const u = ['B', 'KB', 'MB', 'GB', 'TB']
  let i = 0
  while (b >= 1024 && i < u.length - 1) { b /= 1024; i++ }
  return (i === 0 ? b.toFixed(0) : b.toFixed(b >= 100 ? 0 : 1)) + ' ' + u[i]
}

/** Seconds to "2 h 30 min", "45 min", "3 days". */
export function duration(s) {
  s = Math.max(0, Math.round(Number(s || 0)))
  if (s >= 86400 && s % 86400 === 0) { const d = s / 86400; return d + (d === 1 ? ' day' : ' days') }
  if (s >= 3600) { const h = Math.floor(s / 3600), m = Math.round((s % 3600) / 60); return m ? `${h} h ${m} min` : `${h} h` }
  if (s >= 60) return Math.round(s / 60) + ' min'
  return s + ' s'
}

export function when(v) {
  if (!v) return '–'
  const d = typeof v === 'number' ? new Date(v * 1000) : new Date(v)
  if (isNaN(d)) return '–'
  return d.toLocaleString('en-NG', { dateStyle: 'medium', timeStyle: 'short' })
}

export function day(v) {
  const d = new Date(v)
  return isNaN(d) ? '' : d.toLocaleDateString('en-NG', { month: 'short', day: 'numeric' })
}

export function speed(kbps) {
  return kbps >= 1000 ? +(kbps / 1000).toFixed(1) + ' Mbps' : kbps + ' kbps'
}

/** "0801 234 5678" typed by a person is fine; this only checks it is plausible before we call the API. */
export function plausiblePhone(p) {
  const d = String(p || '').replace(/[^\d+]/g, '')
  return /^(\+?234|0)[789]\d{9}$/.test(d) || /^\+\d{8,15}$/.test(d)
}

/**
 * The router's login address arrives in the URL (`link`). We send a one-time
 * ticket there, so it must really be a hotspot on a local network: a crafted
 * link to another site must never receive it.
 */
export function safeHotspotLink(link, extraHosts = []) {
  let u
  try { u = new URL(link) } catch { return null }
  if (u.protocol !== 'http:' && u.protocol !== 'https:') return null
  if (u.username || u.password) return null
  const h = u.hostname.toLowerCase()
  const m = h.match(/^(\d+)\.(\d+)\.(\d+)\.(\d+)$/)
  if (m) {
    const [a, b] = [Number(m[1]), Number(m[2])]
    const ok = a === 10 || (a === 172 && b >= 16 && b <= 31) || (a === 192 && b === 168)
    return ok ? u.toString() : null
  }
  if (h.endsWith('.lan') || h.endsWith('.local') || extraHosts.map((x) => x.toLowerCase()).includes(h)) return u.toString()
  return null
}
