import { createRouter, createWebHistory } from 'vue-router'
import { adminToken } from './api'
import { loadMe, can, homePath, clearSession } from './session'

// One app, two areas on the same host: customers at "/", staff under "/admin".
const routes = [
  { path: '/', component: () => import('./views/Plans.vue') },
  { path: '/order/:reference', component: () => import('./views/Order.vue'), props: true },
  { path: '/connect', component: () => import('./views/Connect.vue') },
  { path: '/help', component: () => import('./views/Report.vue') },
  { path: '/member', component: () => import('./views/MemberAuth.vue') },
  { path: '/member/home', component: () => import('./views/MemberHome.vue') },
  { path: '/member/wallet', component: () => import('./views/MemberHome.vue') },
  {
    path: '/admin',
    component: () => import('./views/admin/Layout.vue'),
    meta: { staff: true },
    children: [
      // `perm` is the permission the server will ask for. Pages a role cannot use are not shown.
      { path: '', component: () => import('./views/admin/Overview.vue'), meta: { perm: 'dashboard' } },
      { path: 'analytics', component: () => import('./views/admin/Analytics.vue'), meta: { perm: 'dashboard' } },
      { path: 'customers', component: () => import('./views/admin/Customers.vue'), meta: { perm: 'customers' } },
      { path: 'customers/:id', component: () => import('./views/admin/CustomerDetail.vue'), props: true, meta: { perm: 'customers' } },
      { path: 'orders', component: () => import('./views/admin/Orders.vue'), meta: { perm: 'orders' } },
      { path: 'plans', component: () => import('./views/admin/PlansAdmin.vue'), meta: { perm: 'plans_read' } },
      { path: 'vouchers', component: () => import('./views/admin/Vouchers.vue'), meta: { perm: 'manual_sale' } },
      { path: 'complaints', component: () => import('./views/admin/Complaints.vue'), meta: { perm: 'complaints' } },
      { path: 'errors', component: () => import('./views/admin/Errors.vue'), meta: { perm: 'errors_view' } },
      { path: 'staff', component: () => import('./views/admin/Staff.vue'), meta: { perm: 'users' } },
      { path: 'network', component: () => import('./views/admin/Network.vue'), meta: { perm: 'dashboard' } },
      { path: 'security', component: () => import('./views/admin/Security.vue'), meta: { perm: 'dashboard' } },
      { path: 'audit', component: () => import('./views/admin/Audit.vue'), meta: { perm: 'activity' } },
      { path: 'settings', component: () => import('./views/admin/Settings.vue') },
    ],
  },
  { path: '/admin/login', component: () => import('./views/admin/Login.vue') },
  { path: '/admin/setup', component: () => import('./views/admin/Setup.vue'), meta: { auth: true } },
  { path: '/:rest(.*)*', redirect: '/' },
]

export const router = createRouter({ history: createWebHistory(), routes, scrollBehavior: () => ({ top: 0 }) })

// Staff pages need a token. The server is the real gatekeeper (every /admin call checks it);
// this only saves a pointless round trip and keeps people on pages their role can use.
router.beforeEach(async (to) => {
  const staff = to.matched.some((r) => r.meta.staff || r.meta.auth)
  if (!staff) return
  if (!adminToken.get()) { clearSession(); return { path: '/admin/login', query: { next: to.fullPath } } }
  let me
  try { me = await loadMe() } catch { clearSession(); adminToken.clear(); return { path: '/admin/login', query: { next: to.fullPath } } }
  // First sign-in (or a reset): 2-step verification must be set up before anything else.
  if (me.setup_required && to.path !== '/admin/setup') return { path: '/admin/setup' }
  if (!me.setup_required && to.path === '/admin/setup' && me.mfa_method !== 'none' && !to.query.change) return { path: homePath() }
  const need = to.matched.map((r) => r.meta.perm).filter(Boolean).at(-1)
  if (need && !can(need)) return { path: homePath() }
})
