<script setup>
import { formatBytes } from '../utils/format'

defineProps({
  proxies: { type: Array, default: () => [] },
  loading: Boolean,
})
</script>

<template>
  <div class="client-proxy-list card">
    <div v-if="loading && proxies.length === 0" class="empty">加载中…</div>
    <div v-else-if="proxies.length === 0" class="empty">
      配置中还没有代理，去"配置"页面添加吧
    </div>
    <div v-else class="proxy-table">
      <div class="proxy-row proxy-row-head">
        <span>代理名</span>
        <span>活跃连接</span>
        <span>累计连接</span>
        <span>上行</span>
        <span>下行</span>
      </div>
      <div v-for="p in proxies" :key="p.name" class="proxy-row">
        <span class="proxy-name">{{ p.name }}</span>
        <span>{{ p.active_conns }}</span>
        <span class="faint">{{ p.total_conns }}</span>
        <span class="mono">{{ formatBytes(p.bytes_up) }}</span>
        <span class="mono">{{ formatBytes(p.bytes_down) }}</span>
      </div>
    </div>
  </div>
</template>

<style scoped>
.client-proxy-list {
  padding: 18px 22px;
}

.empty {
  padding: 40px 0;
  text-align: center;
  color: var(--color-text-faint);
  font-size: 13.5px;
}

.proxy-row {
  display: grid;
  grid-template-columns: 1.6fr 1fr 1fr 1fr 1fr;
  gap: 8px;
  align-items: center;
  padding: 9px 0;
  font-size: 13.5px;
  border-bottom: 1px solid var(--color-border);
}
.proxy-row:last-child {
  border-bottom: none;
}

.proxy-row-head {
  color: var(--color-text-faint);
  font-size: 11.5px;
  font-weight: 700;
  text-transform: uppercase;
  letter-spacing: 0.3px;
  border-bottom: 1px solid var(--color-border);
}

.proxy-name {
  font-weight: 600;
}

.faint {
  color: var(--color-text-faint);
}

.mono {
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  color: var(--color-text-muted);
}
</style>
