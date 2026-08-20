<script setup>
defineProps({
  label: String,
  value: [String, Number],
  sub: String,
  accent: { type: String, default: 'primary' }, // primary | success | warning | danger
})
</script>

<template>
  <div class="stat-card card">
    <div class="stat-label">{{ label }}</div>
    <div class="stat-value" :class="`accent-${accent}`">{{ value }}</div>
    <div v-if="sub" class="stat-sub">{{ sub }}</div>
  </div>
</template>

<style scoped>
.stat-card {
  padding: 20px 22px;
}
.stat-label {
  font-size: 12.5px;
  font-weight: 600;
  color: var(--color-text-muted);
  margin-bottom: 8px;
}
.stat-value {
  font-size: 26px;
  font-weight: 800;
  letter-spacing: -0.5px;
  color: var(--color-text);
}
.accent-success { color: var(--color-success); }
.accent-warning { color: var(--color-warning); }
.accent-danger { color: var(--color-danger); }
.stat-sub {
  font-size: 12px;
  color: var(--color-text-faint);
  margin-top: 4px;
}
</style>
