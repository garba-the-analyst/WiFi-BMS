<template>
  <div>
    <div class="head"><h1>Complaints</h1><div class="row"><select v-model="status" aria-label="Status" style="max-width:160px"><option value="">All</option><option value="open">Open</option><option value="in_progress">In progress</option><option value="resolved">Resolved</option></select><input v-model="q" type="search" placeholder="Phone, order or words" style="max-width:240px" aria-label="Search complaints" /></div></div>
    <Notice :text="error" kind="bad" />
    <div class="cols2" style="grid-template-columns:minmax(0,1.1fr) minmax(0,1fr)">
      <div class="panel tablewrap"><table><thead><tr><th>Ref</th><th>Customer</th><th>Problem</th><th>Status</th></tr></thead><tbody>
        <tr v-for="c in rows" :key="c.id" :style="{ cursor: 'pointer', background: open?.id === c.id ? 'var(--soft)' : '' }" tabindex="0" @click="show(c)" @keydown.enter="show(c)">
          <td class="mono small">{{ c.reference }}</td><td class="nowrap">{{ c.phone }}<div class="muted small">{{ ago(c.created_at) }}</div></td>
          <td><span class="chip">{{ c.category }}</span> {{ c.message.slice(0, 70) }}{{ c.message.length > 70 ? '…' : '' }}</td><td><span :class="['chip', c.status === 'open' ? 'bad' : c.status === 'in_progress' ? 'warn' : 'ok']">{{ c.status.replace('_', ' ') }}</span></td></tr>
        <tr v-if="!rows.length"><td colspan="4" class="muted">No complaints.</td></tr></tbody></table></div>

      <div v-if="open" class="panel stack">
        <div class="row between"><h2>{{ open.phone }}</h2><span class="muted small">{{ when(open.created_at) }}</span></div>
        <p style="margin:0;white-space:pre-wrap">{{ open.message }}</p>
        <div v-if="open.customer" class="notice">Member: {{ open.customer.name || 'Unnamed' }} · wallet {{ naira(open.customer.balance_kobo) }} · {{ open.customer.live_plans }} live plan(s) · <router-link :to="`/admin/customers/${open.customer.id}`">open customer</router-link></div>
        <div v-if="open.order" class="notice">Order {{ open.order.reference }}: {{ open.order.status }} · {{ naira(open.order.amount_kobo) }} · paid with {{ open.order.paid_with }}</div>
        <div v-else-if="open.order_ref" class="notice warn">Order {{ open.order_ref }} was not found. Check the reference with the customer.</div>
        <div class="row">
          <select :value="open.status" aria-label="Change status" style="max-width:170px" @change="update({ status: $event.target.value })"><option value="open">Open</option><option value="in_progress">In progress</option><option value="resolved">Resolved</option></select>
          <button class="btn ghost small" @click="update({ assign: open.assigned_to ? 'none' : 'me' })">{{ open.assigned_to ? 'Unassign (' + open.assigned_to + ')' : 'Assign to me' }}</button>
        </div>
        <h3>Notes</h3>
        <div v-for="n in open.notes" :key="n.id" class="small" style="border-top:1px solid var(--soft);padding-top:6px"><strong>{{ n.display_name || n.author || 'Staff' }}</strong> <span class="muted">{{ when(n.created_at) }}</span><div style="white-space:pre-wrap">{{ n.body }}</div></div>
        <p v-if="!open.notes.length" class="muted small" style="margin:0">No notes yet. Write what you did so the next person knows.</p>
        <form class="row" style="flex-wrap:nowrap" @submit.prevent="note"><input v-model="body" maxlength="2000" placeholder="Add a note" aria-label="Add a note" required /><button class="btn">Add</button></form>
      </div>
      <div v-else class="panel muted">Choose a complaint to see the details.</div>
    </div>
  </div>
</template>
<script setup>
import { ref, watch, onMounted } from 'vue'
import { admin } from '../../api'
import { naira, when, ago } from '../../format'
import Notice from '../../components/Notice.vue'

const rows = ref([]), open = ref(null), status = ref('open'), q = ref(''), body = ref(''), error = ref('')
async function load() { try { error.value = ''; rows.value = await admin.complaints({ status: status.value, q: q.value }) } catch (e) { error.value = e.message } }
async function show(c) { try { open.value = await admin.complaint(c.id) } catch (e) { error.value = e.message } }
async function update(b) { try { await admin.updateComplaint(open.value.id, b); await show(open.value); await load() } catch (e) { error.value = e.message } }
async function note() { try { await admin.addNote(open.value.id, body.value); body.value = ''; await show(open.value); await load() } catch (e) { error.value = e.message } }
let t; watch(q, () => { clearTimeout(t); t = setTimeout(load, 300) }); watch(status, load); onMounted(load)
</script>
