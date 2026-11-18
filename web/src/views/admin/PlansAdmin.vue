<template>
  <div>
    <div class="head"><h1>Plans</h1><button class="btn" @click="edit(null)">New plan</button></div>
    <Notice :text="error" kind="bad" /><Notice :text="info" kind="ok" />

    <form v-if="form" class="panel stack" @submit.prevent="save">
      <h2>{{ form.id ? 'Edit plan' : 'New plan' }}</h2>
      <p class="muted small" style="margin:0">People who already bought keep the plan exactly as it was sold. Changes apply to new purchases.</p>
      <div class="formgrid">
        <div><label for="n">Name</label><input id="n" v-model="form.name" required maxlength="80" /></div>
        <div><label for="k">Kind</label><select id="k" v-model="form.kind"><option value="time">Time (active use)</option><option value="data">Data</option><option value="hybrid">Time and data</option></select></div>
        <div><label for="pr">Price (₦)</label><input id="pr" v-model.number="form.price" type="number" min="0" step="1" required /></div>
        <div v-if="form.kind !== 'data'"><label for="as">Active hours</label><input id="as" v-model.number="form.hours" type="number" min="0.25" step="0.25" /></div>
        <div v-if="form.kind !== 'time'"><label for="db">Data (GB)</label><input id="db" v-model.number="form.gb" type="number" min="0.1" step="0.1" /></div>
        <div><label for="vd">Valid for (days)</label><input id="vd" v-model.number="form.days" type="number" min="0.04" step="0.5" required /></div>
        <div><label for="dl">Devices</label><input id="dl" v-model.number="form.devices" type="number" min="1" max="20" required /></div>
        <div><label for="sc">Device swap cooldown (hours)</label><input id="sc" v-model.number="form.cooldownH" type="number" min="0" step="1" /></div>
        <div><label for="sp">Speed profile</label><select id="sp" v-model="form.speed_profile_id" required><option v-for="s in profiles" :key="s.id" :value="s.id">{{ s.name }} · {{ speed(s.down_kbps) }} / {{ speed(s.up_kbps) }}</option></select></div>
        <div><label for="fu">Fair-use daily limit (GB, optional)</label><input id="fu" v-model.number="form.fupGb" type="number" min="0" step="0.1" /></div>
        <div v-if="form.fupGb > 0"><label for="fs">Slowed speed profile</label><select id="fs" v-model="form.fup_speed_profile_id"><option v-for="s in profiles" :key="s.id" :value="s.id">{{ s.name }}</option></select></div>
      </div>
      <Notice :text="formError" kind="bad" />
      <div class="row"><button class="btn" :disabled="busy">Save plan</button><button type="button" class="btn ghost" @click="form = null">Cancel</button></div>
    </form>

    <div class="panel tablewrap"><table>
      <thead><tr><th>Plan</th><th>Kind</th><th class="right">Price</th><th>Allowance</th><th>Valid</th><th>Devices</th><th>Speed</th><th>Status</th><th></th></tr></thead>
      <tbody>
        <tr v-for="p in plans" :key="p.id"><td><strong>{{ p.name }}</strong></td><td>{{ p.kind }}</td><td class="right">{{ naira(p.price_kobo) }}</td>
          <td>{{ [p.active_seconds ? duration(p.active_seconds) : '', p.data_bytes ? bytes(p.data_bytes) : ''].filter(Boolean).join(' + ') }}</td><td>{{ duration(p.valid_seconds) }}</td><td>{{ p.device_limit }}</td><td>{{ speed(p.down_kbps) }}</td>
          <td><span :class="['chip', p.is_active ? 'ok' : '']">{{ p.is_active ? 'on sale' : 'hidden' }}</span></td>
          <td class="nowrap"><button class="btn ghost small" @click="edit(p)">Edit</button> <button class="btn ghost small" @click="toggle(p)">{{ p.is_active ? 'Hide' : 'Show' }}</button></td></tr>
      </tbody></table></div>

    <form class="panel stack" @submit.prevent="addProfile">
      <h2>Speed profiles</h2>
      <p class="muted small" style="margin:0">{{ profiles.map((s) => `${s.name} (${speed(s.down_kbps)} down / ${speed(s.up_kbps)} up)`).join(' · ') || 'None yet. Create one first.' }}</p>
      <div class="formgrid">
        <div><label for="pn">Name</label><input id="pn" v-model="prof.name" required /></div>
        <div><label for="pd">Download (Mbps)</label><input id="pd" v-model.number="prof.down" type="number" min="0.1" step="0.1" required /></div>
        <div><label for="pu">Upload (Mbps)</label><input id="pu" v-model.number="prof.up" type="number" min="0.1" step="0.1" required /></div>
      </div>
      <div><button class="btn ghost" :disabled="busy">Add speed profile</button></div>
    </form>
  </div>
</template>
<script setup>
import { ref, onMounted } from 'vue'
import { admin } from '../../api'
import { naira, duration, bytes, speed } from '../../format'
import Notice from '../../components/Notice.vue'

const plans = ref([]), profiles = ref([]), form = ref(null), busy = ref(false), error = ref(''), info = ref(''), formError = ref('')
const prof = ref({ name: '', down: 5, up: 2 })
async function load() { try { [plans.value, profiles.value] = await Promise.all([admin.plans(), admin.profiles()]) } catch (e) { error.value = e.message } }
function edit(p) {
  formError.value = ''
  form.value = p
    ? { id: p.id, name: p.name, kind: p.kind, price: p.price_kobo / 100, hours: p.active_seconds ? p.active_seconds / 3600 : 1, gb: p.data_bytes ? p.data_bytes / 1024 ** 3 : 1, days: p.valid_seconds / 86400, devices: p.device_limit, cooldownH: p.swap_cooldown_s / 3600, speed_profile_id: profiles.value.find((s) => s.name === p.speed_profile)?.id, fupGb: p.fup_daily_bytes ? p.fup_daily_bytes / 1024 ** 3 : 0, fup_speed_profile_id: profiles.value[0]?.id }
    : { id: null, name: '', kind: 'time', price: 200, hours: 1, gb: 1, days: 1, devices: 1, cooldownH: 24, speed_profile_id: profiles.value[0]?.id, fupGb: 0, fup_speed_profile_id: profiles.value[0]?.id }
}
async function save() {
  const f = form.value; formError.value = ''; busy.value = true
  const body = {
    name: f.name, kind: f.kind, price_kobo: Math.round(f.price * 100),
    active_seconds: f.kind !== 'data' ? Math.round(f.hours * 3600) : null,
    data_bytes: f.kind !== 'time' ? Math.round(f.gb * 1024 ** 3) : null,
    valid_seconds: Math.round(f.days * 86400), device_limit: f.devices, swap_cooldown_s: Math.round(f.cooldownH * 3600), speed_profile_id: f.speed_profile_id,
    fup_daily_bytes: f.fupGb > 0 ? Math.round(f.fupGb * 1024 ** 3) : null, fup_speed_profile_id: f.fupGb > 0 ? f.fup_speed_profile_id : null,
  }
  try { await admin.savePlan(f.id, body); info.value = 'Plan saved.'; form.value = null; await load() } catch (e) { formError.value = e.message } finally { busy.value = false }
}
async function toggle(p) { try { await admin.setPlanActive(p.id, !p.is_active); await load() } catch (e) { error.value = e.message } }
async function addProfile() {
  busy.value = true; error.value = ''
  try { await admin.createProfile({ name: prof.value.name, down_kbps: Math.round(prof.value.down * 1000), up_kbps: Math.round(prof.value.up * 1000) }); prof.value = { name: '', down: 5, up: 2 }; await load() } catch (e) { error.value = e.message } finally { busy.value = false }
}
onMounted(load)
</script>
