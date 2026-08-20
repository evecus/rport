<script setup>
import { onMounted, onUnmounted, computed } from 'vue'
import { useRouter, useRoute } from 'vue-router'
import { useAuthStore } from '../stores/auth'
import { useAppStore } from '../stores/app'

const router = useRouter()
const route = useRoute()
const auth = useAuthStore()
const appStore = useAppStore()

onMounted(async () => {
  await appStore.refreshAll().catch(() => {})
  appStore.startPolling()
})
onUnmounted(() => appStore.stopPolling())

const modeLabel = computed(() => (appStore.mode === 'server' ? '服务端' : '客户端'))
const running = computed(() => appStore.status?.running)
const isRunnable = computed(() => appStore.status?.is_runnable)

async function handleLogout() {
  await auth.logout()
  router.push({ name: 'login' })
}

const navItems = [
  { name: 'dashboard', label: '概览', icon: 'grid' },
  { name: 'config', label: '配置', icon: 'sliders' },
]
</script>

<template>
  <div class="shell">
    <aside class="sidebar">
      <div class="brand">
        <div class="brand-mark">tx</div>
        <div class="brand-text">
          <div class="brand-name">tunx</div>
          <div class="brand-sub">{{ modeLabel }}模式</div>
        </div>
      </div>

      <nav class="nav">
        <router-link
          v-for="item in navItems"
          :key="item.name"
          :to="{ name: item.name }"
          class="nav-item"
          :class="{ active: route.name === item.name }"
        >
          <span class="nav-icon" v-html="iconSvg(item.icon)"></span>
          {{ item.label }}
        </router-link>
      </nav>

      <div class="sidebar-footer">
        <div class="status-line">
          <span
            class="dot"
            :class="running ? 'dot-success' : (isRunnable ? 'dot-warning' : 'dot-danger')"
          ></span>
          <span class="status-text">
            {{ running ? '运行中' : (isRunnable ? '已停止' : '待配置') }}
          </span>
        </div>
        <button class="btn btn-ghost logout-btn" @click="handleLogout">退出登录</button>
      </div>
    </aside>

    <main class="content">
      <router-view v-slot="{ Component }">
        <transition name="fade" mode="out-in">
          <component :is="Component" />
        </transition>
      </router-view>
    </main>
  </div>
</template>

<script>
function iconSvg(name) {
  const icons = {
    grid: '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="3" y="3" width="7" height="7" rx="1.5"/><rect x="14" y="3" width="7" height="7" rx="1.5"/><rect x="3" y="14" width="7" height="7" rx="1.5"/><rect x="14" y="14" width="7" height="7" rx="1.5"/></svg>',
    sliders: '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><line x1="4" y1="6" x2="20" y2="6"/><circle cx="9" cy="6" r="2" fill="currentColor" stroke="none"/><line x1="4" y1="12" x2="20" y2="12"/><circle cx="15" cy="12" r="2" fill="currentColor" stroke="none"/><line x1="4" y1="18" x2="20" y2="18"/><circle cx="7" cy="18" r="2" fill="currentColor" stroke="none"/></svg>',
  }
  return icons[name] || ''
}
export default { methods: { iconSvg } }
</script>

<style scoped>
.shell {
  display: flex;
  min-height: 100vh;
}

.sidebar {
  width: 232px;
  flex-shrink: 0;
  background: var(--color-surface);
  border-right: 1px solid var(--color-border);
  display: flex;
  flex-direction: column;
  padding: 20px 14px;
  position: sticky;
  top: 0;
  height: 100vh;
}

.brand {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 8px 10px 22px;
}

.brand-mark {
  width: 36px;
  height: 36px;
  border-radius: 10px;
  background: var(--gradient-primary);
  color: white;
  font-weight: 800;
  font-size: 14px;
  display: flex;
  align-items: center;
  justify-content: center;
  letter-spacing: -0.5px;
  box-shadow: 0 4px 12px rgba(99, 102, 241, 0.3);
}

.brand-name {
  font-weight: 800;
  font-size: 16px;
  letter-spacing: -0.3px;
}
.brand-sub {
  font-size: 12px;
  color: var(--color-text-faint);
}

.nav {
  display: flex;
  flex-direction: column;
  gap: 2px;
  flex: 1;
}

.nav-item {
  display: flex;
  align-items: center;
  gap: 10px;
  padding: 10px 12px;
  border-radius: 10px;
  color: var(--color-text-muted);
  font-weight: 600;
  font-size: 14px;
  transition: all 0.15s var(--ease);
}
.nav-item:hover {
  background: var(--color-bg);
  color: var(--color-text);
}
.nav-item.active {
  background: var(--color-primary-soft);
  color: var(--color-primary);
}
.nav-icon {
  display: flex;
  opacity: 0.85;
}

.sidebar-footer {
  padding-top: 12px;
  border-top: 1px solid var(--color-border);
  display: flex;
  flex-direction: column;
  gap: 10px;
}

.status-line {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 8px 10px;
  font-size: 13px;
  font-weight: 600;
  color: var(--color-text-muted);
}

.logout-btn {
  width: 100%;
  justify-content: flex-start;
  font-size: 13px;
}

.content {
  flex: 1;
  min-width: 0;
  padding: 32px 40px;
  max-width: 1200px;
}

@media (max-width: 900px) {
  .sidebar { width: 76px; }
  .brand-text, .nav-item span:not(.nav-icon), .status-text { display: none; }
  .content { padding: 20px; }
}
</style>
