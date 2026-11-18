<template><div v-if="text" :class="['notice', kind]" :role="kind === 'bad' ? 'alert' : 'status'">{{ text }}</div></template>
<script setup>
defineProps({ text: String, kind: { type: String, default: '' } })
</script>
