<template>
  <div class="shell">
    <header class="top">
      <span class="brand">{{ cfg.brand }}</span>
      <router-link to="/member" class="small">Member sign in</router-link>
    </header>

    <h1>Get online</h1>
    <p class="muted" style="margin:6px 0 16px">Pick a plan. Time only counts while you are actually using the internet.</p>

    <router-link to="/connect" class="card row between" style="color:inherit;text-decoration:none;margin-bottom:16px">
      <span><strong>Already have an access code?</strong><br /><span class="muted small">Enter it to connect</span></span>
      <span aria-hidden="true">›</span>
    </router-link>

    <div v-if="loading" class="row" style="justify-content:center;padding:30px"><span class="spinner" aria-label="Loading plans"></span></div>
    <Notice :text="error" kind="bad" />

    <div class="stack" v-if="plans.length" role="radiogroup" aria-label="Plans">
      <button v-for="p in plans" :key="p.id" type="button" role="radio" :aria-checked="picked?.id === p.id" class="card plan" :class="{ picked: picked?.id === p.id }"
        style="width:100%;text-align:left;cursor:pointer;font:inherit;color:inherit" @click="picked = p">
        <span>
          <strong>{{ p.name }}</strong><br />
          <span class="muted small">{{ describe(p) }}</span>
        </span>
        <span class="price">{{ naira(p.price_kobo) }}</span>
      </button>
    </div>
    <p v-else-if="!loading && !error" class="muted">No plans are on sale right now.</p>

    <form v-if="picked" class="card stack" style="margin-top:16px" @submit.prevent="pay">
      <h2>Pay for {{ picked.name }}</h2>
      <div>
        <label for="phone">Your phone number</label>
        <input id="phone" v-model="phone" type="tel" inputmode="tel" autocomplete="tel" placeholder="0801 234 5678" required />
        <p class="muted small" style="margin:4px 0 0">We send your receipt and access code to this number.</p>
      </div>
      <div>
        <label for="email">Email (optional)</label>
        <input id="email" v-model="email" type="email" autocomplete="email" placeholder="you@example.com" />
      </div>
      <Notice :text="payError" kind="bad" />
      <button class="btn block" :disabled="paying">{{ paying ? 'Opening checkout…' : 'Pay ' + naira(picked.price_kobo) }}</button>
      <p class="muted small" style="margin:0;text-align:center">Secure payment by Paystack. Card, bank transfer or USSD.</p>
    </form>
  </div>
</template>

<script setup>
import { ref, onMounted } from 'vue'
import { portal, config } from '../api'
import { hotspot } from '../hotspot'
import { naira, duration, bytes, speed, plausiblePhone } from '../format'
import Notice from '../components/Notice.vue'

const cfg = config()
const plans = ref([]), picked = ref(null), loading = ref(true), error = ref('')
const phone = ref(''), email = ref(''), paying = ref(false), payError = ref('')

function describe(p) {
  const parts = []
  if (p.kind !== 'data' && p.active_seconds) parts.push(duration(p.active_seconds) + ' of use')
  if (p.kind !== 'time' && p.data_bytes) parts.push(bytes(p.data_bytes))
  parts.push(p.device_limit === 1 ? '1 device' : p.device_limit + ' devices')
  parts.push(speed(p.down_kbps))
  parts.push('valid ' + duration(p.valid_seconds))
  return parts.join(' · ')
}

onMounted(async () => {
  try { plans.value = await portal.plans(hotspot()?.site || cfg.site) } catch (e) { error.value = e.message } finally { loading.value = false }
})

async function pay() {
  payError.value = ''
  if (!plausiblePhone(phone.value)) { payError.value = 'Enter a valid phone number, like 0801 234 5678.'; return }
  paying.value = true
  try {
    const o = await portal.order({ site: hotspot()?.site || cfg.site, plan_id: picked.value.id, phone: phone.value, email: email.value || undefined })
    try { localStorage.setItem('bms_last_order', o.reference) } catch { /* ignore */ }
    window.location.assign(o.authorization_url)
  } catch (e) { payError.value = e.message; paying.value = false }
}
</script>
