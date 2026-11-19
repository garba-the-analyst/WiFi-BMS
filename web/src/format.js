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

/** Staff roles in plain words. */
export const ROLE_LABELS = { system_admin: 'System admin', ceo: 'CEO', admin: 'Admin', customer_rep: 'Customer rep' }
export const roleLabel = (r) => ROLE_LABELS[r] || r

/** Mix two #rrggbb colours: t=0 gives a, t=1 gives b. Used to derive hover and dark-mode shades from one brand colour. */
export function mixHex(a, b, t) {
  const p = (h) => [1, 3, 5].map((i) => parseInt(h.slice(i, i + 2), 16))
  const [x, y] = [p(a), p(b)]
  return '#' + x.map((v, i) => Math.round(v + (y[i] - v) * t).toString(16).padStart(2, '0')).join('')
}
export const isHexColour = (c) => /^#[0-9a-fA-F]{6}$/.test(String(c || ''))

/** Plan allowance as a sentence: "5 GB · 2 devices · 2 Mbps" style pieces, for admin tables. */
export function planSummary(p) {
  const bits = []
  if (p.active_seconds) bits.push(duration(p.active_seconds) + ' of use')
  if (p.data_bytes) bits.push(bytes(p.data_bytes))
  bits.push(`${p.device_limit} device${p.device_limit === 1 ? '' : 's'}`)
  bits.push(`${speed(p.down_kbps)} down / ${speed(p.up_kbps)} up`)
  return bits.join(' · ')
}

/** Plain-language time from an ISO string: "3 min ago". */
export function ago(v, now = Date.now()) {
  const d = new Date(v)
  if (isNaN(d)) return '–'
  const s = Math.max(0, Math.round((now - d.getTime()) / 1000))
  if (s < 60) return 'just now'
  if (s < 3600) return Math.round(s / 60) + ' min ago'
  if (s < 86400) return Math.round(s / 3600) + ' h ago'
  return Math.round(s / 86400) + ' d ago'
}

/** How good a radio signal is, in words people can act on. Matches the server's "weak link" rule (-75 dBm). */
export function signalQuality(dbm) {
  if (dbm == null || isNaN(dbm)) return { label: '–', cls: '' }
  if (dbm >= -65) return { label: 'strong', cls: 'ok' }
  if (dbm > -75) return { label: 'fair', cls: 'warn' }
  return { label: 'weak', cls: 'bad' }
}
export const KIND_LABELS = { router: 'Router', ap: 'Access point', cpe: 'Radio link', switch: 'Switch', controller: 'Controller', other: 'Device' }
export const kindLabel = (k) => KIND_LABELS[k] || k
