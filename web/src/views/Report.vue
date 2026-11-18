<template>
  <div class="shell">
    <header class="top"><router-link to="/" class="brand">{{ cfg.brand }}</router-link></header>
    <h1>Report a problem</h1>
    <p class="muted" style="margin:6px 0 16px">Tell us what went wrong and our team will call or message you.{{ cfg.supportPhone ? ' For urgent help call ' + cfg.supportPhone + '.' : '' }}</p>
    <div v-if="done" class="card stack"><Notice kind="ok" :text="`Thank you. Your reference is ${done}. We will get back to you on the number you gave.`" /><router-link class="btn block" to="/">Back to plans</router-link></div>
    <form v-else class="card stack" @submit.prevent="send">
      <div><label for="ph">Your phone number</label><input id="ph" v-model="f.phone" inputmode="tel" autocomplete="tel" required /></div>
      <div><label for="ct">What is it about?</label><select id="ct" v-model="f.category"><option v-for="c in cats" :key="c[0]" :value="c[0]">{{ c[1] }}</option></select></div>
      <div v-if="f.category === 'payment' || f.category === 'voucher'"><label for="or">Order reference or code (if you have it)</label><input id="or" v-model="f.order_ref" maxlength="60" /></div>
      <div><label for="ms">What happened?</label><textarea id="ms" v-model="f.message" minlength="3" maxlength="2000" required></textarea></div>
      <Notice :text="error" kind="bad" />
      <button class="btn block" :disabled="busy">Send</button>
    </form>
  </div>
</template>
<script setup>
import { ref } from 'vue'
import { portal, config } from '../api'
import Notice from '../components/Notice.vue'

const cfg = config()
const cats = [['payment', 'I paid but have no access'], ['connection', "I can't connect"], ['speed', 'The internet is slow'], ['voucher', 'My access code does not work'], ['account', 'My account'], ['other', 'Something else']]
const f = ref({ phone: '', category: 'connection', order_ref: '', message: '' }), busy = ref(false), error = ref(''), done = ref('')
async function send() {
  error.value = ''; busy.value = true
  try { done.value = (await portal.complain({ ...f.value, order_ref: f.value.order_ref || undefined })).reference } catch (e) { error.value = e.message } finally { busy.value = false }
}
</script>
