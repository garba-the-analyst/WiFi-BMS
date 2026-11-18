<template>
  <div class="stack">
    <p class="muted small" style="margin:0">Pick how you want to receive your sign-in code.</p>
    <div class="pills" role="radiogroup" aria-label="2-step verification method">
      <button v-for="m in methods" :key="m.id" type="button" role="radio" :aria-checked="method === m.id" :class="{ on: method === m.id }" :disabled="!m.ok || !!started" @click="method = m.id">{{ m.label }}</button>
    </div>
    <p v-if="unavailable" class="muted small" style="margin:0">{{ unavailable }}</p>

    <form v-if="!started" class="stack" @submit.prevent="start">
      <div v-if="method === 'sms'"><label for="ph">Mobile number that will receive the codes</label><input id="ph" v-model="phone" inputmode="tel" autocomplete="tel" placeholder="0803 123 4567" required /></div>
      <p v-if="method === 'email'" class="small" style="margin:0">A 6-digit code will be emailed to <strong>{{ me.email }}</strong>.</p>
      <Notice :text="error" kind="bad" />
      <button class="btn" :disabled="busy || !ok">{{ method === 'totp' ? 'Show QR code' : 'Send me a code' }}</button>
    </form>

    <form v-else class="stack" @submit.prevent="confirm">
      <template v-if="started.otpauth_uri">
        <p class="small" style="margin:0">Scan this with Google Authenticator, Microsoft Authenticator or Authy, then type the 6-digit code it shows.</p>
        <img v-if="qr" :src="qr" alt="QR code for your authenticator app" width="180" height="180" style="border-radius:8px;border:1px solid var(--line)" />
        <p class="small" style="margin:0">Cannot scan? Enter this key by hand: <span class="mono" style="user-select:all">{{ started.secret }}</span></p>
      </template>
      <p v-else class="small" style="margin:0">We sent a code to <strong>{{ started.sent_to }}</strong>. It works for 10 minutes.</p>
      <div><label for="cc">6-digit code</label><input id="cc" v-model="code" inputmode="numeric" autocomplete="one-time-code" maxlength="6" required style="max-width:180px" /></div>
      <Notice :text="error" kind="bad" />
      <div class="row"><button class="btn" :disabled="busy">Turn on</button><button type="button" class="btn ghost" @click="started = null; code = ''">Back</button></div>
    </form>
  </div>
</template>
<script setup>
import { ref, computed, watch } from 'vue'
import QRCode from 'qrcode'
import { admin } from '../api'
import { loadMe } from '../session'
import Notice from './Notice.vue'

const props = defineProps({ me: { type: Object, required: true } })
const emit = defineEmits(['done'])
const avail = computed(() => props.me.methods_available || { totp: true })
const methods = computed(() => [
  { id: 'totp', label: 'Authenticator app', ok: avail.value.totp },
  { id: 'email', label: 'Email code', ok: avail.value.email },
  { id: 'sms', label: 'SMS code', ok: avail.value.sms },
])
const method = ref('totp'), phone = ref(''), started = ref(null), qr = ref(''), code = ref(''), busy = ref(false), error = ref('')
const ok = computed(() => methods.value.find((m) => m.id === method.value)?.ok)
const unavailable = computed(() => {
  const off = methods.value.filter((m) => !m.ok).map((m) => m.label)
  return off.length ? `${off.join(' and ')} ${off.length > 1 ? 'are' : 'is'} not set up on this server yet, so ${off.length > 1 ? 'they are' : 'it is'} not offered.` : ''
})
watch(method, () => { error.value = '' })
async function start() {
  error.value = ''; busy.value = true
  try {
    started.value = await admin.mfaStart(method.value, phone.value)
    qr.value = started.value.otpauth_uri ? await QRCode.toDataURL(started.value.otpauth_uri, { width: 180, margin: 1 }) : ''
  } catch (e) { error.value = e.message } finally { busy.value = false }
}
async function confirm() {
  error.value = ''; busy.value = true
  try { await admin.mfaConfirm(started.value.challenge, code.value.trim()); await loadMe(true); emit('done') } catch (e) { error.value = e.message } finally { busy.value = false }
}
</script>
