// The one place the app talks to the server. Everything is under /v1 on the same host.

let cfg = { tenant: '', site: '', brand: 'Wi-Fi', hotspotHosts: [] }
export const config = () => cfg
export async function loadConfig() {
  const r = await fetch('/config.json', { cache: 'no-store' })
  cfg = { ...cfg, ...(await r.json()) }
  // The establishment's own settings (name, colour, support phone, sites) live on the server, so one build
  // of this app can serve any business. They win over the static file, which only has to name the tenant.
  try {
    const b = await fetch(`/v1/t/${cfg.tenant}/brand`, { cache: 'no-store' })
    if (b.ok) {
      const v = await b.json()
      cfg = {
        ...cfg,
        brand: v.brand || cfg.brand, tagline: v.tagline || '', primary: v.primary || '',
        supportPhone: v.supportPhone || cfg.supportPhone || '',
        hotspotHosts: [...new Set([...(cfg.hotspotHosts || []), ...(v.hotspotHosts || [])])], sites: v.sites || [],
      }
    }
  } catch { /* the static file is enough to start */ }
  document.title = cfg.brand
  return cfg
}

export class ApiError extends Error {
  constructor(status, message, code) { super(message); this.status = status; this.code = code }
}

const store = (kind) => ({
  get: () => { try { return (kind === 'admin' ? sessionStorage : localStorage).getItem('bms_' + kind) } catch { return null } },
  set: (v) => { try { (kind === 'admin' ? sessionStorage : localStorage).setItem('bms_' + kind, v) } catch { /* private mode */ } },
  clear: () => { try { (kind === 'admin' ? sessionStorage : localStorage).removeItem('bms_' + kind) } catch { /* ignore */ } },
})
export const memberToken = store('member')
export const adminToken = store('admin')

let onAdminExpired = () => {}
let onMfaRequired = null
export const setMfaRequiredHandler = (fn) => { onMfaRequired = fn }
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
    if (res.status === 403 && data && data.code === 'mfa_required' && onMfaRequired) onMfaRequired()
    throw new ApiError(res.status, (data && data.error) || fallback, data && data.code)
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
  complain: (body) => call('POST', `${t()}/complaints`, { body }),
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
  // Sign in is two steps: password, then a code. A first-time account gets a limited token and must enrol.
  login: (b) => call('POST', `${t()}/admin/login`, { body: b }),
  loginVerify: (challenge, code) => call('POST', `${t()}/admin/login/verify`, { body: { challenge, code } }),
  logout: () => a('POST', '/logout'),
  me: () => a('GET', '/me'),
  mfaStart: (method, phone) => a('POST', '/2fa/start', { method, phone: phone || undefined }),
  mfaConfirm: (challenge, code) => a('POST', '/2fa/confirm', { challenge, code }),
  changePassword: (current, next) => a('PUT', '/password', { current, new: next }),
  users: (all) => a('GET', '/users' + qs({ status: all ? 'all' : '' })),
  createUser: (b) => a('POST', '/users', b),
  updateUser: (id, b) => a('PUT', `/users/${id}`, b),
  deleteUser: (id) => a('DELETE', `/users/${id}`),
  resetPassword: (id, password) => a('POST', `/users/${id}/reset-password`, { password }),
  resetMfa: (id) => a('POST', `/users/${id}/reset-2fa`),
  userActivity: (id, o) => a('GET', `/users/${id}/activity` + qs(o)),
  vouchers: (o) => a('GET', '/vouchers' + qs(o)),
  manualSale: (b) => a('POST', '/vouchers/manual', b),
  specialVoucher: (b) => a('POST', '/vouchers/special', b),
  revokeVoucher: (id) => a('POST', `/vouchers/${id}/revoke`),
  complaints: (o) => a('GET', '/complaints' + qs(o)),
  complaint: (id) => a('GET', `/complaints/${id}`),
  updateComplaint: (id, b) => a('PUT', `/complaints/${id}`, b),
  addNote: (id, body) => a('POST', `/complaints/${id}/notes`, { body }),
  errors: (o) => a('GET', '/errors' + qs(o)),
  errorSummary: () => a('GET', '/errors/summary'),
  error: (id) => a('GET', `/errors/${id}`),
  resolveError: (id, all_same) => a('POST', `/errors/${id}/resolve`, { all_same }),
  saveBrand: (b) => a('PUT', '/brand', b),
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
  async downloadErrors(o) {
    const r = await fetch(`${t()}/admin/errors.csv` + qs(o), { headers: { authorization: 'Bearer ' + adminToken.get() } })
    if (!r.ok) throw new ApiError(r.status, 'Could not export the error log.')
    const url = URL.createObjectURL(await r.blob())
    const el = Object.assign(document.createElement('a'), { href: url, download: 'errors.csv' })
    document.body.appendChild(el); el.click(); el.remove(); URL.revokeObjectURL(url)
  },
  setPaystackKey: (secret) => a('PUT', '/secrets/paystack', { secret }),
}
