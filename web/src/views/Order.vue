<template>
  <div class="shell">
    <header class="top"><router-link to="/" class="brand">{{ cfg.brand }}</router-link></header>
    <h1>Your order</h1>

    <div class="card stack" style="margin-top:14px">
      <div v-if="state === 'waiting'" class="row"><span class="spinner"></span><span>Confirming your payment…</span></div>
      <template v-else-if="state === 'ready'">
        <div class="row between"><h2>{{ order.plan }}</h2><span class="chip ok">Paid</span></div>
        <template v-if="order.access_code">
          <p class="muted small" style="margin:0">Your access code. Keep it safe: anyone with it can use your plan.</p>
          <div class="code" aria-label="Access code">{{ order.access_code }}</div>
          <div class="row">
            <button class="btn ghost grow" type="button" @click="copy">{{ copied ? 'Copied' : 'Copy code' }}</button>
            <router-link class="btn grow" :to="{ path: '/connect', query: { code: order.access_code } }">Connect now</router-link>
          </div>
        </template>
        <p v-else class="muted">Your plan is on your account. <router-link to="/member/home">Open my account</router-link></p>
      </template>
      <template v-else-if="state === 'refunded'"><Notice text="This order has been refunded." kind="ok" /></template>
      <template v-else-if="state === 'failed'"><Notice text="The payment did not go through. You have not been charged." kind="bad" /><router-link class="btn block" to="/">Try again</router-link></template>
      <template v-else-if="state === 'slow'">
        <Notice text="We have not heard back from the bank yet. If you were charged, your plan will appear here within a few minutes. You can keep this page open or come back to it." kind="warn" />
        <button class="btn ghost block" type="button" @click="start">Check again</button>
      </template>
      <template v-else-if="state === 'review'"><Notice text="Your payment needs a manual check. We will contact you or refund it. Please keep this reference." kind="warn" /></template>
      <Notice :text="error" kind="bad" />
      <p class="muted small mono" style="margin:0">Reference: {{ reference }}</p>
    </div>

    <div v-if="state === 'ready' || state === 'review'" class="card stack">
      <h3>Problem with this order?</h3>
      <template v-if="refund === 'requested'"><Notice text="Your refund request has been received. We will review it." kind="ok" /></template>
      <template v-else-if="refund === 'processed'"><Notice text="Refund sent. It can take a few days to reach your account." kind="ok" /></template>
      <template v-else-if="refund === 'declined'"><Notice text="Your refund request was reviewed and not approved." kind="warn" /></template>
      <form v-else class="stack" @submit.prevent="askRefund">
        <div><label for="why">Tell us what went wrong</label><textarea id="why" v-model="reason" maxlength="500"></textarea></div>
        <Notice :text="refundError" kind="bad" />
        <button class="btn ghost block" :disabled="asking">Request a refund</button>
      </form>
    </div>
  </div>
</template>

<script setup>
import { ref, onMounted, onUnmounted } from 'vue'
import { portal, config } from '../api'
import Notice from '../components/Notice.vue'

const props = defineProps({ reference: String })
const cfg = config()
const state = ref('waiting'), order = ref({}), error = ref(''), copied = ref(false)
const refund = ref('none'), reason = ref(''), refundError = ref(''), asking = ref(false)
let timer = null, tries = 0

async function check() {
  try {
    order.value = await portal.orderStatus(props.reference)
    const s = order.value.status
    if (s === 'provisioned' || s === 'paid') { state.value = 'ready'; stop(); loadRefund() }
    else if (s === 'refunded') { state.value = 'refunded'; stop() }
    else if (s === 'failed' || s === 'expired') { state.value = 'failed'; stop() }
    else if (s === 'refund_flagged') { state.value = 'review'; stop(); loadRefund() }
    else if (++tries > 60) { state.value = 'slow'; stop() }
  } catch (e) { error.value = e.message; stop() }
}
function stop() { clearInterval(timer); timer = null }
function start() { tries = 0; state.value = 'waiting'; error.value = ''; stop(); check(); timer = setInterval(check, 3000) }
async function loadRefund() { try { refund.value = (await portal.refundStatus(props.reference)).status } catch { /* optional */ } }
async function askRefund() {
  asking.value = true; refundError.value = ''
  try { await portal.refundRequest(props.reference, reason.value); refund.value = 'requested' } catch (e) { refundError.value = e.message } finally { asking.value = false }
}
async function copy() { try { await navigator.clipboard.writeText(order.value.access_code); copied.value = true } catch { /* user can select the code */ } }
onMounted(start)
onUnmounted(stop)
</script>
