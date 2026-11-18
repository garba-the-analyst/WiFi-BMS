import { createRouter, createWebHistory } from 'vue-router'
import { adminToken } from './api'

// One app, two areas on the same host: customers at "/", staff under "/admin".
const routes = [
  { path: '/', component: () => import('./views/Plans.vue') },
  { path: '/order/:reference', component: () => import('./views/Order.vue'), props: true },
  { path: '/connect', component: () => import('./views/Connect.vue') },
  { path: '/member', component: () => import('./views/MemberAuth.vue') },
  { path: '/member/home', component: () => import('./views/MemberHome.vue') },
  { path: '/member/wallet', component: () => import('./views/MemberHome.vue') },
  {
    path: '/admin',
    component: () => import('./views/admin/Layout.vue'),
    meta: { staff: true },
    children: [
      { path: '', component: () => import('./views/admin/Overview.vue') },
      { path: 'analytics', component: () => import('./views/admin/Analytics.vue') },
      { path: 'customers', component: () => import('./views/admin/Customers.vue') },
      { path: 'customers/:id', component: () => import('./views/admin/CustomerDetail.vue'), props: true },
      { path: 'orders', component: () => import('./views/admin/Orders.vue') },
      { path: 'plans', component: () => import('./views/admin/PlansAdmin.vue') },
      { path: 'network', component: () => import('./views/admin/Network.vue') },
      { path: 'security', component: () => import('./views/admin/Security.vue') },
      { path: 'audit', component: () => import('./views/admin/Audit.vue') },
      { path: 'settings', component: () => import('./views/admin/Settings.vue') },
    ],
  },
  { path: '/admin/login', component: () => import('./views/admin/Login.vue') },
  { path: '/:rest(.*)*', redirect: '/' },
]

export const router = createRouter({ history: createWebHistory(), routes, scrollBehavior: () => ({ top: 0 }) })

// Staff pages need a token. The server is the real gatekeeper (every /admin call checks it);
// this only saves a pointless round trip and sends people to the sign-in page.
router.beforeEach((to) => {
  if (to.matched.some((r) => r.meta.staff) && !adminToken.get()) return { path: '/admin/login', query: { next: to.fullPath } }
})
