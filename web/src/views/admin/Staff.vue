<template>
  <div>
    <div class="head"><h1>Staff</h1><div class="row"><label class="check"><input v-model="all" type="checkbox" @change="load" /> Show removed</label><button class="btn" @click="openNew">Add staff</button></div></div>
    <Notice :text="error" kind="bad" /><Notice :text="info" kind="ok" />

    <form v-if="form" class="panel stack" @submit.prevent="create">
      <h2>Add staff member</h2>
      <div class="formgrid">
        <div><label for="se">Email</label><input id="se" v-model="form.email" type="email" required autocomplete="off" /></div>
        <div><label for="sn">Name</label><input id="sn" v-model="form.display_name" maxlength="80" /></div>
        <div><label for="sr">Role</label><select id="sr" v-model="form.role"><option v-for="r in roles" :key="r" :value="r">{{ roleLabel(r) }}</option></select></div>
        <div><label for="sp">Temporary password</label><input id="sp" v-model="form.password" type="text" autocomplete="off" minlength="10" required /></div>
      </div>
      <p class="muted small" style="margin:0">{{ roleHelp[form.role] }} They will be asked to set up 2-step verification the first time they sign in. Give them the password in person or by phone, never in the same message as the email.</p>
      <Notice :text="formError" kind="bad" />
      <div class="row"><button class="btn" :disabled="busy">Create account</button><button type="button" class="btn ghost" @click="form = null">Cancel</button></div>
    </form>

    <div class="panel tablewrap"><table>
      <thead><tr><th>Person</th><th>Role</th><th>2-step</th><th>Last sign-in</th><th class="right">Actions logged</th><th>Status</th><th></th></tr></thead>
      <tbody>
        <tr v-for="u in users" :key="u.id"><td><strong>{{ u.display_name || u.email }}</strong><div class="muted small">{{ u.email }}</div></td>
          <td>{{ roleLabel(u.role) }}</td><td><span :class="['chip', u.mfa_method === 'none' ? 'warn' : 'ok']">{{ methodLabel(u.mfa_method) }}</span></td>
          <td>{{ when(u.last_login_at) }}</td><td class="right">{{ u.actions }}</td><td><span :class="['chip', u.status === 'active' ? 'ok' : 'bad']">{{ u.status }}</span></td>
          <td class="nowrap"><template v-if="u.status !== 'closed'"><button class="btn ghost small" @click="activity(u)">Activity</button> <button v-if="manageable(u)" class="btn ghost small" @click="pick = pick === u.id ? null : u.id">Manage</button></template></td></tr>
        <tr v-if="pick && current" class="manage"><td colspan="7">
          <div class="stack">
            <div class="row">
              <label class="muted small" for="mr">Role</label><select id="mr" style="max-width:170px" :value="current.role" @change="patch(current, { role: $event.target.value })"><option v-for="r in rolesFor(current)" :key="r" :value="r">{{ roleLabel(r) }}</option></select>
              <button class="btn ghost small" @click="patch(current, { status: current.status === 'active' ? 'suspended' : 'active' })">{{ current.status === 'active' ? 'Suspend' : 'Reactivate' }}</button>
              <button class="btn ghost small" :disabled="current.mfa_method === 'none'" @click="resetMfa(current)">Reset 2-step</button>
              <button class="btn danger small" @click="remove(current)">Remove</button>
            </div>
            <form class="row" @submit.prevent="resetPw(current)"><input v-model="newPw" type="text" autocomplete="off" minlength="10" placeholder="New temporary password" style="max-width:260px" aria-label="New temporary password" required /><button class="btn ghost small">Set password</button></form>
          </div></td></tr>
        <tr v-if="!users.length"><td colspan="7" class="muted">No staff.</td></tr>
      </tbody></table></div>

    <div v-if="act" class="panel"><div class="row between"><h2>Activity: {{ act.user.display_name || act.user.email }}</h2><button class="btn ghost small" @click="act = null">Close</button></div>
      <div class="tablewrap"><table><thead><tr><th>When</th><th>Action</th><th>Detail</th><th>From</th></tr></thead><tbody>
        <tr v-for="e in act.rows" :key="e.seq"><td class="nowrap">{{ when(e.ts) }}</td><td class="mono">{{ e.action }}</td><td class="mono small" style="word-break:break-all">{{ JSON.stringify(e.detail) }}</td><td class="mono small">{{ e.ip || '–' }}</td></tr>
        <tr v-if="!act.rows.length"><td colspan="4" class="muted">Nothing yet.</td></tr></tbody></table></div></div>
  </div>
</template>
<script setup>
import { ref, computed, onMounted } from 'vue'
import { admin } from '../../api'
import { session } from '../../session'
import { when, roleLabel } from '../../format'
import Notice from '../../components/Notice.vue'

const roleHelp = {
  system_admin: 'Unlimited access, including site keys and staff.', ceo: 'Master account: everything except the technical site keys.',
  admin: 'Day-to-day running: plans, customers, refunds, vouchers (including special ones) and the error log.', customer_rep: 'Front desk: complaints, customers, manual voucher sales and the error log.',
}
const users = ref([]), all = ref(false), form = ref(null), busy = ref(false), error = ref(''), info = ref(''), formError = ref(''), pick = ref(null), newPw = ref(''), act = ref(null)
// The server enforces who may manage whom; this mirrors it so people are not offered buttons that fail.
const mine = computed(() => session.me?.role)
const roles = computed(() => (mine.value === 'system_admin' ? ['system_admin', 'ceo', 'admin', 'customer_rep'] : ['admin', 'customer_rep']))
const rolesFor = (u) => (roles.value.includes(u.role) ? roles.value : [u.role])
const manageable = (u) => u.id !== session.me?.id && roles.value.includes(u.role)
const current = computed(() => users.value.find((u) => u.id === pick.value))
const methodLabel = (m) => ({ none: 'not set up', totp: 'app', email: 'email', sms: 'SMS' })[m] || m
async function load() { try { users.value = await admin.users(all.value) } catch (e) { error.value = e.message } }
function openNew() { formError.value = ''; form.value = { email: '', display_name: '', role: 'customer_rep', password: '' } }
async function run(fn, ok) { error.value = ''; info.value = ''; busy.value = true; try { await fn(); if (ok) info.value = ok; await load() } catch (e) { error.value = e.message } finally { busy.value = false } }
async function create() {
  formError.value = ''; busy.value = true
  try { await admin.createUser({ ...form.value, display_name: form.value.display_name || undefined }); info.value = 'Account created.'; form.value = null; await load() } catch (e) { formError.value = e.message } finally { busy.value = false }
}
const patch = (u, b) => run(() => admin.updateUser(u.id, b), 'Saved.')
const resetMfa = (u) => window.confirm(`Reset 2-step verification for ${u.email}? They must set it up again at their next sign-in.`) && run(() => admin.resetMfa(u.id), '2-step verification reset.')
const remove = (u) => window.confirm(`Remove ${u.email}? They are signed out and can no longer sign in. Their history stays in the activity log.`) && run(async () => { await admin.deleteUser(u.id); pick.value = null }, 'Account removed.')
const resetPw = (u) => run(async () => { await admin.resetPassword(u.id, newPw.value); newPw.value = '' }, 'Password changed. Tell them in person or by phone.')
async function activity(u) { try { act.value = { user: u, rows: await admin.userActivity(u.id, { limit: 100 }) } } catch (e) { error.value = e.message } }
onMounted(load)
</script>
