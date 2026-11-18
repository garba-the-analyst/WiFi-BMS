<template>
  <div>
    <div class="head"><h1>Customers</h1><input v-model="q" type="search" placeholder="Search phone or name" style="max-width:260px" aria-label="Search customers" /></div>
    <Notice :text="error" kind="bad" />
    <div class="panel tablewrap"><table>
      <thead><tr><th>Member</th><th>Phone</th><th class="right">Wallet</th><th class="right">Live plans</th><th>Last sign-in</th><th>Status</th></tr></thead>
      <tbody>
        <tr v-for="c in rows" :key="c.id"><td><router-link :to="`/admin/customers/${c.id}`">{{ c.display_name || 'Unnamed' }}</router-link></td><td class="nowrap">{{ c.phone }}</td>
          <td class="right">{{ naira(c.balance_kobo) }}</td><td class="right">{{ c.live_plans }}</td><td>{{ when(c.last_login_at) }}</td><td><span :class="['chip', c.status === 'active' ? 'ok' : 'bad']">{{ c.status }}</span></td></tr>
        <tr v-if="!rows.length"><td colspan="6" class="muted">No members found.</td></tr>
      </tbody></table></div>
    <div class="row"><button class="btn ghost small" :disabled="page === 0" @click="page--">Previous</button><button class="btn ghost small" :disabled="rows.length < size" @click="page++">Next</button></div>
  </div>
</template>
<script setup>
import { ref, watch, onMounted } from 'vue'
import { admin } from '../../api'
import { naira, when } from '../../format'
import Notice from '../../components/Notice.vue'

const q = ref(''), rows = ref([]), error = ref(''), page = ref(0), size = 50
let t
async function load() { try { error.value = ''; rows.value = await admin.customers({ q: q.value, limit: size, offset: page.value * size }) } catch (e) { error.value = e.message } }
watch(q, () => { clearTimeout(t); t = setTimeout(() => { page.value = 0; load() }, 300) })
watch(page, load); onMounted(load)
</script>
