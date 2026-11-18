<template>
  <div class="login">
    <h1 class="brand" style="margin-bottom:4px">{{ cfg.brand }}</h1>
    <p class="muted" style="margin:0 0 16px">Staff sign in</p>

    <form v-if="!ch" class="card stack" @submit.prevent="go">
      <div><label for="e">Email</label><input id="e" v-model="email" type="email" autocomplete="username" required /></div>
      <div><label for="p">Password</label><input id="p" v-model="password" type="password" autocomplete="current-password" required /></div>
      <Notice :text="error" kind="bad" />
      <button class="btn block" :disabled="busy">Continue</button>
    </form>

    <form v-else class="card stack" @submit.prevent="verify">
      <p style="margin:0">{{ prompt }}</p>
      <div><label for="c">6-digit code</label><input id="c" ref="codeEl" v-model="code" inputmode="numeric" autocomplete="one-time-code" maxlength="6" required /></div>
      <Notice :text="error" kind="bad" />
      <button class="btn block" :disabled="busy">Sign in</button>
      <button type="button" class="btn ghost small" @click="back">Use a different account</button>
    </form>
  </div>
</template>
<script setup>
import { ref, computed, nextTick } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { admin, adminToken, config } from '../../api'
import { loadMe, homePath } from '../../session'
import Notice from '../../components/Notice.vue'

const cfg = config(), route = useRoute(), router = useRouter()
const email = ref(''), password = ref(''), code = ref(''), ch = ref(null), busy = ref(false), error = ref(''), codeEl = ref(null)
const prompt = computed(() => !ch.value ? '' : ch.value.method === 'totp' ? 'Open your authenticator app and enter the code it shows.'
  : ch.value.method === 'email' ? `We emailed a code to ${ch.value.sent_to}.` : `We sent a code by SMS to ${ch.value.sent_to}.`)

async function finish(token) {
  adminToken.set(token)
  const me = await loadMe(true)
  const next = String(route.query.next || '')
  router.replace(me.setup_required ? '/admin/setup' : next.startsWith('/admin') ? next : homePath())
}
async function go() {
  error.value = ''; busy.value = true
  try {
    const r = await admin.login({ email: email.value, password: password.value })
    if (r.token) return await finish(r.token) // first sign-in: limited access, must enrol
    ch.value = r; password.value = ''
    await nextTick(); codeEl.value && codeEl.value.focus()
  } catch (e) { error.value = e.status === 401 ? 'Wrong email or password.' : e.message } finally { busy.value = false }
}
async function verify() {
  error.value = ''; busy.value = true
  try { await finish((await admin.loginVerify(ch.value.challenge, code.value.trim())).token) } catch (e) {
    error.value = e.message
    if (e.status === 429 || /expired|start again/i.test(e.message)) back(e.message)
  } finally { busy.value = false }
}
function back(msg) { ch.value = null; code.value = ''; if (typeof msg === 'string') error.value = msg }
</script>
