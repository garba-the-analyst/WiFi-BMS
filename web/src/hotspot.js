// What the router tells us when it sends a customer here, kept for the whole visit.
// The router's login.html redirects to: /?site=<site>&mac=$(mac)&link=$(link-login-only)&dst=$(link-orig-esc)
import { config } from './api'
import { safeHotspotLink } from './format'

const KEY = 'bms_hotspot'

export function captureHotspotParams(search = window.location.search) {
  const p = new URLSearchParams(search)
  const mac = p.get('mac')
  if (!mac) return
  const info = { mac, link: p.get('link') || '', dst: p.get('dst') || '', site: p.get('site') || '' }
  try { sessionStorage.setItem(KEY, JSON.stringify(info)) } catch { /* ignore */ }
}

export function hotspot() {
  try { return JSON.parse(sessionStorage.getItem(KEY) || 'null') } catch { return null }
}

/** Log this device in at the router with a one-time ticket. Returns false if we have nowhere safe to send it. */
export function loginAtRouter(ticket) {
  const h = hotspot()
  const link = h && safeHotspotLink(h.link, config().hotspotHosts || [])
  if (!link) return false
  const u = new URL(link)
  u.searchParams.set('username', ticket.username)
  u.searchParams.set('password', ticket.password)
  if (h.dst) u.searchParams.set('dst', h.dst)
  window.location.assign(u.toString())
  return true
}
