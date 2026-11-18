<template>
  <div class="shell">
    <header class="top"><span class="brand">{{ cfg.brand }}</span><button class="btn ghost small" @click="signOut">Sign out</button></header>
    <div v-if="loading" class="row" style="justify-content:center;padding:30px"><span class="spinner"></span></div>
    <Notice :text="error" kind="bad" />
    <Notice :text="info" kind="ok" />

    <template v-if="me">
      <div class="card">
        <div class="muted small">Wallet balance</div>
        <div class="balance">{{ naira(me.balance_kobo) }}</div>
        <div class="muted small">{{ me.display_name || me.phone }}</div>
        <div class="pills" style="margin-top:12px" role="group" aria-label="Top up amount">
          <button v-for="a in amounts" :key="a" type="button" :class="{ on: amount === a }" @click="amount = a">{{ naira(a) }}</button>
        </div>
        <button class="btn block" style="margin-top:12px" :disabled="busy" @click="topup">Add {{ naira(amount) }}</button>
      </div>

      <div class="card stack" v-if="h">
        <h2>Connect this device</h2>
        <p class="muted small" style="margin:0">Uses a plan from your account on this phone.</p>
        <label class="check"><input type="checkbox" v-model="agree" /><span>I accept the <a href="/terms.html" target="_blank" rel="noopener">terms of use</a>.</span></label>
        <button class="btn block" :disabled="busy || !live" @click="connect">{{ live ? 'Connect now' : 'Buy a plan below first' }}</button>
      </div>

      <div class="card stack">
        <h2>My plans</h2>
        <p v-if="!me.entitlements.length" class="muted">No plans yet.</p>
        <div v-for="e in me.entitlements" :key="e.id" class="row between" style="border-top:1px solid var(--soft);padding-top:10px">
          <span><strong>{{ e.plan }}</strong><br /><span class="muted small">{{ left(e) }}</span></span>
          <span :class="['chip', e.status === 'active' || e.status === 'pending' ? 'ok' : '']">{{ e.status }}</span>
        </div>
      </div>

      <div class="card stack">
        <h2>Buy with wallet</h2>
        <div v-for="p in plans" :key="p.id" class="row between">
          <span><strong>{{ p.name }}</strong><br /><span class="muted small">{{ naira(p.price_kobo) }}</span></span>
          <button class="btn small" :disabled="busy || p.price_kobo > me.balance_kobo" @click="buy(p)">{{ p.price_kobo > me.balance_kobo ? 'Top up first' : 'Buy' }}</button>
        </div>
      </div>

      <div class="card stack">
        <h2>My devices</h2>
        <p v-if="!devices.length" class="muted">Devices appear here once they connect.</p>
        <div v-for="d in devices" :key="d.device_id" class="row between" style="border-top:1px solid var(--soft);padding-top:10px">
          <span><strong>{{ d.hostname || d.class || 'Device' }}</strong><br /><span class="muted small">{{ d.os || 'Unknown OS' }} · {{ d.mac }}</span></span>
          <button class="btn ghost small" @click="swapping = d">Replace</button>
        </div>
        <form v-if="swapping" class="stack" @submit.prevent="swap">
          <Notice kind="warn" text="Connect the new device to the Wi-Fi and sign in on it first. Then enter its address here. You can replace a device once per cooldown period." />
          <div><label for="nm">New device address (MAC)</label><input id="nm" v-model="newMac" class="mono" placeholder="AA:BB:CC:DD:EE:FF" required /></div>
          <div class="row"><button class="btn small" :disabled="busy">Replace device</button><button type="button" class="btn ghost small" @click="swapping = null">Cancel</button></div>
        </form>
      </div>

      <form class="card stack" @submit.prevent="claim">
        <h2>Add an access code</h2>
        <p class="muted small" style="margin:0">Bought a code earlier? Move it into your account.</p>
        <input v-model="claimCode" class="mono" placeholder="ABCD2345" aria-label="Access code" autocapitalize="characters" required />
        <button class="btn ghost block" :disabled="busy">Add to my account</button>
      </form>
    </template>
  </div>
</template>

<script setup>
import { ref, computed, onMounted } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { member, memberToken, portal, config } from '../api'
import { hotspot, loginAtRouter } from '../hotspot'
import { naira, duration, bytes, when } from '../format'
import Notice from '../components/Notice.vue'

const cfg = config(), route = useRoute(), router = useRouter(), h = hotspot()
const me = ref(null), plans = ref([]), devices = ref([])
const loading = ref(true), busy = ref(false), error = ref(''), info = ref('')
const amounts = [20000, 50000, 100000, 200000], amount = ref(50000)
const agree = ref(false), swapping = ref(null), newMac = ref(''), claimCode = ref('')
const live = computed(() => me.value?.entitlements.some((e) => e.status === 'active' || e.status === 'pending'))

function left(e) {
  const p = []
  if (e.remaining_active_seconds != null) p.push(duration(e.remaining_active_seconds) + ' left')
  if (e.remaining_bytes != null) p.push(bytes(e.remaining_bytes) + ' left')
  p.push('expires ' + when(e.expires_at))
  return p.join(' · ')
}
function fail(e) { error.value = e.message; if (e.status === 401) { memberToken.clear(); router.replace('/member') } }
async function refresh() {
  [me.value, devices.value] = await Promise.all([member.me(), member.devices().then((d) => d.devices)])
}
async function load() {
  try {
    if (!memberToken.get()) return router.replace('/member')
    await refresh()
    plans.value = (await portal.plans(h?.site || cfg.site)).filter((p) => p.price_kobo > 0)
    const ref = route.query.ref
    if (ref) await settleTopup(String(ref))
  } catch (e) { fail(e) } finally { loading.value = false }
}
async function settleTopup(ref) {
  info.value = 'Confirming your top-up…'
  for (let i = 0; i < 20; i++) {
    const s = await portal.orderStatus(ref)
    if (s.status === 'provisioned' || s.status === 'paid') { await refresh(); info.value = 'Top-up received.'; return router.replace('/member/home') }
    if (s.status === 'failed' || s.status === 'expired') { info.value = ''; error.value = 'The top-up did not go through. You have not been charged.'; return }
    await new Promise((r) => setTimeout(r, 3000))
  }
  info.value = 'Your top-up is still being confirmed. Your balance will update shortly.'
}
async function act(fn, ok) {
  error.value = ''; info.value = ''; busy.value = true
  try { await fn(); if (ok) info.value = ok; await refresh() } catch (e) { fail(e) } finally { busy.value = false }
}
const topup = () => act(async () => { const r = await member.topup(amount.value); window.location.assign(r.authorization_url) })
const buy = (p) => act(() => member.buy(p.id), p.name + ' bought.')
const claim = () => act(async () => { const r = await member.claim(claimCode.value); claimCode.value = ''; info.value = r.moved + ' plan(s) added.' })
const swap = () => act(async () => { await member.swap({ entitlement_id: swapping.value.entitlement_id, old_device_id: swapping.value.device_id, new_mac: newMac.value }); swapping.value = null; newMac.value = '' }, 'Device replaced.')
async function connect() {
  error.value = ''
  if (!agree.value) { error.value = 'Please accept the terms to continue.'; return }
  busy.value = true
  try {
    const t = await member.connect({ mac: h.mac, accept_terms: true })
    if (!loginAtRouter(t)) error.value = 'We could not reach the Wi-Fi router from here. Reconnect to the Wi-Fi and try again.'
  } catch (e) { fail(e) } finally { busy.value = false }
}
async function signOut() { try { await member.logout() } catch { /* already gone */ } memberToken.clear(); router.replace('/member') }
onMounted(load)
</script>
