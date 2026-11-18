<template>
  <div>
    <div class="head"><h1>Settings</h1></div>
    <Notice :text="error" kind="bad" /><Notice :text="info" kind="ok" />

    <div class="panel stack"><h2>Your account</h2>
      <p class="small" style="margin:0" v-if="me">{{ me.display_name || me.email }} · {{ roleLabel(me.role) }} · {{ me.email }}</p>
      <form class="stack" style="max-width:360px" @submit.prevent="changePw">
        <div><label for="cp">Current password</label><input id="cp" v-model="pw.current" type="password" autocomplete="current-password" required /></div>
        <div><label for="np">New password (10 characters or more)</label><input id="np" v-model="pw.next" type="password" autocomplete="new-password" minlength="10" required /></div>
        <div><button class="btn" :disabled="busy">Change password</button></div>
      </form></div>

    <div class="panel stack"><h2>2-step verification</h2>
      <p class="small" style="margin:0" v-if="me">Now using: <strong>{{ methodLabel(me.mfa_method) }}</strong><span v-if="me.phone"> ({{ me.phone }})</span></p>
      <div v-if="!changing"><button class="btn ghost" @click="changing = true">Change method</button></div>
      <MfaEnrol v-else-if="me" :me="me" @done="changing = false; info = '2-step verification updated.'" /></div>

    <form v-if="can('brand')" class="panel stack" @submit.prevent="saveBrand"><h2>Business details</h2>
      <p class="muted small" style="margin:0">Shown to customers on the portal. Changes appear the next time a page loads.</p>
      <div class="formgrid">
        <div><label for="bn">Business name</label><input id="bn" v-model="brand.brand" maxlength="60" required /></div>
        <div><label for="bt">Tagline</label><input id="bt" v-model="brand.tagline" maxlength="120" /></div>
        <div><label for="bs">Support phone</label><input id="bs" v-model="brand.supportPhone" inputmode="tel" maxlength="20" /></div>
        <div><label for="bc">Brand colour</label><div class="row" style="flex-wrap:nowrap"><input id="bc" v-model="brand.primary" type="color" style="max-width:70px;padding:2px" /><span class="mono small">{{ brand.primary }}</span></div></div>
      </div>
      <div><label for="bh">Router login addresses (one per line, optional)</label><textarea id="bh" v-model="hosts" rows="2" placeholder="Only needed if your router is not on a private address"></textarea></div>
      <div><button class="btn" :disabled="busy">Save details</button></div></form>

    <form v-if="can('secrets')" class="panel stack" @submit.prevent="saveKey"><h2>Paystack secret key</h2>
      <p class="muted small" style="margin:0">Stored encrypted and never shown again. Use a <span class="mono">sk_test_</span> key until you have tested a full payment.</p>
      <input v-model="key" type="password" autocomplete="off" placeholder="sk_live_… or sk_test_…" aria-label="Paystack secret key" required />
      <div><button class="btn" :disabled="busy">Save key</button></div></form>

    <div v-if="can('node_tokens')" class="panel stack"><h2>Site agent keys</h2>
      <p class="muted small" style="margin:0">One key per site. The key is shown once when you create it. Revoke a key if a site computer is lost.</p>
      <form class="row" @submit.prevent="newToken"><input v-model="tokSite" placeholder="Site (e.g. site-1)" style="max-width:180px" aria-label="Site" required /><input v-model="tokName" placeholder="Label (e.g. Site 1 PC)" style="max-width:220px" aria-label="Label" required /><button class="btn" :disabled="busy">Create key</button></form>
      <div v-if="token" class="notice warn stack"><strong>Copy this key now. It cannot be shown again.</strong><div class="mono" style="word-break:break-all;user-select:all">{{ token }}</div></div>
      <div v-for="t in tokens.filter((x) => !x.revoked_at)" :key="t.id" class="row between" style="border-top:1px solid var(--soft);padding-top:8px"><span>{{ t.name }} <span class="muted small">· last used {{ when(t.last_used_at) }}</span></span><button class="btn danger small" @click="revoke(t)">Revoke</button></div></div>
  </div>
</template>
<script setup>
import { ref, computed, onMounted } from 'vue'
import { admin, config } from '../../api'
import { session, can } from '../../session'
import { when, roleLabel } from '../../format'
import Notice from '../../components/Notice.vue'
import MfaEnrol from '../../components/MfaEnrol.vue'

const cfg = config()
const me = computed(() => session.me)
const error = ref(''), info = ref(''), busy = ref(false), changing = ref(false)
const pw = ref({ current: '', next: '' }), key = ref('')
const brand = ref({ brand: cfg.brand, tagline: cfg.tagline || '', supportPhone: cfg.supportPhone || '', primary: cfg.primary || '#0F766E' }), hosts = ref((cfg.hotspotHosts || []).join('\n'))
const tokens = ref([]), tokSite = ref(cfg.site || 'site-1'), tokName = ref(''), token = ref('')
const methodLabel = (m) => ({ none: 'not set up', totp: 'authenticator app', email: 'email code', sms: 'SMS code' })[m] || m
async function run(fn, ok) { error.value = ''; info.value = ''; busy.value = true; try { await fn(); if (ok) info.value = ok } catch (e) { error.value = e.message } finally { busy.value = false } }
const changePw = () => run(async () => { await admin.changePassword(pw.value.current, pw.value.next); pw.value = { current: '', next: '' } }, 'Password changed.')
const saveBrand = () => run(() => admin.saveBrand({ ...brand.value, hotspotHosts: hosts.value.split('\n').map((h) => h.trim()).filter(Boolean) }), 'Saved.')
const saveKey = () => run(async () => { await admin.setPaystackKey(key.value); key.value = '' }, 'Paystack key saved.')
const newToken = () => run(async () => { token.value = (await admin.createNodeToken({ site: tokSite.value, name: tokName.value })).token; tokName.value = ''; await load() })
const revoke = (t) => window.confirm(`Revoke "${t.name}"? That site will stop working until it gets a new key.`) && run(async () => { await admin.revokeNodeToken(t.id); await load() }, 'Key revoked.')
async function load() { if (!can('node_tokens')) return; try { tokens.value = (await admin.network()).node_tokens } catch (e) { error.value = e.message } }
onMounted(load)
</script>
