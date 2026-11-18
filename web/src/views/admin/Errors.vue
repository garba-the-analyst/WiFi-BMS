<template>
  <div>
    <div class="head"><h1>Error log</h1><div class="row"><button class="btn ghost small" @click="load">Refresh</button><button class="btn ghost small" @click="download">Download CSV</button></div></div>
    <Notice :text="error" kind="bad" /><Notice :text="info" kind="ok" />

    <div v-if="sum" class="kpis">
      <div class="panel kpi"><div class="l">Open critical</div><div class="v" :style="{ color: sum.open_critical ? 'var(--bad)' : 'var(--ok)' }">{{ sum.open_critical }}</div></div>
      <div v-for="sv in severities" :key="sv" class="panel kpi"><div class="l">{{ sv }} (24 h, open)</div><div class="v">{{ sum.last_24h_by_severity[sv] || 0 }}</div></div>
    </div>
    <div v-if="sum" class="cols2">
      <div class="panel"><h2 style="margin-bottom:8px">Last 24 hours</h2><Chart :labels="sum.per_hour.map((h) => new Date(h.hour).getHours() + ':00')" :series="[{ name: 'Errors', color: 'var(--bad)', values: sum.per_hour.map((h) => h.n) }]" label="Errors per hour" /></div>
      <div class="panel"><h2 style="margin-bottom:8px">Repeating problems (7 days)</h2>
        <div v-for="t in sum.top_problems" :key="t.fingerprint" class="row between" style="padding:6px 0;border-top:1px solid var(--soft)"><span class="small"><span :class="['chip', sevClass(t.severity)]">{{ t.source }}</span> {{ t.message.slice(0, 80) }}</span><span class="nowrap small"><strong>{{ t.times }}×</strong> <button class="btn ghost small" @click="q = t.code">Show</button></span></div>
        <p v-if="!sum.top_problems.length" class="muted small" style="margin:0">Nothing has gone wrong in the last 7 days.</p></div>
    </div>

    <div class="row" style="margin-bottom:12px">
      <select v-model="f.state" aria-label="State" style="max-width:130px"><option value="open">Open</option><option value="resolved">Resolved</option><option value="all">All</option></select>
      <select v-model="f.source" aria-label="Source" style="max-width:160px"><option value="">Every source</option><option v-for="s in sources" :key="s[0]" :value="s[0]">{{ s[1] }}</option></select>
      <select v-model="f.severity" aria-label="Severity" style="max-width:150px"><option value="">Any severity</option><option v-for="s in severities" :key="s" :value="s">{{ s }}</option></select>
      <input v-model="q" type="search" placeholder="Search message, code or request id" style="max-width:300px" aria-label="Search errors" />
    </div>

    <div class="cols2" style="grid-template-columns:minmax(0,1.3fr) minmax(0,1fr)">
      <div class="panel tablewrap"><table><thead><tr><th>When</th><th>Source</th><th>What happened</th><th class="right">7 d</th></tr></thead><tbody>
        <tr v-for="e in rows" :key="e.id" tabindex="0" :style="{ cursor: 'pointer', background: sel?.id === e.id ? 'var(--soft)' : '' }" @click="show(e.id)" @keydown.enter="show(e.id)">
          <td class="nowrap small">{{ ago(e.ts) }}</td><td><span :class="['chip', sevClass(e.severity)]">{{ e.source }}</span></td>
          <td><span class="mono small">{{ e.code }}</span><div class="small">{{ e.message.slice(0, 110) }}{{ e.message.length > 110 ? '…' : '' }}</div></td><td class="right">{{ e.same_7d }}</td></tr>
        <tr v-if="!rows.length"><td colspan="4" class="muted">Nothing here. That is good news.</td></tr></tbody></table>
        <button v-if="rows.length >= 100" class="btn ghost small" style="margin-top:8px" @click="more">Older</button></div>

      <div v-if="sel" class="panel stack">
        <div class="row between"><h2><span :class="['chip', sevClass(sel.severity)]">{{ sel.severity }}</span> {{ sel.source }}</h2><button class="btn ghost small" @click="sel = null">Close</button></div>
        <p style="margin:0;font-weight:600;word-break:break-word">{{ sel.message }}</p>
        <div v-if="sel.hint" class="notice warn"><strong>Check first:</strong> {{ sel.hint }}</div>
        <dl class="small" style="margin:0;display:grid;grid-template-columns:auto 1fr;gap:4px 12px">
          <dt class="muted">Code</dt><dd class="mono" style="margin:0">{{ sel.code }}</dd>
          <dt class="muted">When</dt><dd style="margin:0">{{ when(sel.ts) }}</dd>
          <dt class="muted">Seen</dt><dd style="margin:0">{{ sel.same_total }} time(s), first {{ when(sel.first_seen) }}, last {{ when(sel.last_seen) }}</dd>
          <dt v-if="sel.site" class="muted">Site</dt><dd v-if="sel.site" style="margin:0">{{ sel.site }}</dd>
          <dt v-if="sel.request_id" class="muted">Request id</dt><dd v-if="sel.request_id" class="mono" style="margin:0;user-select:all">{{ sel.request_id }}</dd>
        </dl>
        <div v-if="stack"><h3>Stack trace</h3><pre class="mono small" style="margin:6px 0 0;white-space:pre-wrap;word-break:break-all;max-height:200px;overflow:auto">{{ stack }}</pre></div>
        <div><h3>Details</h3><pre class="mono small" style="margin:6px 0 0;white-space:pre-wrap;word-break:break-all;max-height:260px;overflow:auto">{{ pretty }}</pre></div>
        <div class="row"><button class="btn ghost small" @click="copy">Copy for support</button>
          <template v-if="can('errors_resolve')"><template v-if="!sel.resolved_at"><button class="btn small" @click="resolve(false)">Mark resolved</button><button v-if="sel.same_total > 1" class="btn ghost small" @click="resolve(true)">Resolve all {{ sel.same_total }} like this</button></template>
            <span v-else class="chip ok">resolved by {{ sel.resolved_by || 'staff' }}</span></template></div>
      </div>
      <div v-else class="panel muted">Choose an entry to see everything needed to fix it.</div>
    </div>
  </div>
</template>
<script setup>
import { ref, reactive, computed, watch, onMounted, onUnmounted } from 'vue'
import { admin } from '../../api'
import { can } from '../../session'
import { when, ago } from '../../format'
import Chart from '../../components/Chart.vue'
import Notice from '../../components/Notice.vue'

const sources = [['api', 'Server (API)'], ['database', 'Database'], ['payment', 'Payments'], ['sms', 'SMS'], ['email', 'Email'], ['webapp', 'Web app'], ['hardware', 'Hardware'], ['radius', 'RADIUS'], ['site_agent', 'Site agent'], ['security', 'Security'], ['system', 'System']]
const severities = ['critical', 'error', 'warning', 'info']
const sevClass = (s) => (s === 'critical' || s === 'error' ? 'bad' : s === 'warning' ? 'warn' : '')
const f = reactive({ state: 'open', source: '', severity: '' })
const q = ref(''), rows = ref([]), sum = ref(null), sel = ref(null), error = ref(''), info = ref('')
const filters = () => ({ state: f.state, source: f.source, severity: f.severity, q: q.value, limit: 100 })
const stack = computed(() => sel.value?.details?.stack || '')
const pretty = computed(() => { const d = { ...(sel.value?.details || {}) }; delete d.stack; return JSON.stringify(d, null, 2) })
async function load() {
  try { error.value = ''; [rows.value, sum.value] = await Promise.all([admin.errors(filters()), admin.errorSummary()]) } catch (e) { error.value = e.message }
}
async function more() { try { rows.value = rows.value.concat(await admin.errors({ ...filters(), before: rows.value.at(-1).id })) } catch (e) { error.value = e.message } }
async function show(id) { try { sel.value = await admin.error(id) } catch (e) { error.value = e.message } }
async function resolve(all) { try { const r = await admin.resolveError(sel.value.id, all); info.value = `${r.resolved} marked resolved.`; await show(sel.value.id); await load() } catch (e) { error.value = e.message } }
async function download() { try { await admin.downloadErrors(filters()) } catch (e) { error.value = e.message } }
async function copy() {
  const e = sel.value
  const text = [`[${e.severity}] ${e.source} ${e.code}`, e.message, `When: ${when(e.ts)}`, e.request_id ? `Request id: ${e.request_id}` : '', e.hint ? `Hint: ${e.hint}` : '', JSON.stringify(e.details, null, 2)].filter(Boolean).join('\n')
  try { await navigator.clipboard.writeText(text); info.value = 'Copied.' } catch { error.value = 'Could not copy.' }
}
let t, poll
watch(q, () => { clearTimeout(t); t = setTimeout(load, 300) }); watch(f, load)
onMounted(() => { load(); poll = setInterval(load, 30000) }); onUnmounted(() => clearInterval(poll))
</script>
