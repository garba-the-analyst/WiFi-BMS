<template>
  <div class="shell">
    <header class="top"><router-link to="/" class="brand">{{ cfg.brand }}</router-link></header>
    <h1>Connect</h1>

    <Notice v-if="!h" kind="warn" text="Connect your phone to the Wi-Fi first, then open any website. You will be brought back here to sign in." />

    <form class="card stack" style="margin-top:14px" @submit.prevent="go">
      <div>
        <label for="code">Access code</label>
        <input id="code" v-model="code" class="mono" autocomplete="off" autocapitalize="characters" spellcheck="false" placeholder="ABCD2345" maxlength="20" required />
      </div>
      <label class="check"><input type="checkbox" v-model="agree" /><span>I accept the <a href="/terms.html" target="_blank" rel="noopener">terms of use</a>. My device details are recorded for security.</span></label>
      <Notice :text="error" kind="bad" />
      <Notice v-if="done" kind="ok" text="Connecting…" />
      <button class="btn block" :disabled="busy || !h">{{ busy ? 'Checking…' : 'Connect' }}</button>
    </form>

    <p class="muted small" style="text-align:center;margin-top:16px">No code? <router-link to="/">Buy a plan</router-link> or <router-link to="/member">sign in</router-link>.</p>
  </div>
</template>

<script setup>
import { ref } from 'vue'
import { useRoute } from 'vue-router'
import { portal, config } from '../api'
import { hotspot, loginAtRouter } from '../hotspot'
import Notice from '../components/Notice.vue'

const cfg = config()
const route = useRoute()
const h = hotspot()
const code = ref(String(route.query.code || ''))
const agree = ref(false), busy = ref(false), error = ref(''), done = ref(false)

async function go() {
  error.value = ''
  if (!agree.value) { error.value = 'Please accept the terms to continue.'; return }
  busy.value = true
  try {
    const t = await portal.connect({ site: h.site || cfg.site, mac: h.mac, code: code.value, accept_terms: true })
    done.value = true
    if (!loginAtRouter(t)) { done.value = false; error.value = 'We could not reach the Wi-Fi router from here. Reconnect to the Wi-Fi and try again.' }
  } catch (e) { error.value = e.message } finally { busy.value = false }
}
</script>
