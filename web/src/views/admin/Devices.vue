<template>
  <div>
    <div class="head"><h1>Devices</h1><button class="btn ghost small" @click="load">Refresh</button></div>
    <Notice :text="error" kind="bad" />

    <div v-if="data" class="kpis">
      <div class="panel kpi"><div class="l">Online</div><div class="v" style="color:var(--ok)">{{ data.summary.online }}</div></div>
      <div class="panel kpi"><div class="l">Needs attention</div><div class="v" :style="{ color: data.summary.degraded ? 'var(--warn)' : '' }">{{ data.summary.degraded }}</div></div>
      <div class="panel kpi"><div class="l">Offline</div><div class="v" :style="{ color: data.summary.offline ? 'var(--bad)' : 'var(--ok)' }">{{ data.summary.offline }}</div></div>
      <div class="panel kpi"><div class="l">Customers on Wi-Fi</div><div class="v">{{ data.summary.clients_on_aps }}</div></div>
    </div>

    <div class="row" style="margin-bottom:12px">
      <select v-model="f.status" aria-label="Status" style="max-width:170px"><option value="">Any status</option><option value="offline">Offline</option><option value="degraded">Needs attention</option><option value="online">Online</option></select>
      <select v-model="f.kind" aria-label="Kind" style="max-width:170px"><option value="">Every kind</option><option v-for="(l, k) in KIND_LABELS" :key="k" :value="k">{{ l }}</option></select>
      <input v-model="q" type="search" placeholder="Name, address or model" style="max-width:260px" aria-label="Search devices" />
    </div>

    <div class="cols2" style="grid-template-columns:minmax(0,1.35fr) minmax(0,1fr)">
      <div>
        <div v-if="data && !data.devices.length" class="panel muted">
          <p style="margin:0 0 6px"><strong>No devices reported yet.</strong></p>
          <p class="small" style="margin:0">Devices appear here once the site agent is set up to watch them (a <span class="mono">devices.json</span> file at the site). The setup guide is in <span class="mono">docs/site-setup.md</span>.</p>
        </div>
        <div v-for="g in groups" :key="g.site" class="panel tablewrap" style="margin-bottom:12px">
          <h2 style="margin-bottom:8px">{{ g.site }}</h2>
          <table><thead><tr><th>Device</th><th>Status</th><th>Signal / load</th><th class="right">Customers</th><th>Last heard</th></tr></thead><tbody>
            <tr v-for="d in g.devices" :key="d.id" tabindex="0" :style="{ cursor: 'pointer', background: sel?.device.id === d.id ? 'var(--soft)' : '' }" @click="show(d.id)" @keydown.enter="show(d.id)">
              <td><strong>{{ d.name }}</strong><div class="muted small">{{ kindLabel(d.kind) }}{{ d.model ? ' · ' + d.model : '' }}</div></td>
              <td><span :class="['chip', chip(d.status)]">{{ statusLabel(d.status) }}</span></td>
              <td class="small">
                <span v-if="d.signal_dbm != null" :class="['chip', signalQuality(d.signal_dbm).cls]">{{ Math.round(d.signal_dbm) }} dBm</span>
                <span v-if="d.cpu_pct != null" class="muted"> CPU {{ Math.round(d.cpu_pct) }}%</span>
                <span v-if="d.util_pct != null" class="muted"> · busy {{ Math.round(d.util_pct) }}%</span>
                <span v-if="d.signal_dbm == null && d.cpu_pct == null && d.util_pct == null" class="muted">{{ d.latency_ms != null ? Math.round(d.latency_ms) + ' ms' : '–' }}</span>
              </td>
              <td class="right">{{ d.kind === 'ap' || d.kind === 'router' ? d.clients_count : '–' }}</td>
              <td class="nowrap small">{{ ago(d.last_seen_at) }}</td></tr>
          </tbody></table>
        </div>
      </div>

      <div v-if="sel" class="panel stack">
        <div class="row between"><h2>{{ sel.device.name }}</h2><button class="btn ghost small" @click="sel = null">Close</button></div>
        <div class="row"><span :class="['chip', chip(sel.device.status)]">{{ statusLabel(sel.device.status) }}</span><span class="muted small">{{ kindLabel(sel.device.kind) }}{{ sel.device.model ? ' · ' + sel.device.model : '' }}</span></div>
        <div v-if="sel.device.reasons.length" class="notice warn stack" style="gap:4px"><strong>What is wrong</strong><div v-for="r in sel.device.reasons" :key="r" class="small">{{ r }}</div></div>

        <dl class="small" style="margin:0;display:grid;grid-template-columns:auto 1fr;gap:4px 14px">
          <template v-for="row in facts" :key="row[0]"><dt class="muted">{{ row[0] }}</dt><dd style="margin:0;word-break:break-all">{{ row[1] }}</dd></template>
        </dl>

        <div v-if="signalSeries.length"><h3>Signal, last 24 hours</h3><Chart :labels="sel.samples.map((s) => hhmm(s.ts))" :series="[{ name: 'Signal', color: 'var(--brand)', values: signalSeries }]" :fmt="(v) => Math.round(v - 100) + ' dBm'" label="Signal strength over 24 hours; taller is stronger" /></div>
        <div v-else-if="cpuSeries.some((v) => v)"><h3>CPU, last 24 hours</h3><Chart :labels="sel.samples.map((s) => hhmm(s.ts))" :series="[{ name: 'CPU %', color: 'var(--brand)', values: cpuSeries }]" :fmt="(v) => Math.round(v) + '%'" label="CPU use over 24 hours" /></div>
        <div v-if="clientSeries.some((v) => v)"><h3>Customers connected, last 24 hours</h3><Chart :labels="sel.samples.map((s) => hhmm(s.ts))" :series="[{ name: 'Customers', color: 'var(--ok)', values: clientSeries }]" :fmt="(v) => Math.round(v)" label="Customers connected over 24 hours" /></div>

        <div v-if="sel.device.kind === 'ap' || sel.device.kind === 'router'">
          <h3>Connected now ({{ sel.clients.length }})</h3>
          <div class="tablewrap"><table><thead><tr><th>Who</th><th>Signal</th><th>Online</th><th class="right">Used</th></tr></thead><tbody>
            <tr v-for="c in sel.clients" :key="c.mac">
              <td><strong>{{ c.member_name || c.customer_phone || c.hostname || 'Unknown device' }}</strong>
                <div class="muted small">{{ c.plan || 'no plan found' }} · {{ c.os_family || c.device_class || '' }} <span class="mono">{{ c.mac }}</span></div></td>
              <td class="small"><span v-if="c.signal_dbm != null" :class="['chip', signalQuality(c.signal_dbm).cls]">{{ Math.round(c.signal_dbm) }} dBm</span><span v-else class="muted">–</span></td>
              <td class="nowrap small">{{ c.connected_s != null ? duration(c.connected_s) : '–' }}</td>
              <td class="right small">{{ c.rx_bytes != null || c.tx_bytes != null ? bytes((c.rx_bytes || 0) + (c.tx_bytes || 0)) : '–' }}</td></tr>
            <tr v-if="!sel.clients.length"><td colspan="4" class="muted">Nobody is connected through this device.</td></tr></tbody></table></div>
        </div>

        <div><h3>History</h3>
          <div v-for="e in sel.events" :key="e.id" class="row between small" style="padding:5px 0;border-top:1px solid var(--soft)"><span><span :class="['chip', e.severity === 'info' ? '' : e.severity === 'warning' ? 'warn' : 'bad']">{{ e.severity }}</span> {{ e.message }}</span><span class="muted nowrap">{{ ago(e.ts) }}</span></div>
          <p v-if="!sel.events.length" class="muted small" style="margin:0">No problems recorded for this device.</p>
          <router-link class="small" to="/admin/errors">Open the error log</router-link></div>
      </div>
      <div v-else class="panel muted">Choose a device to see its health, signal and who is connected to it.</div>
    </div>
  </div>
</template>
<script setup>
import { ref, reactive, computed, watch, onMounted, onUnmounted } from 'vue'
import { admin } from '../../api'
import { ago, when, bytes, duration, signalQuality, kindLabel, KIND_LABELS } from '../../format'
import Chart from '../../components/Chart.vue'
import Notice from '../../components/Notice.vue'

const f = reactive({ status: '', kind: '' })
const q = ref(''), data = ref(null), sel = ref(null), error = ref('')
const chip = (s) => (s === 'online' ? 'ok' : s === 'degraded' ? 'warn' : 'bad')
const statusLabel = (s) => ({ online: 'online', degraded: 'needs attention', offline: 'offline' })[s] || s
const hhmm = (ts) => new Date(ts).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })
const groups = computed(() => {
  const by = new Map()
  for (const d of data.value?.devices || []) { if (!by.has(d.site)) by.set(d.site, []); by.get(d.site).push(d) }
  return [...by].map(([site, devices]) => ({ site, devices }))
})
// A radio signal is negative dBm. The chart draws bars upward, so plot "how far above -100 dBm" and label it back.
const signalSeries = computed(() => {
  const v = (sel.value?.samples || []).map((s) => (s.signal_dbm == null ? 0 : Math.max(0, Number(s.signal_dbm) + 100)))
  return v.some((x) => x > 0) ? v : []
})
const cpuSeries = computed(() => (sel.value?.samples || []).map((s) => Number(s.cpu_pct || 0)))
const clientSeries = computed(() => (sel.value?.samples || []).map((s) => Number(s.clients || 0)))
const facts = computed(() => {
  const d = sel.value?.device
  if (!d) return []
  const n = (v, u, dp = 0) => (v == null ? null : `${Number(v).toFixed(dp)}${u}`)
  return [
    ['Address', d.ip], ['Hardware address', d.mac], ['Firmware', d.firmware], ['Linked to', d.peer], ['Last heard', when(d.last_seen_at)],
    ['Online for', d.uptime_s != null ? duration(d.uptime_s) : null], ['Signal', n(d.signal_dbm, ' dBm')], ['Noise', n(d.noise_dbm, ' dBm')],
    ['Link quality', n(d.link_quality_pct, '%')], ['Speed (send / receive)', d.tx_mbps != null || d.rx_mbps != null ? `${n(d.tx_mbps ?? 0, '', 0)} / ${n(d.rx_mbps ?? 0, ' Mbps', 0)}` : null],
    ['Channel', d.channel], ['Radio busy', n(d.util_pct, '%')], ['CPU', n(d.cpu_pct, '%')], ['Memory', n(d.mem_pct, '%')],
    ['Response time', n(d.latency_ms, ' ms', 1)], ['Packets lost', n(d.loss_pct, '%')],
  ].filter((r) => r[1] != null && r[1] !== '')
})
async function load() {
  try { error.value = ''; data.value = await admin.devices({ status: f.status, kind: f.kind, q: q.value }); if (sel.value) await show(sel.value.device.id, true) } catch (e) { error.value = e.message }
}
async function show(id, quiet) { try { sel.value = await admin.device(id) } catch (e) { if (!quiet) error.value = e.message; else sel.value = null } }
let t, poll
watch(q, () => { clearTimeout(t); t = setTimeout(load, 300) }); watch(f, load)
onMounted(() => { load(); poll = setInterval(load, 30000) }); onUnmounted(() => { clearInterval(poll); clearTimeout(t) })
</script>
