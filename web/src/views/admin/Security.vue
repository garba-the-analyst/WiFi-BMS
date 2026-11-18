<template>
  <div>
    <div class="head"><h1>Security</h1></div>
    <Notice :text="error" kind="bad" />
    <div class="kpis" v-if="s"><div v-for="f in s.failures_24h" :key="f.action" class="panel kpi"><div class="l">{{ f.action }} (24 h)</div><div class="v">{{ f.count_24h }}</div></div>
      <div v-if="!s.failures_24h.length" class="panel kpi"><div class="l">Failed sign-ins and rejected requests (24 h)</div><div class="v">0</div></div></div>
    <div class="panel" v-if="s"><h2 style="margin-bottom:8px">Hotspot sharing</h2><p class="muted small" style="margin:0 0 8px">Signals are scored. One signal alone only warns; slowing or disconnecting needs several to agree.</p>
      <div class="tablewrap"><table><thead><tr><th>When</th><th>Device</th><th>Signal</th><th class="right">Score</th><th>Action</th></tr></thead><tbody>
        <tr v-for="(f, i) in s.tether_flags" :key="i"><td>{{ when(f.ts) }}</td><td>{{ f.hostname || f.os_family || '–' }}<br /><span class="mono small muted">{{ f.mac }}</span></td><td>{{ f.signal }}</td><td class="right">{{ f.score }}</td><td><span :class="['chip', f.action === 'disconnect' ? 'bad' : f.action === 'throttle' ? 'warn' : '']">{{ f.action }}</span></td></tr>
        <tr v-if="!s.tether_flags.length"><td colspan="5" class="muted">No sharing flags.</td></tr></tbody></table></div></div>
    <div class="panel" v-if="s"><h2 style="margin-bottom:8px">Payments needing review</h2><div class="tablewrap"><table><thead><tr><th>Date</th><th>Reference</th><th>Phone</th><th class="right">Ordered</th></tr></thead><tbody>
      <tr v-for="o in s.refund_flagged_orders" :key="o.reference"><td>{{ when(o.created_at) }}</td><td class="mono small">{{ o.reference }}</td><td>{{ o.phone }}</td><td class="right">{{ naira(o.amount_kobo) }}</td></tr>
      <tr v-if="!s.refund_flagged_orders.length"><td colspan="4" class="muted">None. A payment lands here when the amount paid differs from the order. Refund it in <router-link to="/admin/orders">Orders &amp; refunds</router-link>.</td></tr></tbody></table></div></div>
  </div>
</template>
<script setup>
import { ref, onMounted } from 'vue'
import { admin } from '../../api'
import { when, naira } from '../../format'
import Notice from '../../components/Notice.vue'
const s = ref(null), error = ref('')
onMounted(async () => { try { s.value = await admin.security() } catch (e) { error.value = e.message } })
</script>
