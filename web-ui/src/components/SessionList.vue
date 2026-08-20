<script setup>
import { formatBytes, formatDuration } from '../utils/format'

defineProps({
  sessions: { type: Array, default: () => [] },
  loading: Boolean,
})
</script>

<template>
  <div class="session-list">
    <div v-if="loading && sessions.length === 0" class="empty">加载中…</div>
    <div v-else-if="sessions.length === 0" class="empty">
      暂无客户端连接
    </div>
    <div v-else class="sessions">
      <div v-for="s in sessions" :key="s.session_id" class="session-card card">
        <div class="session-head">
          <div class="session-title">
            <span class="dot" :class="s.connected ? 'dot-success' : 'dot-danger'"></span>
            <span class="client-id">{{ s.client_id }}</span>
          </div>
          <span class="badge" :class="s.connected ? 'badge-success' : 'badge-danger'">
            {{ s.connected ? '在线' : '离线' }}
          </span>
        </div>
        <div class="session-meta">
          <span>session: {{ s.session_id.slice(0, 12) }}…</span>
          <span>最近心跳: {{ formatDuration(s.last_pong_secs_ago) }}</span>
        </div>

        <div v-if="s.proxies.length" class="proxy-table">
          <div class="proxy-row proxy-row-head">
            <span>代理名</span>
            <span>类型</span>
            <span>本地地址</span>
            <span>远程端口</span>
            <span>连接数</span>
            <span>上行 / 下行</span>
          </div>
          <div v-for="p in s.proxies" :key="p.name" class="proxy-row">
            <span class="proxy-name">{{ p.name }}</span>
            <span class="badge badge-neutral">{{ p.proxy_type.toUpperCase() }}</span>
            <span class="mono">{{ p.local_addr }}</span>
            <span class="mono">:{{ p.remote_port }}</span>
            <span>{{ p.active_conns }} <span class="faint">/ 累计 {{ p.total_conns }}</span></span>
            <span class="mono">{{ formatBytes(p.bytes_up) }} / {{ formatBytes(p.bytes_down) }}</span>
          </div>
        </div>
        <div v-else class="no-proxy">该客户端未注册任何代理</div>
      </div>
    </div>
  </div>
</template>

<style scoped>
.empty {
  padding: 48px 0;
  text-align: center;
  color: var(--color-text-faint);
  font-size: 13.5px;
}

.sessions {
  display: flex;
  flex-direction: column;
  gap: 14px;
}

.session-card {
  padding: 18px 22px;
}

.session-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 6px;
}

.session-title {
  display: flex;
  align-items: center;
  gap: 8px;
  font-weight: 700;
  font-size: 14.5px;
}

.client-id {
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
}

.session-meta {
  display: flex;
  gap: 18px;
  font-size: 12.5px;
  color: var(--color-text-faint);
  margin-bottom: 14px;
}

.proxy-table {
  border-top: 1px solid var(--color-border);
  padding-top: 12px;
}

.proxy-row {
  display: grid;
  grid-template-columns: 1.2fr 0.7fr 1.4fr 0.9fr 1.1fr 1.4fr;
  gap: 8px;
  align-items: center;
  padding: 7px 0;
  font-size: 13px;
}

.proxy-row-head {
  color: var(--color-text-faint);
  font-size: 11.5px;
  font-weight: 700;
  text-transform: uppercase;
  letter-spacing: 0.3px;
  padding-bottom: 8px;
}

.proxy-name {
  font-weight: 600;
}

.mono {
  font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  font-size: 12.5px;
  color: var(--color-text-muted);
}

.faint {
  color: var(--color-text-faint);
  font-size: 12px;
}

.no-proxy {
  border-top: 1px solid var(--color-border);
  padding-top: 12px;
  font-size: 12.5px;
  color: var(--color-text-faint);
}
</style>
