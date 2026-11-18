<template>
  <div>
    <div class="head"><h1>Plans</h1><button v-if="canWrite" class="btn" @click="edit(null)">New plan</button></div>
    <Notice :text="error" kind="bad" /><Notice :text="info" kind="ok" />

    <form v-if="form" class="panel stack" @submit.prevent="save">
      <h2>{{ form.id ? 'Edit plan' : 'New plan' }}</h2>
      <p class="muted small" style="margin:0">People who already bought keep the plan exactly as it was sold. Changes apply to new purchases.</p>
      <div>
        <label>Plan type</label>
        <div class="seg" role="radiogroup" aria-label="Plan type">
          <button v-for="k in kinds" :key="k.id" type="button" role="radio" :aria-checked="form.kind === k.id" :class="{ on: form.kind === k.id }" @click="form.kind = k.id">{{ k.label }}</button>
        </div>
        <p class="muted small" style="margin:6px 0 0">{{ kinds.find((k) => k.id === form.kind).help }}</p>
      </div>
      <div class="formgrid">
        <div><label for="n">Name</label><input id="n" v-model="form.name" required maxlength="80" /></div>
        <div><label for="pr">Price (₦)</label><input id="pr" v-model.number="form.price" type="number" min="0" step="1" required /></div>
        <div v-if="form.kind !== 'data'"><label>Connected time</label>
          <div class="row" style="flex-wrap:nowrap"><input v-model.number="form.dur.count" type="number" min="1" aria-label="Connected time amount" required /><select v-model="form.dur.unit" aria-label="Connected time unit"><option v-for="u in units" :key="u" :value="u">{{ u }}{{ form.dur.count === 1 ? '' : 's' }}</option></select></div></div>
        <div v-if="form.kind !== 'time'"><label for="db">Data allowance (GB)</label><input id="db" v-model.number="form.gb" type="number" min="0.1" step="0.1" required /></div>
        <div><label>Expires after first use</label>
          <div class="row" style="flex-wrap:nowrap"><input v-model.number="form.valid.count" type="number" min="1" aria-label="Expiry amount" required /><select v-model="form.valid.unit" aria-label="Expiry unit"><option v-for="u in units" :key="u" :value="u">{{ u }}{{ form.valid.count === 1 ? '' : 's' }}</option></select></div></div>
        <div><label for="dl">Devices at once</label><input id="dl" v-model.number="form.devices" type="number" min="1" max="20" required /></div>
        <div><label for="bd">Download speed (Mbps)</label><input id="bd" v-model.number="form.down" type="number" min="0.1" step="0.1" required /></div>
        <div><label for="bu">Upload speed (Mbps)</label><input id="bu" v-model.number="form.up" type="number" min="0.1" step="0.1" required /></div>
        <div><label for="sc">Device swap cooldown (hours)</label><input id="sc" v-model.number="form.cooldownH" type="number" min="0" step="1" /></div>
        <div><label for="fu">Fair-use daily limit (GB, optional)</label><input id="fu" v-model.number="form.fupGb" type="number" min="0" step="0.1" /></div>
        <div v-if="form.fupGb > 0"><label for="fs">Slowed speed after the limit</label><select id="fs" v-model="form.fup_speed_profile_id"><option v-for="s in profiles" :key="s.id" :value="s.id">{{ s.name }} · {{ speed(s.down_kbps) }} / {{ speed(s.up_kbps) }}</option></select></div>
      </div>
      <div><label for="ds">Description (shown to customers, optional)</label><input id="ds" v-model="form.description" maxlength="200" /></div>
      <p class="small" style="margin:0"><strong>Summary:</strong> {{ preview }}</p>
      <Notice :text="formError" kind="bad" />
      <div class="row"><button class="btn" :disabled="busy">Save plan</button><button type="button" class="btn ghost" @click="form = null">Cancel</button></div>
    </form>

    <div class="panel tablewrap"><table>
      <thead><tr><th>Plan</th><th>Type</th><th class="right">Price</th><th>What it gives</th><th>Expires</th><th>Status</th><th></th></tr></thead>
      <tbody>
        <tr v-for="p in plans" :key="p.id"><td><strong>{{ p.name }}</strong><div v-if="p.description" class="muted small">{{ p.description }}</div></td><td>{{ kindLabel(p.kind) }}</td><td class="right">{{ naira(p.price_kobo) }}</td>
          <td class="small">{{ planSummary(p) }}</td><td>{{ duration(p.valid_seconds) }}</td>
          <td><span :class="['chip', p.is_active ? 'ok' : '']">{{ p.is_active ? 'on sale' : 'hidden' }}</span></td>
          <td class="nowrap"><template v-if="canWrite"><button class="btn ghost small" @click="edit(p)">Edit</button> <button class="btn ghost small" @click="toggle(p)">{{ p.is_active ? 'Hide' : 'Show' }}</button></template></td></tr>
        <tr v-if="!plans.length"><td colspan="7" class="muted">No plans yet.</td></tr>
      </tbody></table></div>
  </div>
</template>
<script setup>
import { ref, computed, onMounted } from 'vue'
import { admin } from '../../api'
import { can } from '../../session'
import { naira, duration, speed, planSummary } from '../../format'
import Notice from '../../components/Notice.vue'

const units = ['hour', 'day', 'week', 'month']
const kinds = [
  { id: 'time', label: 'Time', help: 'Counts only while the customer is actually online, then expires. Good for hourly, daily, weekly and monthly plans.' },
  { id: 'data', label: 'Data', help: 'A data allowance (GB) that expires after the chosen period, whichever comes first.' },
  { id: 'hybrid', label: 'Time + data', help: 'Ends when either the time or the data runs out.' },
]
const kindLabel = (k) => kinds.find((x) => x.id === k)?.label || k
const canWrite = computed(() => can('plans_write'))
const plans = ref([]), profiles = ref([]), form = ref(null), busy = ref(false), error = ref(''), info = ref(''), formError = ref('')

/** A stored number of seconds back into the biggest whole unit that divides it, for editing. */
function toUnit(s) {
  for (const [u, n] of [['month', 2592000], ['week', 604800], ['day', 86400], ['hour', 3600]]) if (s && s % n === 0) return { count: s / n, unit: u }
  return { count: Math.max(1, Math.round((s || 3600) / 3600)), unit: 'hour' }
}
async function load() { try { [plans.value, profiles.value] = await Promise.all([admin.plans(), admin.profiles()]) } catch (e) { error.value = e.message } }
function edit(p) {
  formError.value = ''; info.value = ''
  form.value = p
    ? { id: p.id, name: p.name, kind: p.kind, price: p.price_kobo / 100, dur: toUnit(p.active_seconds || 3600), gb: p.data_bytes ? +(p.data_bytes / 1024 ** 3).toFixed(2) : 1, valid: toUnit(p.valid_seconds), devices: p.device_limit, down: p.down_kbps / 1000, up: p.up_kbps / 1000, cooldownH: p.swap_cooldown_s / 3600, fupGb: p.fup_daily_bytes ? +(p.fup_daily_bytes / 1024 ** 3).toFixed(2) : 0, fup_speed_profile_id: profiles.value[0]?.id, description: p.description || '' }
    : { id: null, name: '', kind: 'time', price: 200, dur: { count: 1, unit: 'day' }, gb: 5, valid: { count: 1, unit: 'day' }, devices: 1, down: 2, up: 2, cooldownH: 24, fupGb: 0, fup_speed_profile_id: profiles.value[0]?.id, description: '' }
}
const plural = (d) => `${d.count} ${d.unit}${d.count === 1 ? '' : 's'}`
const preview = computed(() => {
  const f = form.value
  if (!f) return ''
  const give = [f.kind !== 'data' ? plural(f.dur) + ' of connected time' : '', f.kind !== 'time' ? f.gb + ' GB' : ''].filter(Boolean).join(' or ')
  return `${naira(Math.round((f.price || 0) * 100))} for ${give}, expires ${plural(f.valid)} after first use, ${f.devices} device${f.devices === 1 ? '' : 's'}, ${f.down} Mbps down / ${f.up} Mbps up.`
})
async function save() {
  const f = form.value; formError.value = ''; busy.value = true
  const body = {
    name: f.name, kind: f.kind, price_kobo: Math.round(f.price * 100), description: f.description || undefined,
    duration: f.kind !== 'data' ? f.dur : undefined, data_bytes: f.kind !== 'time' ? Math.round(f.gb * 1024 ** 3) : undefined,
    valid_for: f.valid, device_limit: f.devices, swap_cooldown_s: Math.round(f.cooldownH * 3600),
    down_kbps: Math.round(f.down * 1000), up_kbps: Math.round(f.up * 1000),
    fup_daily_bytes: f.fupGb > 0 ? Math.round(f.fupGb * 1024 ** 3) : undefined, fup_speed_profile_id: f.fupGb > 0 ? f.fup_speed_profile_id : undefined,
  }
  try { await admin.savePlan(f.id, body); info.value = 'Plan saved.'; form.value = null; await load() } catch (e) { formError.value = e.message } finally { busy.value = false }
}
async function toggle(p) { try { await admin.setPlanActive(p.id, !p.is_active); await load() } catch (e) { error.value = e.message } }
onMounted(load)
</script>
