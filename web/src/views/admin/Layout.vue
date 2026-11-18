<template>
  <div class="admin">
    <nav class="side" aria-label="Main">
      <div class="brand">{{ cfg.brand }}</div>
      <router-link v-for="l in links" :key="l.to" :to="l.to" :exact="l.to === '/admin'">{{ l.label }}</router-link>
      <div style="flex:1"></div>
      <div class="small muted" style="padding:0 10px" v-if="session.me">{{ session.me.display_name || session.me.email }}<br />{{ roleLabel(session.me.role) }}</div>
      <button class="btn ghost small" @click="signOut">Sign out</button>
    </nav>
    <main class="main"><router-view /></main>
  </div>
</template>
<script setup>
import { computed } from 'vue'
import { useRouter } from 'vue-router'
import { admin, adminToken, config, setAdminExpiredHandler, setMfaRequiredHandler } from '../../api'
import { session, can, clearSession } from '../../session'
import { roleLabel } from '../../format'

const cfg = config(), router = useRouter()
// Every entry names the permission the server checks; people only see what they may open.
const all = [
  { to: '/admin', label: 'Overview', perm: 'dashboard' }, { to: '/admin/analytics', label: 'Analytics', perm: 'dashboard' },
  { to: '/admin/customers', label: 'Customers', perm: 'customers' }, { to: '/admin/orders', label: 'Orders & refunds', perm: 'orders' },
  { to: '/admin/vouchers', label: 'Vouchers', perm: 'manual_sale' }, { to: '/admin/complaints', label: 'Complaints', perm: 'complaints' },
  { to: '/admin/plans', label: 'Plans', perm: 'plans_read' }, { to: '/admin/network', label: 'Network', perm: 'dashboard' },
  { to: '/admin/errors', label: 'Error log', perm: 'errors_view' }, { to: '/admin/security', label: 'Security', perm: 'dashboard' },
  { to: '/admin/staff', label: 'Staff', perm: 'users' }, { to: '/admin/audit', label: 'Activity log', perm: 'activity' },
  { to: '/admin/settings', label: 'Settings', perm: null },
]
const links = computed(() => all.filter((l) => !l.perm || can(l.perm)))
const toLogin = () => { clearSession(); router.replace({ path: '/admin/login', query: { next: router.currentRoute.value.fullPath } }) }
setAdminExpiredHandler(toLogin)
setMfaRequiredHandler(() => router.replace('/admin/setup'))
async function signOut() { try { await admin.logout() } catch { /* ignore */ } adminToken.clear(); clearSession(); router.replace('/admin/login') }
</script>
