<template>
  <div>
    <div class="head"><h1>Settings</h1></div>
    <Notice :text="error" kind="bad" /><Notice :text="info" kind="ok" />

    <div class="panel stack"><h2>2-step verification</h2>
      <p class="muted small" style="margin:0">Use an authenticator app (Google Authenticator, Microsoft Authenticator, Authy).</p>
      <div v-if="!totp"><button class="btn" :disabled="busy" @click="setup">Set up</button></div>
      <template v-else>
        <img v-if="qr" :src="qr" alt="QR code for your authenticator app" width="180" height="180" style="border-radius:8px;border:1px solid var(--line)" />
        <p class="small" style="margin:0">Can't scan? Enter this key: <span class="mono">{{ totp.secret }}</span></p>
        <form class="row" @submit.prevent="confirm"><input v-model="code" inputmode="numeric" maxlength="6" placeholder="6-digit code" style="max-width:170px" aria-label="Code from your app" required /><button class="btn" :disabled="busy">Turn on</button></form>
      </template></div>

    <form class="panel stack" @submit.prevent="saveKey"><h2>Paystack secret key</h2>
      <p class="muted small" style="margin:0">Owners only. Stored encrypted and never shown again. Use a <span class="mono">sk_test_</span> key until you have tested a full payment.</p>
      <input v-model="key" type="password" autocomplete="off" placeholder="sk_live_… or sk_test_…" aria-label="Paystack secret key" required />
      <div><button class="btn" :disabled="busy">Save key</button></div></form>

    <div class="panel stack"><h2>Site agent keys</h2>
      <p class="muted small" style="margin:0">One key per site. The key is shown once when you create it. Revoke a key if a site computer is lost.</p>
      <form class="row" @submit.prevent="newToken"><input v-model="tokSite" placeholder="Site (e.g. site-1)" style="max-width:180px" aria-label="Site" required /><input v-model="tokName" placeholder="Label (e.g. Site 1 PC)" style="max-width:220px" aria-label="Label" required /><button class="btn" :disabled="busy">Create key</button></form>
      <div v-if="token" class="notice warn stack"><strong>Copy this key now. It cannot be shown again.</strong><div class="mono" style="word-break:break-all;user-select:all">{{ token }}</div></div>
      <div v-for="t in tokens.filter((x) => !x.revoked_at)" :key="t.id" class="row between" style="border-top:1px solid var(--soft);padding-top:8px"><span>{{ t.name }} <span class="muted small">· last used {{ when(t.last_used_at) }}</span></span><button class="btn danger small" @click="revoke(t)">Revoke</button></div></div>
  </div>
</template>
<script setup>
import { ref, onMounted } from 'vue'
import QRCode from 'qrcode'
import { admin } from '../../api'
import { when } from '../../format'
import Notice from '../../components/Notice.vue'

const error = ref(''), info = ref(''), busy = ref(false)
const totp = ref(null), qr = ref(''), code = ref(''), key = ref('')
const tokens = ref([]), tokSite = ref('site-1'), tokName = ref(''), token = ref('')
async function run(fn, ok) { error.value = ''; info.value = ''; busy.value = true; try { await fn(); if (ok) info.value = ok } catch (e) { error.value = e.message } finally { busy.value = false } }
const setup = () => run(async () => { totp.value = await admin.totpSetup(); qr.value = await QRCode.toDataURL(totp.value.otpauth_uri, { width: 180, margin: 1 }) })
const confirm = () => run(async () => { await admin.totpConfirm(code.value); totp.value = null; code.value = ''; try { sessionStorage.setItem('bms_admin_totp', '1') } catch { /* ignore */ } }, '2-step verification is on. You will need a code each time you sign in.')
const saveKey = () => run(async () => { await admin.setPaystackKey(key.value); key.value = '' }, 'Paystack key saved.')
const newToken = () => run(async () => { token.value = (await admin.createNodeToken({ site: tokSite.value, name: tokName.value })).token; tokName.value = ''; await load() })
const revoke = (t) => window.confirm(`Revoke "${t.name}"? That site will stop working until it gets a new key.`) && run(async () => { await admin.revokeNodeToken(t.id); await load() }, 'Key revoked.')
async function load() { try { tokens.value = (await admin.network()).node_tokens } catch (e) { error.value = e.message } }
onMounted(load)
</script>
