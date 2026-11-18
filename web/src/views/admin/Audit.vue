<template>
  <div>
    <div class="head"><h1>Activity log</h1><div class="row"><button class="btn ghost small" @click="verify">Verify integrity</button><button class="btn ghost small" @click="download">Download CSV</button></div></div>
    <Notice :text="error" kind="bad" />
    <Notice v-if="check" :kind="check.intact ? 'ok' : 'bad'" :text="check.intact ? `All ${check.entries} entries are intact. Nothing has been altered.` : `Problem found: ${check.problems.length} entries do not match their chain. Treat this as a security incident.`" />
    <div class="row" style="margin-bottom:12px"><input v-model="action" type="search" placeholder="Filter by action, e.g. refund. or plan." style="max-width:300px" aria-label="Filter by action" />
      <select v-model="actor" aria-label="Filter by staff member" style="max-width:240px"><option value="">Everyone</option><option v-for="u in staff" :key="u.id" :value="u.id">{{ u.display_name || u.email }}</option></select></div>
    <div class="panel tablewrap"><table><thead><tr><th>#</th><th>When</th><th>Who</th><th>Action</th><th>Detail</th></tr></thead><tbody>
      <tr v-for="e in rows" :key="e.seq"><td class="muted">{{ e.seq }}</td><td class="nowrap">{{ when(e.ts) }}</td><td>{{ e.actor_email || e.actor_kind }}<br /><span class="muted small">{{ e.actor_email ? e.actor_kind : (e.actor_id || '').slice(0, 8) }}</span></td><td class="mono">{{ e.action }}</td><td class="mono small" style="word-break:break-all">{{ JSON.stringify(e.detail) }}</td></tr>
      <tr v-if="!rows.length"><td colspan="5" class="muted">No entries.</td></tr></tbody></table></div>
    <button v-if="rows.length >= 100" class="btn ghost small" @click="more">Older entries</button>
  </div>
</template>
<script setup>
import { ref, watch, onMounted } from 'vue'
import { admin } from '../../api'
import { when } from '../../format'
import Notice from '../../components/Notice.vue'
const staff = ref([]), actor = ref(''), rows = ref([]), action = ref(''), error = ref(''), check = ref(null)
async function load() { try { error.value = ''; rows.value = await admin.audit({ action: action.value, actor: actor.value, limit: 100 }) } catch (e) { error.value = e.message } }
async function more() { try { rows.value = rows.value.concat(await admin.audit({ action: action.value, actor: actor.value, limit: 100, before: rows.value.at(-1).seq })) } catch (e) { error.value = e.message } }
async function verify() { try { check.value = await admin.auditVerify() } catch (e) { error.value = e.message } }
async function download() { try { await admin.downloadAudit() } catch (e) { error.value = e.message } }
let t; watch([action, actor], () => { clearTimeout(t); t = setTimeout(load, 300) }); onMounted(async () => { load(); try { staff.value = await admin.users(true) } catch { /* people without staff access just see no filter */ } })
</script>
