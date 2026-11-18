<template>
  <div class="login" style="max-width:440px">
    <h1 class="brand" style="margin-bottom:4px">{{ cfg.brand }}</h1>
    <p class="muted" style="margin:0 0 16px">{{ session.me?.setup_required ? 'One more step: turn on 2-step verification' : 'Change 2-step verification' }}</p>
    <div class="card">
      <p v-if="session.me?.setup_required" class="small" style="margin-top:0">Staff accounts handle money and customer data, so every account needs a second check at sign-in. This takes about a minute.</p>
      <MfaEnrol v-if="session.me" :me="session.me" @done="done" />
    </div>
    <p class="small"><a href="#" @click.prevent="signOut">Sign out</a></p>
  </div>
</template>
<script setup>
import { useRouter } from 'vue-router'
import { admin, adminToken, config } from '../../api'
import { session, clearSession, homePath } from '../../session'
import MfaEnrol from '../../components/MfaEnrol.vue'

const cfg = config(), router = useRouter()
const done = () => router.replace(homePath())
async function signOut() { try { await admin.logout() } catch { /* ignore */ } adminToken.clear(); clearSession(); router.replace('/admin/login') }
</script>
