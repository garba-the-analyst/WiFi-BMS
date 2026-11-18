<template>
  <div>
    <div class="head"><h1>Vouchers</h1></div>
    <Notice :text="error" kind="bad" /><Notice :text="info" kind="ok" />

    <div v-if="issued" class="panel stack" style="border-color:var(--brand)">
      <h2>{{ issued.title }}</h2>
      <p class="muted small" style="margin:0">{{ issued.note }} Copy or print now: special voucher codes are shown only once.</p>
      <div v-for="c in issued.codes" :key="c" class="code">{{ c }}</div>
      <div class="row"><button class="btn ghost small" @click="copy">Copy all</button><button class="btn ghost small" @click="issued = null">Done</button></div>
    </div>

    <div class="cols2">
      <form class="panel stack" @submit.prevent="sell">
        <h2>Sell a plan (cash or transfer)</h2>
        <p class="muted small" style="margin:0">For a customer who paid you directly. The price comes from the plan, so it cannot be changed here.</p>
        <div><label for="mp">Plan</label><select id="mp" v-model="m.plan_id" required><option v-for="p in plans" :key="p.id" :value="p.id">{{ p.name }} · {{ naira(p.price_kobo) }}</option></select></div>
        <div v-if="sites.length > 1"><label for="ms">Site</label><select id="ms" v-model="m.site"><option v-for="s in sites" :key="s.slug" :value="s.slug">{{ s.name }}</option></select></div>
        <div class="seg" role="radiogroup" aria-label="Give the plan to"><button type="button" :class="{ on: m.to === 'code' }" @click="m.to = 'code'">Make a code</button><button type="button" :class="{ on: m.to === 'member' }" @click="m.to = 'member'">Add to a member</button></div>
        <div v-if="m.to === 'code'"><label for="mph">Customer phone (optional)</label><input id="mph" v-model="m.phone" inputmode="tel" /></div>
        <div v-else>
          <label for="mq">Find the member by phone or name</label><input id="mq" v-model="mq" type="search" autocomplete="off" />
          <div v-if="found.length" class="stack" style="margin-top:8px"><button v-for="c in found" :key="c.id" type="button" class="card" :class="{ picked: m.account_id === c.id }" style="width:100%;text-align:left;cursor:pointer" @click="m.account_id = c.id">{{ c.display_name || 'Unnamed' }} · {{ c.phone }}</button></div>
        </div>
        <div><label for="mm">Paid by</label><select id="mm" v-model="m.method"><option value="cash">Cash</option><option value="transfer">Bank transfer</option></select></div>
        <Notice :text="mErr" kind="bad" />
        <button class="btn" :disabled="busy || (m.to === 'member' && !m.account_id)">Sell plan</button>
      </form>

      <form v-if="can('special_voucher')" class="panel stack" @submit.prevent="special">
        <h2>Special voucher (unlimited)</h2>
        <p class="muted small" style="margin:0">Free access for staff, partners or guests. No data or time limit. Every voucher is recorded with who made it and why.</p>
        <div><label for="sl">Who is it for? (required)</label><input id="sl" v-model="s.label" maxlength="120" required placeholder="e.g. Chairman's guests, Site 1 technicians" /></div>
        <div class="formgrid">
          <div v-if="sites.length > 1"><label for="ss">Site</label><select id="ss" v-model="s.site"><option v-for="x in sites" :key="x.slug" :value="x.slug">{{ x.name }}</option></select></div>
          <div><label for="sd">Devices at once</label><input id="sd" v-model.number="s.devices" type="number" min="1" max="10" required /></div>
          <div><label for="sc">How many codes</label><input id="sc" v-model.number="s.count" type="number" min="1" max="50" required /></div>
          <div><label for="sb">Speed (Mbps down / up)</label><div class="row" style="flex-wrap:nowrap"><input id="sb" v-model.number="s.down" type="number" min="0.1" step="0.1" aria-label="Download Mbps" /><input v-model.number="s.up" type="number" min="0.1" step="0.1" aria-label="Upload Mbps" /></div></div>
        </div>
        <label class="check"><input v-model="s.ends" type="checkbox" /> Stops working after a set time</label>
        <div v-if="s.ends" class="row" style="flex-wrap:nowrap;max-width:300px"><input v-model.number="s.count_n" type="number" min="1" aria-label="Amount" /><select v-model="s.unit" aria-label="Unit"><option v-for="u in ['hour', 'day', 'week', 'month']" :key="u" :value="u">{{ u }}s</option></select></div>
        <Notice :text="sErr" kind="bad" />
        <button class="btn" :disabled="busy">Create voucher{{ s.count > 1 ? 's' : '' }}</button>
      </form>
    </div>

    <div class="panel">
      <div class="row between" style="margin-bottom:8px"><h2>Issued vouchers</h2>
        <div class="row"><select v-model="kind" aria-label="Kind" style="max-width:160px" @change="load"><option value="">All</option><option value="manual">Manual sales</option><option v-if="can('special_voucher')" value="special">Special</option></select><input v-model="q" type="search" placeholder="Search code, label, phone" style="max-width:240px" aria-label="Search vouchers" /></div></div>
      <div class="tablewrap"><table><thead><tr><th>Code</th><th>Kind</th><th>For</th><th>Plan</th><th>Made by</th><th>When</th><th>Status</th><th></th></tr></thead><tbody>
        <tr v-for="v in rows" :key="v.id"><td class="mono">{{ prettyCode(v.code) }}</td><td>{{ v.kind }}</td><td>{{ v.label || v.phone || '–' }}</td><td>{{ v.plan || '–' }}</td><td class="small">{{ v.issued_by || '–' }}</td><td class="nowrap">{{ when(v.created_at) }}</td>
          <td><span :class="['chip', v.revoked_at ? 'bad' : v.status === 'active' ? 'ok' : '']">{{ v.revoked_at ? 'revoked' : v.status || 'unused' }}</span></td>
          <td><button v-if="!v.revoked_at && can('special_voucher')" class="btn ghost small" @click="revoke(v)">Revoke</button></td></tr>
        <tr v-if="!rows.length"><td colspan="8" class="muted">No vouchers yet.</td></tr></tbody></table></div>
    </div>
  </div>
</template>
<script setup>
import { ref, watch, computed, onMounted } from 'vue'
import { admin, config } from '../../api'
import { can } from '../../session'
import { naira, when } from '../../format'
import Notice from '../../components/Notice.vue'

const cfg = config()
const sites = computed(() => (cfg.sites && cfg.sites.length ? cfg.sites : [{ slug: cfg.site, name: cfg.site }]))
const plans = ref([]), rows = ref([]), kind = ref(''), q = ref(''), error = ref(''), info = ref(''), busy = ref(false), issued = ref(null)
const m = ref({ plan_id: '', site: cfg.site, to: 'code', phone: '', account_id: '', method: 'cash' }), mq = ref(''), found = ref([]), mErr = ref('')
const s = ref({ label: '', site: cfg.site, devices: 1, count: 1, down: 2, up: 2, ends: false, count_n: 1, unit: 'month' }), sErr = ref('')
const prettyCode = (c) => (c && c.length === 8 ? c.slice(0, 4) + ' ' + c.slice(4) : c)

async function load() { try { rows.value = await admin.vouchers({ kind: kind.value, q: q.value, limit: 100 }) } catch (e) { error.value = e.message } }
let t1, t2
watch(q, () => { clearTimeout(t1); t1 = setTimeout(load, 300) })
watch(mq, (v) => { clearTimeout(t2); t2 = setTimeout(async () => { found.value = v.trim().length >= 3 ? (await admin.customers({ q: v, limit: 5 }).catch(() => [])) : [] }, 300) })
async function sell() {
  mErr.value = ''; busy.value = true
  try {
    const r = await admin.manualSale({ plan_id: m.value.plan_id, site: m.value.site, method: m.value.method, account_id: m.value.to === 'member' ? m.value.account_id : undefined, phone: m.value.to === 'code' ? m.value.phone || undefined : undefined })
    if (r.access_code) issued.value = { title: 'Access code for ' + r.plan, note: 'Give this to the customer.', codes: [r.access_code] }
    else info.value = `${r.plan} added to the member's account.`
    await load()
  } catch (e) { mErr.value = e.message } finally { busy.value = false }
}
async function special() {
  sErr.value = ''; busy.value = true
  const f = s.value
  try {
    const r = await admin.specialVoucher({ label: f.label, site: f.site, devices: f.devices, count: f.count, down_kbps: Math.round(f.down * 1000), up_kbps: Math.round(f.up * 1000), valid_for: f.ends ? { unit: f.unit, count: f.count_n } : undefined })
    issued.value = { title: `${r.vouchers.length} special voucher${r.vouchers.length === 1 ? '' : 's'} for ${r.label}`, note: `Ends: ${r.ends}.`, codes: r.vouchers.map((v) => v.access_code) }
    s.value.label = ''; await load()
  } catch (e) { sErr.value = e.message } finally { busy.value = false }
}
async function revoke(v) {
  if (!window.confirm(`Switch off ${prettyCode(v.code)}? Anyone connected with it is disconnected shortly.`)) return
  try { await admin.revokeVoucher(v.id); await load() } catch (e) { error.value = e.message }
}
async function copy() { try { await navigator.clipboard.writeText(issued.value.codes.join('\n')); info.value = 'Copied.' } catch { error.value = 'Could not copy. Select the codes and copy by hand.' } }
onMounted(async () => { try { plans.value = (await admin.plans()).filter((p) => p.is_active); m.value.plan_id = plans.value[0]?.id || '' } catch (e) { error.value = e.message } load() })
</script>
