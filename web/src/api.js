// The one place the app talks to the server. Everything is under /v1 on the same host.

let cfg = { tenant: '', site: '', brand: 'Wi-Fi', hotspotHosts: [] }
export const config = () => cfg
export async function loadConfig() {
  const r = await fetch('/config.json', { cache: 'no-store' })
  cfg = { ...cfg, ...(await r.json()) }
  document.title = cfg.brand
  return cfg
}

export class ApiError extends Error {
  constructor(status, message) { super(message); this.status = status }
}

const store = (kind) => ({
  get: () => { try { return (kind === 'admin' ? sessionStorage : localStorage).getItem('bms_' + kind) } catch { return null } },
  set: (v) => { try { (kind === 'admin' ? sessionStorage : localStorage).setItem('bms_' + kind, v) } catch { /* private mode */ } },
  clear: () => { try { (kind === 'admin' ? sessionStorage : localStorage).removeItem('bms_' + kind) } catch { /* ignore */ } },
})
export const memberToken = store('member')
export const adminToken = store('admin')

let onAdminExpired = () => {}
export const setAdminExpiredHandler = (fn) => { onAdminExpired = fn }

async function call(method, path, { body, token, expired } = {}) {
  const headers = {}
  if (body !== undefined) headers['content-type'] = 'application/json'
  if (token) headers.authorization = 'Bearer ' + token
  let res
  try {
    res = await fetch(path, { method, headers, body: body === undefined ? undefined : JSON.stringify(body) })
  } catch {
    throw new ApiError(0, 'Cannot reach the server. Check your connection and try again.')
  }
  const text = await res.text()
  let data = null
  try { data = text ? JSON.parse(text) : null } catch { data = null }
  if (!res.ok) {
    if (res.status === 401 && expired) expired()
    const fallback = { 401: 'Please sign in.', 403: 'You do not have permission to do that.', 429: 'Too many tries. Please wait a few minutes.', 404: 'Not found.' }[res.status] || 'Something went wrong. Please try again.'
    throw new ApiError(res.status, (data && data.error) || fallback)
  }
  return data
}

const t = () => `/v1/t/${cfg.tenant}`

// ---- Customers -------------------------------------------------------------
export const portal = {
  plans: (site = cfg.site) => call('GET', `${t()}/sites/${site}/plans`),
  order: (body) => call('POST', `${t()}/orders`, { body }),
  orderStatus: (ref) => call('GET', `${t()}/orders/${encodeURIComponent(ref)}`),
  connect: (body) => call('POST', `${t()}/connect`, { body }),
  refundRequest: (ref, reason) => call('POST', `${t()}/orders/${encodeURIComponent(ref)}/refund`, { body: { reason } }),
  refundStatus: (ref) => call('GET', `${t()}/orders/${encodeURIComponent(ref)}/refund`),
}

const m = (method, path, body) => call(method, `${t()}/members${path}`, { body, token: memberToken.get(), expired: () => memberToken.clear() })
export const member = {
  register: (b) => call('POST', `${t()}/members/register`, { body: b }),
  login: (b) => call('POST', `${t()}/members/login`, { body: b }),
  otpRequest: (phone) => call('POST', `${t()}/members/otp/request`, { body: { phone } }),
  otpVerify: (phone, code) => call('POST', `${t()}/members/otp/verify`, { body: { phone, code } }),
  logout: () => m('POST', '/logout'),
  me: () => m('GET', '/me'),
  wallet: () => m('GET', '/wallet'),
  topup: (amount_kobo) => m('POST', '/wallet/topup', { site: cfg.site, amount_kobo }),
  buy: (plan_id) => m('POST', '/wallet/buy', { site: cfg.site, plan_id }),
  claim: (code) => m('POST', '/claim', { code }),
  connect: (body) => m('POST', '/connect', { site: cfg.site, ...body }),
  devices: () => m('GET', '/devices'),
  swap: (b) => m('POST', '/devices/swap', b),
}

// ---- Staff -----------------------------------------------------------------
const a = (method, path, body) => call(method, `${t()}/admin${path}`, { body, token: adminToken.get(), expired: () => { adminToken.clear(); onAdminExpired() } })
const qs = (o) => {
  const p = new URLSearchParams()
  Object.entries(o || {}).forEach(([k, v]) => { if (v !== undefined && v !== null && v !== '') p.set(k, v) })
  const s = p.toString()
  return s ? '?' + s : ''
}
export const admin = {
  login: (b) => call('POST', `${t()}/admin/login`, { body: b }),
  logout: () => a('POST', '/logout'),
  totpSetup: () => a('POST', '/totp/setup'),
  totpConfirm: (code) => a('POST', '/totp/confirm', { code }),
  overview: (range) => a('GET', '/overview' + qs({ range })),
  analytics: (what, range) => a('GET', `/analytics/${what}` + qs({ range })),
  customers: (o) => a('GET', '/customers' + qs(o)),
  customer: (id) => a('GET', `/customers/${id}`),
  orders: (o) => a('GET', '/orders' + qs(o)),
  refunds: (status) => a('GET', '/refunds' + qs({ status })),
  refund: (orderId, note) => a('POST', `/orders/${orderId}/refund`, { note }),
  declineRefund: (id, note) => a('POST', `/refunds/${id}/decline`, { note }),
  plans: () => a('GET', '/plans'),
  savePlan: (id, body) => (id ? a('PUT', `/plans/${id}`, body) : a('POST', '/plans', body)),
  setPlanActive: (id, active) => a('PUT', `/plans/${id}/active`, { active }),
  profiles: () => a('GET', '/speed-profiles'),
  createProfile: (b) => a('POST', '/speed-profiles', b),
  network: () => a('GET', '/network'),
  createNodeToken: (b) => a('POST', '/node-tokens', b),
  revokeNodeToken: (id) => a('POST', `/node-tokens/${id}/revoke`),
  security: () => a('GET', '/security'),
  audit: (o) => a('GET', '/audit' + qs(o)),
  auditVerify: () => a('GET', '/audit/verify'),
  auditCsvUrl: () => `${t()}/admin/audit.csv`,
  /** CSV needs the bearer token, so fetch it and hand the browser a file. */
  async downloadAudit() {
    const r = await fetch(`${t()}/admin/audit.csv`, { headers: { authorization: 'Bearer ' + adminToken.get() } })
    if (!r.ok) throw new ApiError(r.status, 'Could not export the audit log.')
    const url = URL.createObjectURL(await r.blob())
    const el = Object.assign(document.createElement('a'), { href: url, download: 'audit.csv' })
    document.body.appendChild(el); el.click(); el.remove(); URL.revokeObjectURL(url)
  },
  setPaystackKey: (secret) => a('PUT', '/secrets/paystack', { secret }),
}
