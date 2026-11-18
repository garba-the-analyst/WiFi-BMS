<template>
  <div class="login">
    <h1 class="brand" style="margin-bottom:4px">{{ cfg.brand }}</h1>
    <p class="muted" style="margin:0 0 16px">Staff sign in</p>
    <form class="card stack" @submit.prevent="go">
      <div><label for="e">Email</label><input id="e" v-model="email" type="email" autocomplete="username" required /></div>
      <div><label for="p">Password</label><input id="p" v-model="password" type="password" autocomplete="current-password" required /></div>
      <div v-if="needCode || code"><label for="c">6-digit code from your authenticator app</label><input id="c" v-model="code" inputmode="numeric" autocomplete="one-time-code" maxlength="6" /></div>
      <Notice :text="error" kind="bad" />
      <button class="btn block" :disabled="busy">Sign in</button>
      <button v-if="!needCode && !code" type="button" class="btn ghost small" @click="needCode = true">I use 2-step verification</button>
    </form>
  </div>
</template>
<script setup>
import { ref } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { admin, adminToken, config } from '../../api'
import Notice from '../../components/Notice.vue'

const cfg = config(), route = useRoute(), router = useRouter()
const email = ref(''), password = ref(''), code = ref(''), needCode = ref(false), busy = ref(false), error = ref('')
async function go() {
  error.value = ''; busy.value = true
  try {
    const r = await admin.login({ email: email.value, password: password.value, totp: code.value || undefined })
    adminToken.set(r.token)
    try { sessionStorage.setItem('bms_admin_role', r.role); sessionStorage.setItem('bms_admin_totp', r.totp_enrolled ? '1' : '0') } catch { /* ignore */ }
    const next = String(route.query.next || '/admin')
    router.replace(next.startsWith('/admin') ? next : '/admin')
  } catch (e) {
    error.value = e.status === 401 ? 'Wrong email, password or code.' : e.message
    if (e.status === 401) needCode.value = true
  } finally { busy.value = false }
}
</script>
