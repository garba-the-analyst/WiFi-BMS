<template>
  <div>
    <div class="head"><div><router-link to="/admin/customers" class="small">‹ Customers</router-link><h1>{{ d?.account.display_name || d?.account.phone || 'Customer' }}</h1></div></div>
    <Notice :text="error" kind="bad" />
    <template v-if="d">
      <div class="panel row" style="gap:28px">
        <div><div class="muted small">Phone</div>{{ d.account.phone }}</div><div><div class="muted small">Status</div>{{ d.account.status }}</div>
        <div><div class="muted small">Joined</div>{{ when(d.account.created_at) }}</div><div><div class="muted small">Last sign-in</div>{{ when(d.account.last_login_at) }}</div>
        <div><div class="muted small">Failed sign-ins</div>{{ d.account.failed_logins }}</div>
      </div>
      <div class="panel"><h2 style="margin-bottom:8px">Plans</h2><div class="tablewrap"><table><thead><tr><th>Plan</th><th>Status</th><th>Granted</th><th>Expires</th><th class="right">Time left</th><th class="right">Data left</th></tr></thead><tbody>
        <tr v-for="e in d.entitlements" :key="e.id"><td>{{ e.plan }}</td><td>{{ e.status }}</td><td>{{ when(e.granted_at) }}</td><td>{{ when(e.expires_at) }}</td><td class="right">{{ e.remaining_active_seconds == null ? '–' : duration(e.remaining_active_seconds) }}</td><td class="right">{{ e.remaining_bytes == null ? '–' : bytes(e.remaining_bytes) }}</td></tr>
        <tr v-if="!d.entitlements.length"><td colspan="6" class="muted">None.</td></tr></tbody></table></div></div>
      <div class="panel"><h2 style="margin-bottom:8px">Devices</h2><div class="tablewrap"><table><thead><tr><th>Address</th><th>System</th><th>Type</th><th>Name</th><th class="right">Confidence</th><th>Last seen</th></tr></thead><tbody>
        <tr v-for="x in d.devices" :key="x.id"><td class="mono">{{ x.mac }} <span v-if="x.mac_randomised" class="chip warn" title="Private address: the same phone may appear under different addresses">private</span></td><td>{{ [x.os_family, x.os_version].filter(Boolean).join(' ') || '–' }}</td><td>{{ x.device_class || '–' }}</td><td>{{ x.hostname || '–' }}</td><td class="right">{{ x.confidence == null ? '–' : x.confidence + '%' }}</td><td>{{ when(x.last_seen_at) }}</td></tr>
        <tr v-if="!d.devices.length"><td colspan="6" class="muted">None.</td></tr></tbody></table></div></div>
      <div class="panel"><h2 style="margin-bottom:8px">Sessions</h2><div class="tablewrap"><table><thead><tr><th>Started</th><th>Ended</th><th>Why</th><th>Device</th><th class="right">Active</th><th class="right">Data</th></tr></thead><tbody>
        <tr v-for="s in d.sessions" :key="s.id"><td>{{ when(s.started_at) }}</td><td>{{ s.ended_at ? when(s.ended_at) : 'online' }}</td><td>{{ s.end_reason || '' }}</td><td>{{ s.hostname || s.os_family || s.mac }}</td><td class="right">{{ duration(s.active_seconds) }}</td><td class="right">{{ bytes(+s.bytes_in + +s.bytes_out) }}</td></tr>
        <tr v-if="!d.sessions.length"><td colspan="6" class="muted">None.</td></tr></tbody></table></div></div>
      <div class="cols2">
        <div class="panel"><h2 style="margin-bottom:8px">Sharing flags</h2><p v-if="!d.tether_flags.length" class="muted">None.</p><div v-for="(f, i) in d.tether_flags" :key="i" class="row between small" style="border-top:1px solid var(--soft);padding:6px 0"><span>{{ when(f.ts) }} · {{ f.signal }} · score {{ f.score }}</span><span class="chip warn">{{ f.action }}</span></div></div>
        <div class="panel"><h2 style="margin-bottom:8px">Recent activity</h2><p v-if="!d.audit.length" class="muted">None.</p><div v-for="a in d.audit" :key="a.seq" class="small" style="border-top:1px solid var(--soft);padding:6px 0">{{ when(a.ts) }} · <span class="mono">{{ a.action }}</span></div></div>
      </div>
    </template>
  </div>
</template>
<script setup>
import { ref, onMounted } from 'vue'
import { admin } from '../../api'
import { when, duration, bytes } from '../../format'
import Notice from '../../components/Notice.vue'
const props = defineProps({ id: String })
const d = ref(null), error = ref('')
onMounted(async () => { try { d.value = await admin.customer(props.id) } catch (e) { error.value = e.message } })
</script>
