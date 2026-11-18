<template>
  <div>
    <div class="head"><h1>Analytics</h1><div class="seg"><button v-for="r in ranges" :key="r" :class="{ on: range === r }" @click="range = r">{{ r }}</button></div></div>
    <Notice :text="error" kind="bad" />
    <div class="cols2">
      <div class="panel"><h2 style="margin-bottom:10px">Plans sold</h2><Chart :labels="rev.labels" :series="[rev.series[2]]" label="Plans sold" /></div>
      <div class="panel"><h2 style="margin-bottom:10px">Active time used</h2><Chart :labels="tr.labels" :series="[tr.series[2]]" :fmt="(v) => duration(v)" label="Active seconds" /></div>
    </div>
    <div class="cols2">
      <div class="panel"><h2 style="margin-bottom:10px">Best-selling plans</h2>
        <div class="tablewrap"><table><thead><tr><th>Plan</th><th class="right">Sold</th><th class="right">Revenue</th></tr></thead>
          <tbody><tr v-for="p in plans" :key="p.name"><td>{{ p.name }}</td><td class="right">{{ p.sold }}</td><td class="right">{{ naira(p.revenue_kobo) }}</td></tr><tr v-if="!plans.length"><td colspan="3" class="muted">No sales in this period.</td></tr></tbody></table></div></div>
      <div class="panel"><h2 style="margin-bottom:10px">Devices seen</h2>
        <div class="tablewrap"><table><thead><tr><th>System</th><th>Type</th><th class="right">Devices</th></tr></thead>
          <tbody><tr v-for="(d, i) in devices" :key="i"><td>{{ d.os }}</td><td>{{ d.class }}</td><td class="right">{{ d.devices }}</td></tr><tr v-if="!devices.length"><td colspan="3" class="muted">No devices in this period.</td></tr></tbody></table></div></div>
    </div>
  </div>
</template>
<script setup>
import { ref, watch, onMounted } from 'vue'
import { admin } from '../../api'
import { naira, day, duration } from '../../format'
import Chart from '../../components/Chart.vue'
import Notice from '../../components/Notice.vue'

const ranges = ['24h', '7d', '30d', '90d'], range = ref('30d'), error = ref('')
const rev = ref({ labels: [], series: [{}, {}, {}] }), tr = ref({ labels: [], series: [{}, {}, {}] }), plans = ref([]), devices = ref([])
const lab = (b, r) => (r === '24h' ? new Date(b).getHours() + ':00' : day(b))
async function load() {
  try {
    error.value = ''
    const [r, t, p, d] = await Promise.all([admin.analytics('revenue', range.value), admin.analytics('traffic', range.value), admin.analytics('plans', range.value), admin.analytics('devices', range.value)])
    rev.value = { labels: r.map((x) => lab(x.bucket, range.value)), series: [{}, {}, { name: 'Plans sold', color: 'var(--brand)', values: r.map((x) => +x.plans_sold || 0) }] }
    tr.value = { labels: t.map((x) => lab(x.bucket, range.value)), series: [{}, {}, { name: 'Active seconds', color: 'var(--brand)', values: t.map((x) => +x.active_seconds || 0) }] }
    plans.value = p; devices.value = d
  } catch (e) { error.value = e.message }
}
watch(range, load); onMounted(load)
</script>
