<template>
  <div class="admin">
    <nav class="side" aria-label="Main">
      <div class="brand">{{ cfg.brand }}</div>
      <router-link v-for="l in links" :key="l.to" :to="l.to" :exact="l.to === '/admin'">{{ l.label }}</router-link>
      <div style="flex:1"></div>
      <button class="btn ghost small" @click="signOut">Sign out</button>
    </nav>
    <main class="main">
      <Notice v-if="noTotp" kind="warn" text="Turn on 2-step verification for your account in Settings. Staff accounts handle money and customer data." />
      <router-view />
    </main>
  </div>
</template>
<script setup>
import { computed } from 'vue'
import { useRouter } from 'vue-router'
import { admin, adminToken, config, setAdminExpiredHandler } from '../../api'
import Notice from '../../components/Notice.vue'

const cfg = config(), router = useRouter()
const links = [
  { to: '/admin', label: 'Overview' }, { to: '/admin/analytics', label: 'Analytics' }, { to: '/admin/customers', label: 'Customers' },
  { to: '/admin/orders', label: 'Orders & refunds' }, { to: '/admin/plans', label: 'Plans' }, { to: '/admin/network', label: 'Network' },
  { to: '/admin/security', label: 'Security' }, { to: '/admin/audit', label: 'Audit log' }, { to: '/admin/settings', label: 'Settings' },
]
const noTotp = computed(() => { try { return sessionStorage.getItem('bms_admin_totp') === '0' } catch { return false } })
setAdminExpiredHandler(() => router.replace({ path: '/admin/login', query: { next: router.currentRoute.value.fullPath } }))
async function signOut() { try { await admin.logout() } catch { /* ignore */ } adminToken.clear(); router.replace('/admin/login') }
</script>
