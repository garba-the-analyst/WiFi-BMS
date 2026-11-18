<template>
  <div class="shell">
    <header class="top"><router-link to="/" class="brand">{{ cfg.brand }}</router-link></header>
    <h1>Member account</h1>
    <p class="muted" style="margin:6px 0 14px">Keep a wallet, buy plans in one tap and use your devices without codes.</p>

    <div class="tabs" role="tablist">
      <button v-for="t in tabs" :key="t.id" role="tab" :aria-selected="tab === t.id" :class="{ on: tab === t.id }" @click="pick(t.id)">{{ t.label }}</button>
    </div>

    <form v-if="tab !== 'sms'" class="card stack" style="margin-top:14px" @submit.prevent="submit">
      <div><label for="phone">Phone number</label><input id="phone" v-model="phone" type="tel" inputmode="tel" autocomplete="tel" placeholder="0801 234 5678" required /></div>
      <div v-if="tab === 'new'"><label for="name">Name (optional)</label><input id="name" v-model="name" autocomplete="name" /></div>
      <div>
        <label for="pw">Password</label>
        <input id="pw" v-model="password" type="password" :autocomplete="tab === 'new' ? 'new-password' : 'current-password'" required />
        <p v-if="tab === 'new'" class="muted small" style="margin:4px 0 0">At least 10 characters. A short sentence works well.</p>
      </div>
      <Notice :text="error" kind="bad" />
      <button class="btn block" :disabled="busy">{{ tab === 'new' ? 'Create account' : 'Sign in' }}</button>
    </form>

    <form v-else class="card stack" style="margin-top:14px" @submit.prevent="sms">
      <div><label for="sphone">Phone number</label><input id="sphone" v-model="phone" type="tel" inputmode="tel" autocomplete="tel" placeholder="0801 234 5678" required :readonly="sent" /></div>
      <div v-if="sent"><label for="otp">6-digit code</label><input id="otp" v-model="otp" inputmode="numeric" autocomplete="one-time-code" maxlength="6" required /></div>
      <Notice v-if="sent" kind="ok" text="We sent a code by SMS. It lasts 10 minutes." />
      <Notice :text="error" kind="bad" />
      <button class="btn block" :disabled="busy">{{ sent ? 'Verify and sign in' : 'Send me a code' }}</button>
    </form>
  </div>
</template>

<script setup>
import { ref } from 'vue'
import { useRouter } from 'vue-router'
import { member, memberToken, config } from '../api'
import { plausiblePhone } from '../format'
import Notice from '../components/Notice.vue'

const cfg = config(), router = useRouter()
const tabs = [{ id: 'in', label: 'Sign in' }, { id: 'new', label: 'Create account' }, { id: 'sms', label: 'SMS code' }]
const tab = ref('in'), phone = ref(''), name = ref(''), password = ref(''), otp = ref('')
const busy = ref(false), error = ref(''), sent = ref(false)
if (memberToken.get()) router.replace('/member/home')

function pick(t) { tab.value = t; error.value = ''; sent.value = false }
async function done(r) { memberToken.set(r.token); router.push('/member/home') }
async function submit() {
  error.value = ''
  if (!plausiblePhone(phone.value)) { error.value = 'Enter a valid phone number.'; return }
  busy.value = true
  try {
    done(tab.value === 'new'
      ? await member.register({ phone: phone.value, password: password.value, display_name: name.value || undefined })
      : await member.login({ phone: phone.value, password: password.value }))
  } catch (e) { error.value = e.message } finally { busy.value = false }
}
async function sms() {
  error.value = ''
  if (!plausiblePhone(phone.value)) { error.value = 'Enter a valid phone number.'; return }
  busy.value = true
  try {
    if (!sent.value) { await member.otpRequest(phone.value); sent.value = true } else done(await member.otpVerify(phone.value, otp.value))
  } catch (e) { error.value = e.message } finally { busy.value = false }
}
</script>
