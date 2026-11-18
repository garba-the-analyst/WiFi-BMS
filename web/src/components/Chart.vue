<template>
  <figure class="chart" style="margin:0">
    <svg :viewBox="`0 0 ${W} ${H}`" width="100%" role="img" :aria-label="label">
      <g v-for="(t, i) in (labels.length ? ticks : [])" :key="i">
        <line :x1="PL" :x2="W" :y1="y(t)" :y2="y(t)" stroke="var(--soft)" />
        <text :x="PL - 6" :y="y(t) + 4" text-anchor="end" font-size="11" fill="var(--muted)">{{ fmt(t) }}</text>
      </g>
      <g v-for="(l, i) in labels" :key="'g' + i">
        <rect v-for="(s, j) in series" :key="j" :x="bx(i, j)" :y="y(s.values[i] || 0)" :width="bw" :height="Math.max(0, base - y(s.values[i] || 0))" :fill="s.color" rx="2">
          <title>{{ l }}: {{ s.name }} {{ fmt(s.values[i] || 0) }}</title>
        </rect>
        <text v-if="showLabel(i)" :x="PL + gw * (i + 0.5)" :y="H - 6" text-anchor="middle" font-size="11" fill="var(--muted)">{{ l }}</text>
      </g>
    </svg>
    <figcaption v-if="series.length > 1" class="row small muted" style="gap:14px;margin-top:6px">
      <span v-for="s in series" :key="s.name"><i :style="{ background: s.color }" style="display:inline-block;width:10px;height:10px;border-radius:2px;margin-right:5px"></i>{{ s.name }}</span>
    </figcaption>
    <p v-if="!labels.length" class="muted small" style="margin:8px 0 0">No data in this period yet.</p>
  </figure>
</template>
<script setup>
import { computed } from 'vue'
const props = defineProps({ labels: { type: Array, default: () => [] }, series: { type: Array, default: () => [] }, fmt: { type: Function, default: (v) => String(v) }, label: { type: String, default: 'Chart' } })
const W = 640, H = 220, PL = 56, PB = 24, PT = 8
const base = H - PB
const series = computed(() => (props.series || []).filter((s) => s && Array.isArray(s.values)))
const max = computed(() => Math.max(1, ...series.value.flatMap((s) => s.values.map((v) => v || 0))))
const y = (v) => base - (v / max.value) * (base - PT)
const ticks = computed(() => [0, 0.5, 1].map((f) => max.value * f))
const gw = computed(() => (W - PL) / Math.max(1, props.labels.length))
const bw = computed(() => Math.max(2, Math.min(28, (gw.value * 0.7) / Math.max(1, series.value.length))))
const bx = (i, j) => PL + gw.value * i + (gw.value - bw.value * series.value.length) / 2 + bw.value * j
const showLabel = (i) => props.labels.length <= 12 || i % Math.ceil(props.labels.length / 8) === 0
</script>
