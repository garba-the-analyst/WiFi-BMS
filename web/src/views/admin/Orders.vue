<template>
  <div>
    <div class="head"><h1>Orders &amp; refunds</h1><div class="seg"><button :class="{ on: tab === 'queue' }" @click="tab = 'queue'">Refund requests <span v-if="queue.length" class="chip bad">{{ queue.length }}</span></button><button :class="{ on: tab === 'orders' }" @click="tab = 'orders'">All orders</button><button :class="{ on: tab === 'history' }" @click="tab = 'history'">Refund history</button></div></div>
    <Notice :text="error" kind="bad" /><Notice :text="info" kind="ok" />
    <Notice v-if="!isOwner" kind="warn" text="Only owners can send refunds. You can review requests and decline them." />

    <div v-if="tab === 'queue'" class="panel tablewrap"><table>
      <thead><tr><th>Requested</th><th>Order</th><th>Phone</th><th class="right">Amount</th><th>Reason</th><th></th></tr></thead>
      <tbody>
        <tr v-for="r in queue" :key="r.id"><td>{{ when(r.created_at) }}</td><td class="mono small">{{ r.reference }}<br /><span class="muted">{{ r.purpose === 'topup' ? 'wallet top-up' : 'plan' }} · {{ r.paid_with }}</span></td><td>{{ r.phone }}</td><td class="right">{{ naira(r.amount_kobo) }}</td><td style="max-width:260px">{{ r.reason || '–' }}</td>
          <td class="nowrap"><button class="btn small" :disabled="busy || !isOwner" @click="refund(r.order_id, r.amount_kobo)">Refund</button> <button class="btn ghost small" :disabled="busy" @click="decline(r.id)">Decline</button></td></tr>
        <tr v-if="!queue.length"><td colspan="6" class="muted">No refund requests waiting.</td></tr>
      </tbody></table></div>

    <template v-if="tab === 'orders'">
      <div class="row" style="margin-bottom:12px"><input v-model="q" type="search" placeholder="Search reference or phone" style="max-width:260px" aria-label="Search orders" />
        <select v-model="status" style="max-width:200px" aria-label="Status"><option value="">All statuses</option><option v-for="s in statuses" :key="s" :value="s">{{ s }}</option></select></div>
      <div class="panel tablewrap"><table>
        <thead><tr><th>Date</th><th>Reference</th><th>What</th><th>Phone</th><th class="right">Amount</th><th>Status</th><th></th></tr></thead>
        <tbody>
          <tr v-for="o in orders" :key="o.id"><td>{{ when(o.created_at) }}</td><td class="mono small">{{ o.reference }}</td><td>{{ o.plan }}<br /><span class="muted small">{{ o.paid_with }}</span></td><td>{{ o.phone }}</td><td class="right">{{ naira(o.amount_kobo) }}</td>
            <td><span :class="['chip', chip(o.status)]">{{ o.status }}</span><span v-if="o.refund_status" class="chip" style="margin-left:4px">refund: {{ o.refund_status }}</span></td>
            <td><button v-if="refundable(o)" class="btn ghost small" :disabled="busy || !isOwner" @click="refund(o.id, o.amount_kobo)">Refund</button></td></tr>
          <tr v-if="!orders.length"><td colspan="7" class="muted">No orders.</td></tr>
        </tbody></table></div>
    </template>

    <div v-if="tab === 'history'" class="panel tablewrap"><table>
      <thead><tr><th>Date</th><th>Order</th><th class="right">Amount</th><th>Result</th><th>Method</th><th>Note</th></tr></thead>
      <tbody><tr v-for="r in history" :key="r.id"><td>{{ when(r.decided_at || r.created_at) }}</td><td class="mono small">{{ r.reference }}</td><td class="right">{{ naira(r.amount_kobo) }}</td><td><span :class="['chip', r.status === 'processed' ? 'ok' : r.status === 'failed' ? 'bad' : '']">{{ r.status }}</span></td><td>{{ r.method || '–' }}</td><td>{{ r.note || '' }}</td></tr>
        <tr v-if="!history.length"><td colspan="6" class="muted">No refunds yet.</td></tr></tbody></table></div>
  </div>
</template>
<script setup>
import { ref, computed, watch, onMounted } from 'vue'
import { admin } from '../../api'
import { naira, when } from '../../format'
import Notice from '../../components/Notice.vue'

const statuses = ['paid', 'provisioned', 'refund_flagged', 'refunded', 'pending', 'failed', 'expired', 'created']
const tab = ref('queue'), q = ref(''), status = ref(''), orders = ref([]), queue = ref([]), history = ref([])
const busy = ref(false), error = ref(''), info = ref('')
const isOwner = computed(() => { try { return sessionStorage.getItem('bms_admin_role') === 'owner' } catch { return false } })
const refundable = (o) => ['paid', 'provisioned', 'refund_flagged'].includes(o.status) && o.refund_status !== 'processed'
const chip = (s) => (s === 'provisioned' || s === 'paid' ? 'ok' : s === 'refund_flagged' || s === 'failed' ? 'bad' : s === 'refunded' ? 'warn' : '')

async function load() {
  try {
    error.value = ''
    const [qu, all] = await Promise.all([admin.refunds('requested'), admin.refunds()])
    queue.value = qu; history.value = all.filter((r) => r.status !== 'requested')
    if (tab.value === 'orders') orders.value = await admin.orders({ q: q.value, status: status.value, limit: 100 })
  } catch (e) { error.value = e.message }
}
async function refund(orderId, kobo) {
  if (!window.confirm(`Refund ${naira(kobo)}? This sends money back and cancels any plan from this order. It cannot be undone.`)) return
  busy.value = true; info.value = ''; error.value = ''
  try { const r = await admin.refund(orderId, ''); info.value = `Refunded ${naira(r.amount_kobo)} by ${r.method}.` ; await load() } catch (e) { error.value = e.message } finally { busy.value = false }
}
async function decline(id) {
  const note = window.prompt('Reason for declining (shown in history):', '')
  if (note === null) return
  busy.value = true
  try { await admin.declineRefund(id, note); await load() } catch (e) { error.value = e.message } finally { busy.value = false }
}
let t
watch([q, status], () => { clearTimeout(t); t = setTimeout(load, 300) })
watch(tab, load); onMounted(load)
</script>
