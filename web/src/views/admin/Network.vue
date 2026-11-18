<template>
  <div>
    <div class="head"><h1>Network</h1></div>
    <Notice :text="error" kind="bad" />
    <div class="kpis"><div v-for="s in n?.sites || []" :key="s.id" class="panel kpi">
      <div class="l">{{ s.name }}</div><div class="v">{{ s.online_now }} online</div>
      <div class="small" :style="{ color: agentOk(s) ? 'var(--ok)' : 'var(--bad)' }">{{ agentOk(s) ? 'Site agent connected' : s.agent_last_seen ? 'Site agent silent since ' + when(s.agent_last_seen) : 'Site agent has never reported' }}</div>
      <div class="small muted" v-if="s.spool_depth > 0">{{ s.spool_depth }} updates waiting to upload</div></div></div>
    <div class="panel"><h2 style="margin-bottom:8px">Access points &amp; routers</h2><div class="tablewrap"><table><thead><tr><th>Name</th><th>Kind</th><th>Model</th><th>Address</th><th>Firmware</th><th>Last seen</th></tr></thead><tbody>
      <tr v-for="x in n?.nodes || []" :key="x.id"><td>{{ x.name }}</td><td>{{ x.kind }}</td><td>{{ x.model || '–' }}</td><td class="mono">{{ x.mgmt_ip || '–' }}</td><td>{{ x.firmware || '–' }}</td><td>{{ when(x.last_seen_at) }}</td></tr>
      <tr v-if="!(n?.nodes || []).length"><td colspan="6" class="muted">No devices registered yet.</td></tr></tbody></table></div></div>
    <div class="panel"><h2 style="margin-bottom:8px">Site agent keys</h2><div class="tablewrap"><table><thead><tr><th>Name</th><th>Created</th><th>Last used</th><th>Status</th></tr></thead><tbody>
      <tr v-for="t in n?.node_tokens || []" :key="t.id"><td>{{ t.name }}</td><td>{{ when(t.created_at) }}</td><td>{{ when(t.last_used_at) }}</td><td><span :class="['chip', t.revoked_at ? 'bad' : 'ok']">{{ t.revoked_at ? 'revoked' : 'active' }}</span></td></tr></tbody></table></div>
      <p class="muted small" style="margin:8px 0 0">Create or revoke keys in <router-link to="/admin/settings">Settings</router-link>.</p></div>
  </div>
</template>
<script setup>
import { ref, onMounted, onUnmounted } from 'vue'
import { admin } from '../../api'
import { when } from '../../format'
import Notice from '../../components/Notice.vue'
const n = ref(null), error = ref('')
const agentOk = (s) => s.agent_last_seen && Date.now() - new Date(s.agent_last_seen).getTime() < 3 * 60 * 1000
async function load() { try { n.value = await admin.network() } catch (e) { error.value = e.message } }
let t; onMounted(() => { load(); t = setInterval(load, 20000) }); onUnmounted(() => clearInterval(t))
</script>
