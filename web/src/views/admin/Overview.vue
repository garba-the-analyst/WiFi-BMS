<template>
  <div>
    <div class="head"><h1>Overview</h1><div class="seg"><button v-for="r in ranges" :key="r" :class="{ on: range === r }" @click="range = r">{{ r }}</button></div></div>
    <Notice :text="error" kind="bad" />
    <div class="kpis" v-if="o">
      <div class="panel kpi"><div class="l">Plan sales</div><div class="v">{{ naira(o.plan_sales_kobo) }}</div></div>
      <div class="panel kpi"><div class="l">Cash in (Paystack)</div><div class="v">{{ naira(o.cash_in_kobo) }}</div></div>
      <div class="panel kpi"><div class="l">Online now</div><div class="v">{{ o.online_now }}</div></div>
      <div class="panel kpi"><div class="l">Orders paid</div><div class="v">{{ o.orders_paid }}</div></div>
      <div class="panel kpi"><div class="l">New members</div><div class="v">{{ o.new_members }}</div></div>
      <div class="panel kpi"><div class="l">Traffic</div><div class="v">{{ bytes(+o.bytes_in + +o.bytes_out) }}</div></div>
      <div class="panel kpi"><div class="l">Wallet float (owed to members)</div><div class="v">{{ naira(o.wallet_float_kobo) }}</div></div>
      <router-link to="/admin/security" class="panel kpi" style="color:inherit;text-decoration:none"><div class="l">Sharing flags</div><div class="v">{{ o.tether_flags }}</div></router-link>
      <router-link v-if="o.orders_refund_flagged" to="/admin/orders" class="panel kpi" style="color:var(--bad);text-decoration:none"><div class="l">Payments needing review</div><div class="v">{{ o.orders_refund_flagged }}</div></router-link>
    </div>
    <div class="cols2">
      <div class="panel"><h2 style="margin-bottom:10px">Sales</h2><Chart :labels="rev.labels" :series="rev.series" :fmt="(v) => naira(v)" label="Sales per period" /></div>
      <div class="panel"><h2 style="margin-bottom:10px">Traffic</h2><Chart :labels="tr.labels" :series="tr.series" :fmt="bytes" label="Traffic per period" /></div>
    </div>
  </div>
</template>
<script setup>
import { ref, watch, onMounted, onUnmounted } from 'vue'
import { admin } from '../../api'
import { naira, bytes, day } from '../../format'
import Chart from '../../components/Chart.vue'
import Notice from '../../components/Notice.vue'

const ranges = ['24h', '7d', '30d', '90d'], range = ref('7d'), error = ref('')
const o = ref(null), rev = ref({ labels: [], series: [] }), tr = ref({ labels: [], series: [] })
const lab = (b, r) => (r === '24h' ? new Date(b).getHours() + ':00' : day(b))
async function load() {
  try {
    error.value = ''
    const [ov, r, t] = await Promise.all([admin.overview(range.value), admin.analytics('revenue', range.value), admin.analytics('traffic', range.value)])
    o.value = ov
    rev.value = { labels: r.map((x) => lab(x.bucket, range.value)), series: [{ name: 'Plans', color: 'var(--brand)', values: r.map((x) => +x.plan_sales_kobo || 0) }, { name: 'Top-ups', color: 'var(--warn)', values: r.map((x) => +x.topups_kobo || 0) }] }
    tr.value = { labels: t.map((x) => lab(x.bucket, range.value)), series: [{ name: 'Download', color: 'var(--brand)', values: t.map((x) => +x.bytes_in || 0) }, { name: 'Upload', color: 'var(--muted)', values: t.map((x) => +x.bytes_out || 0) }] }
  } catch (e) { error.value = e.message }
}
let timer
watch(range, load)
onMounted(() => { load(); timer = setInterval(load, 30000) }) // refreshes itself; "online now" stays current
onUnmounted(() => clearInterval(timer))
</script>
